import { useCallback, useEffect, useState } from "react";
import { api, Entry, Generation, SearchHit } from "./api";

const parentOf = (p: string) => (p.lastIndexOf("/") <= 0 ? "/" : p.slice(0, p.lastIndexOf("/")));
const join = (dir: string, name: string) => (dir === "/" ? `/${name}` : `${dir}/${name}`);

export function App() {
  const [dir, setDir] = useState("/");
  const [entries, setEntries] = useState<Entry[]>([]);
  const [file, setFile] = useState<string | null>(null);
  const [content, setContent] = useState("");
  const [saved, setSaved] = useState("");
  const [generations, setGenerations] = useState<Generation[]>([]);
  const [viewing, setViewing] = useState<number | null>(null);
  // The live generation this editor was opened on; saves are guarded on it.
  const [loaded, setLoaded] = useState<number | null>(null);
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
    (path: string, generation?: number) =>
      run(async () => {
        const [text, gens] = await Promise.all([api.read(path, generation), api.generations(path)]);
        setFile(path);
        setContent(text);
        setSaved(text);
        setGenerations(gens);
        setLoaded(gens[0]?.generation ?? null);
        setViewing(generation ?? null);
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
                  {e.kind === "file" && <small>g{e.generation}</small>}
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
                  await api.write(path, "", "created in UI", 0);
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
                {viewing && <span className="badge">viewing old generation {viewing} (read-only)</span>}
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
                        await api.write(file, content, comment, loaded ?? undefined);
                        await openFile(file);
                        await loadDir(dir);
                      })
                    }
                  >
                    Save new generation
                  </button>
                </div>
              )}
              <h3>History</h3>
              <ul className="versions">
                {generations.map((g) => (
                  <li key={g.generation}>
                    <b>g{g.generation}</b> {g.author} · {new Date(g.created_at).toLocaleString()} · {g.size} B
                    {g.comment && <em> — {g.comment}</em>}
                    {Object.entries(g.metadata).map(([k, v]) => (
                      <code key={k}> {k}={v}</code>
                    ))}
                    <span className="spacer" />
                    <button onClick={() => openFile(file, g.generation === generations[0].generation ? undefined : g.generation)}>View</button>
                    {g.generation !== generations[0].generation && (
                      <button
                        onClick={() =>
                          confirm(`Roll back to generation ${g.generation}? This adds a new generation.`) &&
                          run(async () => {
                            await api.rollback(file, g.generation);
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
