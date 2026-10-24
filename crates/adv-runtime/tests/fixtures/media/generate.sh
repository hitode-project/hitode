#!/bin/sh
# Synthetic, half-second, five-frame red clips with a moving white square.
# Usage: FFMPEG=/path/to/ffmpeg ./generate.sh [output-directory]
set -eu
cd "${1:-$(dirname "$0")}"
ffmpeg=${FFMPEG:-ffmpeg}
make_clip() {
    "$ffmpeg" -hide_banner -loglevel error -y \
        -f lavfi -i 'color=c=red:s=64x48:r=10:d=0.5,drawbox=x=24:y=16:w=16:h=16:color=white:t=fill:enable=mod(n\,2)' \
        -f lavfi -i 'sine=frequency=440:sample_rate=44100:duration=0.5' \
        -map 0:v -map 1:a -threads 1 -filter_threads 1 -shortest "$@"
}
make_clip -c:v mpeg1video -r 25 -frames:v 5 -c:a mp2 -b:a 64k synthetic.mpg
make_clip -c:v wmv2 -c:a wmav2 -b:a 64k synthetic.wmv
make_clip -c:v libx264 -pix_fmt yuv420p -bf 0 -c:a aac -b:a 32k -movflags +faststart synthetic.mp4
make_clip -c:v mpeg4 -bf 0 -c:a pcm_s16le synthetic.avi
