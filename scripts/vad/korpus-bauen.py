#!/usr/bin/env python3
"""Prüfkorpus für die Sprache/Musik/Pause-Erkennung (A3), außerhalb des Repos.

Klassen und Herkunft:
  sprache  – FLEURS dev de_de/en_us (CC BY 4.0), aus scripts/untertitel/testdaten-laden.sh
  stille   – Raumrauschen −60 dBFS mit gelegentlichem Knacken (erzeugt)
  gesang   – Gesangssynthese: Glottis-Impulse, Vibrato, Vokalformanten, Melodie (erzeugt)
  orgel    – additive Orgel: Akkorde aus 3–4 Tönen mit Obertönen, Registerwechsel (erzeugt)
  applaus  – viele zufällige Klatscher (gefiltertes Rauschen, Hüllkurven), Dichte wechselnd (erzeugt)

Erzeugte Klassen sind synthetisch; sie prüfen die Regeln, nicht die Wirklichkeit.
Aufruf: scripts/vad/korpus-bauen.py [ordner] [sekunden-je-klasse=60]
"""
import os, sys, glob, subprocess, wave
import numpy as np

R = 16000
out = sys.argv[1] if len(sys.argv) > 1 else os.path.expanduser("~/Library/Caches/lz-streamer-testdaten/korpus")
secs = int(sys.argv[2]) if len(sys.argv) > 2 else 60
rng = np.random.default_rng(7)

def save(cls, name, x):
    os.makedirs(f"{out}/{cls}", exist_ok=True)
    x = np.clip(x, -1, 1)
    with wave.open(f"{out}/{cls}/{name}.wav", "wb") as w:
        w.setnchannels(1); w.setsampwidth(2); w.setframerate(R)
        w.writeframes((x * 32767).astype("<i2").tobytes())

def formant_filter(x, freqs, bws):
    y = np.zeros_like(x)
    for f, bw in zip(freqs, bws):
        r = np.exp(-np.pi * bw / R); th = 2 * np.pi * f / R
        a1, a2 = -2 * r * np.cos(th), r * r
        z = np.zeros_like(x); y1 = y2 = 0.0
        for i, v in enumerate(x):
            yy = v - a1 * y1 - a2 * y2; z[i] = yy; y2, y1 = y1, yy
        y += z * (1 - r)
    return y

# Sprache: FLEURS
base = os.path.dirname(out)
for lang in ("de_de", "en_us"):
    files = sorted(glob.glob(f"{base}/{lang}/dev/*.wav"))[:12]
    for f in files:
        name = os.path.basename(f)[:-4]
        os.makedirs(f"{out}/sprache", exist_ok=True)
        subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", f, "-af", "silenceremove=start_periods=1:start_threshold=-45dB:stop_periods=-1:stop_threshold=-45dB:stop_duration=0.6,loudnorm=I=-23:TP=-2", "-ac", "1", "-ar", str(R), f"{out}/sprache/{lang}_{name}.wav"], check=True)

# Stille
n = secs * R
x = rng.normal(0, 10 ** (-60 / 20), n)
for t in rng.integers(0, n - 100, 12):
    x[t:t + 30] += rng.normal(0, 0.01, 30)
save("stille", "raum", x)

# Gesang: Melodie aus gehaltenen Tönen (0,4–1,2 s), Vibrato 5,5 Hz, Vokale a/o/e
vowels = {"a": ([800, 1200, 2500], [80, 90, 120]), "o": ([500, 900, 2400], [70, 80, 120]), "e": ([500, 1800, 2500], [70, 90, 120])}
t0, parts = 0, []
notes = [196, 220, 247, 262, 294, 330, 349, 392]
while t0 < n:
    d = int(rng.uniform(0.4, 1.2) * R); f = rng.choice(notes); v = vowels[rng.choice(list(vowels))]
    tt = np.arange(d) / R
    f_inst = f * (1 + 0.012 * np.sin(2 * np.pi * 5.5 * tt))
    ph = np.cumsum(f_inst / R)
    src = (np.mod(ph, 1.0) < 0.08).astype(float) - 0.08  # Impulsfolge
    env = np.minimum(1, tt / 0.05) * np.minimum(1, (tt[-1] - tt) / 0.08 + 0.01)
    parts.append(formant_filter(src, *v) * env); t0 += d
x = np.concatenate(parts)[:n]; save("gesang", "synth", 0.5 * x / np.abs(x).max())

# Orgel: Akkorde, je 2–4 s, Obertöne 1..6
chords = [[131, 165, 196], [147, 175, 220, 294], [165, 196, 247], [175, 220, 262, 349], [196, 247, 294]]
t0, parts = 0, []
while t0 < n:
    d = int(rng.uniform(2, 4) * R); tt = np.arange(d) / R; y = np.zeros(d)
    for f in chords[rng.integers(len(chords))]:
        for h, a in zip(range(1, 7), [1, .6, .4, .3, .2, .15]):
            y += a * np.sin(2 * np.pi * f * h * tt)
    y *= np.minimum(1, tt / 0.03) * np.minimum(1, (tt[-1] - tt) / 0.05 + 0.02)
    parts.append(y); t0 += d
x = np.concatenate(parts)[:n]; save("orgel", "synth", 0.5 * x / np.abs(x).max())

# Applaus: Poisson-Klatscher, Dichte 20–80/s
x = np.zeros(n)
for i in range(0, n, R):
    k = rng.poisson(rng.uniform(20, 80))
    for t in rng.integers(i, min(i + R, n - 800), k):
        L = rng.integers(200, 800)
        clap = rng.normal(0, 1, L) * np.exp(-np.arange(L) / rng.uniform(40, 160))
        x[t:t + L] += clap * rng.uniform(0.05, 0.3)
save("applaus", "synth", 0.6 * x / np.abs(x).max())
print("Korpus:", out)
