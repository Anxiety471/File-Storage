import { useEffect, useRef, useState } from "react";
import { api, GraphNode, MemoryGraph as Graph } from "./api";

interface Sim extends GraphNode { x: number; y: number; vx: number; vy: number; r: number; fixed?: boolean }

const PALETTE = ["#4f7cff", "#e8590c", "#2f9e44", "#ae3ec9", "#f08c00", "#1098ad", "#d6336c", "#5c940d"];
const colorOf = (tags: string[], first: string[]) => { const i = first.indexOf(tags[0]?.split("/")[0]); return i < 0 ? "#868e96" : PALETTE[i % PALETTE.length]; };

/** Obsidian-style graph: notes are nodes, [[links]] are edges. Drag nodes, scroll to zoom, drag the background to pan. */
export function MemoryGraph({ open, filter }: { open: (name: string) => void; filter: string }) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const [graph, setGraph] = useState<Graph | null>(null);
  const [error, setError] = useState("");
  const [hint, setHint] = useState("");
  const first = graph ? [...new Set(graph.nodes.map((n) => n.tags[0]?.split("/")[0]).filter(Boolean))].sort() : [];
  useEffect(() => { api.memoryGraph().then(setGraph).catch((e) => setError(String(e))); }, []);

  useEffect(() => {
    const el = canvas.current;
    if (!el || !graph) return;
    const ctx = el.getContext("2d")!;
    const W = el.clientWidth, H = el.clientHeight, dpr = window.devicePixelRatio || 1;
    el.width = W * dpr; el.height = H * dpr;
    const nodes: Sim[] = graph.nodes.map((n, i) => {
      const a = (i / Math.max(1, graph.nodes.length)) * Math.PI * 2;
      return { ...n, x: Math.cos(a) * 200 + (Math.random() - 0.5) * 20, y: Math.sin(a) * 200 + (Math.random() - 0.5) * 20, vx: 0, vy: 0, r: 5 + Math.min(14, Math.sqrt(n.out + n.inbound) * 4) };
    });
    const byId = new Map(nodes.map((n) => [n.id, n]));
    const edges = graph.edges.map(([a, b]) => [byId.get(a)!, byId.get(b)!] as const).filter(([a, b]) => a && b);
    const q = filter.trim().toLowerCase();
    const matches = (n: Sim) => !q || n.name.toLowerCase().includes(q) || n.tags.some((t) => t.includes(q.replace(/^#/, "")));
    const view = { x: W / 2, y: H / 2, k: 1 };
    let frames = 0, touched = false, hover: Sim | null = null, drag: Sim | null = null, pan = false, moved = false, last = [0, 0], alpha = 1, raf = 0;
    const world = (e: MouseEvent) => { const b = el.getBoundingClientRect(); return [(e.clientX - b.left - view.x) / view.k, (e.clientY - b.top - view.y) / view.k]; };
    const pick = (x: number, y: number) => [...nodes].reverse().find((n) => (n.x - x) ** 2 + (n.y - y) ** 2 <= (n.r + 3) ** 2) ?? null;
    const near = (n: Sim | null) => { const s = new Set<Sim>(); if (n) { s.add(n); edges.forEach(([a, b]) => { if (a === n) s.add(b); if (b === n) s.add(a); }); } return s; };

    const step = () => {
      for (let i = 0; i < nodes.length; i++) for (let j = i + 1; j < nodes.length; j++) {
        const a = nodes[i], b = nodes[j]; let dx = b.x - a.x, dy = b.y - a.y; const d2 = dx * dx + dy * dy + 0.01, d = Math.sqrt(d2);
        const f = Math.min(40, 9000 / d2) * alpha; dx /= d; dy /= d;
        a.vx -= dx * f; a.vy -= dy * f; b.vx += dx * f; b.vy += dy * f;
      }
      for (const [a, b] of edges) {
        const dx = b.x - a.x, dy = b.y - a.y, d = Math.sqrt(dx * dx + dy * dy) || 1, f = (d - 110) * 0.04 * alpha;
        a.vx += (dx / d) * f; a.vy += (dy / d) * f; b.vx -= (dx / d) * f; b.vy -= (dy / d) * f;
      }
      for (const n of nodes) {
        n.vx -= n.x * 0.006 * alpha; n.vy -= n.y * 0.006 * alpha;
        if (!n.fixed) { n.x += n.vx; n.y += n.vy; }
        n.vx *= 0.82; n.vy *= 0.82;
      }
      alpha = Math.max(0.02, alpha * 0.985);
    };
    const css = getComputedStyle(el);
    const ink = css.getPropertyValue("color") || "#333";
    const draw = () => {
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0); ctx.clearRect(0, 0, W, H);
      ctx.translate(view.x, view.y); ctx.scale(view.k, view.k);
      const hot = near(hover ?? drag);
      for (const [a, b] of edges) {
        const on = hot.size > 0 && hot.has(a) && hot.has(b) && (a === (hover ?? drag) || b === (hover ?? drag));
        ctx.globalAlpha = hot.size > 0 && !on ? 0.12 : matches(a) || matches(b) ? 0.55 : 0.1;
        ctx.strokeStyle = on ? "#4f7cff" : "#868e96"; ctx.lineWidth = (on ? 2 : 1) / view.k;
        ctx.beginPath(); ctx.moveTo(a.x, a.y); ctx.lineTo(b.x, b.y); ctx.stroke();
      }
      for (const n of nodes) {
        const dim = (hot.size > 0 && !hot.has(n)) || !matches(n);
        ctx.globalAlpha = dim ? 0.18 : 1;
        ctx.beginPath(); ctx.arc(n.x, n.y, n.r, 0, Math.PI * 2);
        const color = colorOf(n.tags, first);
        if (n.path) { ctx.fillStyle = color; ctx.fill(); } else { ctx.setLineDash([3, 3]); ctx.strokeStyle = "#868e96"; ctx.lineWidth = 1.5; ctx.stroke(); ctx.setLineDash([]); }
        if (!dim && (view.k > 0.7 || hot.has(n) || n.r > 9)) { ctx.fillStyle = ink; ctx.font = `${12 / Math.max(view.k, 0.8)}px sans-serif`; ctx.textAlign = "center"; ctx.fillText(n.name, n.x, n.y + n.r + 13 / Math.max(view.k, 0.8)); }
      }
      ctx.globalAlpha = 1;
    };
    const fit = () => {
      const xs = nodes.map((n) => n.x), ys = nodes.map((n) => n.y), pad = 60;
      const w = Math.max(...xs) - Math.min(...xs) + pad * 2, h = Math.max(...ys) - Math.min(...ys) + pad * 2;
      const k = Math.min(1.2, W / w, H / h), tx = W / 2 - ((Math.max(...xs) + Math.min(...xs)) / 2) * k, ty = H / 2 - ((Math.max(...ys) + Math.min(...ys)) / 2) * k;
      view.k += (k - view.k) * 0.1; view.x += (tx - view.x) * 0.1; view.y += (ty - view.y) * 0.1;
    };
    const loop = () => { step(); if (!touched && ++frames < 400) fit(); draw(); raf = requestAnimationFrame(loop); };
    loop();

    const down = (e: MouseEvent) => { touched = true; const [x, y] = world(e); drag = pick(x, y); moved = false; last = [e.clientX, e.clientY]; if (drag) drag.fixed = true; else pan = true; alpha = Math.max(alpha, 0.3); };
    const move = (e: MouseEvent) => {
      const [x, y] = world(e);
      if (drag) { drag.x = x; drag.y = y; moved = true; alpha = Math.max(alpha, 0.3); }
      else if (pan) { view.x += e.clientX - last[0]; view.y += e.clientY - last[1]; moved = true; }
      else { hover = pick(x, y); el.style.cursor = hover ? "pointer" : "grab"; setHint(hover ? `${hover.name} · ${hover.inbound} in · ${hover.out} out${hover.tags.length ? " · " + hover.tags.map((t) => "#" + t).join(" ") : ""}${hover.path ? "" : " · not written yet"}` : ""); }
      last = [e.clientX, e.clientY];
    };
    const up = () => { if (drag && !moved && drag.path) open(drag.name); if (drag) drag.fixed = false; drag = null; pan = false; };
    const wheel = (e: WheelEvent) => { touched = true; e.preventDefault(); const b = el.getBoundingClientRect(), k = Math.min(4, Math.max(0.25, view.k * (e.deltaY < 0 ? 1.12 : 1 / 1.12))); const px = e.clientX - b.left, py = e.clientY - b.top; view.x = px - ((px - view.x) / view.k) * k; view.y = py - ((py - view.y) / view.k) * k; view.k = k; };
    el.addEventListener("mousedown", down); window.addEventListener("mousemove", move); window.addEventListener("mouseup", up); el.addEventListener("wheel", wheel, { passive: false });
    return () => { cancelAnimationFrame(raf); el.removeEventListener("mousedown", down); window.removeEventListener("mousemove", move); window.removeEventListener("mouseup", up); el.removeEventListener("wheel", wheel); };
  }, [graph, filter, open]); // eslint-disable-line react-hooks/exhaustive-deps

  if (error) return <p role="alert">{error}</p>;
  if (graph && !graph.nodes.length) return <p>No memories yet, so there's nothing to draw. Notes appear here as soon as an agent saves one.</p>;
  return <div className="memory-graph"><canvas ref={canvas} aria-label="Memory graph" />
    {first.length > 0 && <div className="memory-tags">{first.map((t, i) => <span key={t} className="memory-key"><i style={{ background: PALETTE[i % PALETTE.length] }} />#{t}</span>)}</div>}
    <div className="memory-graph-hint">{hint || "Drag nodes, scroll to zoom, click a note to open it. Colour is the note's first tag; dashed circles are linked notes that don't exist yet."}</div></div>;
}
