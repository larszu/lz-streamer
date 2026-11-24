#!/bin/zsh
# GPU-Skalierer (libplacebo über Vulkan/MoltenVK) gegen Lanczos, Verfahren wie messen.sh:
# UHD-Referenz → HD (ffmpeg-Lanczos) → UHD → VMAF-4K, VMAF-NEG, PSNR-Y, SSIM.
# Danach Durchsatz 1080p → 2160p über 1500 Bilder (Hochladen, Skalieren, Herunterladen).
# Aufruf: scripts/skalierung/messen-placebo.sh <uhd-clip> [bilder=50] [arbeitsordner]
# Braucht: scripts/skalierung/ffmpeg-placebo-bauen.sh und shader-laden.sh.
set -e
SRC=$1; N=${2:-50}; W=${3:-${TMPDIR:-/tmp}/lzs-skalierung-placebo}
FP=${FFMPEG_PLACEBO:-$HOME/Library/Caches/lz-streamer-werkzeuge/ffmpeg-placebo/bin/ffmpeg}
SH=${SHADER:-$HOME/Library/Caches/lz-streamer-werkzeuge/shader}
export DYLD_FALLBACK_LIBRARY_PATH=/opt/homebrew/lib
mkdir -p $W; cd $W
ffmpeg -v error -y -ss 2 -i "$SRC" -frames:v $N -vf "scale=3840:2160:out_range=tv,format=yuv420p" -color_range tv -strict -1 ref.y4m
ffmpeg -v error -y -i ref.y4m -vf scale=1920:1080:flags=lanczos hd.y4m
metric() {
  ffmpeg -hide_banner -i $1 -i ref.y4m -lavfi "[0:v]split[a][b];[1:v]split[c][d];[a][c]libvmaf=model=version=vmaf_4k_v0.6.1:n_threads=8:feature=name=psnr|name=float_ssim:log_fmt=json:log_path=m.json;[b][d]libvmaf=model=version=vmaf_v0.6.1neg:n_threads=8:log_fmt=json:log_path=n.json" -f null - >/dev/null 2>&1
  python3 -I -c "import json;d=json.load(open('m.json'))['pooled_metrics'];n=json.load(open('n.json'))['pooled_metrics'];print('%s;%.2f;%.2f;%.2f;%.4f'%('$2',d['vmaf']['mean'],n['vmaf']['mean'],d['psnr_y']['mean'],d['float_ssim']['mean']))"
}
TAG="setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=tv"
OUT="format=yuv420p:colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=tv"
pl() { # $1 Name, $2 libplacebo-Optionen
  $FP -v error -y -init_hw_device vulkan=vk -filter_hw_device vk -i hd.y4m \
    -vf "$TAG,format=yuv420p,hwupload,libplacebo=w=3840:h=2160:$OUT:$2,hwdownload,format=yuv420p" -strict -1 u.y4m
  metric u.y4m "libplacebo $1"; rm -f u.y4m
}
echo "variante;vmaf4k;vmafneg;psnr_y;ssim"
ffmpeg -v error -y -i hd.y4m -vf scale=3840:2160:flags=lanczos u.y4m; metric u.y4m "ffmpeg lanczos"; rm -f u.y4m
gst-launch-1.0 -q filesrc location=hd.y4m ! y4mdec ! videoconvert ! video/x-raw,format=NV12 ! videoscale n-threads=8 method=lanczos envelope=3 ! video/x-raw,width=3840,height=2160 ! videoconvert ! video/x-raw,format=I420 ! y4menc ! filesink location=u.y4m
metric u.y4m "GStreamer lanczos e3"; rm -f u.y4m
pl "lanczos" "upscaler=lanczos:dithering=none"
pl "ewa_lanczos" "upscaler=ewa_lanczos:dithering=none"
pl "ewa_lanczossharp" "upscaler=ewa_lanczossharp:dithering=none"
for s in ravu-r3.hook ravu-r4.hook ravu-lite-r3.hook ravu-lite-ar-r4.hook ravu-zoom-ar-r3.hook FSRCNNX_x2_8-0-4-1.glsl FSRCNNX_x2_16-0-4-1.glsl; do
  pl "${s%.*} (+ewa_lanczos)" "upscaler=ewa_lanczos:dithering=none:custom_shader_path=$SH/$s"
done
echo "durchsatz;bilder/s (1500 Bilder, Vulkan, mit Hoch- und Herunterladen)"
ffmpeg -v error -y -i hd.y4m -vf "loop=loop=29:size=$N" -frames:v 1500 -c:v rawvideo -f nut loop.nut
thr() {
  local r=$( { $FP -hide_banner -benchmark -init_hw_device vulkan=vk -filter_hw_device vk -i loop.nut \
    -vf "$TAG,format=yuv420p,hwupload,libplacebo=w=3840:h=2160:$OUT:$2,hwdownload,format=yuv420p" -f null - ; } 2>&1 | awk '/bench: utime/{print $2, $4}' | tr -d 'utimesr=' )
  python3 -I -c "u,r='$r'.split();u=float(u);r=float(r);print('%s;%.1f;CPU %.1f ms/Bild'%('$1',1500/r,u*1000/1500))"
}
thr "basis (nur hoch/runter, bilinear)" "upscaler=bilinear:dithering=none"
thr "ewa_lanczos" "upscaler=ewa_lanczos:dithering=none"
for s in ravu-r3.hook ravu-lite-r3.hook ravu-zoom-ar-r3.hook FSRCNNX_x2_8-0-4-1.glsl FSRCNNX_x2_16-0-4-1.glsl; do
  thr "${s%.*}" "upscaler=ewa_lanczos:dithering=none:custom_shader_path=$SH/$s"
done
rm -f loop.nut
