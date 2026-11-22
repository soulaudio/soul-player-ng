#!/usr/bin/env bash
# Builds a tagged demo library that exercises every format / rate Soul Player plays — handy
# on a machine with no music, for exclusive-mode rate switching, DSD and surround fold-down.
#
#   scripts/demo-library/make-demo-library.sh [dest]   (default: ~/Music/Soul Player Demo)
#
# Needs: ffmpeg (brew install ffmpeg), python3, cargo. Re-running overwrites the files.
# Albums (artist "Soul Demo Ensemble", 30 s synthesized arpeggio pieces + folder covers):
#   Hi-Res Ladder          24-bit FLAC at 44.1 / 48 / 88.2 / 96 / 176.4 / 192 / 352.8 / 384 kHz
#   Lossless Formats       FLAC 16/44.1, ALAC 24/96, WAV 24/96, AIFF 16/44.1, WavPack 24/48, FLAC 24/192
#   Lossy Formats          MP3 320, AAC 256, Opus 160, Vorbis q6
#   Direct Stream Digital  DSD64 (DSF + DFF), DSD128, DSD256, DSD512 — real 1-bit (dsdgen)
#   Surround Fold-Down     FLAC 5.1 24/48 + the repo's ALAC / Opus 5.1 fixtures
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
dest="${1:-$HOME/Music/Soul Player Demo}"
ART="Soul Demo Ensemble"
command -v ffmpeg >/dev/null || { echo "need ffmpeg (brew install ffmpeg)"; exit 2; }

# A plucked arpeggio over I-vi-IV-V plus a soft bass; peak below -6 dBFS.
#   piece <rate> <base_hz> <notes_per_sec> <secs> [channels]
piece() {
  local rate=$1 f=$2 nps=$3 secs=$4
  local n="floor(t*$nps)" tau="(t-floor(t*$nps)/$nps)"
  local bar="mod(floor(t*$nps/8),4)"
  local rt="(0*eq($bar,0)-3*eq($bar,1)+5*eq($bar,2)+7*eq($bar,3))"
  local m="mod($n,8)"
  local semi="(4*eq($m,1)+7*eq($m,2)+12*eq($m,3)+16*eq($m,4)+12*eq($m,5)+7*eq($m,6)+4*eq($m,7))"
  local env="min(1,$tau*300)*exp(-4*$tau)"
  local fade="min(1,t*2)*min(1,($secs-t)*1.5)"
  local note="$env*sin(2*PI*$f*pow(2,($rt+$semi)/12)*$tau)"
  local bass="0.12*sin(2*PI*$f/2*pow(2,$rt/12)*t)"
  printf "aevalsrc=exprs='%s*(0.30*%s+%s)|%s*(0.22*%s+%s)':s=%s:d=%s" \
    "$fade" "$note" "$bass" "$fade" "$note" "$bass" "$rate" "$secs"
}
cover() { # cover <dir> <color1> <color2>
  ffmpeg -v error -y -f lavfi -i "gradients=s=600x600:c0=$2:c1=$3:x0=0:y0=0:x1=600:y1=600:d=1" \
    -frames:v 1 "$1/cover.jpg"
}
tags() { # tags <album> <track> <title> <year>  -> T array
  T=(-metadata "artist=$ART" -metadata "album_artist=$ART" -metadata "album=$1"
     -metadata "track=$2" -metadata "title=$3" -metadata "date=$4" -metadata "genre=Electronic")
}
enc() { # enc <album> <n> <title> <year> <ext> <rate> <f0> <nps> <codec args...>
  local album=$1 n=$2 title=$3 year=$4 ext=$5 rate=$6 f0=$7 nps=$8; shift 8
  tags "$album" "$n" "$title" "$year"
  ffmpeg -v error -y -f lavfi -i "$(piece "$rate" "$f0" "$nps" 30)" "$@" "${T[@]}" \
    "$dest/$ART/$album/$(printf %02d "$n") - $title.$ext"
}
F24=(-c:a flac -sample_fmt s32 -bits_per_raw_sample 24)

album() { mkdir -p "$dest/$ART/$1"; cover "$dest/$ART/$1" "$2" "$3"; }

album "Hi-Res Ladder" 0x1d3557 0xe63946
i=0
for spec in "44100 220 44.1" "48000 233.08 48" "88200 246.94 88.2" "96000 261.63 96" \
            "176400 277.18 176.4" "192000 293.66 192" "352800 311.13 352.8" "384000 329.63 384"; do
  set -- $spec; i=$((i+1))
  enc "Hi-Res Ladder" $i "Ladder $3 kHz" 2026 flac "$1" "$2" $((3 + i % 3)) "${F24[@]}"
done

album "Lossless Formats" 0x2a9d8f 0xe9c46a
enc "Lossless Formats" 1 "FLAC 16-44.1" 2025  flac 44100  196    4 -c:a flac -sample_fmt s16
enc "Lossless Formats" 2 "ALAC 24-96" 2025    m4a  96000  207.65 4 -c:a alac -sample_fmt s32p
enc "Lossless Formats" 3 "WAV 24-96" 2025     wav  96000  220    4 -c:a pcm_s24le
enc "Lossless Formats" 4 "AIFF 16-44.1" 2025  aiff 44100  233.08 4 -c:a pcm_s16be -write_id3v2 1
enc "Lossless Formats" 5 "WavPack 24-48" 2025 wv   48000  246.94 4 -c:a wavpack -sample_fmt s32p -bits_per_raw_sample 24
enc "Lossless Formats" 6 "FLAC 24-192" 2025   flac 192000 261.63 4 "${F24[@]}"

album "Lossy Formats" 0xf4a261 0x264653
enc "Lossy Formats" 1 "MP3 320" 2024   mp3  44100 220    5 -c:a libmp3lame -b:a 320k
enc "Lossy Formats" 2 "AAC 256" 2024   m4a  44100 246.94 5 -c:a aac -b:a 256k
enc "Lossy Formats" 3 "Opus 160" 2024  opus 48000 261.63 5 -c:a libopus -b:a 160k
enc "Lossy Formats" 4 "Vorbis q6" 2024 ogg  44100 293.66 5 -c:a vorbis -strict -2 -q:a 6

album "Direct Stream Digital" 0x3d348b 0xf7b801
cargo build -q --release --manifest-path "$here/dsdgen/Cargo.toml"
dsdgen="$here/dsdgen/target/release/dsdgen"
d="$dest/$ART/Direct Stream Digital"
"$dsdgen" "$d/01 - DSD64 (DSF).dsf" 64  220    4 30 &
"$dsdgen" "$d/02 - DSD64 (DFF).dff" 64  246.94 4 30 &
"$dsdgen" "$d/03 - DSD128.dsf"      128 261.63 5 30 &
"$dsdgen" "$d/04 - DSD256.dsf"      256 293.66 3 20 &
"$dsdgen" "$d/05 - DSD512.dsf"      512 329.63 4 15 &
wait

album "Surround Fold-Down" 0x6a4c93 0x8ac926
tags "Surround Fold-Down" 1 "FLAC 5.1 24-48" 2026
ffmpeg -v error -y -f lavfi -i "$(piece 48000 220 4 30)" -filter_complex \
  "[0:a]pan=5.1|FL=c0|FR=c1|FC=0.5*c0+0.5*c1|LFE=0.3*c0|BL=0.4*c0|BR=0.4*c1" \
  "${F24[@]}" "${T[@]}" "$dest/$ART/Surround Fold-Down/01 - FLAC 5.1 24-48.flac"
cp "$root/testdata/tone-fc-5.1.m4a"  "$dest/$ART/Surround Fold-Down/02 - ALAC 5.1 (centre first).m4a"
cp "$root/testdata/tone-fc-5.1.opus" "$dest/$ART/Surround Fold-Down/03 - Opus 5.1.opus"

# Tag what ffmpeg can't (DSD, WAV ID3, the copied fixtures) with mutagen in a throwaway venv.
venv="${XDG_CACHE_HOME:-$HOME/.cache}/soul-demo-library-venv"
[ -x "$venv/bin/python" ] || python3 -m venv "$venv"
"$venv/bin/python" -c "import mutagen" 2>/dev/null || "$venv/bin/pip" -q install mutagen
"$venv/bin/python" -I "$here/tag_missing.py" "$dest/$ART" | grep -v '^ok' || true

echo "done: $(find "$dest" -type f ! -name cover.jpg | wc -l | tr -d ' ') tracks, $(du -sh "$dest" | cut -f1) in $dest"
echo "add it in Soul Player: Settings → Sources → Add local folder."
