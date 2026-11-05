#!/bin/zsh
# Skalierer-Messung: UHD-Referenz → HD (Lanczos) → UHD mit verschiedenen
# Skalierern → VMAF (4K-Modell und NEG-Modell), PSNR-Y, SSIM gegen die Referenz.
# Aufruf: scripts/skalierung/messen.sh <uhd-clip> [bilder=50] [arbeitsordner]
# Braucht: ffmpeg mit libvmaf (Homebrew), GStreamer. Bis zu ~3 GB Platz im Arbeitsordner.
set -e
SRC=$1; N=${2:-50}; W=${3:-${TMPDIR:-/tmp}/lzs-skalierung}
mkdir -p $W; cd $W
# Referenz in begrenztem Wertebereich (wie die GStreamer-Pipeline), 8 bit 4:2:0
ffmpeg -v error -y -ss 2 -i "$SRC" -frames:v $N -vf "scale=3840:2160:out_range=tv,format=yuv420p" -color_range tv -strict -1 ref.y4m
ffmpeg -v error -y -i ref.y4m -vf scale=1920:1080:flags=lanczos hd.y4m
metric() {
  ffmpeg -hide_banner -i $1 -i ref.y4m -lavfi "[0:v]split[a][b];[1:v]split[c][d];[a][c]libvmaf=model=version=vmaf_4k_v0.6.1:n_threads=8:feature=name=psnr|name=float_ssim:log_fmt=json:log_path=m.json;[b][d]libvmaf=model=version=vmaf_v0.6.1neg:n_threads=8:log_fmt=json:log_path=n.json" -f null - >/dev/null 2>&1
  python3 -I -c "import json;d=json.load(open('m.json'))['pooled_metrics'];n=json.load(open('n.json'))['pooled_metrics'];print('%s;%.2f;%.2f;%.2f;%.4f'%('$2',d['vmaf']['mean'],n['vmaf']['mean'],d['psnr_y']['mean'],d['float_ssim']['mean']))"
}
g() { gst-launch-1.0 -q filesrc location=hd.y4m ! y4mdec ! videoconvert ! video/x-raw,format=NV12 ! videoscale n-threads=8 ${=2} ! video/x-raw,width=3840,height=2160 ! videoconvert ! video/x-raw,format=I420 ! y4menc ! filesink location=u.y4m; metric u.y4m "GStreamer $1"; rm -f u.y4m; }
f() { ffmpeg -v error -y -i hd.y4m -vf scale=3840:2160:flags=$2 u.y4m; metric u.y4m "ffmpeg $1"; rm -f u.y4m; }
t() { # Zeit je Bild: Lauf mit Skalierer minus Lauf ohne
  local base=$( { /usr/bin/time -p gst-launch-1.0 -q filesrc location=hd.y4m ! y4mdec ! videoconvert ! video/x-raw,format=NV12 ! fakesink sync=false >/dev/null; } 2>&1 | awk '/real/{print $2}' | tr , .)
  local out=$( { /usr/bin/time -p gst-launch-1.0 -q filesrc location=hd.y4m ! y4mdec ! videoconvert ! video/x-raw,format=NV12 ! videoscale n-threads=8 ${=2} ! video/x-raw,width=3840,height=2160 ! fakesink sync=false >/dev/null; } 2>&1 | awk '/real|user/{print $2}' | tr , . | tr '\n' ' ')
  python3 -I -c "import sys;b=float('$base');r,u=map(float,'$out'.split());n=$N;print('%s;%.1f ms/Bild;CPU %.1f ms/Bild'%('$1',(r-b)*1000/n,u*1000/n))"
}
echo "variante;vmaf4k;vmafneg;psnr_y;ssim"
g "bilinear" "method=bilinear"
g "lanczos e2" "method=lanczos"
g "lanczos e3" "method=lanczos envelope=3"
g "lanczos e4" "method=lanczos envelope=4"
g "lanczos e3 sharpness 1.2" "method=lanczos envelope=3 sharpness=1.2"
g "catrom" "method=catrom"
g "4-tap" "method=4-tap"
g "lanczos sharpen 0.3" "method=lanczos sharpen=0.3"
f "bicubic" "bicubic"
f "lanczos" "lanczos"
f "spline" "spline"
echo "zeit"
for v in "bilinear:method=bilinear" "lanczos e2:method=lanczos" "lanczos e3:method=lanczos envelope=3" "lanczos e4:method=lanczos envelope=4" "catrom:method=catrom"; do t "${v%%:*}" "${v#*:}"; done
