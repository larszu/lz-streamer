#!/bin/zsh
# Skalier-Shader für den libplacebo-Prüfstand (#35) laden, außerhalb des Repos.
# RAVU: bjin/mpv-prescalers (LGPL-3.0), FSRCNNX: igv/FSRCNN-TensorFlow (GPL-3.0).
# Aufruf: scripts/skalierung/shader-laden.sh [ziel=~/Library/Caches/lz-streamer-werkzeuge/shader]
set -e
Z=${1:-$HOME/Library/Caches/lz-streamer-werkzeuge/shader}
mkdir -p $Z && cd $Z
P=https://raw.githubusercontent.com/bjin/mpv-prescalers/master
for f in ravu-r3.hook ravu-r4.hook ravu-lite-r3.hook ravu-lite-ar-r4.hook ravu-zoom-ar-r3.hook LICENSE; do
  curl -sSfL -o ${f/LICENSE/LICENSE-mpv-prescalers} $P/$f || echo "fehlt: $f"
done
F=https://github.com/igv/FSRCNN-TensorFlow/releases/download/1.1
for f in FSRCNNX_x2_8-0-4-1.glsl FSRCNNX_x2_16-0-4-1.glsl; do
  curl -sSfL -o $f $F/$f || echo "fehlt: $f"
done
curl -sSfL -o LICENSE-FSRCNN https://raw.githubusercontent.com/igv/FSRCNN-TensorFlow/master/LICENSE || true
ls -la
