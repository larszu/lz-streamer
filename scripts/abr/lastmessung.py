#!/usr/bin/env python3
"""Last der ABR-Leiter messen: Kanal mit Testbild und HLS starten, CPU-Zeit
des Prozesses und Bilder/s je Stufe aus den Statusmeldungen lesen.

Aufruf: python3 scripts/abr/lastmessung.py [Pfad zu lz-streamer] [Sekunden]
Vorher: cargo build --release
"""
import json, os, subprocess, sys, tempfile, threading, time

EXE = sys.argv[1] if len(sys.argv) > 1 else "target/release/lz-streamer"
SECONDS = int(sys.argv[2]) if len(sys.argv) > 2 else 30
WARMUP = 8

RUNS = [
    # Name, Breite, Höhe, Bitrate, Encoder, Leiter an, Skalierung (das UHD-Testbild wird aus HD hochskaliert)
    ("1080p50 ohne Leiter", 1920, 1080, 8000, "auto", False, "high"),
    ("1080p50 + 720p/480p", 1920, 1080, 8000, "auto", True, "high"),
    ("2160p50 ohne Leiter (Lanczos hoch)", 3840, 2160, 20000, "auto", False, "high"),
    ("2160p50 + 1080p/720p (Lanczos hoch)", 3840, 2160, 20000, "auto", True, "high"),
    ("2160p50 ohne Leiter (bilinear hoch)", 3840, 2160, 20000, "auto", False, "fast"),
    ("2160p50 + 1080p/720p (bilinear hoch)", 3840, 2160, 20000, "auto", True, "fast"),
    ("1080p50 + 720p/480p, x264", 1920, 1080, 8000, "x264enc", True, "high"),
]


def cputime(pid):
    env = dict(os.environ, LC_ALL="C")
    out = subprocess.run(["ps", "-o", "time=", "-p", str(pid)], capture_output=True, text=True, env=env).stdout.strip()
    if not out:
        return None
    parts = out.replace("-", ":").split(":")
    secs = 0.0
    for p in parts:
        secs = secs * 60 + float(p)
    return secs


def run(name, w, h, kbps, encoder, ladder, scale, tmp):
    hls = os.path.join(tmp, "hls")
    cfg = {
        "role": "encoder",
        "encoder": {
            "input": {"kind": "test"},
            "video": {"width": w, "height": h, "fps": 50, "bitrate_kbps": kbps, "encoder": encoder, "scale_quality": scale},
            "srt": {"mode": "listener", "host": "127.0.0.1", "port": 47011},
            "hls": {"enabled": True, "dir": hls, "segment_s": 2, "abr": ladder},
        },
    }
    path = os.path.join(tmp, "cfg.json")
    with open(path, "w") as f:
        json.dump(cfg, f)
    p = subprocess.Popen([EXE, "channel", "--config", path], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    last = {}

    def read():
        for line in p.stdout:
            try:
                m = json.loads(line)
            except ValueError:
                continue
            if m.get("type") == "status":
                last["s"] = m
            elif m.get("type") == "fatal":
                last["fatal"] = m.get("text")

    threading.Thread(target=read, daemon=True).start()
    time.sleep(WARMUP)
    c0, t0 = cputime(p.pid), time.time()
    fps = []
    end = t0 + SECONDS
    while time.time() < end:
        time.sleep(2)
        s = last.get("s")
        if s:
            fps.append((s["fps"], [r["fps"] for r in s.get("abr", [])]))
    c1, t1 = cputime(p.pid), time.time()
    s = last.get("s", {})
    p.stdin.close()
    p.wait(timeout=15)
    if c0 is None or c1 is None:
        return f"| {name} | Abbruch {last.get('fatal', '')} | | | |"
    cpu = (c1 - c0) / (t1 - t0) * 100
    main = sum(f[0] for f in fps) / max(len(fps), 1)
    rungs = [sum(f[1][i] for f in fps if len(f[1]) > i) / max(len(fps), 1) for i in range(len(s.get("abr", [])))]
    res = " / ".join([f"{w}×{h}"] + [r["resolution"] for r in s.get("abr", [])])
    rates = " / ".join(f"{x:.1f}" for x in [main] + rungs)
    return f"| {name} | {s.get('encoder')} | {res} | {cpu:.0f} % | {rates} |"


def main():
    print(f"Gemessen über {SECONDS} s nach {WARMUP} s Anlauf; CPU in % eines Kerns (Prozess gesamt).")
    print("| Fall | Encoder | Stufen | CPU | Bilder/s je Stufe |")
    print("|---|---|---|---|---|")
    for r in RUNS:
        with tempfile.TemporaryDirectory() as tmp:
            print(run(*r, tmp), flush=True)


if __name__ == "__main__":
    main()
