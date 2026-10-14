import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, ApiError, Node, Revision, SearchHit } from "./api";
import { Badge, extOf, fileType, isTextViewer } from "./filetypes";
import { Icon } from "./icons";
import { BinaryCard, CodeEditor, DocxView, HtmlView, ImageView, Loading, MarkdownView, MediaView, PdfView, SheetView } from "./viewers";

const BADGE_ICON = {
  doc: "file", pdf: "pdf", sheet: "sheet", slides: "slides", image: "image",
  code: "code", text: "file", media: "play", archive: "archive", other: "file",
} as const satisfies Record<Badge, string>;
const badgeIcon = (b: Badge) => BADGE_ICON[b];

function KindIcon({ node }: { node: Pick<Node, "kind" | "name" | "mime"> }) {
  if (node.kind === "folder")
    return (
      <span className="kind folder">
        <Icon name="folder" filled />
      </span>
    );
  const badge = fileType(node.name, node.mime).badge;
  return (
    <span className={`kind badge-${badge}`}>
      <Icon name={badgeIcon(badge)} />
    </span>
  );
}

type View =
  | { kind: "drive"; path: string }
  | { kind: "starred" }
  | { kind: "trash" }
  | { kind: "search"; q: string };

type Dialog =
  | { kind: "prompt"; title: string; label: string; initial: string; action: string; submit: (v: string) => Promise<void> }
  | { kind: "confirm"; title: string; body: string; action: string; danger?: boolean; submit: () => Promise<void> };

type Toast = { text: string; tone: "ok" | "error" };

const parentOf = (p: string) => (p.lastIndexOf("/") <= 0 ? "/" : p.slice(0, p.lastIndexOf("/")));
const join = (dir: string, name: string) => (dir === "/" ? `/${name}` : `${dir}/${name}`);

function formatSize(n: number | null) {
  if (n === null) return "—";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

function formatWhen(iso: string) {
  const t = new Date(iso).getTime();
  const s = Math.round((Date.now() - t) / 1000);
  if (s < 45) return "just now";
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86400) return `${Math.round(s / 3600)} h ago`;
  return new Date(iso).toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" });
}

const sortNodes = (ns: Node[]) =>
  [...ns].sort((a, b) => (a.kind === b.kind ? a.name.localeCompare(b.name) : a.kind === "folder" ? -1 : 1));

export function App() {
  const [view, setView] = useState<View>({ kind: "drive", path: "/" });
  const [nodes, setNodes] = useState<Node[]>([]);
  const [hits, setHits] = useState<SearchHit[]>([]);
  const [loading, setLoading] = useState(true);
  const [query, setQuery] = useState("");
  const [file, setFile] = useState<string | null>(null);
  const [dirty, setDirty] = useState(false);
  const [dialog, setDialog] = useState<Dialog | null>(null);
  const [toast, setToast] = useState<Toast | null>(null);
  const [newMenu, setNewMenu] = useState(false);

  const notify = useCallback((text: string, tone: Toast["tone"] = "ok") => setToast({ text, tone }), []);
  useEffect(() => {
    if (!toast) return;
    const t = setTimeout(() => setToast(null), toast.tone === "error" ? 6000 : 3000);
    return () => clearTimeout(t);
  }, [toast]);

  const run = useCallback(
    async (fn: () => Promise<void>) => {
      try {
        await fn();
      } catch (e) {
        notify(e instanceof Error ? e.message : String(e), "error");
      }
    },
    [notify],
  );

  const refresh = useCallback(
    () =>
      run(async () => {
        setLoading(true);
        try {
          if (view.kind === "drive") setNodes(sortNodes(await api.tree(view.path)));
          else if (view.kind === "starred") setNodes(sortNodes(await api.starred()));
          else if (view.kind === "trash") setNodes(await api.trashed());
          else setHits(await api.search(view.q));
        } finally {
          setLoading(false);
        }
      }),
    [run, view],
  );

  useEffect(() => {
    refresh();
  }, [refresh]);

  // Leaving the editor with unsaved text asks first.
  const guard = useCallback(
    (go: () => void) => {
      if (!file || !dirty) return go();
      setDialog({
        kind: "confirm",
        title: "Discard unsaved changes?",
        body: "Your edits to this file have not been saved as a revision.",
        action: "Discard",
        danger: true,
        submit: async () => {
          setDirty(false);
          go();
        },
      });
    },
    [file, dirty],
  );

  const navigate = (v: View) =>
    guard(() => {
      setFile(null);
      setView(v);
    });

  const currentDir = view.kind === "drive" ? view.path : "/";

  const newFile = () => {
    setNewMenu(false);
    setDialog({
      kind: "prompt",
      title: "New file",
      label: `Create in ${currentDir}`,
      initial: "untitled.md",
      action: "Create",
      submit: async (name) => {
        const path = join(currentDir, name);
        await api.write(path, "", "created in UI", 0); // create only, never overwrite
        if (view.kind !== "drive") setView({ kind: "drive", path: currentDir });
        await refresh();
        setFile(path);
      },
    });
  };

  const uploadInput = useRef<HTMLInputElement>(null);
  const [dropping, setDropping] = useState(false);

  // Uploading onto an existing name adds a revision to that file, so nothing is overwritten for good.
  const upload = (files: File[]) =>
    run(async () => {
      if (files.length === 0) return;
      const dir = currentDir;
      notify(files.length === 1 ? `Uploading “${files[0].name}”…` : `Uploading ${files.length} files…`);
      let done = 0;
      for (const f of files) {
        await api.write(join(dir, f.name), f, `uploaded ${f.name}`);
        done++;
      }
      if (view.kind !== "drive") setView({ kind: "drive", path: dir });
      else await refresh();
      notify(done === 1 ? `Uploaded “${files[0].name}”` : `Uploaded ${done} files`);
    });

  const newFolder = () => {
    setNewMenu(false);
    setDialog({
      kind: "prompt",
      title: "New folder",
      label: `Create in ${currentDir}`,
      initial: "",
      action: "Create",
      submit: async (name) => {
        await api.mkdir(join(currentDir, name));
        if (view.kind !== "drive") setView({ kind: "drive", path: currentDir });
        else await refresh();
      },
    });
  };

  const rename = (n: Node) =>
    setDialog({
      kind: "prompt",
      title: `Rename ${n.kind}`,
      label: "New name",
      initial: n.name,
      action: "Rename",
      submit: async (name) => {
        await api.update(n.id, { name });
        await refresh();
      },
    });

  const toggleStar = (n: Node) =>
    run(async () => {
      await api.update(n.id, { starred: !n.starred });
      await refresh();
    });

  const trash = (n: Node, after?: () => void) =>
    run(async () => {
      await api.update(n.id, { trashed: true });
      after?.();
      await refresh();
      notify(`Moved “${n.name}” to the trash`);
    });

  const restore = (n: Node) =>
    run(async () => {
      await api.update(n.id, { trashed: false });
      await refresh();
      notify(`Restored “${n.name}”`);
    });

  const purge = (n: Node) =>
    setDialog({
      kind: "confirm",
      title: `Delete “${n.name}” forever?`,
      body: "Every revision is removed. This cannot be undone.",
      action: "Delete forever",
      danger: true,
      submit: async () => {
        await api.purge(n.id);
        await refresh();
      },
    });

  const openNode = (n: Node) =>
    n.kind === "folder" ? navigate({ kind: "drive", path: n.path }) : guard(() => setFile(n.path));

  const title =
    view.kind === "starred" ? "Starred" : view.kind === "trash" ? "Trash" : view.kind === "search" ? `Results for “${view.q}”` : null;

  return (
    <div className="shell">
      <header className="topbar">
        <a className="brand" onClick={() => navigate({ kind: "drive", path: "/" })}>
          <span className="brand-mark">
            <Icon name="drive" size={18} />
          </span>
          Drawer
        </a>
        <form
          className="search"
          onSubmit={(e) => {
            e.preventDefault();
            const q = query.trim();
            if (q) navigate({ kind: "search", q });
          }}
        >
          <Icon name="search" size={18} />
          <input placeholder="Search names and contents" value={query} onChange={(e) => setQuery(e.target.value)} />
          {query && (
            <button
              type="button"
              className="icon-btn"
              aria-label="Clear search"
              onClick={() => {
                setQuery("");
                if (view.kind === "search") navigate({ kind: "drive", path: "/" });
              }}
            >
              <Icon name="close" size={16} />
            </button>
          )}
        </form>
      </header>

      <div className="body">
        <nav className="sidebar">
          <div className="new-wrap">
            <button className="new-btn" onClick={() => setNewMenu((v) => !v)}>
              <Icon name="plus" /> New
            </button>
            {newMenu && (
              <>
                <div className="scrim" onClick={() => setNewMenu(false)} />
                <div className="menu">
                  <button onClick={newFolder}>
                    <Icon name="folder" size={18} /> New folder
                  </button>
                  <button onClick={newFile}>
                    <Icon name="file" size={18} /> New file
                  </button>
                  <hr />
                  <button
                    onClick={() => {
                      setNewMenu(false);
                      uploadInput.current?.click();
                    }}
                  >
                    <Icon name="upload" size={18} /> Upload files
                  </button>
                </div>
              </>
            )}
          </div>
          <a className={view.kind === "drive" ? "nav active" : "nav"} onClick={() => navigate({ kind: "drive", path: "/" })}>
            <Icon name="drive" /> My Drawer
          </a>
          <a className={view.kind === "starred" ? "nav active" : "nav"} onClick={() => navigate({ kind: "starred" })}>
            <Icon name="star" /> Starred
          </a>
          <a className={view.kind === "trash" ? "nav active" : "nav"} onClick={() => navigate({ kind: "trash" })}>
            <Icon name="trash" /> Trash
          </a>
        </nav>

        <input
          ref={uploadInput}
          type="file"
          multiple
          hidden
          onChange={(e) => {
            const files = Array.from(e.target.files ?? []);
            e.target.value = "";
            upload(files);
          }}
        />
        <main
          className={dropping ? "content dropping" : "content"}
          onDragOver={(e) => {
            if (file || !e.dataTransfer.types.includes("Files")) return;
            e.preventDefault();
            setDropping(true);
          }}
          onDragLeave={(e) => {
            if (!e.currentTarget.contains(e.relatedTarget as globalThis.Node | null)) setDropping(false);
          }}
          onDrop={(e) => {
            if (file) return;
            e.preventDefault();
            setDropping(false);
            upload(Array.from(e.dataTransfer.files));
          }}
        >
          {dropping && (
            <div className="drop-hint">
              <Icon name="upload" size={32} />
              Drop to upload to {currentDir === "/" ? "My Drawer" : currentDir}
            </div>
          )}
          {file ? (
            <Editor
              key={file}
              path={file}
              run={run}
              notify={notify}
              setDialog={setDialog}
              onDirty={setDirty}
              onClose={() =>
                guard(() => {
                  setFile(null);
                  refresh();
                })
              }
              onTrash={(n) => trash(n, () => setFile(null))}
            />
          ) : (
            <>
              <div className="content-head">
                {title ? (
                  <h2>{title}</h2>
                ) : (
                  <Breadcrumbs path={currentDir} go={(p) => navigate({ kind: "drive", path: p })} />
                )}
              </div>
              {view.kind === "trash" && nodes.length > 0 && (
                <p className="hint">Items in the trash keep their history. Restore puts them back where they were.</p>
              )}
              {view.kind === "search" ? (
                <SearchResults hits={hits} loading={loading} open={(p) => guard(() => setFile(p))} />
              ) : (
                <FileTable
                  nodes={nodes}
                  loading={loading}
                  mode={view.kind === "trash" ? "trash" : view.kind === "starred" ? "starred" : "drive"}
                  open={openNode}
                  star={toggleStar}
                  rename={rename}
                  trash={(n) => trash(n)}
                  restore={restore}
                  purge={purge}
                  empty={
                    view.kind === "trash"
                      ? "Trash is empty."
                      : view.kind === "starred"
                        ? "Nothing starred yet. Star files and folders to find them here."
                        : "This folder is empty. Drop files here, or use New to upload or create one."
                  }
                />
              )}
            </>
          )}
        </main>
      </div>

      {dialog && <DialogBox dialog={dialog} close={() => setDialog(null)} notify={notify} />}
      {toast && (
        <div className={`toast ${toast.tone}`} role="status">
          {toast.text}
        </div>
      )}
    </div>
  );
}

function Breadcrumbs({ path, go }: { path: string; go: (p: string) => void }) {
  const parts = path.split("/").filter(Boolean);
  return (
    <div className="crumbs">
      <a onClick={() => go("/")}>My Drawer</a>
      {parts.map((p, i) => (
        <span key={i}>
          <Icon name="chevron" size={16} />
          <a onClick={() => go("/" + parts.slice(0, i + 1).join("/"))}>{p}</a>
        </span>
      ))}
    </div>
  );
}

function FileTable(props: {
  nodes: Node[];
  loading: boolean;
  mode: "drive" | "starred" | "trash";
  empty: string;
  open: (n: Node) => void;
  star: (n: Node) => void;
  rename: (n: Node) => void;
  trash: (n: Node) => void;
  restore: (n: Node) => void;
  purge: (n: Node) => void;
}) {
  const { nodes, mode } = props;
  if (!props.loading && nodes.length === 0) return <div className="empty">{props.empty}</div>;
  return (
    <table className="files">
      <thead>
        <tr>
          <th>Name</th>
          <th className="col-md">{mode === "drive" ? "Owner" : "Location"}</th>
          <th className="col-sm">{mode === "trash" ? "Trashed" : "Modified"}</th>
          <th className="col-sm num">Size</th>
          <th className="col-actions" aria-label="Actions" />
        </tr>
      </thead>
      <tbody>
        {nodes.map((n) => (
          <tr key={n.id} onDoubleClick={() => mode !== "trash" && props.open(n)}>
            <td>
              <button className="name" disabled={mode === "trash"} onClick={() => props.open(n)}>
                <KindIcon node={n} />
                <span className="label">{n.name}</span>
                {n.starred && mode !== "trash" && (
                  <span className="star-mark" title="Starred">
                    <Icon name="star" size={14} filled />
                  </span>
                )}
              </button>
            </td>
            <td className="col-md muted">{mode === "drive" ? n.owner : parentOf(n.path) === "/" ? "My Drawer" : parentOf(n.path)}</td>
            <td className="col-sm muted">{formatWhen(n.modified_at)}</td>
            <td className="col-sm num muted">{n.kind === "folder" ? "—" : formatSize(n.size)}</td>
            <td className="col-actions">
              <div className="row-actions">
                {mode === "trash" ? (
                  <>
                    <button className="icon-btn" title="Restore" aria-label={`Restore ${n.name}`} onClick={() => props.restore(n)}>
                      <Icon name="restore" size={18} />
                    </button>
                    <button className="icon-btn danger" title="Delete forever" aria-label={`Delete ${n.name} forever`} onClick={() => props.purge(n)}>
                      <Icon name="trash" size={18} />
                    </button>
                  </>
                ) : (
                  <>
                    <button className="icon-btn" title={n.starred ? "Unstar" : "Star"} aria-label={n.starred ? `Unstar ${n.name}` : `Star ${n.name}`} onClick={() => props.star(n)}>
                      <Icon name="star" size={18} filled={n.starred} />
                    </button>
                    <button className="icon-btn" title="Rename" aria-label={`Rename ${n.name}`} onClick={() => props.rename(n)}>
                      <Icon name="edit" size={18} />
                    </button>
                    <button className="icon-btn" title="Move to trash" aria-label={`Move ${n.name} to trash`} onClick={() => props.trash(n)}>
                      <Icon name="trash" size={18} />
                    </button>
                  </>
                )}
              </div>
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function SearchResults({ hits, loading, open }: { hits: SearchHit[]; loading: boolean; open: (p: string) => void }) {
  if (!loading && hits.length === 0) return <div className="empty">No files match. Search looks at names and text contents.</div>;
  return (
    <ul className="hits">
      {hits.map((h) => (
        <li key={h.id}>
          <button onClick={() => open(h.path)}>
            <KindIcon node={{ kind: "file", name: h.path.split("/").pop() ?? "", mime: null }} />
            <span>
              <span className="label">{h.path.split("/").pop()}</span>
              <span className="muted"> in {parentOf(h.path) === "/" ? "My Drawer" : parentOf(h.path)}</span>
              {h.matched === "content" && <span className="snippet">{h.snippet}</span>}
            </span>
          </button>
        </li>
      ))}
    </ul>
  );
}

function Editor(props: {
  path: string;
  run: (fn: () => Promise<void>) => Promise<void>;
  notify: (text: string, tone?: "ok" | "error") => void;
  setDialog: (d: Dialog) => void;
  onDirty: (d: boolean) => void;
  onClose: () => void;
  onTrash: (n: Node) => void;
}) {
  const { path, run, notify, onDirty, setDialog } = props;
  const name = path.split("/").pop() ?? "";
  const [node, setNode] = useState<Node | null>(null);
  const [revisions, setRevisions] = useState<Revision[]>([]);
  const [blob, setBlob] = useState<Blob | null>(null);
  const [content, setContent] = useState("");
  const [saved, setSaved] = useState("");
  // The head revision the editor was opened on; saves are guarded on it so a stale save gets a 412.
  const [base, setBase] = useState<number | null>(null);
  const [viewing, setViewing] = useState<Revision | null>(null);
  const [comment, setComment] = useState("");
  const [saving, setSaving] = useState(false);
  const [showHistory, setShowHistory] = useState(true);
  const [mode, setMode] = useState<"edit" | "preview" | null>(null);
  const uploadInput = useRef<HTMLInputElement>(null);

  const type = fileType(name, node?.mime);
  const editable = isTextViewer(type.viewer);
  const hasPreview = type.viewer === "markdown" || type.viewer === "html" || type.viewer === "csv";
  const dirty = editable && viewing === null && content !== saved;
  useEffect(() => onDirty(dirty), [dirty, onDirty]);
  useEffect(() => () => onDirty(false), [onDirty]);

  useEffect(() => {
    if (!dirty) return;
    const warn = (e: BeforeUnloadEvent) => e.preventDefault();
    window.addEventListener("beforeunload", warn);
    return () => window.removeEventListener("beforeunload", warn);
  }, [dirty]);

  const load = useCallback(
    (revision?: Revision) =>
      run(async () => {
        const [data, meta, revs] = await Promise.all([api.readBlob(path, revision?.revision), api.stat(path), api.revisions(path)]);
        const t = fileType(name, meta.mime);
        const text = isTextViewer(t.viewer) ? await data.text() : "";
        setNode(meta);
        setRevisions(revs);
        setBlob(data);
        setContent(text);
        if (!revision) {
          setSaved(text);
          setBase(meta.head_revision);
        }
        setViewing(revision ?? null);
        // Documents with a rendered form open on it; empty ones open in the editor.
        setMode((m) => m ?? (text.trim() ? "preview" : "edit"));
      }),
    [run, path, name],
  );

  useEffect(() => {
    load();
  }, [load]);

  const conflict = useCallback(
    () =>
      setDialog({
        kind: "confirm",
        title: "This file changed while you were editing",
        body: "Someone saved a newer revision. Loading it replaces your unsaved work, so copy anything you want to keep first.",
        action: "Load latest",
        submit: async () => {
          await load();
        },
      }),
    [setDialog, load],
  );

  const commit = useCallback(
    async (data: string | Blob, note: string) => {
      setSaving(true);
      try {
        const out = await api.write(path, data, note, base ?? undefined);
        setComment("");
        await load();
        notify(out.new_revision ? `Saved revision ${out.revision}` : "No changes to save");
      } catch (e) {
        if (e instanceof ApiError && e.status === 412) conflict();
        else notify(e instanceof Error ? e.message : String(e), "error");
      } finally {
        setSaving(false);
      }
    },
    [path, base, load, notify, conflict],
  );

  const save = useCallback(() => {
    if (dirty && !saving) commit(content, comment);
  }, [dirty, saving, commit, content, comment]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "s") {
        e.preventDefault();
        save();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [save]);

  const head = revisions[0]?.revision;
  const where = useMemo(() => (parentOf(path) === "/" ? "My Drawer" : parentOf(path)), [path]);

  // Opening another revision replaces the editor text, so unsaved edits are confirmed first.
  const openRevision = (r: Revision) => {
    const go = async () => {
      await load(r.revision === head ? undefined : r);
    };
    if (!dirty) return void go();
    setDialog({
      kind: "confirm",
      title: "Discard unsaved changes?",
      body: `Opening revision ${r.revision} replaces the text you have not saved yet.`,
      action: "Discard",
      danger: true,
      submit: go,
    });
  };

  const showPreview = hasPreview && mode === "preview";
  let body: React.ReactNode;
  if (!blob || !node) body = <Loading />;
  else if (editable && !showPreview)
    body = <CodeEditor value={content} onChange={setContent} lang={type.lang} readOnly={viewing !== null} />;
  else if (type.viewer === "markdown") body = <MarkdownView source={content} />;
  else if (type.viewer === "html") body = <HtmlView source={content} />;
  else if (type.viewer === "csv") body = <SheetView text={content} delimiter={extOf(name) === "tsv" ? "\t" : ","} />;
  else if (type.viewer === "pdf") body = <PdfView blob={blob} />;
  else if (type.viewer === "docx") body = <DocxView blob={blob} />;
  else if (type.viewer === "sheet") body = <SheetView blob={blob} />;
  else if (type.viewer === "image") body = <ImageView blob={blob} name={name} svg={extOf(name) === "svg"} />;
  else if (type.viewer === "audio" || type.viewer === "video") body = <MediaView blob={blob} kind={type.viewer} />;
  else
    body = (
      <BinaryCard name={name} label={type.label} size={formatSize(blob.size)} onDownload={() => run(() => api.download(path, viewing?.revision))} />
    );

  return (
    <div className="editor">
      <div className="editor-bar">
        <button className="icon-btn" aria-label="Back to folder" title="Back" onClick={props.onClose}>
          <Icon name="back" />
        </button>
        <span className={`kind badge-${type.badge}`}>
          <Icon name={badgeIcon(type.badge)} />
        </span>
        <div className="editor-title">
          <span className="label">{name}</span>
          <span className="muted small">
            {where} · {type.label}
            {node?.head_revision ? ` · revision ${node.head_revision}` : ""}
            {dirty && <span className="unsaved"> · Unsaved changes</span>}
          </span>
        </div>
        <span className="spacer" />
        {hasPreview && (
          <div className="segmented" role="tablist" aria-label="View mode">
            <button role="tab" aria-selected={mode === "preview"} className={mode === "preview" ? "on" : ""} onClick={() => setMode("preview")}>
              <Icon name="eye" size={16} /> Preview
            </button>
            <button role="tab" aria-selected={mode === "edit"} className={mode === "edit" ? "on" : ""} onClick={() => setMode("edit")}>
              <Icon name="code" size={16} /> Edit
            </button>
          </div>
        )}
        {node && (
          <>
            <button className="icon-btn" title="Download" aria-label="Download" onClick={() => run(() => api.download(path, viewing?.revision))}>
              <Icon name="download" />
            </button>
            <button
              className="icon-btn"
              title={node.starred ? "Unstar" : "Star"}
              aria-label={node.starred ? "Unstar" : "Star"}
              onClick={() => run(async () => setNode(await api.update(node.id, { starred: !node.starred })))}
            >
              <Icon name="star" filled={node.starred} />
            </button>
            <button className="icon-btn" title="Move to trash" aria-label="Move to trash" onClick={() => props.onTrash(node)}>
              <Icon name="trash" />
            </button>
            <button
              className={showHistory ? "icon-btn on" : "icon-btn"}
              title="Version history"
              aria-label="Version history"
              aria-pressed={showHistory}
              onClick={() => setShowHistory((v) => !v)}
            >
              <Icon name="history" />
            </button>
          </>
        )}
      </div>

      <div className="editor-body">
        <div className="editor-main">
          {viewing && (
            <div className="banner">
              <span>
                Viewing revision {viewing.revision} by {viewing.author}, {formatWhen(viewing.created_at)}. Read-only.
              </span>
              <span className="spacer" />
              <button className="btn" onClick={() => load()}>
                Back to latest
              </button>
              <button
                className="btn primary"
                onClick={() =>
                  setDialog({
                    kind: "confirm",
                    title: `Restore revision ${viewing.revision}?`,
                    body: "This adds a new revision with this content. Nothing in the history is lost.",
                    action: "Restore",
                    submit: async () => {
                      await api.rollback(path, viewing.revision);
                      await load();
                      notify(`Restored revision ${viewing.revision}`);
                    },
                  })
                }
              >
                Restore this version
              </button>
            </div>
          )}
          <div className="viewer">{body}</div>
          {viewing === null && editable && !showPreview && (
            <div className="save-bar">
              <input
                placeholder="Describe your change (optional)"
                value={comment}
                onChange={(e) => setComment(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && save()}
              />
              <button className="btn primary" disabled={!dirty || saving} onClick={save}>
                {saving ? "Saving…" : "Save revision"}
              </button>
            </div>
          )}
          {viewing === null && !editable && node && (
            <div className="save-bar">
              <input
                placeholder="Describe the new version (optional)"
                value={comment}
                onChange={(e) => setComment(e.target.value)}
              />
              <input
                ref={uploadInput}
                type="file"
                hidden
                onChange={(e) => {
                  const f = e.target.files?.[0];
                  e.target.value = "";
                  if (f) commit(f, comment || `uploaded ${f.name}`);
                }}
              />
              <button className="btn primary" disabled={saving} onClick={() => uploadInput.current?.click()}>
                <Icon name="upload" size={18} /> {saving ? "Uploading…" : "Upload new version"}
              </button>
            </div>
          )}
        </div>

        {showHistory && (
          <aside className="history">
            <h3>Version history</h3>
            <ol>
              {revisions.map((r) => {
                const active = viewing ? viewing.revision === r.revision : r.revision === head;
                return (
                  <li key={r.revision} className={active ? "active" : ""}>
                    <button onClick={() => openRevision(r)}>
                      <span className="rev-head">
                        <b>Revision {r.revision}</b>
                        {r.revision === head && <span className="pill">Current</span>}
                      </span>
                      <span className="muted small">
                        {r.author} · {formatWhen(r.created_at)} · {formatSize(r.size)}
                      </span>
                      {r.comment && <span className="rev-comment">{r.comment}</span>}
                      {Object.keys(r.properties).length > 0 && (
                        <span className="props">
                          {Object.entries(r.properties).map(([k, v]) => (
                            <code key={k}>
                              {k}: {v}
                            </code>
                          ))}
                        </span>
                      )}
                    </button>
                  </li>
                );
              })}
            </ol>
          </aside>
        )}
      </div>
    </div>
  );
}

function DialogBox({ dialog, close, notify }: { dialog: Dialog; close: () => void; notify: (t: string, tone?: "ok" | "error") => void }) {
  const [value, setValue] = useState(dialog.kind === "prompt" ? dialog.initial : "");
  const [busy, setBusy] = useState(false);
  const input = useRef<HTMLInputElement>(null);

  useEffect(() => {
    const el = input.current;
    if (!el) return;
    el.focus();
    // Select the name without its extension, like Drive does.
    const dot = el.value.lastIndexOf(".");
    el.setSelectionRange(0, dot > 0 ? dot : el.value.length);
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && close();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [close]);

  const submit = async () => {
    if (dialog.kind === "prompt" && !value.trim()) return;
    setBusy(true);
    try {
      if (dialog.kind === "prompt") await dialog.submit(value.trim());
      else await dialog.submit();
      close();
    } catch (e) {
      notify(e instanceof Error ? e.message : String(e), "error");
      setBusy(false);
    }
  };

  return (
    <div className="modal-scrim" onMouseDown={close}>
      <form
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="dialog-title"
        onMouseDown={(e) => e.stopPropagation()}
        onSubmit={(e) => {
          e.preventDefault();
          submit();
        }}
      >
        <h3 id="dialog-title">{dialog.title}</h3>
        {dialog.kind === "prompt" ? (
          <label>
            <span className="muted small">{dialog.label}</span>
            <input ref={input} value={value} onChange={(e) => setValue(e.target.value)} />
          </label>
        ) : (
          <p>{dialog.body}</p>
        )}
        <div className="modal-actions">
          <button type="button" className="btn" onClick={close}>
            Cancel
          </button>
          <button
            type="submit"
            className={dialog.kind === "confirm" && dialog.danger ? "btn danger" : "btn primary"}
            disabled={busy || (dialog.kind === "prompt" && !value.trim())}
            autoFocus={dialog.kind === "confirm"}
          >
            {dialog.action}
          </button>
        </div>
      </form>
    </div>
  );
}
