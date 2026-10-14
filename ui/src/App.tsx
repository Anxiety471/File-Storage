import { useCallback, useEffect, useState } from "react";
import { api, Entry, SearchHit, VersionInfo } from "./api";

const parentOf = (p: string) => (p.lastIndexOf("/") <= 0 ? "/" : p.slice(0, p.lastIndexOf("/")));
const join = (dir: string, name: string) => (dir === "/" ? `/${name}` : `${dir}/${name}`);

export function App() {
  const [dir, setDir] = useState("/");
  const [entries, setEntries] = useState<Entry[]>([]);
  const [file, setFile] = useState<string | null>(null);
  const [content, setContent] = useState("");
  const [saved, setSaved] = useState("");
  const [versions, setVersions] = useState<VersionInfo[]>([]);
  const [viewing, setViewing] = useState<number | null>(null);
  const [comment, setComment] = useState("");
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<SearchHit[] | null>(null);
  const [error, setError] = useState("");

  const run = useCallback(async (fn: () => Promise<void>) => {
    try {
      setError("");
      await fn();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  const loadDir = useCallback(
    (d: string) =>
      run(async () => {
        setEntries(await api.tree(d));
        setDir(d);
      }),
    [run],
  );

  const openFile = useCallback(
    (path: string, version?: number) =>
      run(async () => {
        const [text, vs] = await Promise.all([api.read(path, version), api.versions(path)]);
        setFile(path);
        setContent(text);
        setSaved(text);
        setVersions(vs);
        setViewing(version ?? null);
        setComment("");
      }),
    [run],
  );

  useEffect(() => {
    loadDir("/");
  }, [loadDir]);

  const crumbs = dir.split("/").filter(Boolean);

  return (
    <div className="app">
      <header>
        <h1>Drawer</h1>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            if (!query.trim()) return setHits(null);
            run(async () => setHits(await api.search(query)));
          }}
        >
          <input placeholder="Search names and contents" value={query} onChange={(e) => setQuery(e.target.value)} />
        </form>
      </header>
      {error && <div className="error">{error}</div>}
      <main>
        <aside>
          <nav className="crumbs">
            <a onClick={() => loadDir("/")}>/</a>
            {crumbs.map((c, i) => (
              <a key={i} onClick={() => loadDir("/" + crumbs.slice(0, i + 1).join("/"))}>
                {c}/
              </a>
            ))}
          </nav>
          {hits ? (
            <ul>
              {hits.map((h) => (
                <li key={h.path} onClick={() => openFile(h.path)}>
                  <b>{h.path}</b>
                  <small>{h.matched === "name" ? "name match" : h.snippet}</small>
                </li>
              ))}
              {hits.length === 0 && <li className="muted">No matches</li>}
              <li className="muted" onClick={() => setHits(null)}>
                clear search
              </li>
            </ul>
          ) : (
            <ul>
              {dir !== "/" && <li onClick={() => loadDir(parentOf(dir))}>..</li>}
              {entries.map((e) => (
                <li
                  key={e.path}
                  className={e.path === file ? "active" : ""}
                  onClick={() => (e.kind === "folder" ? loadDir(e.path) : openFile(e.path))}
                >
                  {e.kind === "folder" ? "▸ " : ""}
                  {e.name}
                  {e.kind === "file" && <small>v{e.version}</small>}
                </li>
              ))}
              {entries.length === 0 && <li className="muted">Empty folder</li>}
            </ul>
          )}
          <div className="actions">
            <button
              onClick={() => {
                const name = prompt("New file name");
                if (name) run(async () => {
                  const path = join(dir, name);
                  await api.write(path, "", "created in UI");
                  await loadDir(dir);
                  await openFile(path);
                });
              }}
            >
              New file
            </button>
            <button
              onClick={() => {
                const name = prompt("New folder name");
                if (name) run(async () => {
                  await api.mkdir(join(dir, name));
                  await loadDir(dir);
                });
              }}
            >
              New folder
            </button>
          </div>
        </aside>
        <section>
          {file ? (
            <>
              <div className="bar">
                <b>{file}</b>
                {viewing && <span className="badge">viewing old v{viewing} (read-only)</span>}
                <span className="spacer" />
                <button
                  onClick={() => {
                    if (confirm(`Delete ${file}? History is kept.`))
                      run(async () => {
                        await api.remove(file);
                        setFile(null);
                        await loadDir(dir);
                      });
                  }}
                >
                  Delete
                </button>
              </div>
              <textarea value={content} readOnly={viewing !== null} onChange={(e) => setContent(e.target.value)} />
              {viewing === null && (
                <div className="bar">
                  <input placeholder="Commit comment (optional)" value={comment} onChange={(e) => setComment(e.target.value)} />
                  <button
                    disabled={content === saved}
                    onClick={() =>
                      run(async () => {
                        await api.write(file, content, comment);
                        await openFile(file);
                        await loadDir(dir);
                      })
                    }
                  >
                    Save new version
                  </button>
                </div>
              )}
              <h3>History</h3>
              <ul className="versions">
                {versions.map((v) => (
                  <li key={v.version}>
                    <b>v{v.version}</b> {v.author} · {new Date(v.created_at).toLocaleString()} · {v.size} B
                    {v.comment && <em> — {v.comment}</em>}
                    <span className="spacer" />
                    <button onClick={() => openFile(file, v.version === versions[0].version ? undefined : v.version)}>View</button>
                    {v.version !== versions[0].version && (
                      <button
                        onClick={() =>
                          confirm(`Roll back to v${v.version}? This adds a new version.`) &&
                          run(async () => {
                            await api.rollback(file, v.version);
                            await openFile(file);
                          })
                        }
                      >
                        Roll back
                      </button>
                    )}
                  </li>
                ))}
              </ul>
            </>
          ) : (
            <p className="muted">Pick a file, or make a new one.</p>
          )}
        </section>
      </main>
    </div>
  );
}
