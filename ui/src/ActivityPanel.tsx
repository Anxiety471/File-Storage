import { useEffect, useState } from "react";
import { api, ActivityEvent } from "./api";
export function ActivityPanel({ close }: { close: () => void }) {
  const [events, setEvents] = useState<ActivityEvent[]>([]);
  const [error, setError] = useState("");
  const [more, setMore] = useState(true);
  const [busy, setBusy] = useState(false);
  useEffect(() => { let active = true; api.activity().then((items) => { if (active) { setEvents(items); setMore(items.length === 100); } }).catch((e) => { if (active) setError(String(e)); }); return () => { active = false; }; }, []);
  const older = async () => { setBusy(true); try { const items = await api.activity(events.at(-1)?.id); setEvents((old) => [...old, ...items]); setMore(items.length === 100); } catch (e) { setError(String(e)); } finally { setBusy(false); } };
  return <div className="modal-scrim" onMouseDown={close}><section className="modal operations-modal" role="dialog" aria-modal="true" aria-labelledby="activity-title" onMouseDown={(e) => e.stopPropagation()}>
    <h3 id="activity-title">Activity history</h3><p>File reads, archive requests, changes, sharing, logins and MCP requests. Status indicates the HTTP response; downloads may still be in progress. Route templates hide public link secrets.</p>
    {error && <p role="alert">{error}</p>}<ul className="operations-list">{events.map((event) => <li key={event.id}><strong>{event.method} {event.route} · {event.status}</strong><span>{event.actor} · {new Date(event.created_at).toLocaleString()}</span></li>)}</ul>
    {!events.length && <p>No activity recorded yet.</p>}<div className="modal-actions">{more && <button className="btn" disabled={busy} onClick={() => void older()}>Load older events</button>}<button className="btn" onClick={close}>Done</button></div>
  </section></div>;
}
