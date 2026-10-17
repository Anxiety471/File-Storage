import { useEffect, useState } from "react";
import { api, MemoryDetail, MemoryNote } from "./api";
import { MarkdownView } from "./viewers";

// Obsidian-style display: hide frontmatter (tags show as chips) and turn [[links]] into in-panel links.
const prepare = (text: string) => text
  .replace(/^---\r?\n[\s\S]*?\r?\n---\r?\n?/, "")
  .replace(/\[\[([^\]|#]+)(?:#[^\]|]*)?(?:\|([^\]]+))?\]\]/g, (_, target: string, alias?: string) => `[${alias ?? target}](#memory:${encodeURIComponent(target.trim())})`);

export function MemoryPanel({ close }: { close: () => void }) {
  const [notes, setNotes] = useState<MemoryNote[]>([]);
  const [tags, setTags] = useState<{ tag: string; count: number }[]>([]);
  const [query, setQuery] = useState("");
  const [tag, setTag] = useState("");
  const [open, setOpen] = useState<MemoryDetail | null>(null);
  const [error, setError] = useState("");
  useEffect(() => { api.memoryTags().then(setTags).catch((e) => setError(String(e))); }, []);
  useEffect(() => { let live = true; api.memoryNotes(query, tag).then((n) => { if (live) setNotes(n); }).catch((e) => { if (live) setError(String(e)); }); return () => { live = false; }; }, [query, tag]);
  const show = (name: string) => { setError(""); api.memoryNote(name).then(setOpen).catch((e) => setError(String(e))); };
  const onClick = (e: React.MouseEvent) => {
    const href = (e.target as HTMLElement).closest("a")?.getAttribute("href");
    if (href?.startsWith("#memory:")) { e.preventDefault(); show(decodeURIComponent(href.slice(8))); }
  };
  return <div className="modal-scrim" onMouseDown={close}><section className="modal operations-modal memory-modal" role="dialog" aria-modal="true" aria-labelledby="memory-title" onMouseDown={(e) => e.stopPropagation()}>
    <h3 id="memory-title">Memories</h3><p>Markdown notes AI agents save in /memory. Link notes with [[Note Name]] and tag with #tag.</p>
    {error && <p role="alert">{error}</p>}
    {open ? <div onClick={onClick}>
      <button className="btn" onClick={() => setOpen(null)}>Back to list</button>
      <h4>{open.name}</h4>
      <div className="memory-tags">{open.tags.map((t) => <button key={t} className="btn memory-tag" onClick={() => { setTag(t); setOpen(null); }}>#{t}</button>)}</div>
      <MarkdownView source={prepare(open.content)} />
      {open.backlinks.length > 0 && <><h4>Linked from</h4><ul className="operations-list">{open.backlinks.map((b) => <li key={b.path}><a href={`#memory:${encodeURIComponent(b.name)}`}>{b.name}</a></li>)}</ul></>}
      <span className="memory-meta">{open.path} · revision {open.revision} · {open.modified_by}</span>
    </div> : <>
      <input type="search" placeholder="Search memories" aria-label="Search memories" value={query} onChange={(e) => setQuery(e.target.value)} />
      <div className="memory-tags">{tag && <button className="btn memory-tag" onClick={() => setTag("")}>#{tag} ✕</button>}{!tag && tags.map((t) => <button key={t.tag} className="btn memory-tag" onClick={() => setTag(t.tag)}>#{t.tag} {t.count}</button>)}</div>
      <ul className="operations-list">{notes.map((n) => <li key={n.path}><a href={`#memory:${encodeURIComponent(n.name)}`} onClick={(e) => { e.preventDefault(); show(n.name); }}><strong>{n.name}</strong></a>
        {n.snippet && <span>{n.snippet}</span>}<span>{n.tags.map((t) => `#${t}`).join(" ")} · {n.modified_by} · {new Date(n.modified_at).toLocaleString()}</span></li>)}</ul>
      {!notes.length && <p>No memories yet. Agents save them with the memory_remember tool or `drawer memory remember`.</p>}
    </>}
    <div className="modal-actions"><button className="btn" onClick={close}>Done</button></div>
  </section></div>;
}
