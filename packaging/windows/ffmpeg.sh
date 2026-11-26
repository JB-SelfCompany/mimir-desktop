#!/usr/bin/env bash
# The ffmpeg.exe the Windows packages ship: a static LGPL build with only what Mimir runs (src/media: voice and call
# AAC, video messages from a DirectShow camera through Media Foundation H.264, posters of the videos Android sends).
# Cross-compiled with mingw-w64 by .github/workflows/release.yml; FFMPEG_CROSS= builds natively in an MSYS2 mingw64
# shell. The --enable lists are checked against our command lines by a test in src/media/ffmpeg.rs.
# Usage: bash packaging/windows/ffmpeg.sh <out dir>   (writes ffmpeg.exe and FFMPEG-LICENSE.txt)
set -euo pipefail
url=https://ffmpeg.org/releases/ffmpeg-9.1.tar.xz
sha256=1a4d394ae63d190a526cd052eb4415c156b5c5e2df37efab41a7de0b8fde2433
cross=${FFMPEG_CROSS-x86_64-w64-mingw32-}
mkdir -p "$1"
out=$(cd "$1" && pwd)
work=$(mktemp -d)
cd "$work"
curl -fsSL -o ffmpeg.tar.xz "$url"
echo "$sha256  ffmpeg.tar.xz" | sha256sum -c -
tar -xf ffmpeg.tar.xz --strip-components=1
# Names are configure's: the raw f32le format is pcm_f32le; buffer/abuffer and their sinks are always built.
# ponytail: AV1 has no software decoder without libdav1d: AV1 attachments show the neutral poster tile.
# zlib (static, mingw-w64: libz-mingw-w64-dev in CI) inflates compressed mov headers and matroska tracks.
./configure --target-os=mingw32 --arch=x86_64 ${cross:+--enable-cross-compile --cross-prefix=$cross} \
  --enable-static --disable-shared --extra-ldflags=-static --disable-autodetect --enable-w32threads --enable-zlib \
  --disable-everything --disable-doc --disable-debug --disable-network --disable-ffprobe --disable-ffplay \
  --enable-mediafoundation \
  --enable-protocol=file,pipe \
  --enable-indev=dshow,lavfi \
  --enable-demuxer=aac,amr,avi,flac,matroska,mov,mp3,ogg,pcm_f32le,wav \
  --enable-muxer=adts,image2,ipod,mp4,pcm_f32le,rawvideo \
  --enable-encoder=aac,h264_mf,mjpeg,pcm_f32le,rawvideo \
  --enable-decoder=aac,amrnb,amrwb,flac,h263,h264,hevc,mjpeg,mp3float,mpeg4,opus,pcm_f32le,pcm_s16le,pcm_s24le,rawvideo,vorbis,vp8,vp9,wrapped_avframe \
  --enable-parser=aac,flac,h263,h264,hevc,mjpeg,mpeg4video,mpegaudio,opus,vorbis,vp8,vp9 \
  --enable-filter=aformat,anull,aresample,atrim,crop,format,fps,hflip,null,scale,setsar,sine,split,testsrc,transpose,trim,vflip
# configure drops an unknown or unavailable component without failing: these are the ones we cannot do without.
for c in H264_MF_ENCODER AAC_ENCODER DSHOW_INDEV LAVFI_INDEV PCM_F32LE_DEMUXER ZLIB; do
  grep -q "define CONFIG_$c 1" config.h config_components.h || { echo "ffmpeg.sh: CONFIG_$c is off" >&2; exit 1; }
done
make -j"$(nproc)"
cp ffmpeg.exe "$out/"
cp COPYING.LGPLv2.1 "$out/FFMPEG-LICENSE.txt"
