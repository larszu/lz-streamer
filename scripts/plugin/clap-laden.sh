#!/bin/zsh
# Freies CLAP-Plugin für die P2-Tests: Airwindows Consolidated (MIT) ohne Installer
# (ohne Administratorrechte) nach ~/Library/Audio/Plug-Ins/CLAP kopieren.
# Aufruf: scripts/plugin/clap-laden.sh
set -e
W=${TMPDIR:-/tmp}/lzs-clap-laden
mkdir -p $W && cd $W
URL=$(gh api repos/baconpaul/airwin2rack/releases --jq '[.[] | select(.tag_name=="DAWPlugin")][0].assets[] | select(.name | test("macOS.*dmg$")) | .browser_download_url')
[ -f ac.dmg ] || curl -sSfL -o ac.dmg "$URL"
MNT=$(hdiutil attach -nobrowse -readonly ac.dmg | awk -F'\t' '/\/Volumes\//{print $NF}')
trap 'hdiutil detach -quiet "$MNT"' EXIT
CLAP=$(find "$MNT" -maxdepth 3 -name "*.clap" | head -1)
if [ -z "$CLAP" ]; then
  PKG=$(find "$MNT" -maxdepth 2 -name "*.pkg" | head -1)
  rm -rf pkg && pkgutil --expand-full "$PKG" pkg
  CLAP=$(find pkg -name "*.clap" -type d | head -1)
fi
mkdir -p ~/Library/Audio/Plug-Ins/CLAP
rm -rf ~/Library/Audio/Plug-Ins/CLAP/"$(basename "$CLAP")"
cp -R "$CLAP" ~/Library/Audio/Plug-Ins/CLAP/
ls -d ~/Library/Audio/Plug-Ins/CLAP/*.clap
