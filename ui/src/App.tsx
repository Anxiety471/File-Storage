import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, ApiError, Diff, Node, Revision, SearchHit, setToken, UNAUTHORIZED_EVENT } from "./api";
import { Badge, extOf, fileType, isTextViewer } from "./filetypes";
import { Icon } from "./icons";
import { BinaryCard, CodeEditor, DiffView, DocxView, HtmlView, ImageView, Loading, MarkdownView, MediaView, PdfView, SheetView } from "./viewers";

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
  | { kind: "recent" }
  | { kind: "starred" }
  | { kind: "trash" }
  | { kind: "search"; q: string };

type Dialog =
  | {
      kind: "prompt";
      title: string;
      label: string;
      initial: string;
      action: string;
      secret?: boolean;
      /** Select only the name before the extension, like Drive. Off for paths and tokens. */
      selectStem?: boolean;
      submit: (v: string) => Promise<void>;
    }
  | { kind: "confirm"; title: string; body: string; action: string; danger?: boolean; submit: () => Promise<void> };

type Toast = { text: string; tone: "ok" | "error"; action?: { label: string; run: () => void } };

type SortKey = "name" | "modified" | "size";
type Sort = { key: SortKey; dir: 1 | -1 };

/** How often open folders and files check for changes made by agents or other people. */
const POLL_MS = 5000;

// The view and open file live in the URL hash, so reload, back/forward and links work.
const encPath = (p: string) => p.split("/").map(encodeURIComponent).join("/");

function toHash(view: View, file: string | null): string {
  let h =
    view.kind === "drive"
      ? `#/drive${view.path === "/" ? "" : encPath(view.path)}`
      : view.kind === "search"
        ? `#/search/${encodeURIComponent(view.q)}`
        : `#/${view.kind}`;
  if (file) h += `?file=${encodeURIComponent(file)}`;
  return h;
}

function fromHash(hash: string): { view: View; file: string | null } {
  const raw = hash.replace(/^#/, "");
  const at = raw.indexOf("?");
  const route = at < 0 ? raw : raw.slice(0, at);
  const file = at < 0 ? null : new URLSearchParams(raw.slice(at + 1)).get("file");
  const decode = (s: string) => {
    try {
      return decodeURIComponent(s);
    } catch {
      return s;
    }
  };
  let view: View = { kind: "drive", path: "/" };
  if (route === "/recent" || route === "/starred" || route === "/trash") view = { kind: route.slice(1) as "recent" | "starred" | "trash" };
  else if (route.startsWith("/search/")) view = { kind: "search", q: decode(route.slice(8)) };
  else if (route.startsWith("/drive/")) view = { kind: "drive", path: decode(route.slice(6)) };
  return { view, file };
}

function loadSort(): Sort {
  try {
    const s = JSON.parse(localStorage.getItem("drawer_sort") ?? "");
    if (["name", "modified", "size"].includes(s.key) && (s.dir === 1 || s.dir === -1)) return s;
  } catch {
    // no saved sort
  }
  return { key: "name", dir: 1 };
}

// Folders first, then by the chosen column; name breaks ties.
function sortNodes(ns: Node[], sort: Sort = { key: "name", dir: 1 }) {
  const by = (a: Node, b: Node) => {
    const name = a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" });
    let d = 0;
    if (sort.key === "modified") d = a.modified_at.localeCompare(b.modified_at);
    else if (sort.key === "size") d = (a.size ?? 0) - (b.size ?? 0);
    return (d || name) * sort.dir;
  };
  return [...ns].sort((a, b) => (a.kind === b.kind ? by(a, b) : a.kind === "folder" ? -1 : 1));
}

/** Cheap fingerprint of a listing, so polling only re-renders when something changed. */
const fingerprint = (ns: Node[]) => ns.map((n) => `${n.id}:${n.name}:${n.modified_at}:${n.starred}:${n.head_revision}`).join("|");

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


export function App() {
  const initial = useMemo(() => fromHash(window.location.hash), []);
  const [view, setView] = useState<View>(initial.view);
  const [nodes, setNodes] = useState<Node[]>([]);
  const [hits, setHits] = useState<SearchHit[]>([]);
  const [loading, setLoading] = useState(true);
  const [query, setQuery] = useState(initial.view.kind === "search" ? initial.view.q : "");
  const [file, setFile] = useState<string | null>(initial.file);
  const [dirty, setDirty] = useState(false);
  const [dialog, setDialog] = useState<Dialog | null>(null);
  const [toast, setToast] = useState<Toast | null>(null);
  const [newMenu, setNewMenu] = useState(false);
  const [sort, setSort] = useState<Sort>(loadSort);
  const searchInput = useRef<HTMLInputElement>(null);

  const notify = useCallback(
    (text: string, tone: Toast["tone"] = "ok", action?: Toast["action"]) => setToast({ text, tone, action }),
    [],
  );
  useEffect(() => {
    if (!toast) return;
    const t = setTimeout(() => setToast(null), toast.tone === "error" || toast.action ? 6000 : 3000);
    return () => clearTimeout(t);
  }, [toast]);

  const run = useCallback(
    async (fn: () => Promise<void>) => {
      try {
        await fn();
      } catch (e) {
        if (e instanceof ApiError && e.status === 401) return; // the token dialog explains it
        notify(e instanceof Error ? e.message : String(e), "error");
      }
    },
    [notify],
  );

  const load = useCallback(async () => {
    if (view.kind === "drive") return api.tree(view.path);
    if (view.kind === "starred") return api.starred();
    if (view.kind === "trash") return api.trashed();
    if (view.kind === "recent") return api.recent();
    return null;
  }, [view]);
  // Recent keeps the server's newest-first order; everything else follows the chosen column.
  const shown = useMemo(() => (view.kind === "recent" ? nodes : sortNodes(nodes, sort)), [nodes, sort, view.kind]);

  const refresh = useCallback(
    () =>
      run(async () => {
        setLoading(true);
        try {
          if (view.kind === "search") setHits(await api.search(view.q));
          else setNodes((await load()) ?? []);
        } finally {
          setLoading(false);
        }
      }),
    [run, view, load],
  );

  useEffect(() => {
    refresh();
  }, [refresh]);

  // Agents write while people watch: re-check the open folder now and then, and when the tab
  // comes back into view. Quiet: no spinner, no error toasts, no re-render if nothing changed.
  const dialogOpen = dialog !== null;
  useEffect(() => {
    if (file || view.kind === "search") return;
    const tick = async () => {
      if (document.visibilityState !== "visible" || dialogOpen) return;
      try {
        const next = await load();
        if (next) setNodes((prev) => (fingerprint(prev) === fingerprint(next) ? prev : next));
      } catch {
        // the next explicit action reports errors
      }
    };
    const t = setInterval(tick, POLL_MS);
    document.addEventListener("visibilitychange", tick);
    return () => {
      clearInterval(t);
      document.removeEventListener("visibilitychange", tick);
    };
  }, [file, view, load, dialogOpen]);

  // A 401 means the server runs with a token this browser doesn't have yet.
  const asking = useRef(false);
  useEffect(() => {
    const ask = () => {
      if (asking.current) return;
      asking.current = true;
      setDialog({
        kind: "prompt",
        title: "Access token",
        label: "This Drawer runs with DRAWER_TOKEN. Paste the token to continue; it is kept in this browser.",
        initial: "",
        action: "Continue",
        secret: true,
        submit: async (v) => {
          setToken(v);
          asking.current = false;
          await refresh();
        },
      });
    };
    window.addEventListener(UNAUTHORIZED_EVENT, ask);
    return () => window.removeEventListener(UNAUTHORIZED_EVENT, ask);
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

  // Keep the URL in step with what is on screen, and follow back/forward.
  const popping = useRef(false);
  useEffect(() => {
    const h = toHash(view, file);
    if (window.location.hash !== h) {
      // A change that came from back/forward only tidies the URL; it must not add history.
      if (window.location.hash && !popping.current) window.history.pushState(null, "", h);
      else window.history.replaceState(null, "", h);
    }
    popping.current = false;
    document.title = file ? `${file.split("/").pop()} – Drawer` : "Drawer";
  }, [view, file]);
  const current = useRef({ view, file, dirty, guard });
  current.current = { view, file, dirty, guard };
  useEffect(() => {
    const onPop = () => {
      const next = fromHash(window.location.hash);
      const { view: v, file: f, dirty: d, guard: g } = current.current;
      const go = () => {
        setView(next.view);
        setFile(next.file);
        if (next.view.kind === "search") setQuery(next.view.q);
      };
      if (!f || !d) {
        popping.current = true;
        return go();
      }
      // Unsaved edits: put the URL back until the guard lets the change through.
      window.history.replaceState(null, "", toHash(v, f));
      g(go);
    };
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, []);

  // "/" jumps to search, like most file browsers and code hosts.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement | null;
      const typing = t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable);
      if (e.key === "/" && !typing && !e.metaKey && !e.ctrlKey && !e.altKey) {
        e.preventDefault();
        searchInput.current?.focus();
        searchInput.current?.select();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const navigate = (v: View) =>
    guard(() => {
      setFile(null);
      setView(v);
    });

  const changeSort = (key: SortKey) => {
    const next: Sort = { key, dir: sort.key === key ? (sort.dir === 1 ? -1 : 1) : key === "name" ? 1 : -1 };
    setSort(next);
    try {
      localStorage.setItem("drawer_sort", JSON.stringify(next));
    } catch {
      // storage blocked: the sort lasts until reload
    }
  };

  const currentDir = view.kind === "drive" ? view.path : "/";

  const newFile = () => {
    setNewMenu(false);
    setDialog({
      kind: "prompt",
      title: "New file",
      label: `Create in ${currentDir === "/" ? "My Drawer" : currentDir}`,
      initial: "untitled.md",
      action: "Create",
      selectStem: true,
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
        if (files.length > 1) notify(`Uploading ${done} of ${files.length}…`);
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
      label: `Create in ${currentDir === "/" ? "My Drawer" : currentDir}`,
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
      selectStem: n.kind === "file",
      submit: async (name) => {
        await api.update(n.id, { name });
        await refresh();
      },
    });

  const move = (n: Node) =>
    setDialog({
      kind: "prompt",
      title: `Move “${n.name}”`,
      label: "New path. Missing folders are created; the id and history stay the same.",
      initial: n.path,
      action: "Move",
      submit: async (to) => {
        const moved = await api.move(n.id, to.startsWith("/") ? to : `/${to}`);
        await refresh();
        notify(`Moved to ${moved.path}`, "ok", {
          label: "Open folder",
          run: () => navigate({ kind: "drive", path: parentOf(moved.path) }),
        });
      },
    });

  const copy = (n: Node) =>
    setDialog({
      kind: "prompt",
      title: `Copy “${n.name}”`,
      label: "Path of the copy. The content is shared, not uploaded again.",
      initial: join(parentOf(n.path), `Copy of ${n.name}`),
      action: "Copy",
      submit: async (to) => {
        const out = await api.copy(n.id, to.startsWith("/") ? to : `/${to}`);
        await refresh();
        notify(`Copied to ${out.node.path}`);
      },
    });

  const toggleStar = (n: Node) =>
    run(async () => {
      await api.update(n.id, { starred: !n.starred });
      await refresh();
    });

  const restore = (n: Node) =>
    run(async () => {
      await api.update(n.id, { trashed: false });
      await refresh();
      notify(`Restored “${n.name}”`);
    });

  const trash = (n: Node, after?: () => void) =>
    run(async () => {
      await api.update(n.id, { trashed: true });
      after?.();
      await refresh();
      notify(`Moved “${n.name}” to the trash`, "ok", { label: "Undo", run: () => restore(n) });
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
    view.kind === "starred"
      ? "Starred"
      : view.kind === "trash"
        ? "Trash"
        : view.kind === "recent"
          ? "Recent"
          : view.kind === "search"
            ? `Results for “${view.q}”`
            : null;

  const navItem = (kind: "drive" | "recent" | "starred" | "trash", icon: Parameters<typeof Icon>[0]["name"], label: string) => (
    <a
      href={kind === "drive" ? "#/drive" : `#/${kind}`}
      className={view.kind === kind ? "nav active" : "nav"}
      aria-current={view.kind === kind ? "page" : undefined}
      title={label}
      onClick={(e) => {
        e.preventDefault();
        navigate(kind === "drive" ? { kind, path: "/" } : { kind });
      }}
    >
      <Icon name={icon} /> <span className="nav-label">{label}</span>
    </a>
  );

  return (
    <div className="shell">
      <header className="topbar">
        <a
          className="brand"
          href="#/drive"
          onClick={(e) => {
            e.preventDefault();
            navigate({ kind: "drive", path: "/" });
          }}
        >
          <span className="brand-mark">
            <Icon name="drive" size={18} />
          </span>
          <span className="brand-name">Drawer</span>
        </a>
        <form
          className="search"
          role="search"
          onSubmit={(e) => {
            e.preventDefault();
            const q = query.trim();
            if (q) navigate({ kind: "search", q });
          }}
        >
          <Icon name="search" size={18} />
          <input
            ref={searchInput}
            placeholder="Search names and contents"
            aria-label="Search names and contents"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => e.key === "Escape" && e.currentTarget.blur()}
          />
          {query ? (
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
          ) : (
            <kbd className="kbd" title="Press / to search">
              /
            </kbd>
          )}
        </form>
      </header>

      <div className="body">
        <nav className="sidebar" aria-label="Drawer">
          <div className="new-wrap">
            <button className="new-btn" aria-haspopup="menu" aria-expanded={newMenu} onClick={() => setNewMenu((v) => !v)}>
              <Icon name="plus" /> <span className="nav-label">New</span>
            </button>
            {newMenu && (
              <>
                <div className="scrim" onClick={() => setNewMenu(false)} />
                <div className="menu" role="menu">
                  <button role="menuitem" onClick={newFolder}>
                    <Icon name="folder" size={18} /> New folder
                  </button>
                  <button role="menuitem" onClick={newFile}>
                    <Icon name="file" size={18} /> New file
                  </button>
                  <hr />
                  <button
                    role="menuitem"
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
          {navItem("drive", "drive", "My Drawer")}
          {navItem("recent", "clock", "Recent")}
          {navItem("starred", "star", "Starred")}
          {navItem("trash", "trash", "Trash")}
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
          {loading && !file && <div className="progress" aria-hidden="true" />}
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
                <span className="spacer" />
                <button className="icon-btn" title="Refresh" aria-label="Refresh" onClick={() => refresh()}>
                  <Icon name="refresh" size={18} />
                </button>
              </div>
              {view.kind === "trash" && nodes.length > 0 && (
                <p className="hint">Items in the trash keep their history. Restore puts them back where they were.</p>
              )}
              {view.kind === "search" ? (
                <SearchResults hits={hits} loading={loading} open={(p) => guard(() => setFile(p))} />
              ) : (
                <FileTable
                  nodes={shown}
                  loading={loading}
                  mode={view.kind === "drive" ? "drive" : view.kind}
                  sort={view.kind === "recent" ? null : sort}
                  onSort={changeSort}
                  open={openNode}
                  star={toggleStar}
                  rename={rename}
                  move={move}
                  copy={copy}
                  download={(n) => run(() => api.download(n.path))}
                  trash={(n) => trash(n)}
                  restore={restore}
                  purge={purge}
                  empty={
                    view.kind === "trash"
                      ? "Trash is empty."
                      : view.kind === "starred"
                        ? "Nothing starred yet. Star files and folders to find them here."
                        : view.kind === "recent"
                          ? "No files yet. Files that agents or people change show up here, newest first."
                          : "This folder is empty. Drop files here, or use New to upload or create one."
                  }
                />
              )}
            </>
          )}
        </main>
      </div>

      {dialog && (
        <DialogBox
          dialog={dialog}
          close={() => {
            asking.current = false;
            setDialog(null);
          }}
          notify={notify}
        />
      )}
      {toast && (
        <div className={`toast ${toast.tone}`} role="status" aria-live="polite">
          <span>{toast.text}</span>
          {toast.action && (
            <button
              className="toast-action"
              onClick={() => {
                toast.action?.run();
                setToast(null);
              }}
            >
              {toast.action.label}
            </button>
          )}
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
  mode: "drive" | "recent" | "starred" | "trash";
  /** null: the order is fixed (Recent), so headers don't sort. */
  sort: Sort | null;
  onSort: (k: SortKey) => void;
  empty: string;
  open: (n: Node) => void;
  star: (n: Node) => void;
  rename: (n: Node) => void;
  move: (n: Node) => void;
  copy: (n: Node) => void;
  download: (n: Node) => void;
  trash: (n: Node) => void;
  restore: (n: Node) => void;
  purge: (n: Node) => void;
}) {
  const { nodes, mode, sort } = props;
  const [menu, setMenu] = useState<string | null>(null);
  if (!props.loading && nodes.length === 0) return <div className="empty">{props.empty}</div>;

  const head = (key: SortKey, label: string, className: string) => {
    const on = sort?.key === key;
    return (
      <th className={className} aria-sort={on ? (sort.dir === 1 ? "ascending" : "descending") : undefined}>
        {sort ? (
          <button className={on ? "th-sort on" : "th-sort"} onClick={() => props.onSort(key)}>
            {label}
            <span className="sort-arrow" aria-hidden="true">
              {on ? (sort.dir === 1 ? "↑" : "↓") : ""}
            </span>
          </button>
        ) : (
          label
        )}
      </th>
    );
  };

  const where = (n: Node) => (parentOf(n.path) === "/" ? "My Drawer" : parentOf(n.path));
  const pick = (fn: (n: Node) => void, n: Node) => () => {
    setMenu(null);
    fn(n);
  };

  return (
    <table className="files">
      <thead>
        <tr>
          {head("name", "Name", "")}
          <th className="col-md">{mode === "drive" ? "Last change by" : "Location"}</th>
          {head("modified", mode === "trash" ? "Trashed" : "Modified", "col-sm")}
          {head("size", "Size", "col-sm num")}
          <th className="col-actions" aria-label="Actions" />
        </tr>
      </thead>
      <tbody>
        {nodes.map((n) => (
          <tr
            key={n.id}
            className={menu === n.id ? "menu-open" : undefined}
            onDoubleClick={() => mode !== "trash" && props.open(n)}
            onContextMenu={(e) => {
              if (mode === "trash") return;
              e.preventDefault();
              setMenu(n.id);
            }}
          >
            <td>
              <button className="name" disabled={mode === "trash"} onClick={() => props.open(n)} title={n.path}>
                <KindIcon node={n} />
                <span className="label">{n.name}</span>
                {n.starred && mode !== "trash" && (
                  <span className="star-mark" title="Starred">
                    <Icon name="star" size={14} filled />
                  </span>
                )}
              </button>
            </td>
            <td className="col-md muted">
              {mode === "drive" ? (n.kind === "folder" ? "—" : (n.modified_by ?? n.owner)) : where(n)}
            </td>
            <td className="col-sm muted" title={new Date(n.modified_at).toLocaleString()}>
              {formatWhen(n.modified_at)}
              {mode === "recent" && n.modified_by && <span className="by"> · {n.modified_by}</span>}
            </td>
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
                    <button className="icon-btn" title="Move to trash" aria-label={`Move ${n.name} to trash`} onClick={() => props.trash(n)}>
                      <Icon name="trash" size={18} />
                    </button>
                    <div className="row-menu">
                      <button
                        className="icon-btn"
                        title="More actions"
                        aria-label={`More actions for ${n.name}`}
                        aria-haspopup="menu"
                        aria-expanded={menu === n.id}
                        onClick={() => setMenu(menu === n.id ? null : n.id)}
                      >
                        <Icon name="more" size={18} />
                      </button>
                      {menu === n.id && (
                        <>
                          <div className="scrim" onClick={() => setMenu(null)} onContextMenu={(e) => { e.preventDefault(); setMenu(null); }} />
                          <div className="menu right" role="menu" onKeyDown={(e) => e.key === "Escape" && setMenu(null)}>
                            <button role="menuitem" autoFocus onClick={pick(props.open, n)}>
                              <Icon name={n.kind === "folder" ? "folder" : "eye"} size={18} /> Open
                            </button>
                            <button role="menuitem" onClick={pick(props.rename, n)}>
                              <Icon name="edit" size={18} /> Rename
                            </button>
                            <button role="menuitem" onClick={pick(props.move, n)}>
                              <Icon name="move" size={18} /> Move
                            </button>
                            {n.kind === "file" && (
                              <>
                                <button role="menuitem" onClick={pick(props.copy, n)}>
                                  <Icon name="copy" size={18} /> Make a copy
                                </button>
                                <button role="menuitem" onClick={pick(props.download, n)}>
                                  <Icon name="download" size={18} /> Download
                                </button>
                              </>
                            )}
                            <hr />
                            <button role="menuitem" onClick={pick(props.trash, n)}>
                              <Icon name="trash" size={18} /> Move to trash
                            </button>
                          </div>
                        </>
                      )}
                    </div>
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
              {h.matched === "content" && (
                <span className="snippet">
                  {h.line !== undefined && <span className="line-no">{h.line}</span>}
                  {h.snippet}
                </span>
              )}
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
  // A diff of one revision against the one before it, shown in place of the file.
  const [comparing, setComparing] = useState<{ rev: Revision; diff: Diff } | null>(null);
  // A newer head someone saved while this editor has unsaved changes.
  const [remote, setRemote] = useState<Node | null>(null);
  const uploadInput = useRef<HTMLInputElement>(null);

  const type = fileType(name, node?.mime);
  const editable = isTextViewer(type.viewer);
  const hasPreview = type.viewer === "markdown" || type.viewer === "html" || type.viewer === "csv";
  const dirty = editable && viewing === null && content !== saved;
  const dirtyRef = useRef(dirty);
  dirtyRef.current = dirty;
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
        setComparing(null);
        if (!revision) setRemote(null);
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

  // Follow changes agents and other people save. A clean editor reloads by itself; one with
  // unsaved edits shows a banner instead, so nothing typed is lost.
  useEffect(() => {
    if (viewing || base === null) return;
    const tick = async () => {
      if (document.visibilityState !== "visible" || saving) return;
      try {
        const meta = await api.stat(path);
        if (meta.head_revision === null || meta.head_revision === base) return;
        if (dirtyRef.current) setRemote(meta);
        else {
          await load();
          notify(`Updated to revision ${meta.head_revision}${meta.modified_by ? ` by ${meta.modified_by}` : ""}`);
        }
      } catch {
        // moved or trashed elsewhere: the next explicit action says so
      }
    };
    const t = setInterval(tick, POLL_MS);
    return () => clearInterval(t);
  }, [path, base, viewing, saving, load, notify]);

  const compare = (r: Revision) =>
    run(async () => {
      if (comparing?.rev.revision === r.revision) return setComparing(null);
      const diff = await api.diff(path, r.revision - 1, r.revision);
      setComparing({ rev: r, diff });
    });

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
  else if (comparing) body = <DiffView diff={comparing.diff.diff} />;
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
            {node?.modified_by ? ` by ${node.modified_by}` : ""}
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
          {remote && dirty && (
            <div className="banner">
              <span>
                {remote.modified_by ?? "Someone"} saved revision {remote.head_revision} while you were editing. Saving now would be
                refused, so copy anything you want to keep first.
              </span>
              <span className="spacer" />
              <button className="btn" onClick={conflict}>
                Load latest
              </button>
            </div>
          )}
          {comparing && (
            <div className="banner info">
              <Icon name="diff" size={18} />
              <span>
                Changes in revision {comparing.rev.revision}
                {comparing.rev.revision > 1 ? ` since revision ${comparing.rev.revision - 1}` : ""}, by {comparing.rev.author}:{" "}
                <span className="added">+{comparing.diff.added}</span> <span className="removed">−{comparing.diff.removed}</span>
              </span>
              <span className="spacer" />
              <button className="btn" onClick={() => setComparing(null)}>
                Close
              </button>
            </div>
          )}
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
          {viewing === null && !comparing && editable && !showPreview && (
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
          {viewing === null && !comparing && !editable && node && (
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
                    {editable && (
                      <button
                        className={comparing?.rev.revision === r.revision ? "icon-btn rev-diff on" : "icon-btn rev-diff"}
                        title={r.revision > 1 ? `What changed since revision ${r.revision - 1}` : "Show the first revision as added lines"}
                        aria-label={`Changes in revision ${r.revision}`}
                        aria-pressed={comparing?.rev.revision === r.revision}
                        onClick={() => compare(r)}
                      >
                        <Icon name="diff" size={16} />
                      </button>
                    )}
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
    const dot = dialog.kind === "prompt" && dialog.selectStem ? el.value.lastIndexOf(".") : -1;
    el.setSelectionRange(0, dot > 0 ? dot : el.value.length);
  }, [dialog]);

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
            <input
              ref={input}
              value={value}
              type={dialog.secret ? "password" : "text"}
              autoComplete={dialog.secret ? "current-password" : "off"}
              spellCheck={false}
              onChange={(e) => setValue(e.target.value)}
            />
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
