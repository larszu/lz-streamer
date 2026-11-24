#!/usr/bin/env python3
"""Linearen Kopf für A3 lernen (Softmax-Regression über YAMNet-Einbettung + Silero + Aktivität).

Eingang: JSON-Zeilen aus
  lz-streamer klassen-messen --korpus <lernen> --verfahren netz --ohne-kopf --merkmale lernen.jsonl
Ausgang: a3-kopf.json (Format von vadml::Head), im Modellordner ablegen.

Jede Klasse geht mit gleichem Gewicht ein (Fenster je Klasse ausgeglichen).
Fenster unter 15 % Aktivität (Stille) entscheidet lz-streamer über den Pegel; sie
zählen hier trotzdem mit, damit der Kopf leise Ränder nicht als Sprache lernt.

Aufruf: scripts/vad/kopf-lernen.py lernen.jsonl a3-kopf.json [l2=1e-3] [runden=400]
Braucht numpy.
"""
import json
import sys

import numpy as np

src, dst = sys.argv[1], sys.argv[2]
l2 = float(sys.argv[3]) if len(sys.argv) > 3 else 1e-3
rounds = int(sys.argv[4]) if len(sys.argv) > 4 else 400
CLASSES = ["stille", "sprache", "gesang", "orgel", "applaus"]

X, y = [], []
for line in open(src):
    r = json.loads(line)
    X.append(r["emb"] + [r["speech_p"], r["speech_frac"], r["active"]])
    y.append(CLASSES.index(r["truth"]))
X = np.asarray(X, dtype=np.float64)
y = np.asarray(y)
mean, std = X.mean(0), X.std(0) + 1e-6
Z = (X - mean) / std
K, D = len(CLASSES), Z.shape[1]
counts = np.bincount(y, minlength=K)
sw = (1.0 / np.maximum(counts, 1))[y]
sw = sw / sw.sum()
Y = np.eye(K)[y]
W = np.zeros((K, D))
b = np.zeros(K)
# Adam auf gewichteter Kreuzentropie
mW, vW, mb, vb = (np.zeros_like(W), np.zeros_like(W), np.zeros_like(b), np.zeros_like(b))
lr, b1, b2 = 0.05, 0.9, 0.999
for t in range(1, rounds + 1):
    logits = Z @ W.T + b
    logits -= logits.max(1, keepdims=True)
    P = np.exp(logits)
    P /= P.sum(1, keepdims=True)
    G = (P - Y) * sw[:, None]
    gW = G.T @ Z + l2 * W
    gb = G.sum(0)
    for g, m, v, p in ((gW, mW, vW, W), (gb, mb, vb, b)):
        m *= b1
        m += (1 - b1) * g
        v *= b2
        v += (1 - b2) * g * g
        p -= lr * (m / (1 - b1 ** t)) / (np.sqrt(v / (1 - b2 ** t)) + 1e-8)
pred = (Z @ W.T + b).argmax(1)
for k, c in enumerate(CLASSES):
    sel = y == k
    if sel.any():
        print(f"lernen {c}: {np.mean(pred[sel] == k) * 100:.1f} % von {sel.sum()}")
json.dump({"classes": CLASSES, "mean": mean.tolist(), "std": std.tolist(), "w": W.tolist(), "b": b.tolist()}, open(dst, "w"))
print("geschrieben:", dst)
