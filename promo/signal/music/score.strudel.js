// AETHER / SIGNAL — the score.
// Paste into https://strudel.cc to hear it live, or run `npm run music`
// to record it to public/score.wav for the film.
//
// 120 BPM, one cycle = one bar = 2 s. Bar 0 is a sync click; the film starts
// at bar 1. Every time below is film time in seconds and mirrors
// src/film/timeline.json.
//
// The harmony follows the picture: D minor while the agent is alone,
// B-flat when the door opens, an A dominant under the hole-punch, and the
// only major chord in the piece — D major 9 — on the logo.

setcpm(30)

const OFF = 2          // sync bar, in seconds
const LEN = 41         // bars to the end of the film, sync bar included
const SLOT = 0.25      // gate resolution, in seconds
const N = Math.round((LEN * 2) / SLOT)

// ---- time helpers ---------------------------------------------------------
const cyc = (t) => (t + OFF) / 2
// 1 while film time is inside any [a, b) range; keeps layers on the bar grid
// (Strudel reads double-quoted strings as mini-notation, so plain strings
// in this helper code use single quotes.)
const gate = (...rs) =>
  '[' + Array.from({ length: N }, (_, i) => {
    const t = i * SLOT - OFF
    return rs.some(([a, b]) => t >= a && t < b) ? 1 : 0
  }).join(' ') + ']/' + LEN
// a level that follows film time, sampled every slot
const level = (f) =>
  '[' + Array.from({ length: N }, (_, i) => +f(i * SLOT - OFF).toFixed(3)).join(' ') + ']/' + LEN
// one bar of `pat`, starting exactly at film time t
const once = (t, pat) => arrange([Math.max(0.001, cyc(t)), silence], [1, pat], [Math.max(0.01, LEN - cyc(t) - 1), silence])
const at = (ts, pat) => stack(...ts.map((t) => once(t, pat)))
const ramp = (t, a, b) => Math.min(1, Math.max(0, (t - a) / (b - a)))

// ---- harmony ---------------------------------------------------------------
const CH = {
  dm9: 'd2,a2,e3,f3', dm7: 'd2,a2,c3,f3', gm: 'g1,d2,bb2,f3', bb: 'bb1,f2,d3,a3',
  f: 'f2,c3,e3,a3', c: 'c2,g2,e3,g3', a7: 'a1,e2,c#3,g3', dM9: 'd2,a2,f#3,c#4,e4',
}
const chordAt = (t) => {
  if (t < 6) return 'd1,a1'
  if (t < 12) return CH.dm9
  if (t < 22) return [CH.dm7, CH.gm][Math.floor((t - 12) / 4) % 2]
  if (t < 34) return [CH.bb, CH.f][Math.floor((t - 22) / 4) % 2]
  if (t < 40) return [CH.c, CH.a7][Math.floor((t - 34) / 4) % 2]
  if (t < 57) return [CH.dm9, CH.bb, CH.f, CH.c][Math.floor((t - 40) / 2) % 4]
  if (t < 64) return 'd1,a1'
  if (t < 71) return [CH.bb, CH.c, CH.dm9, CH.a7][Math.floor((t - 64) / 2) % 4]
  return CH.dM9
}
const bars = Array.from({ length: LEN }, (_, b) => chordAt(b * 2 - OFF))
const chords = '<' + bars.map((c) => '[' + c + ']').join(' ') + '>'
// the arp walks the chord two octaves up, sixteen steps per bar
const up = (n, k) => n.replace(/(\d)$/, (d) => String(+d + k))
const arps = '<' + bars.map((c) => {
  const ns = c.split(',').slice(-4)
  const line = [...ns.map((n) => up(n, 2)), ...ns.map((n) => up(n, 3)).reverse()]
  return '[' + [...line, ...line].join(' ') + ']'
}).join(' ') + '>'
const roots = '<' + bars.map((c) => up(c.split(',')[0], c.split(',')[0].endsWith('1') ? 0 : -1)).join(' ') + '>'

// ---- sections (film seconds) ----------------------------------------------
const TYPING = [[0.6, 2.8], [27.3, 27.7], [29.3, 30.1], [45.6, 47.6], [60.3, 61.7], [74, 75.1]]
const HITS = [5.5, 12, 22, 34, 45, 57, 64]
const CLICKS = [25.6, 28.6, 31.8, 58.4, 59.2]
const PACKETS = [[25.9, 27.2], [30.6, 31.7], [37.3, 40.5], [47.7, 48.8], [52.2, 53.0]]
const GROOVE = [[40.5, 57], [64, 70.75]]

// pad level: a swell in, a lift at every door, silence after the revoke
const padLevel = level((t) => 0.8 * (
  t < 5.5 ? 0.35 * ramp(t, 0, 3)
  : t < 12 ? 0.55
  : t < 22 ? 0.45 - 0.2 * ramp(t, 20, 22)
  : t < 34 ? 0.55
  : t < 40.5 ? 0.45 + 0.3 * ramp(t, 36, 40.5)
  : t < 59.2 ? 0.6
  : t < 64 ? 0.15
  : t < 71 ? 0.4 + 0.45 * ramp(t, 64, 71)
  : 0.9 * (1 - ramp(t, 76, 78.5))))

stack(
  // sync click on bar 0, trimmed away after recording
  once(-OFF, note("c7").s("sine").decay(0.02).sustain(0).gain(0.9)),

  // the sub drone that never leaves
  note("<[d1,d2,a2]>").s("sine").attack(1).release(2).gain(level((t) => (t < 0 ? 0 : t < 59.2 ? 0.13 * ramp(t, 0, 4) : t < 71 ? 0.2 : 0.16 * (1 - ramp(t, 76, 78.5))))),

  // pad
  note(chords).s("supersaw").unison(4).detune(0.18).spread(0.7)
    .attack(0.8).release(2.5).lpf(level((t) => (t < 22 ? 1300 : t < 40.5 ? 1100 : t < 57 ? 1800 : t < 71 ? 900 + 2600 * ramp(t, 64, 71) : 3200)))
    .gain(padLevel).room(0.7).size(6).orbit(1),

  // isolation: a lonely bell answered only by its own echo
  note("<[~ a5 ~ ~ ~ ~ f5 ~] [~ ~ ~ e5 ~ ~ ~ ~] [~ d6 ~ ~ ~ a5 ~ ~] [~ ~ ~ ~ c6 ~ ~ ~]>")
    .s("sine").decay(0.6).sustain(0).gain(0.5).delay(0.6).delaytime(0.375).delayfeedback(0.6)
    .room(0.8).size(8).pan(sine.slow(7)).mask(gate([6, 22])).orbit(2),

  // arp: starts when the door opens, blooms in the groove
  note(arps).s("triangle").decay(0.12).sustain(0)
    .gain(level((t) => (t < 22 ? 0 : t < 34 ? 0.26 : t < 40.5 ? 0.32 : t < 59.2 ? 0.4 : t < 64 ? 0 : t < 71 ? 0.36 + 0.14 * ramp(t, 64, 71) : 0)))
    .delay(0.4).delaytime(0.1875).delayfeedback(0.45).pan(sine.fast(0.5).range(0.25, 0.75)).lpf(4200).orbit(3),

  // bass: eighths, ducked like a sidechain
  note(roots).struct("x*8").s("sawtooth").lpf(320).lpq(4).lpenv(2).lpd(0.12).decay(0.2).sustain(0.3)
    .gain("[0.35 0.8]*4").mask(gate([34, 40.5], ...GROOVE)).postgain(0.7),

  // kick: a pitched-down sine, four on the floor
  note("c2*4").s("sine").penv(30).pdecay(0.06).decay(0.32).sustain(0).gain(1.1).mask(gate(...GROOVE)),
  note("c2 ~ ~ ~").s("sine").penv(30).pdecay(0.06).decay(0.32).sustain(0).gain(0.8).mask(gate([36.25, 40.5])),

  // hats and a noise snare
  s("[~ white]*4").decay(0.03).sustain(0).hpf(8000).gain(0.28).mask(gate([22, 34], [36.25, 57], [64, 70.75])),
  s("white*16").decay(0.015).sustain(0).hpf(9500).gain("[0.12 0.05]*8").mask(gate(...GROOVE)),
  s("~ white ~ white").decay(0.16).sustain(0).bpf(1900).bpq(1.2).gain(0.5).room(0.35).mask(gate([45, 57], [64, 69])),
  // roll into the logo
  s("white*16").decay(0.05).sustain(0).bpf(level((t) => 1200 + 5000 * ramp(t, 69, 71))).gain(level((t) => 0.08 + 0.5 * ramp(t, 69, 71))).mask(gate([69, 71])),

  // risers before each threshold
  s("pink*16").decay(0.2).sustain(0)
    .hpf(level((t) => 300 + 6000 * Math.max(ramp(t, 19.5, 22), ramp(t, 38.3, 40.5), ramp(t, 66, 71))))
    .gain(level((t) => 0.35 * Math.max(ramp(t, 19.5, 22), ramp(t, 38.3, 40.5), ramp(t, 66, 71))))
    .mask(gate([19.5, 22], [38.3, 40.5], [66, 71])),

  // keystrokes
  s("white*32").decay(0.008).sustain(0).hpf(5000).gain(0.22).degradeBy(0.25).pan(rand.range(0.4, 0.6)).mask(gate(...TYPING)),
  // interface clicks
  at(CLICKS, note("a6").s("sine").decay(0.025).sustain(0).gain(0.4)),
  // packets: small digital chatter along the wire
  note("a6 e7 c7 g6 d7 b6 f#7 a7").add(note(irand(3))).fast(2).s("square").decay(0.02).sustain(0).lpf(6000).gain(0.07)
    .degradeBy(0.35).mask(gate(...PACKETS)).pan(rand),

  // impacts: sub drop + a breath of noise
  at(HITS, note("d1").s("sine").penv(24).pdecay(0.5).decay(2.2).sustain(0).gain(1.1)),
  at(HITS, s("brown").decay(0.9).sustain(0).lpf(900).gain(0.45).room(0.6).size(5)),
  // the direct packet dies on a wall
  once(35.5, note("d1").s("square").penv(-12).pdecay(0.3).decay(0.45).sustain(0).crush(5).lpf(1400).gain(0.45)),
  once(35.5, s("white").decay(0.25).sustain(0).bpf(2500).gain(0.35)),
  // the wall cracks, then opens: glass and a drop
  once(39.8, note("[c#7,g7,a#7,d#8]").s("sine").decay(1.4).sustain(0).gain(0.18).room(0.6)),
  once(39.8, s("white").decay(0.12).sustain(0).hpf(5000).gain(0.35)),
  at([40.5], note("d1").s("sine").penv(30).pdecay(0.7).decay(2.8).sustain(0).gain(1.2)),
  at([40.5], s("brown").decay(1.2).sustain(0).lpf(1200).gain(0.5).room(0.7).size(6)),
  // revoke: the braid comes apart downward, then a dry refusal
  once(59.3, note("[d5 a4 f4 d4 a3 f3 d3 a2]").s("triangle").decay(0.2).sustain(0).gain(0.3).delay(0.5).delaytime(0.125).delayfeedback(0.5)),
  once(59.3, s("crackle*8").density(0.2).decay(0.3).gain(0.25)),
  once(61.9, note("[d2 ~ d2 ~ ~ ~ ~ ~]").s("square").decay(0.12).sustain(0.4).crush(6).lpf(900).gain(0.35)),
  // the mark: every thread arrives at once
  once(71, note("d1").s("sine").penv(36).pdecay(0.9).decay(4).sustain(0).gain(1.3)),
  once(71, note("[d2,a2,f#3,c#4,e4,a4]").s("supersaw").unison(5).detune(0.2).spread(0.9).attack(0.005).decay(5).sustain(0).lpf(6000).gain(0.9).room(0.9).size(9)),
  once(71.4, note("[a5 e6 f#6 c#7 e7 a7 ~ ~ ~ ~ ~ ~ ~ ~ ~ ~]").s("sine").decay(0.7).sustain(0).gain(0.18).delay(0.5).delaytime(0.25).delayfeedback(0.55).room(0.8)),
)
