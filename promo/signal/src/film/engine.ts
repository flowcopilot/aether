// A deterministic renderer: every pixel is a function of time `t` alone,
// so Remotion can render any frame in any tab in any order.
//
// Visual grammar of the film:
//   grey hairlines  = an agent alone
//   red             = a wall
//   spectrum        = a connection (only a signed permission produces colour)
import TL from "./timeline.json";

export const W = 1920;
export const H = 1080;
const TAU = Math.PI * 2;
const AT = TL.at;

type C2 = CanvasRenderingContext2D;
type V3 = [number, number, number];
type Pt = { x: number; y: number; k: number; z: number };
type Cam = { D: number; ry: number; rx: number; f: number; tx?: number; ty?: number; tz?: number };

export type Fonts = { mono: string; serif: string };
let F: Fonts = { mono: "monospace", serif: "serif" };

// ---------------------------------------------------------------- utilities
const clamp = (x: number, a = 0, b = 1) => Math.min(b, Math.max(a, x));
const lerp = (a: number, b: number, t: number) => a + (b - a) * t;
const seg = (t: number, a: number, b: number) => clamp((t - a) / (b - a));
const eio = (t: number) => (t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2);
const eo = (t: number) => 1 - Math.pow(1 - t, 3);
const eo5 = (t: number) => 1 - Math.pow(1 - t, 5);
const ei = (t: number) => t * t * t;
const win = (t: number, a: number, b: number, fi = 0.4, fo = 0.4) =>
  seg(t, a, a + fi) * (1 - seg(t, b - fo, b));

function rng(seed: number) {
  let s = seed | 0;
  return () => {
    s = (s + 0x6d2b79f5) | 0;
    let r = Math.imul(s ^ (s >>> 15), 1 | s);
    r = (r + Math.imul(r ^ (r >>> 7), 61 | r)) ^ r;
    return ((r ^ (r >>> 14)) >>> 0) / 4294967296;
  };
}
const hash = (a: number, b = 0) => rng(a * 7919 + b * 104729 + 13)();

const HEX = "0123456789abcdef";
function hexKey(seed: number) {
  const r = rng(seed);
  let s = "";
  for (let i = 0; i < 64; i++) s += HEX[(r() * 16) | 0];
  return s;
}
const groups = (k: string) => k.match(/.{4}/g) as string[];
const HOST = hexKey(7);
const ANA = hexKey(31);

// ------------------------------------------------------------------ colour
const INK = [214, 226, 238];
const RED = [255, 78, 52];
const hueAt = (u: number) => (195 + (((u % 1) + 1) % 1) * 300) % 360;
function hsl2rgb(h: number, s: number, l: number) {
  s /= 100;
  l /= 100;
  const k = (n: number) => (n + h / 30) % 12;
  const a = s * Math.min(l, 1 - l);
  const f = (n: number) => l - a * Math.max(-1, Math.min(k(n) - 3, Math.min(9 - k(n), 1)));
  return [255 * f(0), 255 * f(8), 255 * f(4)];
}
const ink = (a: number) => `rgba(${INK[0]},${INK[1]},${INK[2]},${clamp(a)})`;
const red = (a: number) => `rgba(${RED[0]},${RED[1]},${RED[2]},${clamp(a)})`;
function col(u: number, chroma: number, a: number, l = 72) {
  const s = hsl2rgb(hueAt(u), 92, l);
  const c = clamp(chroma);
  return `rgba(${lerp(INK[0], s[0], c) | 0},${lerp(INK[1], s[1], c) | 0},${lerp(INK[2], s[2], c) | 0},${clamp(a)})`;
}
const spec = (u: number, a: number, l = 72) => col(u, 1, a, l);

const mono = (size: number, w = 400) => `${w} ${size}px "${F.mono}", monospace`;
const serif = (size: number) => `italic 400 ${size}px "${F.serif}", serif`;

function typed(id: keyof typeof TL.type, t: number) {
  const e = TL.type[id];
  const n = Math.floor(clamp((t - e.t0) * e.cps, 0, e.text.length));
  return e.text.slice(0, n);
}
const typingDone = (id: keyof typeof TL.type) => TL.type[id].t0 + TL.type[id].text.length / TL.type[id].cps;
const caret = (t: number) => (Math.floor(t * 2.2) % 2 === 0 ? 1 : 0);

// ------------------------------------------------------------------- camera
function proj(p: V3, cam: Cam): Pt | null {
  const x = p[0] - (cam.tx || 0);
  const y = p[1] - (cam.ty || 0);
  const z = p[2] - (cam.tz || 0);
  let c = Math.cos(cam.ry);
  let s = Math.sin(cam.ry);
  const x1 = x * c - z * s;
  const z1 = x * s + z * c;
  c = Math.cos(cam.rx);
  s = Math.sin(cam.rx);
  const y1 = y * c - z1 * s;
  const z2 = y * s + z1 * c;
  const Z = z2 + cam.D;
  if (Z < 0.05) return null;
  const k = cam.f / Z;
  return { x: W / 2 + x1 * k, y: H / 2 + y1 * k, k, z: Z };
}

function line(ctx: C2, a: Pt | null, b: Pt | null) {
  if (!a || !b) return;
  ctx.moveTo(a.x, a.y);
  ctx.lineTo(b.x, b.y);
}

const CUBE_E = [
  [0, 1], [1, 3], [3, 2], [2, 0], [4, 5], [5, 7], [7, 6], [6, 4], [0, 4], [1, 5], [2, 6], [3, 7],
];
function cube(ctx: C2, cam: Cam, c: V3, s: number, stroke: string, lw = 1) {
  const v: (Pt | null)[] = [];
  for (let i = 0; i < 8; i++)
    v.push(proj([c[0] + (i & 1 ? s : -s), c[1] + (i & 2 ? s : -s), c[2] + (i & 4 ? s : -s)], cam));
  ctx.strokeStyle = stroke;
  ctx.lineWidth = lw;
  ctx.beginPath();
  for (const [a, b] of CUBE_E) line(ctx, v[a], v[b]);
  ctx.stroke();
}

// ------------------------------------------------------------------- sigil
// The agent's face, derived from the 32 bytes of its endpoint key:
// 64 ticks from the nibbles, 16 arcs from the four-hex groups.
type SigilOpts = { alpha?: number; rev?: number; t?: number; spin?: number; chroma?: number; hot?: number; detail?: boolean };
function sigil(ctx: C2, x: number, y: number, R: number, key: string, o: SigilOpts = {}) {
  const gs = groups(key).map((h) => parseInt(h, 16) / 65535);
  const nib = [...key].map((h) => parseInt(h, 16));
  const a = o.alpha ?? 1;
  const rev = o.rev ?? 1;
  const t = o.t ?? 0;
  const ch = o.chroma ?? 0;
  const hot = o.hot ?? 0;
  if (a <= 0.003 || rev <= 0) return;
  ctx.save();
  ctx.translate(x, y);
  ctx.rotate(o.spin ?? 0);
  ctx.lineCap = "round";
  const lw = Math.max(0.6, R * 0.0055);
  ctx.lineWidth = lw;
  ctx.strokeStyle = col(t * 0.03, ch * 0.5, 0.55 * a);
  ctx.beginPath();
  ctx.arc(0, 0, R, -Math.PI / 2, -Math.PI / 2 + TAU * eio(clamp(rev * 1.5)));
  ctx.stroke();
  if (o.detail !== false && R > 10) {
    ctx.beginPath();
    for (let i = 0; i < 64; i++) {
      const p = clamp(rev * 2.2 - (i / 64) * 1.2);
      if (p <= 0) continue;
      const ang = (i / 64) * TAU - Math.PI / 2;
      const len = R * (0.025 + (nib[i] / 15) * 0.1) * eo(p);
      const r0 = R * 0.965;
      ctx.moveTo(Math.cos(ang) * r0, Math.sin(ang) * r0);
      ctx.lineTo(Math.cos(ang) * (r0 - len), Math.sin(ang) * (r0 - len));
    }
    ctx.strokeStyle = col(0.1, ch * 0.4, 0.5 * a);
    ctx.stroke();
    ctx.strokeStyle = ink(0.16 * a * rev);
    for (const k of [0.25, 0.31]) {
      ctx.beginPath();
      ctx.arc(0, 0, R * k, 0, TAU);
      ctx.stroke();
    }
  }
  const nodes: [number, number][] = [];
  for (let i = 0; i < 16; i++) {
    const p = clamp(rev * 1.6 - (i / 16) * 0.6);
    if (p <= 0) continue;
    const v = gs[i];
    const lane = i % 4;
    const rr = R * (0.42 + lane * 0.135);
    const a0 = (i / 16) * TAU - Math.PI / 2 + gs[(i + 5) % 16] * 0.5 + Math.sin(t * 0.7 + i) * 0.04 * hot;
    const span = (0.22 + v * 1.4) * eo(p);
    const c = col(i / 16 + t * 0.06, ch, (0.55 + 0.4 * ch) * a);
    ctx.strokeStyle = c;
    ctx.lineWidth = Math.max(1, R * (0.011 + (nib[i * 4] % 3) * 0.006));
    ctx.beginPath();
    ctx.arc(0, 0, rr, a0, a0 + span);
    ctx.stroke();
    const ex = Math.cos(a0 + span) * rr;
    const ey = Math.sin(a0 + span) * rr;
    nodes.push([ex, ey]);
    if (R > 10) {
      ctx.fillStyle = c;
      ctx.beginPath();
      ctx.arc(ex, ey, Math.max(1, R * 0.013), 0, TAU);
      ctx.fill();
    }
  }
  if (R > 30 && nodes.length > 3) {
    ctx.strokeStyle = col(0.6, ch * 0.6, 0.12 * a);
    ctx.lineWidth = Math.max(0.5, lw * 0.8);
    ctx.beginPath();
    for (let i = 0; i < nodes.length; i++) {
      if (nib[i * 3] % 3 !== 0) continue;
      const j = (i + 3 + (nib[i] % 5)) % nodes.length;
      ctx.moveTo(nodes[i][0], nodes[i][1]);
      ctx.lineTo(nodes[j][0], nodes[j][1]);
    }
    ctx.stroke();
  }
  ctx.fillStyle = col(t * 0.1, ch, a * (0.75 + 0.25 * hot));
  ctx.beginPath();
  ctx.arc(0, 0, Math.max(1.1, R * 0.024 * (1 + hot * 0.6)), 0, TAU);
  ctx.fill();
  ctx.restore();
}

// -------------------------------------------------------------------- braid
// Three strands, one per phase, pinched at both ends. `p` reveals from A.
type BraidOpts = { alpha?: number; amp?: number; unravel?: number; lw?: number; from?: number };
function braid(ctx: C2, A: { x: number; y: number }, B: { x: number; y: number }, p: number, t: number, o: BraidOpts = {}) {
  if (p <= 0) return;
  const dx = B.x - A.x;
  const dy = B.y - A.y;
  const L = Math.hypot(dx, dy) || 1;
  const nx = -dy / L;
  const ny = dx / L;
  const N = Math.max(16, (L / 5) | 0);
  const un = o.unravel ?? 0;
  const a = (o.alpha ?? 1) * (1 - un * 0.9);
  const from = o.from ?? 0;
  ctx.save();
  ctx.globalCompositeOperation = "lighter";
  ctx.lineCap = "round";
  for (let s = 0; s < 3; s++) {
    ctx.beginPath();
    let started = false;
    for (let i = 0; i <= N; i++) {
      const u = i / N;
      if (u > p || u < from) continue;
      const env = Math.sin(Math.PI * u);
      const w = (o.amp ?? 12) * env * (1 + un * 2);
      const ph = ((u * L) / 70) * TAU - t * 2.6 + (s * TAU) / 3;
      const drift = un * un * (hash(s, i >> 2) - 0.5) * 160 * env;
      const off = Math.sin(ph) * w + drift;
      const x = A.x + dx * u + nx * off;
      const y = A.y + dy * u + ny * off + un * un * 60 * env * (hash(i >> 3, s) - 0.3);
      if (un > 0 && hash(i >> 1, s + 9) < un * 1.1) {
        started = false;
        continue;
      }
      if (!started) {
        ctx.moveTo(x, y);
        started = true;
      } else ctx.lineTo(x, y);
    }
    ctx.strokeStyle = spec(s / 3 + t * 0.05, a * 0.85);
    ctx.lineWidth = o.lw ?? 1.7;
    ctx.stroke();
  }
  ctx.restore();
}

function glowDot(ctx: C2, x: number, y: number, r: number, c: string) {
  ctx.save();
  ctx.globalCompositeOperation = "lighter";
  ctx.fillStyle = c;
  ctx.beginPath();
  ctx.arc(x, y, r, 0, TAU);
  ctx.fill();
  ctx.globalAlpha = 0.35;
  ctx.beginPath();
  ctx.arc(x, y, r * 2.6, 0, TAU);
  ctx.fill();
  ctx.restore();
}

// ------------------------------------------------------------------- UI bits
function rrect(ctx: C2, x: number, y: number, w: number, h: number, r: number) {
  ctx.beginPath();
  ctx.moveTo(x + r, y);
  ctx.arcTo(x + w, y, x + w, y + h, r);
  ctx.arcTo(x + w, y + h, x, y + h, r);
  ctx.arcTo(x, y + h, x, y, r);
  ctx.arcTo(x, y, x + w, y, r);
  ctx.closePath();
}
function panel(ctx: C2, x: number, y: number, w: number, h: number, a: number, title?: string) {
  if (a <= 0) return;
  ctx.save();
  rrect(ctx, x, y, w, h, 10);
  ctx.fillStyle = `rgba(9,12,17,${0.86 * a})`;
  ctx.fill();
  ctx.strokeStyle = ink(0.2 * a);
  ctx.lineWidth = 1;
  ctx.stroke();
  // corner ticks — the only ornament
  ctx.strokeStyle = ink(0.55 * a);
  ctx.beginPath();
  ctx.moveTo(x, y + 14);
  ctx.lineTo(x, y);
  ctx.lineTo(x + 14, y);
  ctx.moveTo(x + w - 14, y + h);
  ctx.lineTo(x + w, y + h);
  ctx.lineTo(x + w, y + h - 14);
  ctx.stroke();
  if (title) {
    ctx.font = mono(12, 500);
    ctx.letterSpacing = "2.5px";
    ctx.fillStyle = ink(0.5 * a);
    ctx.fillText(title.toUpperCase(), x + 22, y + 30);
    ctx.letterSpacing = "0px";
  }
  ctx.restore();
}
function button(ctx: C2, x: number, y: number, w: number, h: number, label: string, a: number, press = 0, danger = false) {
  if (a <= 0) return;
  ctx.save();
  rrect(ctx, x, y, w, h, 6);
  const c = danger ? RED : INK;
  ctx.fillStyle = `rgba(${c[0]},${c[1]},${c[2]},${(0.08 + press * 0.5) * a})`;
  ctx.fill();
  ctx.strokeStyle = `rgba(${c[0]},${c[1]},${c[2]},${(0.5 + press * 0.5) * a})`;
  ctx.stroke();
  ctx.font = mono(14, 500);
  ctx.textAlign = "center";
  ctx.fillStyle = `rgba(${c[0]},${c[1]},${c[2]},${a})`;
  ctx.fillText(label, x + w / 2, y + h / 2 + 5);
  ctx.restore();
}
function field(ctx: C2, x: number, y: number, w: number, label: string, value: string, a: number, vc?: string) {
  if (a <= 0) return;
  ctx.font = mono(12);
  ctx.fillStyle = ink(0.42 * a);
  ctx.fillText(label, x, y);
  ctx.strokeStyle = ink(0.16 * a);
  ctx.beginPath();
  ctx.moveTo(x, y + 30);
  ctx.lineTo(x + w, y + 30);
  ctx.stroke();
  ctx.font = mono(16);
  ctx.fillStyle = vc ?? ink(0.92 * a);
  ctx.fillText(value, x, y + 22);
}
function pointer(ctx: C2, x: number, y: number, a: number, press: number) {
  if (a <= 0) return;
  ctx.save();
  ctx.translate(x, y);
  if (press > 0) {
    ctx.strokeStyle = ink(0.6 * press * a);
    ctx.beginPath();
    ctx.arc(0, 0, 8 + (1 - press) * 26, 0, TAU);
    ctx.stroke();
  }
  ctx.scale(1 - press * 0.12, 1 - press * 0.12);
  ctx.beginPath();
  ctx.moveTo(0, 0);
  ctx.lineTo(0, 22);
  ctx.lineTo(6, 16.5);
  ctx.lineTo(10.5, 26);
  ctx.lineTo(14, 24.5);
  ctx.lineTo(9.5, 15.2);
  ctx.lineTo(17, 15);
  ctx.closePath();
  ctx.fillStyle = `rgba(240,244,250,${a})`;
  ctx.fill();
  ctx.strokeStyle = `rgba(0,0,0,${0.8 * a})`;
  ctx.lineWidth = 1.2;
  ctx.stroke();
  ctx.restore();
}
type Key = [number, number, number];
function path(t: number, keys: Key[]) {
  if (t <= keys[0][0]) return { x: keys[0][1], y: keys[0][2] };
  for (let i = 0; i < keys.length - 1; i++) {
    const [t0, x0, y0] = keys[i];
    const [t1, x1, y1] = keys[i + 1];
    if (t <= t1) {
      const p = eio(seg(t, t0, t1));
      return { x: lerp(x0, x1, p), y: lerp(y0, y1, p) };
    }
  }
  const k = keys[keys.length - 1];
  return { x: k[1], y: k[2] };
}
const pressAt = (t: number, ts: number[]) => ts.reduce((m, c) => Math.max(m, t >= c && t < c + 0.35 ? 1 - (t - c) / 0.35 : 0), 0);

// Text that decodes from noise into its final characters.
function scramble(text: string, t: number, t0: number, dur: number, seed = 1) {
  let s = "";
  for (let i = 0; i < text.length; i++) {
    const settle = t0 + (i / Math.max(1, text.length)) * dur * 0.7 + dur * 0.3;
    if (t >= settle || text[i] === " ") s += text[i];
    else if (t < t0 + (i / text.length) * dur * 0.3) s += " ";
    else s += HEX[(hash(i + seed * 97, Math.floor(t * 30)) * 16) | 0];
  }
  return s;
}

// A packet of glyphs flying along an arc from one place to another.
function glyphFlight(ctx: C2, text: string, t: number, t0: number, dur: number, A: { x: number; y: number }, B: { x: number; y: number }, lift: number, size = 15) {
  const n = text.length;
  ctx.save();
  ctx.font = mono(size, 500);
  ctx.textAlign = "center";
  ctx.globalCompositeOperation = "lighter";
  for (let i = 0; i < n; i++) {
    const st = t0 + (i / n) * dur * 0.25;
    const p = seg(t, st, st + dur * 0.75);
    if (p <= 0 || p >= 1) continue;
    const e = eio(p);
    const cx = (A.x + B.x) / 2;
    const cy = Math.min(A.y, B.y) - lift;
    const x = (1 - e) * (1 - e) * A.x + 2 * (1 - e) * e * cx + e * e * B.x + Math.sin(i * 1.7 + t * 6) * 6 * Math.sin(Math.PI * p);
    const y = (1 - e) * (1 - e) * A.y + 2 * (1 - e) * e * cy + e * e * B.y;
    ctx.fillStyle = spec(i / n + t * 0.2, 0.9 * Math.sin(Math.PI * p) + 0.1);
    ctx.fillText(text[i], x, y);
  }
  ctx.restore();
}

// ================================================================ SCENES ===

// 00 — BOOT: one command, one key.
const TERM = { x: 200, y: 390, lh: 48, size: 26 };
function endpointPositions(ctx: C2) {
  ctx.font = mono(TERM.size);
  const cw = ctx.measureText("0").width;
  const x0 = TERM.x + 34 + cw * 11;
  const y = TERM.y + TERM.lh * 4;
  return groups(HOST).map((_, i) => ({ x: x0 + i * cw * 5, y }));
}
function scene0(ctx: C2, t: number) {
  const fade = 1 - seg(t, 4.9, 5.6);
  if (fade <= 0) return;
  ctx.font = mono(TERM.size);
  ctx.fillStyle = ink(0.4 * fade);
  ctx.fillText("$", TERM.x, TERM.y);
  const s = typed("cmd", t);
  ctx.fillStyle = ink(0.95 * fade);
  ctx.fillText(s, TERM.x + 34, TERM.y);
  if (t < AT.out) {
    const w = ctx.measureText(s).width;
    ctx.fillStyle = ink(caret(t) * 0.9);
    ctx.fillRect(TERM.x + 36 + w, TERM.y - 22, 14, 28);
  }
  const outs: [string, string][] = [
    ["scope", "direct · workspace ~/code/atlas · read-only"],
    ["admin", "https://127.0.0.1:52817/"],
    ["relay", "https://aether-relay.tudorandrei.xyz"],
  ];
  const cw = ctx.measureText("0").width;
  outs.forEach(([k, v], i) => {
    const a = seg(t, AT.out + i * 0.14, AT.out + i * 0.14 + 0.06) * fade;
    ctx.fillStyle = ink(0.38 * a);
    ctx.fillText(k, TERM.x + 34, TERM.y + TERM.lh * (i + 1));
    ctx.fillStyle = ink(0.72 * a);
    ctx.fillText(v, TERM.x + 34 + cw * 11, TERM.y + TERM.lh * (i + 1));
  });
  const te = AT.out + 0.5;
  const la = seg(t, te, te + 0.06) * fade;
  ctx.fillStyle = ink(0.38 * la);
  ctx.fillText("endpoint", TERM.x + 34, TERM.y + TERM.lh * 4);
  // the endpoint groups themselves are drawn by scene1 so they can fly
}

// 01 — IDENTITY: the key folds into a face.
function heroCam(t: number): Cam {
  // exponential pull-back from inside the room to the whole field
  const p = eio(seg(t, 12.0, 20.5));
  const D = 1.2 * Math.exp(Math.log(78 / 1.2) * p) + (t > 20.5 ? (t - 20.5) * 6 : 0);
  return { D, ry: 0.42 * eio(seg(t, 12.4, 22)), rx: 0.16 * eio(seg(t, 12.4, 22)), f: 900 };
}
function scene1(ctx: C2, t: number) {
  const pos = endpointPositions(ctx);
  const gs = groups(HOST);
  const te = AT.out + 0.5;
  const labelFade = 1 - seg(t, 11.6, 12.5);
  if (labelFade <= 0) return;
  const spin = t * 0.035;
  for (let i = 0; i < 16; i++) {
    const appear = te + 0.1 + i * 0.045;
    if (t < appear) continue;
    const settle = appear + 0.25;
    const txt = t < settle ? scramble(gs[i], t, appear, 0.25, i) : gs[i];
    const st = AT.flyStart + i * 0.035;
    const p = eio(seg(t, st, st + 1.35));
    const ang = (i / 16) * TAU - Math.PI / 2 + spin;
    const B = { x: W / 2 + Math.cos(ang) * 382, y: H / 2 + Math.sin(ang) * 382 + 6 };
    const A = pos[i];
    const cx = lerp(A.x, W / 2, 0.55);
    const cy = lerp(A.y, H / 2, 0.2) - 160;
    const x = (1 - p) * (1 - p) * A.x + 2 * (1 - p) * p * cx + p * p * B.x;
    const y = (1 - p) * (1 - p) * A.y + 2 * (1 - p) * p * cy + p * p * B.y;
    const size = lerp(TERM.size, 17, p);
    ctx.font = mono(size, 500);
    ctx.textAlign = p > 0.5 ? "center" : "left";
    const flying = p > 0 && p < 1;
    if (flying) {
      for (let g = 1; g <= 3; g++) {
        const q = eio(seg(t - g * 0.025, st, st + 1.35));
        const gx = (1 - q) * (1 - q) * A.x + 2 * (1 - q) * q * cx + q * q * B.x;
        const gy = (1 - q) * (1 - q) * A.y + 2 * (1 - q) * q * cy + q * q * B.y;
        ctx.fillStyle = ink(0.18 / g);
        ctx.fillText(txt, gx, gy);
      }
    }
    ctx.fillStyle = ink((p >= 1 ? 0.55 : 0.95) * labelFade);
    ctx.fillText(txt, x, y);
    // landed: a hairline from the label to the ring
    if (p >= 1) {
      const lp = seg(t, st + 1.35, st + 1.8);
      ctx.strokeStyle = ink(0.22 * labelFade);
      ctx.beginPath();
      ctx.moveTo(W / 2 + Math.cos(ang) * 312, H / 2 + Math.sin(ang) * 312);
      ctx.lineTo(W / 2 + Math.cos(ang) * lerp(312, 362, lp), H / 2 + Math.sin(ang) * lerp(312, 362, lp));
      ctx.stroke();
    }
  }
  ctx.textAlign = "left";
  if (t < 12) {
    const cam = heroCam(t);
    const c = proj([0, 0, 0], cam)!;
    sigil(ctx, c.x, c.y, c.k * 0.4, HOST, { rev: seg(t, 5.9, 8.2), t, spin, hot: 0.4 });
  }
}

// 02 — ISOLATION / 07 — NETWORK: the field of rooms.
type Room = { p: V3; s: number; seed: number; ph: number; per: number; dir: V3; key?: string; nn: number; d: number };
const ROOMS: Room[] = [];
const LINKS: { a: number; b: number; t0: number; u: number }[] = [];
(function build() {
  const r = rng(99);
  ROOMS.push({ p: [0, 0, 0], s: 0.55, seed: 7, ph: 0, per: 99, dir: [1, 0, 0], key: HOST, nn: -1, d: 0 });
  while (ROOMS.length < 720) {
    const p: V3 = [(r() * 2 - 1) * 78, (r() * 2 - 1) * 30, (r() * 2 - 1) * 60 + 18];
    if (Math.hypot(p[0], p[1], p[2]) < 4) continue;
    let d: V3 = [r() * 2 - 1, r() * 2 - 1, r() * 2 - 1];
    const m = Math.hypot(...d) || 1;
    d = [d[0] / m, d[1] / m, d[2] / m];
    ROOMS.push({ p, s: 0.55, seed: (r() * 1e9) | 0, ph: r() * 10, per: 2.4 + r() * 4, dir: d, nn: -1, d: Math.hypot(...p) });
  }
  const seen = new Set<string>();
  ROOMS.forEach((a, i) => {
    const ds = ROOMS.map((b, j) => [j === i ? 1e9 : Math.hypot(a.p[0] - b.p[0], a.p[1] - b.p[1], a.p[2] - b.p[2]), j]).sort((x, y) => x[0] - y[0]);
    for (const [, j] of ds.slice(0, 2)) {
      const k = i < j ? `${i}-${j}` : `${j}-${i}`;
      if (seen.has(k)) continue;
      seen.add(k);
      const mid = Math.hypot((a.p[0] + ROOMS[j].p[0]) / 2, (a.p[1] + ROOMS[j].p[1]) / 2, (a.p[2] + ROOMS[j].p[2]) / 2);
      LINKS.push({ a: i, b: j, t0: AT.cascade + mid * 0.048 + hash(i, j) * 0.5, u: hash(j, i) });
    }
  });
})();

type FieldOpts = { rays?: boolean; links?: boolean; implode?: number; alpha: number; heroChroma?: number };
function drawField(ctx: C2, t: number, cam: Cam, o: FieldOpts) {
  const q = ei(o.implode ?? 0);
  const P = (p: V3): V3 => [p[0] * (1 - q), p[1] * (1 - q), p[2] * (1 - q)];
  const lit = new Float32Array(ROOMS.length);
  let linkCount = 0;
  if (o.links) {
    ctx.save();
    ctx.globalCompositeOperation = "lighter";
    for (const L of LINKS) {
      const g = eo(seg(t, L.t0, L.t0 + 0.7));
      if (g <= 0) continue;
      linkCount++;
      lit[L.a] = Math.max(lit[L.a], g);
      lit[L.b] = Math.max(lit[L.b], g);
      const A = proj(P(ROOMS[L.a].p), cam);
      const B = proj(P(ROOMS[L.b].p), cam);
      if (!A || !B) continue;
      const depth = clamp(1 - (Math.min(A.z, B.z) - 20) / 140);
      const a = o.alpha * depth * (0.25 + 0.55 * g);
      ctx.strokeStyle = spec(L.u + t * 0.04, a);
      ctx.lineWidth = 0.6 + depth * 1.1;
      ctx.beginPath();
      ctx.moveTo(A.x, A.y);
      ctx.lineTo(lerp(A.x, B.x, g), lerp(A.y, B.y, g));
      ctx.stroke();
      if (g >= 1) {
        const u = (t * 0.55 + L.u * 3) % 1;
        const x = lerp(A.x, B.x, u);
        const y = lerp(A.y, B.y, u);
        ctx.fillStyle = spec(L.u + 0.3, a * 1.2, 82);
        ctx.fillRect(x - 1, y - 1, 2 + depth * 1.5, 2 + depth * 1.5);
      }
    }
    ctx.restore();
  }
  for (let i = ROOMS.length - 1; i >= 0; i--) {
    const R = ROOMS[i];
    const c = proj(P(R.p), cam);
    if (!c) continue;
    const near = clamp((c.z - 0.9) / 2.5);
    const depth = clamp(1 - (c.z - 25) / 130);
    const isHero = i === 0;
    const a = o.alpha * near * (isHero ? 1 : 0.25 + 0.75 * depth);
    if (a <= 0.004) continue;
    if (c.x < -300 || c.x > W + 300 || c.y < -300 || c.y > H + 300) continue;
    const rs = c.k * 0.4;
    const chroma = isHero ? Math.max(o.heroChroma ?? 0, lit[i]) : lit[i];
    const boxA = isHero ? seg(t, 12.2, 13.2) * a : a * (0.1 + 0.3 * depth);
    if (c.k * R.s > 3) cube(ctx, cam, P(R.p), R.s * (1 - q * 0.6), col(i * 0.13, chroma * 0.5, 0.55 * boxA), isHero ? 1.2 : 0.8);
    if (rs > 12) {
      R.key ??= hexKey(R.seed);
      sigil(ctx, c.x, c.y, rs, R.key, { alpha: a * 0.9, t: t + R.ph, spin: t * 0.03 + R.ph, chroma, hot: 0.3, detail: rs > 22 });
    } else if (rs > 2.5) {
      ctx.strokeStyle = col(i * 0.13 + t * 0.05, chroma, a * 0.7);
      ctx.lineWidth = 0.8;
      ctx.beginPath();
      ctx.arc(c.x, c.y, rs, 0, TAU);
      ctx.stroke();
      ctx.fillStyle = col(i * 0.13, chroma, a);
      ctx.fillRect(c.x - 0.8, c.y - 0.8, 1.6, 1.6);
    } else {
      ctx.fillStyle = col(i * 0.13, chroma, a * 0.9);
      ctx.fillRect(c.x - 0.9, c.y - 0.9, 1.8, 1.8);
    }
    // alone: every so often an agent reaches out and hits its own wall
    if (o.rays && !isHero && rs > 1.2) {
      const local = (t + R.ph) % R.per;
      if (local < 1.1) {
        const g = eo(seg(local, 0, 0.35));
        const m = Math.max(Math.abs(R.dir[0]), Math.abs(R.dir[1]), Math.abs(R.dir[2]));
        const hit: V3 = [R.p[0] + (R.dir[0] / m) * R.s, R.p[1] + (R.dir[1] / m) * R.s, R.p[2] + (R.dir[2] / m) * R.s];
        const e = proj([lerp(R.p[0], hit[0], g), lerp(R.p[1], hit[1], g), lerp(R.p[2], hit[2], g)], cam);
        if (e) {
          ctx.strokeStyle = ink(0.7 * a * (1 - seg(local, 0.35, 0.8)));
          ctx.beginPath();
          ctx.moveTo(c.x, c.y);
          ctx.lineTo(e.x, e.y);
          ctx.stroke();
          if (local > 0.35) {
            const f = 1 - seg(local, 0.35, 1.1);
            glowDot(ctx, e.x, e.y, 1.2 + c.k * 0.04, red(f * a * 1.4));
          }
        }
      }
    }
  }
  return linkCount;
}

const LABELLED: [number, string][] = [];
(function pickLabels() {
  const names = ["laptop · NAT", "vps · firewall: deny in", "office · 10.0.0.0/8", "phone · CGNAT", "homelab · ufw default deny"];
  const cam = heroCam(20.5);
  const cand = ROOMS.map((r, i) => ({ i, c: proj(r.p, cam) }))
    .filter((o) => o.i > 0 && o.c && o.c.x > 260 && o.c.x < W - 560 && o.c.y > 190 && o.c.y < H - 300 && o.c.z < 75)
    .sort((a, b) => a.c!.z - b.c!.z);
  const picked: typeof cand = [];
  for (const o of cand) {
    if (picked.every((p) => Math.hypot(p.c!.x - o.c!.x, p.c!.y - o.c!.y) > 230)) picked.push(o);
    if (picked.length === names.length) break;
  }
  picked.forEach((o, k) => LABELLED.push([o.i, names[k]]));
})();

function scene2(ctx: C2, t: number) {
  const cam = heroCam(t);
  const fade = 1 - seg(t, 21.2, 22);
  drawField(ctx, t, cam, { rays: true, alpha: fade });
  const c0 = proj([0, 0, 0], cam)!;
  if (t < 12.6) sigil(ctx, c0.x, c0.y, c0.k * 0.4, HOST, { t, spin: t * 0.035, hot: 0.4, alpha: 1 });
  LABELLED.forEach(([i, name], k) => {
    const a = win(t, 17.4 + k * 0.18, 21.6, 0.4, 0.5) * fade;
    if (a <= 0) return;
    const c = proj(ROOMS[i].p, cam);
    if (!c) return;
    const x1 = c.x + 34;
    const y1 = c.y - 34;
    ctx.strokeStyle = ink(0.4 * a);
    ctx.beginPath();
    ctx.moveTo(c.x + 6, c.y - 6);
    ctx.lineTo(x1, y1);
    ctx.lineTo(x1 + 22, y1);
    ctx.stroke();
    ctx.font = mono(14);
    ctx.fillStyle = ink(0.7 * a);
    ctx.fillText(scramble(name, t, 17.4 + k * 0.18, 0.5, k + 3), x1 + 30, y1 + 5);
  });
}

// 03 — INVITE and 04 — TRAVERSAL share one stage: two rooms.
const TUDOR: V3 = [-6, -0.8, 0];
const ANA_P: V3 = [6, -0.8, 0];
const ROOM_S = 2.4;
const SIG_RW = 1.7;
function stageCam(t: number): Cam {
  if (t < 34) return { D: 15 - 0.4 * seg(t, 22, 34), ry: -0.1 + 0.06 * seg(t, 22, 34), rx: 0.05, f: 900 };
  const p = eio(seg(t, 34, 37.5));
  return { D: lerp(14.6, 18.5, p), ry: -0.04 + 0.12 * seg(t, 34, 45), rx: lerp(0.05, 0.2, p), ty: lerp(0, -2.6, p), f: 900 };
}
function drawRoom(ctx: C2, t: number, cam: Cam, c: V3, key: string, alpha: number, chroma: number, door: number, side: number) {
  cube(ctx, cam, c, ROOM_S, col(0.1, chroma * 0.4, 0.42 * alpha), 1.1);
  const p = proj(c, cam);
  if (p) sigil(ctx, p.x, p.y, p.k * SIG_RW, key, { alpha, t, spin: t * 0.04 + (side > 0 ? 2 : 0), chroma, hot: 0.5 });
  if (door > 0) {
    const fx = c[0] + side * ROOM_S;
    const h = 0.75 * eo(door);
    const q = [
      proj([fx, c[1] - h, -h], cam),
      proj([fx, c[1] - h, h], cam),
      proj([fx, c[1] + h, h], cam),
      proj([fx, c[1] + h, -h], cam),
    ];
    if (q.every(Boolean)) {
      ctx.save();
      ctx.beginPath();
      q.forEach((v, i) => (i ? ctx.lineTo(v!.x, v!.y) : ctx.moveTo(v!.x, v!.y)));
      ctx.closePath();
      ctx.fillStyle = `rgba(3,5,8,${0.9 * alpha})`;
      ctx.fill();
      ctx.globalCompositeOperation = "lighter";
      ctx.strokeStyle = col(t * 0.1, chroma, 0.95 * alpha, 80);
      ctx.lineWidth = 1.6;
      ctx.stroke();
      ctx.restore();
    }
  }
}
function roomLabel(ctx: C2, cam: Cam, c: V3, name: string, role: string, a: number) {
  const p = proj([c[0], c[1] - ROOM_S - 1.1, c[2]], cam);
  if (!p || a <= 0) return;
  ctx.textAlign = "center";
  ctx.font = mono(16, 500);
  ctx.fillStyle = ink(0.9 * a);
  ctx.fillText(name, p.x, p.y - 10);
  ctx.font = mono(12);
  ctx.letterSpacing = "2px";
  ctx.fillStyle = ink(0.4 * a);
  ctx.fillText(role.toUpperCase(), p.x, p.y + 10);
  ctx.letterSpacing = "0px";
  ctx.textAlign = "left";
}

function scene3(ctx: C2, t: number) {
  const cam = stageCam(t);
  const inA = seg(t, 22.1, 23.4);
  const doors = eo(seg(t, AT.doors, AT.doors + 1.1));
  drawRoom(ctx, t, cam, TUDOR, HOST, inA, 0, doors, 1);
  drawRoom(ctx, t, cam, ANA_P, ANA, inA, 0, doors, -1);
  roomLabel(ctx, cam, TUDOR, "tudor", "host · runs the agent", inA);
  roomLabel(ctx, cam, ANA_P, "ana", "caller · sends tasks", inA);
  const tp = proj(TUDOR, cam)!;

  // signing: the first colour in the film is a signature
  const sg = seg(t, AT.sign, AT.sign + 1.2);
  if (sg > 0) {
    const R = tp.k * SIG_RW * 1.2;
    const fadeS = 1 - seg(t, 31.2, 32.4);
    ctx.save();
    ctx.globalCompositeOperation = "lighter";
    ctx.lineWidth = 2;
    for (let s = 0; s < 3; s++) {
      ctx.strokeStyle = spec(s / 3 + t * 0.1, 0.85 * fadeS);
      ctx.beginPath();
      const a0 = -Math.PI / 2 + s * 0.08 + t * 0.9;
      ctx.arc(tp.x, tp.y, R + s * 5, a0, a0 + TAU * eo(sg));
      ctx.stroke();
    }
    ctx.font = mono(12, 500);
    ctx.fillStyle = spec(0.2, 0.8 * fadeS * seg(t, AT.sign + 0.3, AT.sign + 0.6));
    ctx.textAlign = "center";
    ctx.fillText("ed25519 · signed by node issuer", tp.x, tp.y + R + 34);
    ctx.textAlign = "left";
    ctx.restore();
  }

  const pa = win(t, 24.0, 33.9, 0.5, 0.5);
  const pt = win(t, 26.4, 33.9, 0.5, 0.5);
  // caller panel — the words are the real Aether page's words
  const ax = 1080;
  const ay = 690;
  panel(ctx, ax, ay, 480, 250, pa, "Connect to a friend's agent");
  if (pa > 0) {
    ctx.font = mono(13);
    ctx.fillStyle = ink(0.55 * pa);
    ctx.fillText("1  Send your friend this connect code", ax + 22, ay + 64);
    const g = groups(ANA);
    ctx.font = mono(16, 500);
    ctx.fillStyle = ink(0.9 * pa);
    ctx.fillText(g.slice(0, 8).join(" "), ax + 22, ay + 94);
    ctx.fillText(g.slice(8).join(" "), ax + 22, ay + 118);
    button(ctx, ax + 390, ay + 48, 68, 26, "Copy", pa, pressAt(t, [AT.copyClick]));
    const s2 = seg(t, AT.inviteFly + 0.6, AT.inviteFly + 0.9);
    ctx.font = mono(13);
    ctx.fillStyle = ink((0.3 + 0.3 * s2) * pa);
    ctx.fillText("2  Paste the invite they send back", ax + 22, ay + 160);
    ctx.strokeStyle = ink(0.18 * pa);
    ctx.strokeRect(ax + 22, ay + 176, 330, 34);
    if (s2 > 0) {
      ctx.font = mono(13);
      ctx.fillStyle = spec(t * 0.1, s2 * pa);
      ctx.fillText(TL.type.invite.text.slice(0, 34) + "…", ax + 32, ay + 198);
    }
    button(ctx, ax + 366, ay + 176, 92, 34, "Connect", pa * (0.4 + 0.6 * s2), pressAt(t, [AT.connectClick]));
    const ok = seg(t, AT.connectClick + 0.25, AT.connectClick + 0.45);
    if (ok > 0) {
      ctx.font = mono(13, 500);
      ctx.fillStyle = spec(0.35, ok * pa);
      ctx.fillText("✓ contact saved · tudor", ax + 22, ay + 234);
    }
  }
  // host panel
  const hx = 360;
  const hy = 690;
  panel(ctx, hx, hy, 480, 250, pt, "Who can use your agent");
  if (pt > 0) {
    ctx.font = mono(13);
    ctx.fillStyle = ink(0.55 * pt);
    ctx.fillText("Invite someone", hx + 22, hy + 60);
    field(ctx, hx + 22, hy + 88, 130, "name", typed("name", t), pt);
    const codeIn = seg(t, AT.codeFly + 1.0, AT.codeFly + 1.3);
    field(ctx, hx + 172, hy + 88, 286, "connect code", codeIn > 0 ? groups(ANA).slice(0, 5).join(" ") + " …" : "", pt, ink(0.9 * pt * codeIn));
    field(ctx, hx + 22, hy + 146, 130, "access", "7 days", pt);
    button(ctx, hx + 312, hy + 142, 146, 34, "Create invite", pt, pressAt(t, [AT.createClick]));
    const inv = typed("invite", t);
    if (inv.length) {
      ctx.font = mono(12);
      ctx.fillStyle = ink(0.45 * pt);
      ctx.fillText("Send them one of these. They are shown only once.", hx + 22, hy + 206);
      ctx.font = mono(13, 500);
      ctx.fillStyle = spec(t * 0.1, pt);
      ctx.fillText(inv.length > 40 ? inv.slice(0, 40) + "…" : inv, hx + 22, hy + 230);
    }
  }
  // the connect code travels to the host, then the invite travels back
  glyphFlight(ctx, groups(ANA).slice(0, 6).join(""), t, AT.codeFly, 1.3, { x: ax + 150, y: ay + 100 }, { x: hx + 300, y: hy + 108 }, 240);
  glyphFlight(ctx, TL.type.invite.text.slice(0, 30), t, AT.inviteFly, 1.1, { x: hx + 150, y: hy + 226 }, { x: ax + 170, y: ay + 196 }, 200, 14);

  const cur = path(t, [
    [24.4, 1400, 1010],
    [25.3, ax + 424, ay + 62],
    [26.4, ax + 424, ay + 62],
    [27.9, hx + 385, hy + 160],
    [30.8, hx + 385, hy + 160],
    [31.6, ax + 412, ay + 194],
    [32.6, ax + 412, ay + 194],
    [33.6, ax + 520, ay + 320],
  ]);
  pointer(ctx, cur.x, cur.y, win(t, 24.3, 33.7, 0.3, 0.4), pressAt(t, [AT.copyClick, AT.createClick, AT.connectClick]));
}

// NAT membranes, the relay, and the hole-punch
const MEM_X = 2.2;
function membrane(ctx: C2, cam: Cam, x: number, a: number, label: string, crack: number, open: number, t: number, seed: number) {
  if (a <= 0) return;
  ctx.save();
  ctx.strokeStyle = ink(0.09 * a);
  ctx.lineWidth = 0.8;
  ctx.beginPath();
  for (let i = 0; i <= 10; i++) {
    const y = lerp(-4.4, 2.8, i / 10);
    line(ctx, proj([x, y, -3.4], cam), proj([x, y, 3.4], cam));
    const z = lerp(-3.4, 3.4, i / 10);
    line(ctx, proj([x, -4.4, z], cam), proj([x, 2.8, z], cam));
  }
  ctx.stroke();
  ctx.strokeStyle = ink(0.3 * a);
  ctx.beginPath();
  const cs: V3[] = [[x, -4.4, -3.4], [x, -4.4, 3.4], [x, 2.8, 3.4], [x, 2.8, -3.4]];
  cs.forEach((v, i) => line(ctx, proj(v, cam), proj(cs[(i + 1) % 4], cam)));
  ctx.stroke();
  const lp = proj([x, -4.8, 0], cam);
  if (lp) {
    ctx.textAlign = "center";
    ctx.font = mono(13, 500);
    ctx.fillStyle = ink(0.6 * a);
    ctx.fillText(label, lp.x, lp.y);
    ctx.textAlign = "left";
  }
  const cy = TUDOR[1];
  if (crack > 0) {
    const r = rng(seed);
    ctx.globalCompositeOperation = "lighter";
    ctx.strokeStyle = col(t * 0.1, open, 0.8 * a, 80);
    ctx.lineWidth = 1.2;
    for (let k = 0; k < 15; k++) {
      const ang = (k / 15) * TAU + r() * 0.3;
      const len = (0.7 + r() * 1.8) * eo(crack);
      let py = cy;
      let pz = 0;
      ctx.beginPath();
      const s0 = proj([x, py, pz], cam);
      if (s0) ctx.moveTo(s0.x, s0.y);
      for (let j = 1; j <= 5; j++) {
        const d = (len * j) / 5;
        const jit = (r() - 0.5) * 0.35;
        py = cy + Math.sin(ang + jit) * d;
        pz = Math.cos(ang + jit) * d;
        const s = proj([x, py, pz], cam);
        if (s) ctx.lineTo(s.x, s.y);
      }
      ctx.stroke();
    }
  }
  if (open > 0) {
    const rad = 0.95 * eo5(open);
    ctx.beginPath();
    for (let i = 0; i <= 48; i++) {
      const g = (i / 48) * TAU;
      const s = proj([x, cy + Math.sin(g) * rad, Math.cos(g) * rad], cam);
      if (s) {
        if (i) ctx.lineTo(s.x, s.y);
        else ctx.moveTo(s.x, s.y);
      }
    }
    ctx.globalCompositeOperation = "source-over";
    ctx.fillStyle = `rgba(3,5,8,${0.95 * a})`;
    ctx.fill();
    ctx.globalCompositeOperation = "lighter";
    ctx.strokeStyle = spec(t * 0.12, a, 78);
    ctx.lineWidth = 2;
    ctx.stroke();
  }
  ctx.restore();
}
function ripple(ctx: C2, cam: Cam, x: number, t: number, t0: number, color: (a: number) => string) {
  const p = seg(t, t0, t0 + 0.9);
  if (p <= 0 || p >= 1) return;
  for (let k = 0; k < 3; k++) {
    const rad = eo(clamp(p * 1.3 - k * 0.15)) * 2.6;
    ctx.strokeStyle = color((1 - p) * 0.7);
    ctx.beginPath();
    for (let i = 0; i <= 40; i++) {
      const g = (i / 40) * TAU;
      const s = proj([x, TUDOR[1] + Math.sin(g) * rad, Math.cos(g) * rad], cam);
      if (s) {
        if (i) ctx.lineTo(s.x, s.y);
        else ctx.moveTo(s.x, s.y);
      }
    }
    ctx.stroke();
  }
}
function comet(ctx: C2, A: V3, B: V3, u: number, cam: Cam, c: (a: number) => string) {
  ctx.save();
  ctx.globalCompositeOperation = "lighter";
  ctx.lineCap = "round";
  for (let k = 0; k < 8; k++) {
    const u0 = clamp(u - (k + 1) * 0.03);
    const u1 = clamp(u - k * 0.03);
    const a = proj([lerp(A[0], B[0], u0), lerp(A[1], B[1], u0), lerp(A[2], B[2], u0)], cam);
    const b = proj([lerp(A[0], B[0], u1), lerp(A[1], B[1], u1), lerp(A[2], B[2], u1)], cam);
    ctx.strokeStyle = c(1 - k / 8);
    ctx.lineWidth = 3 - k * 0.3;
    ctx.beginPath();
    line(ctx, a, b);
    ctx.stroke();
  }
  ctx.restore();
}
function sparks(ctx: C2, cam: Cam, o: V3, t: number, t0: number, seed: number) {
  const dt = t - t0;
  if (dt < 0 || dt > 1.1) return;
  const r = rng(seed);
  ctx.save();
  ctx.globalCompositeOperation = "lighter";
  for (let i = 0; i < 44; i++) {
    const v: V3 = [-(0.5 + r() * 3.5), (r() - 0.7) * 4, (r() - 0.5) * 4];
    const p: V3 = [o[0] + v[0] * dt, o[1] + v[1] * dt + 4 * dt * dt, o[2] + v[2] * dt];
    const q: V3 = [p[0] - v[0] * 0.04, p[1] - v[1] * 0.04, p[2] - v[2] * 0.04];
    ctx.strokeStyle = red((1 - dt / 1.1) * (0.5 + r() * 0.5));
    ctx.lineWidth = 1.4;
    ctx.beginPath();
    line(ctx, proj(p, cam), proj(q, cam));
    ctx.stroke();
  }
  ctx.restore();
}
const RELAY: V3 = [0, -7.2, 1.5];
function bez(A: V3, C: V3, B: V3, u: number): V3 {
  const m = 1 - u;
  return [m * m * A[0] + 2 * m * u * C[0] + u * u * B[0], m * m * A[1] + 2 * m * u * C[1] + u * u * B[1], m * m * A[2] + 2 * m * u * C[2] + u * u * B[2]];
}
function relayPoint(u: number): V3 {
  return u < 0.5 ? bez(TUDOR, [-5, -7.5, 1], RELAY, u * 2) : bez(RELAY, [5, -7.5, 1], ANA_P, u * 2 - 1);
}

const LOG4: [number, string, string, "ink" | "red" | "spec"][] = [
  [34.4, "dial", "tudor → ana  " + groups(ANA)[0] + "…" + groups(ANA)[15], "ink"],
  [35.5, "direct", "✕  no inbound route · CGNAT", "red"],
  [36.3, "relay", "✓  aether-relay.tudorandrei.xyz", "ink"],
  [AT.punch, "punch", "…  both sides, at once", "ink"],
  [AT.open, "direct", "✓  path upgraded · relay released", "spec"],
  [AT.open + 0.7, "session", "QUIC · TLS 1.3 · ed25519 on both ends", "spec"],
];
function scene4(ctx: C2, t: number) {
  const cam = stageCam(t);
  const open = seg(t, AT.open, AT.open + 0.6);
  const chroma = seg(t, AT.open + 0.3, AT.open + 1.2);
  const memA = seg(t, 34, 34.8) * (1 - seg(t, 44.4, 45));
  const all = 1 - seg(t, 44.5, 45);
  const crack = seg(t, AT.crack, AT.crack + 0.7);
  membrane(ctx, cam, -MEM_X, memA, "NAT · 192.168.1.1", crack, open, t, 5);
  membrane(ctx, cam, MEM_X, memA, "CGNAT · 100.64.0.0/10", crack, open, t, 9);
  drawRoom(ctx, t, cam, TUDOR, HOST, all, chroma, 1, 1);
  drawRoom(ctx, t, cam, ANA_P, ANA, all, chroma, 1, -1);
  roomLabel(ctx, cam, TUDOR, "tudor", "host", all);
  roomLabel(ctx, cam, ANA_P, "ana", "caller", all);

  // 1. the direct packet leaves tudor's NAT and dies at ana's
  const hitP: V3 = [MEM_X, TUDOR[1], 0];
  const dp = seg(t, AT.directTry, AT.directHit);
  if (dp > 0 && dp < 1) comet(ctx, TUDOR, hitP, eio(dp), cam, (a) => ink(a));
  ripple(ctx, cam, -MEM_X, t, lerp(AT.directTry, AT.directHit, 0.5), (a) => ink(a * 0.5));
  ripple(ctx, cam, MEM_X, t, AT.directHit, (a) => red(a));
  sparks(ctx, cam, hitP, t, AT.directHit, 3);

  // 2. the relay: stateless, forwards encrypted packets
  const ra = seg(t, AT.relayOn, AT.relayOn + 0.5) * all;
  const rp = proj(RELAY, cam);
  if (rp && ra > 0) {
    const pulse = 1 + Math.sin(t * 5) * 0.08;
    ctx.save();
    ctx.globalCompositeOperation = "lighter";
    ctx.strokeStyle = ink(0.8 * ra);
    ctx.beginPath();
    ctx.arc(rp.x, rp.y, 18 * pulse, 0, TAU);
    ctx.stroke();
    ctx.beginPath();
    ctx.arc(rp.x, rp.y, 30 * pulse, 0, TAU * eo(ra));
    ctx.strokeStyle = ink(0.3 * ra);
    ctx.stroke();
    glowDot(ctx, rp.x, rp.y, 3, ink(ra));
    ctx.restore();
    ctx.textAlign = "center";
    ctx.font = mono(14, 500);
    ctx.fillStyle = ink(0.85 * ra);
    ctx.fillText("aether-relay.tudorandrei.xyz", rp.x, rp.y - 44);
    ctx.font = mono(12);
    ctx.fillStyle = ink(0.4 * ra);
    ctx.fillText("stateless · sees only encrypted packets", rp.x, rp.y + 52);
    ctx.textAlign = "left";
  }
  const pathA = seg(t, AT.relayOn, AT.relayOn + 1) * (1 - seg(t, AT.open + 0.2, AT.open + 1.2)) * all;
  if (pathA > 0) {
    ctx.save();
    ctx.setLineDash([5, 9]);
    ctx.lineDashOffset = -t * 50;
    ctx.strokeStyle = ink(0.55 * pathA);
    ctx.lineWidth = 1.2;
    ctx.beginPath();
    const grow = seg(t, AT.relayOn, AT.relayOn + 1);
    for (let i = 0; i <= 80; i++) {
      const u = (i / 80) * grow;
      const s = proj(relayPoint(u), cam);
      if (s) {
        if (i) ctx.lineTo(s.x, s.y);
        else ctx.moveTo(s.x, s.y);
      }
    }
    ctx.stroke();
    ctx.restore();
    if (grow >= 1) {
      for (let k = 0; k < 4; k++) {
        const u = (t * 0.55 + k / 4) % 1;
        const s = proj(relayPoint(u), cam);
        if (s) glowDot(ctx, s.x, s.y, 2.2, ink(0.9 * pathA));
      }
    }
  }
  // 3. hole-punch: both sides fire at once
  const pp = seg(t, AT.punch, AT.crack);
  if (pp > 0 && pp < 1) {
    comet(ctx, TUDOR, [MEM_X, TUDOR[1], 0], eio(pp), cam, (a) => spec(0.1, a));
    comet(ctx, ANA_P, [-MEM_X, TUDOR[1], 0], eio(pp), cam, (a) => spec(0.6, a));
  }
  // 4. the braid: a direct, end-to-end path
  const A = proj(TUDOR, cam)!;
  const B = proj(ANA_P, cam)!;
  braid(ctx, A, B, eio(seg(t, AT.open, AT.open + 0.8)), t, { alpha: all, amp: 10 });
  if (t > AT.open + 0.8) {
    for (let k = 0; k < 5; k++) {
      const u = (t * 0.45 + k / 5) % 1;
      glowDot(ctx, lerp(A.x, B.x, u), lerp(A.y, B.y, u), 2.4, spec(k / 5 + t * 0.1, 0.9 * all, 85));
    }
  }
  // the log
  ctx.font = mono(15);
  LOG4.forEach(([ts, k, v, c], i) => {
    const a = seg(t, ts, ts + 0.12) * all;
    if (a <= 0) return;
    const y = 190 + i * 28;
    ctx.fillStyle = ink(0.38 * a);
    ctx.fillText(k, 110, y);
    ctx.fillStyle = c === "red" ? red(a) : c === "spec" ? spec(0.12 + (i - 4) * 0.18, a, 78) : ink(0.8 * a);
    ctx.fillText(t < ts + 0.3 ? scramble(v, t, ts, 0.3, i) : v, 210, y);
  });
}

// 05 — DELEGATION and 06 — REVOCATION: inside the host's sandbox.
function hexPts(cx: number, cy: number, R: number) {
  return Array.from({ length: 6 }, (_, i) => {
    const a = -Math.PI / 2 + (i * TAU) / 6;
    return { x: cx + Math.cos(a) * R, y: cy + Math.sin(a) * R };
  });
}
function rayHex(cx: number, cy: number, R: number, dx: number, dy: number) {
  const pts = hexPts(cx, cy, R);
  let best = 1e9;
  for (let i = 0; i < 6; i++) {
    const a = pts[i];
    const b = pts[(i + 1) % 6];
    const ex = b.x - a.x;
    const ey = b.y - a.y;
    const den = dx * ey - dy * ex;
    if (Math.abs(den) < 1e-9) continue;
    const s = ((a.x - cx) * ey - (a.y - cy) * ex) / den;
    const u = ((a.x - cx) * dy - (a.y - cy) * dx) / den;
    if (s > 0 && u >= 0 && u <= 1) best = Math.min(best, s);
  }
  return { x: cx + dx * best, y: cy + dy * best };
}
const OUTSIDE: [string, number, number][] = [
  ["~/.codex", 70, 330],
  ["~/.ssh", 60, 470],
  ["MCP servers", 60, 650],
  ["memories", 70, 790],
];
const FILES = ["README.md", "Cargo.toml", "crates/aether", "src/env.rs", "PLAN.md", "scripts/"];
const RESULTS: [string, "ink" | "spec"][] = [
  ["▸ install steps assume mise; add a fallback", "ink"],
  ["▸ 'node request' example omits --scope", "ink"],
  ["▸ limits table matches the code: 1 MiB/frame", "ink"],
  ["✓ completed · 11.4 s", "spec"],
];
function wrapCmd(s: string, cut: number) {
  return s.length <= cut ? [s] : [s.slice(0, cut), s.slice(cut)];
}
function scene56(ctx: C2, t: number) {
  const inA = seg(t, 45, 45.6);
  const L = eio(seg(t, 57, 57.9));
  const cx = 540;
  const cy = lerp(560, 430, L);
  const sc = lerp(1, 0.76, L);
  const HR = 300 * sc;
  const SR = 150 * sc;
  const revoked = seg(t, AT.unravel, AT.unravel + 1.4);
  const chroma = inA * (1 - seg(t, AT.unravel + 0.3, AT.unravel + 1.3));
  const anaP = { x: 1450, y: 720 };
  const work = win(t, AT.reqArrive, AT.resultSend + 0.4, 0.3, 0.6);

  // sandbox
  const hp = hexPts(cx, cy, HR);
  const door = rayHex(cx, cy, HR, anaP.x - cx, anaP.y - cy);
  ctx.save();
  ctx.lineWidth = 1.2;
  for (const [k, al] of [[1, 0.5], [0.965, 0.18]] as const) {
    const pts = hexPts(cx, cy, HR * k);
    ctx.strokeStyle = ink(al * inA);
    ctx.beginPath();
    pts.forEach((p, i) => (i ? ctx.lineTo(p.x, p.y) : ctx.moveTo(p.x, p.y)));
    ctx.closePath();
    ctx.stroke();
  }
  // the door in the wall: open while ana holds a grant, sealed after revoke
  const seal = seg(t, AT.unravel + 0.2, AT.unravel + 0.6);
  ctx.globalCompositeOperation = "lighter";
  ctx.strokeStyle = seal > 0 ? `rgba(255,255,255,${(1 - seal) * 0.9 + 0.3})` : spec(t * 0.1, 0.9 * inA, 80);
  ctx.lineWidth = seal > 0 ? 2 + (1 - seal) * 5 : 3;
  ctx.beginPath();
  ctx.moveTo(door.x, door.y - 22 * sc);
  ctx.lineTo(door.x, door.y + 22 * sc);
  ctx.stroke();
  ctx.restore();
  ctx.textAlign = "center";
  ctx.font = mono(12, 500);
  ctx.letterSpacing = "3px";
  ctx.fillStyle = ink(0.45 * inA);
  ctx.fillText("SANDBOX · TUDOR", cx, hp[0].y - 20);
  ctx.letterSpacing = "0px";
  const lbA = inA * (1 - L);
  ctx.font = mono(14);
  ctx.fillStyle = ink(0.7 * lbA);
  ctx.fillText("workspace  ~/code/atlas  · read-only", cx, hp[3].y + 34);
  ctx.fillStyle = ink(0.45 * lbA);
  ctx.fillText("CODEX_HOME  <data-dir>/codex-home", cx, hp[3].y + 58);
  ctx.textAlign = "left";

  // what the task cannot reach
  OUTSIDE.forEach(([name, x, y], i) => {
    const bt = AT.blocked[i];
    const a = inA * (1 - L * 0.7);
    const hitT = bt + 0.35;
    const blocked = t > hitT;
    ctx.font = mono(15);
    ctx.fillStyle = blocked ? red(a * lerp(1, 0.55, seg(t, hitT, hitT + 1.5))) : ink(0.4 * a);
    ctx.fillText((blocked ? "✕ " : "  ") + name, x, y);
    const g = seg(t, bt, hitT);
    if (g > 0 && t < hitT + 0.9) {
      const e = rayHex(cx, cy, HR * 0.965, x + 60 - cx, y - 5 - cy);
      const s0 = { x: cx + (e.x - cx) * (SR / HR), y: cy + (e.y - cy) * (SR / HR) };
      ctx.strokeStyle = ink(0.75 * (1 - seg(t, hitT, hitT + 0.6)));
      ctx.beginPath();
      ctx.moveTo(s0.x, s0.y);
      ctx.lineTo(lerp(s0.x, e.x, eo(g)), lerp(s0.y, e.y, eo(g)));
      ctx.stroke();
      if (t > hitT) glowDot(ctx, e.x, e.y, 3, red(1 - seg(t, hitT, hitT + 0.9)));
    }
  });

  // the host's agent at work
  sigil(ctx, cx, cy, SR, HOST, { alpha: inA, t: t * (1 + work * 2), spin: t * (0.04 + work * 0.5), chroma, hot: 0.5 + work });
  FILES.forEach((f, i) => {
    const a = work * inA;
    if (a <= 0) return;
    const ang = (i / FILES.length) * TAU + t * 0.35;
    const rr = SR + 58 * sc;
    const x = cx + Math.cos(ang) * rr;
    const y = cy + Math.sin(ang) * rr * 0.9;
    const isR = i === 0;
    const hl = isR ? seg(t, 49.4, 49.9) : 0;
    ctx.textAlign = "center";
    ctx.font = mono(13, isR ? 600 : 400);
    ctx.fillStyle = hl > 0 ? spec(t * 0.2, a) : ink(0.45 * a);
    ctx.fillText(f, x, y);
    ctx.textAlign = "left";
  });

  // the caller's agent
  sigil(ctx, anaP.x, anaP.y, 78, ANA, { alpha: inA, t, spin: -t * 0.05, chroma, hot: 0.4 });
  ctx.textAlign = "center";
  ctx.font = mono(15, 500);
  ctx.fillStyle = ink(0.8 * inA);
  ctx.fillText("ana", anaP.x, anaP.y + 112);
  ctx.textAlign = "left";
  const hostP = { x: cx, y: cy };
  braid(ctx, anaP, hostP, 1, t, { alpha: inA, amp: 11, unravel: revoked });
  // request frame out, result frames back
  const frame = (u: number, label: string, a: number) => {
    const x = lerp(anaP.x, hostP.x, u);
    const y = lerp(anaP.y, hostP.y, u);
    ctx.save();
    ctx.font = mono(12, 500);
    const w = ctx.measureText(label).width + 26;
    rrect(ctx, x - w / 2, y - 15, w, 30, 5);
    ctx.fillStyle = `rgba(6,9,13,${0.9 * a})`;
    ctx.fill();
    ctx.strokeStyle = spec(t * 0.2, a);
    ctx.stroke();
    ctx.fillStyle = ink(a);
    ctx.textAlign = "center";
    ctx.fillText(label, x, y + 4);
    ctx.restore();
  };
  const rq = seg(t, AT.reqSend, AT.reqArrive);
  if (rq > 0 && rq < 1) frame(eio(rq), "A2A frame · 312 B · signed", Math.sin(Math.PI * rq) * 0.6 + 0.4);
  for (let k = 0; k < 3; k++) {
    const st = AT.resultSend + k * 0.2;
    const q = seg(t, st, st + 0.8);
    if (q > 0 && q < 1) frame(1 - eio(q), `result ${k + 1}/3`, Math.sin(Math.PI * q) * 0.6 + 0.4);
  }

  // caller terminal
  const tx = 1020;
  const ty = 110;
  const tw = 840;
  const th = 380;
  panel(ctx, tx, ty, tw, th, inA, "ana@studio — terminal");
  if (inA > 0) {
    const shake = t > AT.denied && t < AT.denied + 0.3 ? (hash(Math.floor(t * 60), 3) - 0.5) * 14 * (1 - (t - AT.denied) / 0.3) : 0;
    ctx.save();
    ctx.translate(shake, 0);
    ctx.font = mono(18);
    let y = ty + 72;
    const lh = 29;
    const printCmd = (id: "req" | "req2") => {
      const lines = wrapCmd(typed(id, t), 51);
      lines.forEach((ln, i) => {
        ctx.fillStyle = ink(0.4 * inA);
        if (i === 0) ctx.fillText("$", tx + 26, y);
        ctx.fillStyle = ink(0.95 * inA);
        ctx.fillText(ln, tx + 50 + (i ? 24 : 0), y);
        y += lh;
      });
      return lines;
    };
    const fade1 = 1 - seg(t, 57.2, 57.8);
    if (fade1 > 0) {
      ctx.globalAlpha = fade1;
      printCmd("req");
      if (t > AT.reqSend && t < AT.resultArrive) {
        const sp = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"[Math.floor(t * 12) % 10];
        ctx.fillStyle = ink(0.5 * inA);
        ctx.fillText(`${sp} sent to tudor · waiting`, tx + 50, y);
      }
      RESULTS.forEach(([s, c], i) => {
        const a = seg(t, AT.resultArrive + i * 0.28, AT.resultArrive + i * 0.28 + 0.1) * inA;
        if (a <= 0) return;
        ctx.fillStyle = c === "spec" ? spec(0.3, a, 78) : ink(0.85 * a);
        ctx.fillText(s, tx + 50, y + i * lh);
      });
      ctx.globalAlpha = 1;
    } else {
      y = ty + 72;
      const s2 = seg(t, 57.8, 58.1);
      ctx.globalAlpha = s2;
      ctx.fillStyle = ink(0.4 * inA);
      ctx.fillText("✓ completed · 11.4 s", tx + 50, y);
      y += lh * 1.6;
      printCmd("req2");
      const d = seg(t, AT.denied, AT.denied + 0.05);
      if (d > 0) {
        ctx.font = mono(22, 600);
        ctx.fillStyle = red(d);
        ctx.fillText("error: permission denied", tx + 50, y + 8);
        ctx.font = mono(14);
        ctx.fillStyle = ink(0.45 * seg(t, AT.denied + 0.5, AT.denied + 0.8));
        ctx.fillText("the grant was revoked · a restart does not restore it", tx + 50, y + 40);
      }
      if (t < typingDone("req2") + 0.1 && t > TL.type.req2.t0 - 1) {
        ctx.font = mono(18);
        ctx.fillStyle = ink(caret(t) * 0.8);
        ctx.fillRect(tx + 50, y - 20, 11, 24);
      }
      ctx.globalAlpha = 1;
    }
    ctx.restore();
  }

  // the host revokes
  const pa = win(t, 57.5, 63.8, 0.4, 0.5);
  const px = 120;
  const py = 790;
  panel(ctx, px, py, 820, 160, pa, "Who can use your agent");
  if (pa > 0) {
    const confirm = t >= AT.revokeClick + 0.1 && t < AT.confirmClick + 0.1;
    const gone = seg(t, AT.confirmClick + 0.1, AT.confirmClick + 0.5);
    ctx.font = mono(12);
    ctx.fillStyle = ink(0.35 * pa);
    ctx.fillText("NAME", px + 24, py + 72);
    ctx.fillText("LAST TASK", px + 150, py + 72);
    ctx.fillText("ACCESS ENDS", px + 450, py + 72);
    ctx.font = mono(16);
    if (!confirm && gone === 0) {
      ctx.fillStyle = ink(0.95 * pa);
      ctx.fillText("ana", px + 24, py + 112);
      ctx.fillText("Review the README", px + 150, py + 112);
      ctx.fillText("in 7 days", px + 450, py + 112);
      button(ctx, px + 690, py + 90, 106, 34, "Revoke", pa, pressAt(t, [AT.revokeClick]), true);
    } else if (confirm) {
      ctx.fillStyle = ink(0.95 * pa);
      ctx.fillText("Revoke ana? Their next task fails.", px + 24, py + 112);
      button(ctx, px + 560, py + 90, 116, 34, "Cancel", pa);
      button(ctx, px + 690, py + 90, 106, 34, "Revoke", pa, pressAt(t, [AT.confirmClick]), true);
    } else {
      ctx.fillStyle = ink(0.45 * pa * gone);
      ctx.fillText("Nobody else can use your agent.", px + 24, py + 112);
    }
  }
  const cur = path(t, [
    [57.6, 980, 1000],
    [58.25, px + 743, py + 108],
    [59.9, px + 743, py + 108],
    [60.6, 1000, 1010],
  ]);
  pointer(ctx, cur.x, cur.y, win(t, 57.5, 60.7, 0.3, 0.3), pressAt(t, [AT.revokeClick, AT.confirmClick]));
}

// 07 — NETWORK
function scene7(ctx: C2, t: number) {
  const p = seg(t, 64, 71.2);
  const cam: Cam = { D: lerp(125, 70, eio(p)) - 30 * ei(seg(t, AT.implode, AT.logo)), ry: 0.55 + (t - 64) * 0.07, rx: 0.2, f: 900 };
  const inA = seg(t, 64, 65);
  const count = drawField(ctx, t, cam, { links: true, alpha: inA, implode: seg(t, AT.implode, AT.logo), heroChroma: 1 });
  return count;
}

// 08 — the mark
function scene8(ctx: C2, t: number) {
  const out = 1 - seg(t, 77.3, 78);
  const r = seg(t, AT.logo, AT.logo + 1.3);
  sigil(ctx, W / 2, 360, 118, HOST, { rev: r, t, spin: t * 0.05, chroma: 1, hot: 0.6, alpha: out });
  // a quiet halo of ticks
  ctx.save();
  ctx.strokeStyle = ink(0.15 * out * r);
  ctx.setLineDash([1, 7]);
  ctx.beginPath();
  ctx.arc(W / 2, 360, 150 + 20 * eo(r), 0, TAU);
  ctx.stroke();
  ctx.restore();
  const wm = "AETHER";
  ctx.font = mono(92, 300);
  ctx.letterSpacing = "46px";
  ctx.textAlign = "center";
  const txt = scramble(wm, t, AT.wordmark, 1.0, 5);
  ctx.fillStyle = ink(out * seg(t, AT.wordmark, AT.wordmark + 0.2));
  ctx.fillText(txt, W / 2 + 23, 610);
  ctx.letterSpacing = "0px";
  ctx.font = serif(40);
  ctx.fillStyle = ink(0.85 * out * seg(t, AT.tagline, AT.tagline + 0.8));
  ctx.fillText("Agent to agent. Key to key.", W / 2, 676);
  // call to action
  const cta = typed("cta", t);
  const ca = seg(t, TL.type.cta.t0 - 0.4, TL.type.cta.t0) * out;
  if (ca > 0) {
    ctx.font = mono(30, 500);
    const full = "$ " + TL.type.cta.text;
    const w = ctx.measureText(full).width;
    const bx = W / 2 - w / 2 - 34;
    ctx.textAlign = "left";
    rrect(ctx, bx, 770, w + 68, 64, 8);
    ctx.strokeStyle = spec(t * 0.1, 0.7 * ca);
    ctx.lineWidth = 1.2;
    ctx.stroke();
    ctx.fillStyle = ink(0.4 * ca);
    ctx.fillText("$", bx + 34, 812);
    ctx.fillStyle = ink(ca);
    const ctaX = bx + 34 + ctx.measureText("$ ").width;
    ctx.fillText(cta, ctaX, 812);
    ctx.fillStyle = ink(caret(t) * ca);
    ctx.fillRect(ctaX + ctx.measureText(cta).width + 4, 787, 16, 32);
    ctx.textAlign = "center";
    ctx.font = mono(17);
    ctx.fillStyle = ink(0.5 * out * seg(t, AT.url, AT.url + 0.5));
    ctx.fillText("github.com/flowcopilot/aether", W / 2, 884);
  }
  ctx.textAlign = "left";
}

// ============================================================== compositing
const SCENES: [number, string][] = [
  [0, "00 BOOT"], [5.5, "01 IDENTITY"], [12, "02 ISOLATION"], [22, "03 INVITE"], [34, "04 TRAVERSAL"],
  [45, "05 DELEGATION"], [57, "06 REVOCATION"], [64, "07 NETWORK"],
];
function hud(ctx: C2, t: number, links: number) {
  const a = 0.55 * seg(t, 0.2, 1.2) * (1 - seg(t, 70.4, 71));
  if (a <= 0) return;
  const m = 48;
  ctx.strokeStyle = ink(a);
  ctx.lineWidth = 1;
  ctx.beginPath();
  for (const [x, y, sx, sy] of [[m, m, 1, 1], [W - m, m, -1, 1], [m, H - m, 1, -1], [W - m, H - m, -1, -1]]) {
    ctx.moveTo(x, y + sy * 22);
    ctx.lineTo(x, y);
    ctx.lineTo(x + sx * 22, y);
  }
  ctx.stroke();
  ctx.font = mono(12, 500);
  ctx.letterSpacing = "2.5px";
  ctx.fillStyle = ink(a);
  const sc = [...SCENES].reverse().find(([s]) => t >= s)![1];
  ctx.fillText("AETHER / SIGNAL", m + 34, m + 16);
  ctx.fillStyle = ink(a * 0.7);
  ctx.fillText(sc, m + 34, m + 36);
  ctx.textAlign = "right";
  const fr = Math.floor(t * 30);
  const tc = `${String(Math.floor(t / 60)).padStart(2, "0")}:${String(Math.floor(t) % 60).padStart(2, "0")}:${String(fr % 30).padStart(2, "0")}`;
  ctx.fillStyle = ink(a);
  ctx.fillText(tc, W - m - 34, m + 16);
  const linkText = t < 64 ? (t >= AT.open ? "links 1" : "links 0") : `links ${links}`;
  ctx.fillStyle = t >= AT.open + 0.5 && t < AT.unravel + 0.5 ? spec(t * 0.1, a * 1.6) : t >= 64 && links > 0 ? spec(t * 0.1, a * 1.6) : ink(a * 0.7);
  ctx.fillText(t >= AT.unravel + 0.5 && t < 64 ? "links 0" : linkText, W - m - 34, H - m - 12);
  ctx.textAlign = "left";
  ctx.letterSpacing = "0px";
}

function captions(ctx: C2, t: number) {
  for (const c of TL.captions) {
    if (t < c.t0 - 0.05 || t > c.t1 + 0.05) continue;
    const out = 1 - seg(t, c.t1 - 0.55, c.t1);
    ctx.font = serif(54);
    const full = ctx.measureText(c.text).width;
    let x = W / 2 - full / 2;
    const y = 1004;
    ctx.save();
    ctx.shadowColor = "rgba(0,0,0,0.9)";
    ctx.shadowBlur = 24;
    for (let i = 0; i < c.text.length; i++) {
      const ch = c.text[i];
      const w = ctx.measureText(c.text.slice(0, i + 1)).width - ctx.measureText(c.text.slice(0, i)).width;
      const lp = eo(seg(t, c.t0 + i * 0.022, c.t0 + i * 0.022 + 0.7));
      ctx.fillStyle = ink(0.96 * lp * out);
      ctx.fillText(ch, x, y + (1 - lp) * 16 - out * 0 + (1 - out) * -8);
      x += w;
    }
    ctx.restore();
  }
}

export type Surfaces = {
  out: C2;
  scene: HTMLCanvasElement;
  sctx: C2;
  b1: HTMLCanvasElement;
  b1c: C2;
  b2: HTMLCanvasElement;
  b2c: C2;
  grain: HTMLCanvasElement[];
};
export function makeSurfaces(out: C2, doc: Document): Surfaces {
  const mk = (w: number, h: number) => {
    const c = doc.createElement("canvas");
    c.width = w;
    c.height = h;
    return c;
  };
  const scene = mk(W, H);
  const b1 = mk(W / 4, H / 4);
  const b2 = mk(W / 12, H / 12);
  const grain = Array.from({ length: 8 }, (_, k) => {
    const c = mk(256, 256);
    const g = c.getContext("2d")!;
    const img = g.createImageData(256, 256);
    const r = rng(1000 + k);
    for (let i = 0; i < img.data.length; i += 4) {
      const v = (r() * 255) | 0;
      img.data[i] = img.data[i + 1] = img.data[i + 2] = v;
      img.data[i + 3] = 255;
    }
    g.putImageData(img, 0, 0);
    return c;
  });
  return { out, scene, sctx: scene.getContext("2d")!, b1, b1c: b1.getContext("2d")!, b2, b2c: b2.getContext("2d")!, grain };
}

export function renderFrame(S: Surfaces, t: number, fonts: Fonts) {
  F = fonts;
  const g = S.sctx;
  g.setTransform(1, 0, 0, 1, 0, 0);
  g.globalCompositeOperation = "source-over";
  g.globalAlpha = 1;
  g.fillStyle = "#030508";
  g.fillRect(0, 0, W, H);
  g.textBaseline = "alphabetic";

  let links = 0;
  if (t < 5.7) scene0(g, t);
  if (t >= 3 && t < 12.6) scene1(g, t);
  if (t >= 12 && t < 22) scene2(g, t);
  if (t >= 22 && t < 34) scene3(g, t);
  if (t >= 34 && t < 45) scene4(g, t);
  if (t >= 45 && t < 64) scene56(g, t);
  if (t >= 64 && t < AT.logo) links = scene7(g, t);
  if (t >= AT.logo) scene8(g, t);

  const X = S.out;
  X.globalCompositeOperation = "source-over";
  X.globalAlpha = 1;
  X.filter = "none";
  X.drawImage(S.scene, 0, 0);
  // bloom: two blurred, downscaled copies added back
  S.b1c.globalCompositeOperation = "copy";
  S.b1c.filter = "blur(3px)";
  S.b1c.drawImage(S.scene, 0, 0, W / 4, H / 4);
  S.b1c.filter = "none";
  S.b2c.globalCompositeOperation = "copy";
  S.b2c.filter = "blur(3px)";
  S.b2c.drawImage(S.scene, 0, 0, W / 12, H / 12);
  S.b2c.filter = "none";
  X.globalCompositeOperation = "lighter";
  X.globalAlpha = 0.65;
  X.drawImage(S.b1, 0, 0, W, H);
  X.globalAlpha = 0.55;
  X.drawImage(S.b2, 0, 0, W, H);
  X.globalAlpha = 1;
  X.globalCompositeOperation = "source-over";

  hud(X, t, links);
  captions(X, t);

  // cut glitch: slices of the frame slip sideways for a few frames
  for (const c of TL.glitches) {
    const d = t - c;
    if (d < 0 || d > 0.2) continue;
    const k = 1 - d / 0.2;
    S.sctx.globalCompositeOperation = "copy";
    S.sctx.drawImage(X.canvas, 0, 0);
    const fr = Math.floor(t * 30);
    for (let i = 0; i < 14; i++) {
      const y = hash(fr, i) * H;
      const h = 4 + hash(i, fr) * 60;
      const dx = (hash(fr + 7, i) - 0.5) * 220 * k;
      X.drawImage(S.scene, 0, y, W, h, dx, y, W, h);
    }
    X.globalCompositeOperation = "lighter";
    X.fillStyle = `rgba(255,255,255,${0.22 * k * k})`;
    X.fillRect(0, 0, W, H);
    X.globalCompositeOperation = "source-over";
  }
  // the implosion resolves in a flash
  const fl = t - AT.logo;
  if (fl >= 0 && fl < 0.8) {
    X.globalCompositeOperation = "lighter";
    X.fillStyle = `rgba(235,240,255,${0.85 * Math.exp(-fl * 7)})`;
    X.fillRect(0, 0, W, H);
    X.globalCompositeOperation = "source-over";
  }
  // vignette
  const vg = X.createRadialGradient(W / 2, H / 2, H * 0.35, W / 2, H / 2, H * 1.05);
  vg.addColorStop(0, "rgba(0,0,0,0)");
  vg.addColorStop(1, "rgba(0,0,0,0.72)");
  X.fillStyle = vg;
  X.fillRect(0, 0, W, H);
  // film grain
  const fr = Math.floor(t * 30);
  X.globalCompositeOperation = "overlay";
  X.globalAlpha = 0.09;
  const pat = X.createPattern(S.grain[fr % 8], "repeat")!;
  X.save();
  X.translate(hash(fr, 1) * 256, hash(fr, 2) * 256);
  X.fillStyle = pat;
  X.fillRect(-256, -256, W + 512, H + 512);
  X.restore();
  X.globalAlpha = 1;
  X.globalCompositeOperation = "source-over";
  // head and tail from black
  const blk = Math.max(1 - seg(t, 0, 0.5), seg(t, 77.6, 78));
  if (blk > 0) {
    X.fillStyle = `rgba(0,0,0,${blk})`;
    X.fillRect(0, 0, W, H);
  }
}
