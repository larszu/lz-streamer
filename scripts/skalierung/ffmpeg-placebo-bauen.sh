#!/bin/zsh
# ffmpeg mit libplacebo (Vulkan über MoltenVK) und libvmaf für den Skalierer-Prüfstand (#35).
# Baut neben das System-ffmpeg in einen eigenen Ordner, ersetzt nichts aus Homebrew.
# Braucht: brew install libplacebo molten-vk vulkan-headers nasm (libvmaf kommt mit ffmpeg)
# Aufruf: scripts/skalierung/ffmpeg-placebo-bauen.sh [ziel=~/Library/Caches/lz-streamer-werkzeuge/ffmpeg-placebo]
set -e
V=9.0.2
Z=${1:-$HOME/Library/Caches/lz-streamer-werkzeuge/ffmpeg-placebo}
mkdir -p $Z/src && cd $Z/src
[ -f ffmpeg-$V.tar.xz ] || curl -sSLO https://ffmpeg.org/releases/ffmpeg-$V.tar.xz
[ -d ffmpeg-$V ] || tar xf ffmpeg-$V.tar.xz
cd ffmpeg-$V
./configure --prefix=$Z --disable-doc --disable-debug --enable-gpl --enable-version3 \
  --enable-vulkan --enable-libplacebo --enable-libvmaf \
  --enable-libx264 --enable-videotoolbox --disable-ffplay \
  --extra-cflags=-I/opt/homebrew/include --extra-ldflags=-L/opt/homebrew/lib
make -j10
make install
$Z/bin/ffmpeg -hide_banner -filters | grep -E "libplacebo|libvmaf"
