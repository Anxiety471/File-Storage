export interface Entry {
  name: string;
  path: string;
  kind: "file" | "folder";
  size: number | null;
  version: number | null;
  updated_at: string;
}

export interface VersionInfo {
  path: string;
  version: number;
  hash: string;
  size: number;
  mime: string;
  author: string;
  comment: string;
  created_at: string;
}

export interface SearchHit {
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

async function check(r: Response): Promise<Response> {
  if (!r.ok) {
    const body = await r.json().catch(() => ({}));
    throw new Error(body.error ?? `${r.status} ${r.statusText}`);
  }
  return r;
}

const q = (path: string, extra = "") => `path=${encodeURIComponent(path)}${extra}`;

export const api = {
  async tree(path: string): Promise<Entry[]> {
    const r = await check(await fetch(`${BASE}/tree?${q(path)}`, { headers: headers() }));
    return (await r.json()).entries;
  },
  async mkdir(path: string): Promise<void> {
    await check(
      await fetch(`${BASE}/folders`, {
        method: "POST",
        headers: headers({ "content-type": "application/json" }),
        body: JSON.stringify({ path }),
      }),
    );
  },
  async read(path: string, version?: number): Promise<string> {
    const extra = version ? `&version=${version}` : "";
    const r = await check(await fetch(`${BASE}/files?${q(path, extra)}`, { headers: headers() }));
    return r.text();
  },
  async write(path: string, content: string, comment: string): Promise<VersionInfo> {
    const r = await check(
      await fetch(`${BASE}/files?${q(path)}`, {
        method: "PUT",
        headers: headers({ "x-drawer-author": "ui", "x-drawer-comment": comment }),
        body: content,
      }),
    );
    return r.json();
  },
  async remove(path: string): Promise<void> {
    await check(await fetch(`${BASE}/files?${q(path)}`, { method: "DELETE", headers: headers() }));
  },
  async versions(path: string): Promise<VersionInfo[]> {
    const r = await check(await fetch(`${BASE}/versions?${q(path)}`, { headers: headers() }));
    return (await r.json()).versions;
  },
  async rollback(path: string, version: number): Promise<void> {
    await check(
      await fetch(`${BASE}/rollback`, {
        method: "POST",
        headers: headers({ "content-type": "application/json" }),
        body: JSON.stringify({ path, version, author: "ui" }),
      }),
    );
  },
  async search(query: string): Promise<SearchHit[]> {
    const r = await check(
      await fetch(`${BASE}/search?q=${encodeURIComponent(query)}`, { headers: headers() }),
    );
    return (await r.json()).hits;
  },
};
