export interface Node {
  id: string;
  parent_id: string | null;
  name: string;
  path: string;
  kind: "file" | "folder";
  owner: string;
  head_revision: number | null;
  hash: string | null;
  size: number | null;
  mime: string | null;
  properties: Record<string, string>;
  trashed: boolean;
  starred: boolean;
  created_at: string;
  modified_at: string;
}

export interface Revision {
  node_id: string;
  revision: number;
  hash: string;
  size: number;
  mime: string;
  author: string;
  comment: string;
  properties: Record<string, string>;
  created_at: string;
}

export interface WriteOutcome {
  node: Node;
  revision: number;
  new_revision: boolean;
}

export interface SearchHit {
  id: string;
  path: string;
  matched: "name" | "content";
  snippet: string;
}

const BASE = "/drawer/v1";

// Optional bearer token, kept in localStorage when the server runs with DRAWER_TOKEN.
function headers(extra: Record<string, string> = {}): Record<string, string> {
  const token = localStorage.getItem("drawer_token");
  return token ? { ...extra, Authorization: `Bearer ${token}` } : extra;
}

export class ApiError extends Error {
  constructor(public status: number, message: string) {
    super(message);
  }
}

async function check(r: Response): Promise<Response> {
  if (!r.ok) {
    const body = await r.json().catch(() => ({}));
    throw new ApiError(r.status, body.error ?? `${r.status} ${r.statusText}`);
  }
  return r;
}

const q = (path: string, extra = "") => `path=${encodeURIComponent(path)}${extra}`;
const json = (method: string, body: unknown): RequestInit => ({
  method,
  headers: headers({ "content-type": "application/json" }),
  body: JSON.stringify(body),
});

export const api = {
  async tree(path: string): Promise<Node[]> {
    const r = await check(await fetch(`${BASE}/tree?${q(path)}`, { headers: headers() }));
    return (await r.json()).entries;
  },
  async stat(path: string): Promise<Node> {
    const r = await check(await fetch(`${BASE}/files/meta?${q(path)}`, { headers: headers() }));
    return r.json();
  },
  async mkdir(path: string): Promise<void> {
    await check(await fetch(`${BASE}/folders`, json("POST", { path })));
  },
  async read(path: string, revision?: number): Promise<string> {
    const extra = revision ? `&revision=${revision}` : "";
    const r = await check(await fetch(`${BASE}/files?${q(path, extra)}`, { headers: headers() }));
    return r.text();
  },
  // Raw bytes of a revision (the head by default), for viewers and downloads.
  async readBlob(path: string, revision?: number, download = false): Promise<Blob> {
    const extra = (revision ? `&revision=${revision}` : "") + (download ? "&download=true" : "");
    const r = await check(await fetch(`${BASE}/files?${q(path, extra)}`, { headers: headers() }));
    return r.blob();
  },
  // Saves the file through the browser's download prompt.
  async download(path: string, revision?: number): Promise<void> {
    const blob = await api.readBlob(path, revision, true);
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    const name = path.split("/").pop() ?? "download";
    const dot = name.lastIndexOf(".");
    a.download = revision ? `${dot > 0 ? name.slice(0, dot) : name} (revision ${revision})${dot > 0 ? name.slice(dot) : ""}` : name;
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 10_000);
  },
  // content is text or a File/Blob for uploads.
  // ifHeadRevision: 0 = create only; N = only if the head is still N. A stale save gets a 412.
  async write(path: string, content: string | Blob, comment: string, ifHeadRevision?: number): Promise<WriteOutcome> {
    const guard = ifHeadRevision === undefined ? "" : `&if_head_revision=${ifHeadRevision}`;
    const r = await check(
      await fetch(`${BASE}/files?${q(path, guard)}`, {
        method: "PUT",
        // header values must be ASCII; non-ASCII comments are percent-encoded
        headers: headers({ "x-drawer-author": "ui", "x-drawer-comment": /^[\x20-\x7e]*$/.test(comment) ? comment : encodeURIComponent(comment) }),
        body: content,
      }),
    );
    return r.json();
  },
  // Moves the file to the trash. Restore it from the Trash view.
  async trash(path: string): Promise<void> {
    await check(await fetch(`${BASE}/files?${q(path)}`, { method: "DELETE", headers: headers() }));
  },
  async revisions(path: string): Promise<Revision[]> {
    const r = await check(await fetch(`${BASE}/revisions?${q(path)}`, { headers: headers() }));
    return (await r.json()).revisions;
  },
  async rollback(path: string, revision: number): Promise<void> {
    await check(await fetch(`${BASE}/rollback`, json("POST", { path, revision, author: "ui" })));
  },
  async update(id: string, patch: { name?: string; parent_id?: string; starred?: boolean; trashed?: boolean }): Promise<Node> {
    const r = await check(
      await fetch(`${BASE}/nodes/${encodeURIComponent(id)}`, {
        ...json("PATCH", patch),
        headers: headers({ "content-type": "application/json", "x-drawer-author": "ui" }),
      }),
    );
    return r.json();
  },
  // Permanent: the node must already be in the trash.
  async purge(id: string): Promise<void> {
    await check(
      await fetch(`${BASE}/nodes/${encodeURIComponent(id)}`, {
        method: "DELETE",
        headers: headers({ "x-drawer-author": "ui" }),
      }),
    );
  },
  async starred(): Promise<Node[]> {
    const r = await check(await fetch(`${BASE}/starred`, { headers: headers() }));
    return (await r.json()).nodes;
  },
  async trashed(): Promise<Node[]> {
    const r = await check(await fetch(`${BASE}/trash`, { headers: headers() }));
    return (await r.json()).nodes;
  },
  async search(query: string): Promise<SearchHit[]> {
    const r = await check(await fetch(`${BASE}/search?q=${encodeURIComponent(query)}`, { headers: headers() }));
    return (await r.json()).hits;
  },
};
