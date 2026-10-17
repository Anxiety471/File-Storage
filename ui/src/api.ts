import { uploadChecksum } from "./checksum";
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

export interface UpdateStatus {
  current: string;
  latest: string;
  update_available: boolean;
}

export interface UpdateResult {
  current: string;
  latest: string;
  updated: boolean;
  restart_required: boolean;
}

export interface SearchHit {
  id: string;
  path: string;
  matched: "name" | "content";
  snippet: string;
  line?: number;
}

export interface DownloadLink {
  max_downloads: number | null;
  downloads: number;
  revision: number | null;
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
  if (token) window.dispatchEvent(new Event(AUTHENTICATED_EVENT));
}
let memoryToken: string | null = null;

// Optional bearer token, kept in localStorage when the server runs with DRAWER_TOKEN.
function headers(extra: Record<string, string> = {}): Record<string, string> {
  const token = memoryToken ?? storedToken();
  return token ? { ...extra, Authorization: `Bearer ${token}` } : extra;
}

/** Fired on a 401 so the app can ask for the token. */
export const UNAUTHORIZED_EVENT = "drawer:unauthorized";
export const AUTHENTICATED_EVENT = "drawer:authenticated";

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

export interface AuthStatus {
  mode: "password";
  authenticated: boolean;
  user: string | null;
}

export const api = {
  /** Is a newer release published than the one this server runs? */
  async updateCheck(): Promise<UpdateStatus> {
    const r = await check(await fetch(`${BASE}/update`, { headers: headers() }));
    return r.json();
  },
  /** Installs the latest release if it is newer. The server runs the new version after a restart. */
  async updateInstall(): Promise<UpdateResult> {
    const r = await check(await fetch(`${BASE}/update`, { method: "POST", headers: headers() }));
    return r.json();
  },
  async authStatus(): Promise<AuthStatus> {
    const response = await fetch("/auth/me", { credentials: "same-origin" });
    if (!response.ok) throw new ApiError(response.status, "Password authentication is not configured");
    return response.json();
  },
  async login(password: string): Promise<void> {
    const response = await fetch("/auth/login", {
      method: "POST", credentials: "same-origin",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ password }),
    });
    if (!response.ok) throw new ApiError(response.status, "Incorrect administrator password");
    window.dispatchEvent(new Event(AUTHENTICATED_EVENT));
  },
  async logout(): Promise<void> {
    await check(await fetch("/auth/logout", { method: "POST", credentials: "same-origin" }));
  },
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
    // The browser's native download manager streams files without buffering multi-GB
    // content in JavaScript memory. HttpOnly sessions travel with same-origin requests.
    const status = await api.authStatus().catch(() => null);
    if (status?.authenticated) {
      const extra = (revision ? `&revision=${revision}` : "") + "&download=true";
      const a = document.createElement("a");
      a.href = `${BASE}/files?${q(path, extra)}`;
      a.download = path.split("/").pop() || "download";
      a.click();
      return;
    }
    // Legacy DRAWER_TOKEN mode needs an Authorization header and therefore a fetch/blob.
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
  /** Upload in 4 MiB chunks; restart the same selected File to resume a failed transfer. */
  async upload(
    path: string,
    file: File,
    onProgress: (done: number, total: number) => void,
    signal?: AbortSignal,
  ): Promise<WriteOutcome> {
    onProgress(0, file.size);
    const sha256 = await uploadChecksum(file, signal);
    const key = `drawer:transfer:v2:${path}:${sha256}`;
    const opts = { headers: headers() };
    let id: string | null = null;
    try { id = localStorage.getItem(key); } catch { /* Storage may be blocked. */ }
    let offset = 0;
    if (id) {
      try {
        const r = await check(await fetch(`${BASE}/transfers/${encodeURIComponent(id)}`, opts));
        const state: { received: number; size: number; path: string; sha256: string | null } = await r.json();
        if (state.size !== file.size || state.path !== path || state.sha256 !== sha256) throw new Error("Upload changed");
        offset = state.received;
      } catch {
        try { localStorage.removeItem(key); } catch { /* Optional resume cache. */ }
        id = null;
      }
    }
    if (!id) {
      const r = await check(await fetch(`${BASE}/transfers`, json("POST", { path, size: file.size, sha256 })));
      const state: { id: string } = await r.json();
      id = state.id;
      try { localStorage.setItem(key, id); } catch { /* Uploads still work without resume caching. */ }
    }
    onProgress(offset, file.size);
    while (offset < file.size) {
      if (signal?.aborted) throw new DOMException("Upload paused", "AbortError");
      const chunk = file.slice(offset, Math.min(offset + 4 * 1024 * 1024, file.size));
      const r = await check(await fetch(`${BASE}/transfers/${encodeURIComponent(id)}`, {
        method: "PUT",
        headers: headers({ "x-upload-offset": String(offset) }),
        body: chunk,
        signal,
      }));
      const state: { received: number } = await r.json();
      if (state.received <= offset) throw new Error("Transfer made no progress");
      offset = state.received;
      onProgress(offset, file.size);
    }
    const r = await check(await fetch(`${BASE}/transfers/${encodeURIComponent(id)}/complete`, {
      method: "POST", headers: headers(), signal,
    }));
    const outcome: WriteOutcome = await r.json();
    if (outcome.node.hash !== sha256) throw new Error("Server returned a different file checksum");
    try { localStorage.removeItem(key); } catch { /* Optional resume cache. */ }
    return outcome;
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
  async createLink(id: string, days = 7, revision?: number, maxDownloads?: number): Promise<CreatedDownloadLink> {
    const r = await check(await fetch(`${BASE}/nodes/${encodeURIComponent(id)}/links`, json("POST", { expires_in_days: days, revision, max_downloads: maxDownloads })));
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
