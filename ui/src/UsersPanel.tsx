import { useEffect, useState } from "react";
import { api, UserAccount } from "./api";
export function UsersPanel({ close }: { close: () => void }) {
  const [users, setUsers] = useState<UserAccount[]>([]);
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [mib, setMib] = useState(1024);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [quotas, setQuotas] = useState<Record<string, number>>({});
  useEffect(() => { let active = true; api.users().then((items) => { if (active) setUsers(items); }).catch((e) => { if (active) setError(String(e)); }); return () => { active = false; }; }, []);
  const perform = async (action: () => Promise<void>) => { setBusy(true); setError(""); try { await action(); setUsers(await api.users()); } catch (e) { setError(String(e)); } finally { setBusy(false); } };
  return <div className="modal-scrim" onMouseDown={close}><section className="modal operations-modal" role="dialog" aria-modal="true" aria-labelledby="users-title" onMouseDown={(e) => e.stopPropagation()}>
    <h3 id="users-title">Users and quotas</h3><p>Each user has a private workspace. Quotas count all revision bytes, including history and trash. Lowering a quota below current usage blocks new writes. Share files through download or upload-only links.</p>
    <form onSubmit={(e) => { e.preventDefault(); void perform(async () => { await api.createUser(username, password, Math.round(mib * 1024 * 1024)); setUsername(""); setPassword(""); }); }}>
      <label>Username <input aria-label="New username" autoComplete="off" pattern="[a-z0-9_-]{1,32}" required value={username} onChange={(e) => setUsername(e.target.value)} /></label>
      <label>Password <input aria-label="New user password" type="password" autoComplete="new-password" minLength={12} required value={password} onChange={(e) => setPassword(e.target.value)} /></label>
      <label>Quota MiB <input aria-label="New user quota MiB" type="number" min="1" required value={mib} onChange={(e) => setMib(Number(e.target.value))} /></label><button className="btn primary" disabled={busy}>Create user</button>
    </form>{error && <p role="alert">{error}</p>}
    <ul className="operations-list">{users.map((user) => <li key={user.id}><strong>{user.username} · {user.disabled ? "Disabled" : "Active"}</strong><div className="operations-actions"><label>Quota MiB <input aria-label={`Quota for ${user.username}`} type="number" min="1" value={quotas[user.id] ?? user.quota_bytes / 1024 / 1024} onChange={(e) => setQuotas((q) => ({ ...q, [user.id]: Number(e.target.value) }))} /></label><button className="btn" disabled={busy || !quotas[user.id] || quotas[user.id] < 1} onClick={() => void perform(() => api.changeUser(user.id, { quota_bytes: Math.round(quotas[user.id] * 1024 * 1024) }))}>Save quota</button><button className="btn" disabled={busy} onClick={() => void perform(() => api.changeUser(user.id, { disabled: !user.disabled }))}>{user.disabled ? "Enable user" : "Disable user"}</button></div></li>)}</ul>
    <div className="modal-actions"><button className="btn" onClick={close}>Done</button></div>
  </section></div>;
}
