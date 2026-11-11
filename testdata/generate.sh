#!/usr/bin/env bash
# Regenerate the committed `tone.*` audio fixtures used by the end-to-end
# seek-audit harness (`crates/soul-player-backend/tests/seek_audit.rs`) and the
# library-scan format matrix (`src/library.rs`).
#
# Every fixture carries the SAME 2-second stereo signal — left = 440 Hz (A4),
# right = 659.25 Hz (E5), -6 dB, 50 ms in/out fades — the canonical `soul-audio`
# test tone (identical to the core repo's `testdata/generate.sh`). Deriving every
# format from ONE master means:
#   * each format decodes to the same audible content (decode correctness), and
#   * the seek harness can cross-check a format's post-seek audio against an
#     independent decode of the SAME file.
# 44.1 kHz on purpose (Opus resamples to its native 48 kHz, MP1 is 32 kHz) so the
# harness exercises several native rates.
#
# The LEFT channel is a single 440 Hz sinusoid (period ~100 samples at 44.1 kHz),
# which is why the harness's cross-correlation — searched over +/-64 lags on
# channel 0 — has a unique peak at lag 0.
#
# Run:  bash testdata/generate.sh   (needs ffmpeg with libmp3lame + libopus +
# libvorbis). Fixtures are committed; CI never runs ffmpeg.
set -euo pipefail
cd "$(dirname "$0")"

GRAPH="aevalsrc=exprs=0.5*sin(2*PI*440*t)|0.5*sin(2*PI*659.25*t):s=44100:d=2:c=stereo,afade=t=in:d=0.05,afade=t=out:st=1.95:d=0.05"

# Lossless master + derivations (bit-exact content across all three).
ffmpeg -y -hide_banner -loglevel error -f lavfi -i "$GRAPH" -c:a pcm_s16le tone.wav
ffmpeg -y -hide_banner -loglevel error -i tone.wav -c:a flac tone.flac
ffmpeg -y -hide_banner -loglevel error -i tone.wav -c:a alac -movflags +faststart tone.m4a

# Lossy derivations. The seek harness uses a looser cross-correlation floor for
# these (the decode is deterministic, so in practice they still match ~1.0).
ffmpeg -y -hide_banner -loglevel error -i tone.wav -c:a libmp3lame -b:a 192k tone.mp3
ffmpeg -y -hide_banner -loglevel error -i tone.wav -c:a mp2 -b:a 192k tone.mp2
# AAC-LC in two framings: ADTS (.aac, no container length — core header-walks for
# an exact frame count) and MP4 (.m4a). `tone-aac.m4a` is NOT `tone.m4a` (that is
# the lossless ALAC fixture above — same container, different codec).
ffmpeg -y -hide_banner -loglevel error -i tone.wav -c:a aac -b:a 192k -f adts tone.aac
ffmpeg -y -hide_banner -loglevel error -i tone.wav -c:a aac -b:a 192k -movflags +faststart tone-aac.m4a
ffmpeg -y -hide_banner -loglevel error -i tone.wav -c:a libvorbis -q:a 5 tone-vorbis.ogg
# Opus is always 48 kHz internally — ffmpeg resamples the 44.1 kHz master.
ffmpeg -y -hide_banner -loglevel error -i tone.wav -c:a libopus -b:a 96k tone.opus

# MPEG-1 Layer I (.mp1): ffmpeg has NO Layer I encoder, so we cannot synthesize a
# fresh tone. Instead we take the committed Layer I conformance vector (a short
# ~265 Hz tone, 32 kHz stereo, 0.576 s) and concatenate it 4x. Layer I frames are
# self-contained (no bit reservoir, CBR), so raw byte concatenation yields a valid
# ~2.34 s stream — long enough for the harness to seek ~30 %/70 % into. The source
# vector is committed at `tone-layer1-src.mp1`; keep it to re-run this step.
if [ -f tone-layer1-src.mp1 ]; then
  cat tone-layer1-src.mp1 tone-layer1-src.mp1 tone-layer1-src.mp1 tone-layer1-src.mp1 > tone.mp1
fi

echo "generated:"
ls -la tone.wav tone.flac tone.m4a tone.mp3 tone.mp2 tone.aac tone-aac.m4a \
  tone-vorbis.ogg tone.opus tone.mp1
