#!/bin/zsh
# Testmaterial für die Untertitel-Messung (nicht im Repo):
# Google FLEURS (CC BY 4.0, https://huggingface.co/datasets/google/fleurs),
# Teil „dev“ für Deutsch (de_de, ≈ 228 MB) und Englisch (en_us, ≈ 171 MB).
#
# Baut je Sprache ein Testvideo: N Äußerungen hintereinander mit 2 s Pause,
# Bild = laufendes Testbild mit Zeitcode, Ton = die Aufnahmen. Dazu
#   <sprache>.ref.txt     – Referenztext (Transkripte in Reihenfolge)
#   <sprache>.onsets.tsv  – Beginn jeder Äußerung im Video (s) und ihr Text
#
# Aufruf: scripts/untertitel/testdaten-laden.sh [anzahl=12] [ordner]
set -e
N=${1:-12}
D=${2:-$HOME/Library/Caches/lz-streamer-testdaten}
mkdir -p $D; cd $D
BASE=https://huggingface.co/datasets/google/fleurs/resolve/main/data
for L in de_de en_us; do [[ -f $L.dev.tsv ]] || curl -fsSL -o $L.dev.tsv $BASE/$L/dev.tsv; done
for L in de_de en_us; do
  [[ -d $L/dev ]] || { mkdir -p $L; curl -fsSL $BASE/$L/audio/dev.tar.gz | tar -xz -C $L; }
  # erste N verschiedene Sätze (FLEURS hat mehrere Sprecher je Satz)
  python3 -I - "$L" "$N" <<'PY'
import sys, csv, subprocess, os
lang, n = sys.argv[1], int(sys.argv[2])
seen, rows = set(), []
for r in csv.reader(open(f"{lang}.dev.tsv"), delimiter="\t"):
    if r[0] in seen or not os.path.exists(f"{lang}/dev/{r[1]}"):
        continue
    seen.add(r[0]); rows.append((r[1], r[2], r[0]))
    if len(rows) == n:
        break
t, parts, onsets = 2.0, [], []
ids = [i for _, _, i in rows]
for f, text, _ in rows:
    dur = float(subprocess.check_output(["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", f"{lang}/dev/{f}"]).decode())
    # Sprachbeginn: Ende der Anfangsstille (silencedetect, −40 dB, 0,15 s)
    log = subprocess.run(["ffmpeg", "-hide_banner", "-i", f"{lang}/dev/{f}", "-af", "silencedetect=n=-40dB:d=0.15", "-f", "null", "-"], capture_output=True, text=True).stderr
    lead = 0.0
    for line in log.splitlines():
        if "silence_start: 0" in line.replace("silence_start: -", "silence_start: "):
            continue
        if "silence_end" in line:
            v = float(line.split("silence_end:")[1].split("|")[0])
            if v < 2.0 and lead == 0.0:
                lead = v
            break
    onsets.append((t + lead, text)); parts.append(f"{lang}/dev/{f}"); t += dur + 2.0
open(f"{lang}.ref.txt", "w").write(" ".join(x for _, x in onsets) + "\n")
open(f"{lang}.onsets.tsv", "w").write("".join(f"{a:.3f}\t{b}\n" for a, b in onsets))
# Übersetzungsreferenz: FLEURS-Sätze sind über die ID zwischen Sprachen gleich
other = "en_us" if lang == "de_de" else "de_de"
if os.path.exists(f"{other}.dev.tsv"):
    tr = {}
    for r in csv.reader(open(f"{other}.dev.tsv"), delimiter="\t"):
        tr.setdefault(r[0], r[2])
    open(f"{lang}.ref.{other[:2]}.txt", "w").write(" ".join(tr.get(i, "") for i in ids) + "\n")
# Tonspur: 2 s Stille, dann jede Äußerung gefolgt von 2 s Stille
inputs, filt = [], []
for i, p in enumerate(parts):
    inputs += ["-i", p]
    filt.append(f"[{i}:a]aresample=48000,aformat=channel_layouts=mono,apad=pad_dur=2[a{i}]")
cat = "".join(f"[a{i}]" for i in range(len(parts)))
k = len(parts)
filt.append(f"[{k}:a]atrim=0:2[s]")
filt.append(f"[s]{cat}concat=n={k+1}:v=0:a=1[aud]")
subprocess.check_call(["ffmpeg", "-v", "error", "-y", *inputs, "-f", "lavfi", "-i", "anullsrc=r=48000:cl=mono",
    "-f", "lavfi", "-i", f"testsrc2=size=1280x720:rate=25:duration={t:.2f}",
    "-filter_complex", ";".join(filt),
    "-map", f"{len(parts)+1}:v", "-map", "[aud]", "-shortest", "-c:v", "libx264", "-preset", "veryfast", "-c:a", "aac", "-b:a", "192k", f"{lang}.mp4"])
print(lang, len(parts), "Äußerungen,", f"{t:.0f} s")
PY
done
ls -la $D/*.mp4
