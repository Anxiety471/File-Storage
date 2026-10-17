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
  /** Author of the head revision: who last changed the content. */
  modified_by: string | null;
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
  line?: number;
}

export interface DownloadLink {
  id: string;
  created_at: string;
  expires_at: string;
}

export interface CreatedDownloadLink extends DownloadLink {
  token: string;
}

export interface Diff {
  path: string;
  from: number;
  to: number;
  added: number;
  removed: number;
  diff: string;
}

const BASE = "/drawer/v1";

const TOKEN_KEY = "drawer_token";

function storedToken(): string | null {
  try {
    return localStorage.getItem(TOKEN_KEY);
  } catch {
    return null;
  }
}

/** Remember the bearer token for a server that runs with DRAWER_TOKEN. Empty forgets it. */
export function setToken(token: string) {
  try {
    if (token) localStorage.setItem(TOKEN_KEY, token);
    else localStorage.removeItem(TOKEN_KEY);
  } catch {
    // storage blocked: the token lasts until reload
  }
  memoryToken = token || null;
}
let memoryToken: string | null = null;

// Optional bearer token, kept in localStorage when the server runs with DRAWER_TOKEN.
function headers(extra: Record<string, string> = {}): Record<string, string> {
  const token = memoryToken ?? storedToken();
  return token ? { ...extra, Authorization: `Bearer ${token}` } : extra;
}

/** Fired on a 401 so the app can ask for the token. */
export const UNAUTHORIZED_EVENT = "drawer:unauthorized";

export class ApiError extends Error {
  constructor(public status: number, message: string) {
    super(message);
  }
}

async function check(r: Response): Promise<Response> {
  if (r.status === 401) {
    window.dispatchEvent(new Event(UNAUTHORIZED_EVENT));
    throw new ApiError(401, "This Drawer needs an access token");
  }
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
  async recent(limit = 50): Promise<Node[]> {
    const r = await check(await fetch(`${BASE}/recent?limit=${limit}`, { headers: headers() }));
    return (await r.json()).nodes;
  },
  async diff(path: string, from?: number, to?: number): Promise<Diff> {
    const extra = (from !== undefined ? `&from=${from}` : "") + (to !== undefined ? `&to=${to}` : "");
    const r = await check(await fetch(`${BASE}/diff?${q(path, extra)}`, { headers: headers() }));
    return r.json();
  },
  // Rename or move by path, keeping the id and history. Missing folders are created.
  async move(id: string, to: string): Promise<Node> {
    const r = await check(
      await fetch(`${BASE}/move`, { ...json("POST", { from_id: id, to }), headers: headers({ "content-type": "application/json", "x-drawer-author": "ui" }) }),
    );
    return r.json();
  },
  async copy(id: string, to: string): Promise<WriteOutcome> {
    const r = await check(await fetch(`${BASE}/copy`, json("POST", { from_id: id, to, author: "ui", if_head_revision: 0 })));
    return r.json();
  },
  async links(id: string): Promise<DownloadLink[]> {
    const r = await check(await fetch(`${BASE}/nodes/${encodeURIComponent(id)}/links`, { headers: headers() }));
    return (await r.json()).links;
  },
  async createLink(id: string, days = 7): Promise<CreatedDownloadLink> {
    const r = await check(await fetch(`${BASE}/nodes/${encodeURIComponent(id)}/links`, json("POST", { expires_in_days: days })));
    return r.json();
  },
  async revokeLink(id: string, linkId: string): Promise<void> {
    await check(await fetch(`${BASE}/nodes/${encodeURIComponent(id)}/links/${encodeURIComponent(linkId)}`, { method: "DELETE", headers: headers() }));
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
