#!/usr/bin/env python3
"""Hilfsprozess für Klare Sprache K2: HTDemucs (demucs, MIT) im Segmentbetrieb.

lz-streamer startet ihn und schickt den Ton (48 kHz, interleaved f32) in Blöcken;
zurück kommt die Gesangs-/Sprach-Spur (Quelle `vocals`) mit absolutem
Sample-Index. Die feste Gesamtverzögerung hält lz-streamer selbst; kommt ein
Segment zu spät, nimmt es dort das Original.

Protokoll (little endian):
  Start, Hilfsprozess → lz-streamer: b"K2OK", u32 Segment-Samples, u32 Vorlauf-Samples
  lz-streamer → Hilfsprozess:   u32 frames, frames×kanäle×f32
  Hilfsprozess → lz-streamer:   u64 start, u32 frames, frames×kanäle×f32

Aufruf: trenner.py <kanäle> [segment-s=4] [vorlauf-s=1] [modell=htdemucs] [gerät=auto]
Gewichte lädt demucs beim ersten Start nach $HF_HOME bzw. $TORCH_HOME (lz-streamer setzt beide auf <daten>/modelle/k2).
"""
import queue
import struct
import sys
import threading
import time

import numpy as np
import torch
import torchaudio.functional as AF
from demucs.apply import apply_model
from demucs.pretrained import get_model

RATE = 48000
ch = int(sys.argv[1])
seg = int(float(sys.argv[2] if len(sys.argv) > 2 else 4) * RATE)
ctx = int(float(sys.argv[3] if len(sys.argv) > 3 else 1) * RATE)
name = sys.argv[4] if len(sys.argv) > 4 else "htdemucs"
dev = sys.argv[5] if len(sys.argv) > 5 else "auto"
if dev == "auto":
    dev = "mps" if torch.backends.mps.is_available() else ("cuda" if torch.cuda.is_available() else "cpu")

inp, out = sys.stdin.buffer, sys.stdout.buffer
blocks = queue.Queue()


def read_exact(n):
    b = bytearray()
    while len(b) < n:
        c = inp.read(n - len(b))
        if not c:
            return None
        b += c
    return bytes(b)


def reader():
    """stdin sofort lesen, auch während das Modell lädt (sonst staut sich der Ton)."""
    while True:
        h = read_exact(4)
        if h is None:
            blocks.put(None)
            return
        (frames,) = struct.unpack("<I", h)
        d = read_exact(frames * ch * 4)
        if d is None:
            blocks.put(None)
            return
        blocks.put(np.frombuffer(d, dtype="<f4").reshape(frames, ch))


threading.Thread(target=reader, daemon=True).start()
model = get_model(name).to(dev).eval()
voc = model.sources.index("vocals")
sr = model.samplerate


def separate(x):
    """x: [frames, ch] float32 bei 48 kHz → vocals gleicher Form."""
    t = torch.from_numpy(x.T.copy())
    if ch == 1:
        t = t.repeat(2, 1)
    t = AF.resample(t, RATE, sr)
    ref = t.mean(0)
    mean, std = ref.mean(), ref.std() + 1e-8
    with torch.no_grad():
        y = apply_model(model, ((t - mean) / std)[None].to(dev), split=False, device=dev)[0, voc]
    y = AF.resample(y.cpu() * std + mean, sr, RATE)
    n = x.shape[0]
    y = y[:, :n] if y.shape[1] >= n else torch.nn.functional.pad(y, (0, n - y.shape[1]))
    if ch == 1:
        y = y.mean(0, keepdim=True)
    return y.T.numpy().astype(np.float32)


# einmal vorwärmen (erster Lauf auf MPS ist langsam)
separate(np.zeros((seg + 2 * ctx, ch), np.float32))
out.write(b"K2OK" + struct.pack("<II", seg, ctx))
out.flush()

base = -ctx  # Eingang ab Index `base`; vorne Stille als Vorlauf des ersten Segments
buf = np.zeros((ctx, ch), np.float32)
nxt = 0  # Start des nächsten Segments
while True:
    data = blocks.get()
    if data is None:
        sys.exit(0)
    parts = [data]
    while not blocks.empty():
        d = blocks.get()
        if d is None:
            sys.exit(0)
        parts.append(d)
    buf = np.concatenate([buf] + parts)
    # Segment [nxt, nxt+seg) braucht Kontext [nxt-ctx, nxt+seg+ctx)
    while base + len(buf) >= nxt + seg + ctx:
        a = nxt - ctx - base
        t0 = time.perf_counter()
        y = separate(buf[a:a + seg + 2 * ctx])[ctx:ctx + seg]
        out.write(struct.pack("<QI", nxt, seg) + y.astype("<f4").tobytes())
        out.flush()
        print(f"segment {nxt / RATE:.1f} s in {(time.perf_counter() - t0) * 1000:.0f} ms", file=sys.stderr, flush=True)
        nxt += seg
        drop = nxt - ctx - base
        buf = buf[drop:]
        base += drop
