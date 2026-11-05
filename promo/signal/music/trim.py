# /// script
# dependencies = ["numpy"]
# ///
"""Align music/raw.wav to the film and write public/score.wav.

The score opens with one sync bar: a single click at bar 0, then the film at
bar 1 (2 s later). Find the click, cut 2 s after it, fade the tail, and
normalise the peak to -1 dBFS.
"""

import json
import wave
from pathlib import Path

import numpy as np

HERE = Path(__file__).parent
TL = json.loads((HERE.parent / "src/film/timeline.json").read_text())

with wave.open(str(HERE / "raw.wav")) as w:
    rate = w.getframerate()
    x = np.frombuffer(w.readframes(w.getnframes()), np.int16).reshape(-1, 2) / 32768.0

click = int(np.argmax(np.abs(x[:, 0]) > 0.05))
start = click + 2 * rate
n = int(TL["duration"] * rate)
y = x[start : start + n].copy()
if len(y) < n:
    raise SystemExit(f"take too short: {len(y) / rate:.1f} s of {TL['duration']} s")

fade = int(0.6 * rate)
y[-fade:] *= np.linspace(1, 0, fade)[:, None]
y *= 10 ** (-1 / 20) / np.abs(y).max()

out = HERE.parent / "public/score.wav"
out.parent.mkdir(exist_ok=True)
with wave.open(str(out), "wb") as w:
    w.setnchannels(2)
    w.setsampwidth(2)
    w.setframerate(rate)
    w.writeframes((y * 32767).astype(np.int16).tobytes())

print(f"click at {click / rate:.3f} s · wrote {out.relative_to(HERE.parent)} · {len(y) / rate:.2f} s")
for s in range(0, int(TL["duration"]), 2):
    seg = y[s * rate : (s + 2) * rate]
    print(f"{s:>3}s {'#' * int(np.sqrt((seg**2).mean()) * 150)}")
