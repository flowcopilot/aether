# AETHER / SIGNAL

A 78-second film about Aether, made with [Remotion](https://www.remotion.dev)
and scored in [Strudel](https://strudel.cc).

## The idea

The film has one rule for colour:

| Colour | Meaning |
| --- | --- |
| grey hairlines | an agent alone |
| red | a wall: a NAT, a firewall, a refused request |
| the spectrum | a connection — only a signed permission produces it |

Each agent's face (the sigil) is drawn from its 32-byte endpoint key: 64
ticks from the nibbles and 16 arcs from the four-hex groups. The command
lines, the page copy ("Send your friend this connect code", "Who can use your
agent"), the `aether-invite:` prefix, the relay URL, the Codex sandbox, and
the revoke behaviour all come from the Aether code and README.

| Time | Scene | What it shows |
| --- | --- | --- |
| 0:00 | Boot | `aether node serve` prints an endpoint key |
| 0:05 | Identity | the key folds into the agent's sigil |
| 0:12 | Isolation | 720 agents, each in its own room, hitting its own walls |
| 0:22 | Invite | the connect code and the signed invite, on the real page layout |
| 0:34 | Traversal | direct dial fails at CGNAT, the relay carries it, the hole-punch opens a direct path |
| 0:45 | Delegation | a task runs in the host's sandbox; `~/.codex`, `~/.ssh`, MCP servers, and memories stay out of reach |
| 0:57 | Revocation | one click, the braid comes apart, `permission denied` |
| 1:04 | Network | the field connects, link by link |
| 1:11 | Mark | `aether node serve` |

## Files

- `src/film/timeline.json` — every time in the film. The picture and the
  score both read it (the score keeps a copy of the times it needs).
- `src/film/engine.ts` — the renderer. Every frame is a pure function of time,
  so Remotion can render frames in any order and in parallel.
- `src/Composition.tsx` — the Remotion composition: one canvas, fonts, audio.
- `music/score.strudel.js` — the score. Paste it into
  [strudel.cc](https://strudel.cc) to hear it live.
- `music/record.mjs`, `music/record.html` — play the score in headless Chromium
  and capture the Web Audio output as PCM.
- `music/trim.py` — find the sync click, align the take to the film, write
  `public/score.wav`.

## Commands

Use Node.js 26.12.1 and npm 11.21.1, pinned in the repository's root `mise.toml`.

```sh
npm i
npm run dev      # Remotion Studio
npm run music    # record the Strudel score (real time, about 90 s)
npm run render   # out/aether-signal.mp4, with the score
```

`public/score.wav` and `out/` are not in Git: run `npm run music` once on a
fresh clone before `npm run render`. `npm run music` needs network access (it loads `@strudel/web` 1.3.0 from
unpkg) and a Chrome for Testing binary; set `CHROMIUM` to use another one.

## Recording notes

Strudel has no offline export, so the score is recorded in real time. Three
things had to be solved:

1. **Mini-notation.** Strudel parses every double-quoted string as a pattern.
   Plain strings in the score's helper code use single quotes.
2. **Worklets.** `initStrudel()` loads the AudioWorklets (and so `supersaw`)
   only after a first click. The recorder calls `initAudio()` itself.
3. **Two AudioContexts.** Strudel creates more than one; a node can only
   connect inside its own. The recorder taps each context and keeps the
   loudest take.

The score opens with one sync bar: a click, then the film 2 s later.
`trim.py` finds the click and cuts there.
