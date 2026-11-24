#!/usr/bin/env python3
"""YAMNet (Apache-2.0, tensorflow/models) aus yamnet.h5 nach ONNX übertragen.

Nachbau des Netzkerns (MobileNetV1, Eingang Log-Mel-Ausschnitt 96×64) in PyTorch,
Gewichte aus der h5-Datei. Das Log-Mel-Merkmal rechnet lz-streamer selbst
(src/vadml.rs, gleich wie yamnet/features.py).

Aufruf (im venv mit torch, h5py, onnx):
  scripts/vad/yamnet-onnx.py <modelle/a3>
Erwartet <ordner>/yamnet.h5 (https://storage.googleapis.com/audioset/yamnet.h5),
schreibt <ordner>/yamnet.onnx mit Eingang `patch` [N,96,64] und den Ausgängen
`scores` [N,521] (Sigmoid) und `embedding` [N,1024].
"""
import sys
import h5py
import numpy as np
import torch
import torch.nn.functional as F

d = sys.argv[1]
h5 = h5py.File(f"{d}/yamnet.h5", "r")


def get(path):
    """Variable `path` (z. B. layer2/depthwise_conv/bn/beta) aus der h5-Datei;
    Keras legt sie unter <layer>/<layer>/<var>:0 ab."""
    layer, var = path.rsplit("/", 1)
    return torch.tensor(np.array(h5[f"{layer}/{layer}/{var}:0"]))


# (Art, Schritt, Kanäle) laut yamnet.py _YAMNET_LAYER_DEFS
DEFS = [("conv", 2, 32), ("sep", 1, 64), ("sep", 2, 128), ("sep", 1, 128), ("sep", 2, 256), ("sep", 1, 256), ("sep", 2, 512),
        ("sep", 1, 512), ("sep", 1, 512), ("sep", 1, 512), ("sep", 1, 512), ("sep", 1, 512), ("sep", 2, 1024), ("sep", 1, 1024)]
EPS = 1e-4


def same_pad(x, k, s):
    """TF-'same'-Auffüllung (bei Schritt 2 hinten eins mehr)."""
    out = []
    for size in (x.shape[3], x.shape[2]):
        n = -(-size // s)
        total = max((n - 1) * s + k - size, 0)
        out += [total // 2, total - total // 2]
    return F.pad(x, out)


class Yamnet(torch.nn.Module):
    def __init__(self):
        super().__init__()
        self.layers = []
        for i, (kind, stride, ch) in enumerate(DEFS, start=1):
            p = f"layer{i}"
            if kind == "conv":
                ws = [("conv", get(f"{p}/conv/kernel").permute(3, 2, 0, 1), f"{p}/conv/bn", 1)]
            else:
                ws = [("dw", get(f"{p}/depthwise_conv/depthwise_kernel").permute(2, 3, 0, 1), f"{p}/depthwise_conv/bn", stride),
                      ("pw", get(f"{p}/pointwise_conv/kernel").permute(3, 2, 0, 1), f"{p}/pointwise_conv/bn", 1)]
            for j, (t, k, bn, s) in enumerate(ws):
                name = f"l{i}_{j}"
                self.register_buffer(f"{name}_k", k.contiguous())
                mean, var, beta = get(f"{bn}/moving_mean"), get(f"{bn}/moving_variance"), get(f"{bn}/beta")
                # BN ohne Skalierung (scale=False) in Faktor und Versatz falten
                a = 1.0 / torch.sqrt(var + EPS)
                self.register_buffer(f"{name}_a", a.view(1, -1, 1, 1))
                self.register_buffer(f"{name}_b", (beta - mean * a).view(1, -1, 1, 1))
                st = stride if t == "conv" else s
                self.layers.append((name, t, st))
        self.register_buffer("lw", torch.tensor(np.array(h5["logits/logits/kernel:0"])).t().contiguous())
        self.register_buffer("lb", torch.tensor(np.array(h5["logits/logits/bias:0"])))

    def forward(self, patch):
        x = patch.unsqueeze(1)
        for name, t, s in self.layers:
            k = getattr(self, f"{name}_k")
            if t == "pw":
                x = F.conv2d(x, k)
            else:
                x = same_pad(x, 3, s)
                x = F.conv2d(x, k, stride=s, groups=1 if t == "conv" else x.shape[1])
            x = F.relu(x * getattr(self, f"{name}_a") + getattr(self, f"{name}_b"))
        emb = x.mean(dim=(2, 3))
        scores = torch.sigmoid(emb @ self.lw.t() + self.lb)
        return scores, emb


m = Yamnet().eval()
dummy = torch.zeros(1, 96, 64)
torch.onnx.export(m, (dummy,), f"{d}/yamnet.onnx", input_names=["patch"], output_names=["scores", "embedding"],
                  dynamic_axes={"patch": {0: "n"}, "scores": {0: "n"}, "embedding": {0: "n"}}, opset_version=17, dynamo=False)
print("geschrieben:", f"{d}/yamnet.onnx")
