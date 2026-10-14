import { useCallback, useEffect, useState } from "react";
import { api, Node, Revision, SearchHit } from "./api";

const parentOf = (p: string) => (p.lastIndexOf("/") <= 0 ? "/" : p.slice(0, p.lastIndexOf("/")));
const join = (dir: string, name: string) => (dir === "/" ? `/${name}` : `${dir}/${name}`);

export function App() {
  const [dir, setDir] = useState("/");
  const [entries, setEntries] = useState<Node[]>([]);
  const [showTrash, setShowTrash] = useState(false);
  const [bin, setBin] = useState<Node[]>([]);
  const [file, setFile] = useState<string | null>(null);
  const [node, setNode] = useState<Node | null>(null);
  const [content, setContent] = useState("");
  const [saved, setSaved] = useState("");
  const [revisions, setRevisions] = useState<Revision[]>([]);
  const [viewing, setViewing] = useState<number | null>(null);
  // The head revision this editor was opened on; saves are guarded on it.
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
        setShowTrash(false);
      }),
    [run],
  );

  const loadBin = useCallback(() => run(async () => {
    setBin(await api.trashed());
    setShowTrash(true);
  }), [run]);

  const openFile = useCallback(
    (path: string, revision?: number) =>
      run(async () => {
        const [text, meta, revs] = await Promise.all([
          api.read(path, revision),
          api.stat(path),
          api.revisions(path),
        ]);
        setFile(path);
        setNode(meta);
        setContent(text);
        setSaved(text);
        setRevisions(revs);
        setLoaded(meta.head_revision);
        setViewing(revision ?? null);
        setComment("");
      }),
    [run],
  );

  useEffect(() => {
    loadDir("/");
  }, [loadDir]);

  const crumbs = dir.split("/").filter(Boolean);
  const head = revisions[0]?.revision;

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
            <a className="trash-link" onClick={loadBin}>
              Trash
            </a>
          </nav>
          {showTrash ? (
            <ul>
              {bin.map((n) => (
                <li key={n.id}>
                  {n.kind === "folder" ? "▸ " : ""}
                  {n.path}
                  <small>
                    <button onClick={() => run(async () => { await api.update(n.id, { trashed: false }); await loadBin(); })}>
                      Restore
                    </button>
                    <button
                      onClick={() =>
                        confirm(`Delete ${n.path} forever? This cannot be undone.`) &&
                        run(async () => { await api.purge(n.id); await loadBin(); })
                      }
                    >
                      Delete forever
                    </button>
                  </small>
                </li>
              ))}
              {bin.length === 0 && <li className="muted">Trash is empty</li>}
              <li className="muted" onClick={() => loadDir(dir)}>
                back to files
              </li>
            </ul>
          ) : hits ? (
            <ul>
              {hits.map((h) => (
                <li key={h.id} onClick={() => openFile(h.path)}>
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
                  key={e.id}
                  className={e.path === file ? "active" : ""}
                  onClick={() => (e.kind === "folder" ? loadDir(e.path) : openFile(e.path))}
                >
                  {e.kind === "folder" ? "▸ " : ""}
                  {e.starred ? "★ " : ""}
                  {e.name}
                  {e.kind === "file" && <small>r{e.head_revision}</small>}
                </li>
              ))}
              {entries.length === 0 && <li className="muted">Empty folder</li>}
            </ul>
          )}
          <div className="actions">
            <button
              onClick={() => {
                const name = prompt("New file name");
                if (name)
                  run(async () => {
                    const path = join(dir, name);
                    // create only: never overwrite a file that is already there
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
                if (name)
                  run(async () => {
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
          {file && node ? (
            <>
              <div className="bar">
                <b>{file}</b>
                {viewing && <span className="badge">viewing revision {viewing} (read-only)</span>}
                <span className="spacer" />
                <button
                  onClick={() =>
                    run(async () => {
                      setNode(await api.update(node.id, { starred: !node.starred }));
                    })
                  }
                >
                  {node.starred ? "Unstar" : "Star"}
                </button>
                <button
                  onClick={() => {
                    if (confirm(`Move ${file} to the trash? You can restore it from there.`))
                      run(async () => {
                        await api.trash(file);
                        setFile(null);
                        setNode(null);
                        await loadDir(dir);
                      });
                  }}
                >
                  Move to trash
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
                    Save new revision
                  </button>
                </div>
              )}
              <h3>History</h3>
              <ul className="versions">
                {revisions.map((v) => (
                  <li key={v.revision}>
                    <b>r{v.revision}</b> {v.author} · {new Date(v.created_at).toLocaleString()} · {v.size} B
                    {v.comment && <em> — {v.comment}</em>}
                    {Object.entries(v.properties).map(([k, x]) => (
                      <code key={k}> {k}={x}</code>
                    ))}
                    <span className="spacer" />
                    <button onClick={() => openFile(file, v.revision === head ? undefined : v.revision)}>View</button>
                    {v.revision !== head && (
                      <button
                        onClick={() =>
                          confirm(`Roll back to revision ${v.revision}? This adds a new revision.`) &&
                          run(async () => {
                            await api.rollback(file, v.revision);
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
