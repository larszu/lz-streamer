#!/usr/bin/env python3
"""A3-Korpus aus MUSAN (CC BY 4.0) und Commons-Aufnahmen bauen, getrennt in lernen/ und pruefen/.

Klassen und Herkunft (16 kHz mono, je Datei höchstens --max-s Sekunden):
  sprache  MUSAN speech (LibriVox, US-Gov), Pausen > 0,6 s entfernt
  gesang   MUSAN music mit Gesang (vocals=Y); nur Abschnitte, in denen HTDemucs
           deutlich Gesang findet (Stimme ≥ −35 dBFS und höchstens 10 dB unter
           der Begleitung), in ganzen Sekunden
  orgel    Instrumentalmusik: Commons-Orgelaufnahmen und MUSAN music ohne Gesang
           (vocals=N), dazu Abschnitte ohne Gesang aus den Gesangsstücken
  applaus  Commons-Applaus
  stille   MUSAN noise (Raumgeräusche) auf −60 dBFS abgesenkt

Aufteilung: Sprache nach Datei abwechselnd, Musik nach Künstler (Prüfsumme),
Commons nach Seitenkennung (gerade/ungerade). Kein Stück, kein Künstler liegt in beiden Teilen.

Aufruf (venv mit numpy, soundfile, torch, demucs):
  scripts/vad/musan-korpus.py <musan> <commons> <ziel> [--max-s 60] [--je-klasse 60]
"""
import argparse
import hashlib
import os
import random
import subprocess

import numpy as np
import soundfile as sf

ap = argparse.ArgumentParser()
ap.add_argument("musan")
ap.add_argument("commons")
ap.add_argument("ziel")
ap.add_argument("--max-s", type=int, default=60)
ap.add_argument("--je-klasse", type=int, default=60, help="Dateien je Klasse und Teil (höchstens)")
a = ap.parse_args()
R = 16000
rnd = random.Random(7)


def part_of(key):
    return "lernen" if int(hashlib.md5(key.encode()).hexdigest(), 16) % 2 == 0 else "pruefen"


def save(part, cls, name, x):
    d = f"{a.ziel}/{part}/{cls}"
    os.makedirs(d, exist_ok=True)
    sf.write(f"{d}/{name}.wav", np.clip(x, -1, 1).astype(np.float32), R, subtype="PCM_16")


def load(path, start=0.0, dur=None, filt=None):
    cmd = ["ffmpeg", "-v", "error", "-ss", str(start), "-i", path]
    if dur:
        cmd += ["-t", str(dur)]
    if filt:
        cmd += ["-af", filt]
    cmd += ["-ac", "1", "-ar", str(R), "-f", "f32le", "-"]
    return np.frombuffer(subprocess.run(cmd, check=True, capture_output=True).stdout, dtype="<f4")


def annotations(sub):
    rows = []
    p = f"{a.musan}/{sub}/ANNOTATIONS"
    if os.path.exists(p):
        for line in open(p):
            f = line.split()
            if len(f) >= 2 and f[0].startswith(("music-", "speech-")):
                rows.append(f)
    return rows


count = {}


def room(part, cls):
    return count.get((part, cls), 0) < a.je_klasse


def add(part, cls):
    count[(part, cls)] = count.get((part, cls), 0) + 1


# Sprache
for sub in ("speech/librivox", "speech/us-gov"):
    files = sorted(f for f in os.listdir(f"{a.musan}/{sub}") if f.endswith(".wav"))
    for i, f in enumerate(files):
        part = "lernen" if i % 2 == 0 else "pruefen"
        if not room(part, "sprache"):
            continue
        x = load(f"{a.musan}/{sub}/{f}", 0, a.max_s * 2,
                 "silenceremove=start_periods=1:start_threshold=-45dB:stop_periods=-1:stop_threshold=-45dB:stop_duration=0.6,loudnorm=I=-23:TP=-2")
        x = x[: a.max_s * R]
        if len(x) >= 3 * R:
            save(part, "sprache", f[:-4], x)
            add(part, "sprache")

# Stille: Raumgeräusche leise
noise = sorted(f for f in os.listdir(f"{a.musan}/noise/free-sound") if f.endswith(".wav"))
rnd.shuffle(noise)
for i, f in enumerate(noise[:80]):
    part = "lernen" if i % 2 == 0 else "pruefen"
    if not room(part, "stille"):
        continue
    x = load(f"{a.musan}/noise/free-sound/{f}", 0, 20)
    if len(x) < 3 * R:
        continue
    rms = np.sqrt(np.mean(x ** 2)) + 1e-9
    save(part, "stille", f[:-4], x * (10 ** (-60 / 20) / rms))
    add(part, "stille")

# Musik
music = []
for sub in ("music/fma", "music/fma-western-art", "music/hd-classical", "music/jamendo", "music/rfm"):
    for f in annotations(sub):
        music.append((sub, f[0], f[2] if len(f) > 2 else "N", f[3] if len(f) > 3 else f[0]))
rnd.shuffle(music)

sep = None


def vocals_by_second(x):
    """HTDemucs: Pegel von Stimme und Begleitung je Sekunde (dBFS)."""
    global sep
    import torch
    import torchaudio.functional as AF
    from demucs.apply import apply_model
    from demucs.pretrained import get_model
    if sep is None:
        dev = "mps" if torch.backends.mps.is_available() else "cpu"
        sep = (get_model("htdemucs").to(dev).eval(), dev)
    m, dev = sep
    t = AF.resample(torch.from_numpy(x.copy())[None].repeat(2, 1), R, m.samplerate)
    with torch.no_grad():
        y = apply_model(m, t[None].to(dev), device=dev, split=True, overlap=0.1)[0].cpu()
    v = AF.resample(y[m.sources.index("vocals")].mean(0), m.samplerate, R).numpy()
    acc = AF.resample(sum(y[i] for i in range(len(m.sources)) if m.sources[i] != "vocals").mean(0), m.samplerate, R).numpy()
    n = len(x) // R
    db = lambda s: 10 * np.log10(np.mean(s ** 2) + 1e-12)  # noqa: E731
    return [(db(v[k * R:(k + 1) * R]), db(acc[k * R:(k + 1) * R])) for k in range(n)]


for sub, name, voc, artist in music:
    part = part_of(artist)
    path = f"{a.musan}/{sub}/{name}.wav"
    if not os.path.exists(path):
        continue
    if voc == "Y":
        if not room(part, "gesang"):
            continue
        x = load(path, 20, a.max_s * 2, "loudnorm=I=-23:TP=-2")
        if len(x) < 10 * R:
            continue
        lv = vocals_by_second(x)
        sing = [vd > -35 and vd - ad > -10 for vd, ad in lv]
        none = [vd < -50 or vd - ad < -25 for vd, ad in lv]
        # zusammenhängende Abschnitte ≥ 3 s
        for flags, cls in ((sing, "gesang"), (none, "orgel")):
            k, segs = 0, []
            while k < len(flags):
                if flags[k]:
                    j = k
                    while j < len(flags) and flags[j]:
                        j += 1
                    if j - k >= 3:
                        segs.append((k, j))
                    k = j
                else:
                    k += 1
            for s, e in segs:
                save(part, cls, f"{name}_{s}", x[s * R:e * R])
        add(part, "gesang")
    else:
        if not room(part, "orgel"):
            continue
        x = load(path, 20, a.max_s, "loudnorm=I=-23:TP=-2")
        if len(x) >= 5 * R:
            save(part, "orgel", name, x)
            add(part, "orgel")

# Commons: Orgel und Applaus
for cls in ("orgel", "applaus"):
    d = f"{a.commons}/{cls}"
    if not os.path.isdir(d):
        continue
    for f in sorted(os.listdir(d)):
        if not f.endswith(".wav"):
            continue
        pid = int(f.split("_")[1].split(".")[0])
        part = "lernen" if pid % 2 == 0 else "pruefen"
        x = load(f"{d}/{f}", 0, a.max_s, "loudnorm=I=-23:TP=-2")
        if len(x) >= 3 * R:
            save(part, cls, f[:-4], x)

for part in ("lernen", "pruefen"):
    for cls in ("stille", "sprache", "gesang", "orgel", "applaus"):
        d = f"{a.ziel}/{part}/{cls}"
        n = len(os.listdir(d)) if os.path.isdir(d) else 0
        s = sum(sf.info(f"{d}/{f}").frames for f in os.listdir(d)) // R if n else 0
        print(f"{part}/{cls}: {n} Dateien, {s} s")
