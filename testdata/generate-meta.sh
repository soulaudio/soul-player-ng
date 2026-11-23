#!/usr/bin/env bash
# Regenerate the committed `meta.*` **real-music** fixtures — the Aria corpus
# that `crates/soul-player-host/tests/format_matrix_e2e.rs` drives through both
# a local folder source and a real `soul-server` remote source.
#
# Companion to `generate.sh`, which makes the synthetic `tone.*` signal fixtures.
# The split matters: `tone.*` fixtures carry a signal the harnesses can measure
# (a known sinusoid, cross-correlated after a seek), while `meta.*` fixtures
# carry real musical structure AND real tags, which is what a library scan and
# the `.soulidx` wire protocol are actually judged on.
#
# THE MASTER is `meta.wav` — a 1.5 s, 44.1 kHz/16-bit stereo excerpt of the
# **Open Goldberg Variations**: Kimiko Ishizaka playing the Aria from J.S. Bach's
# Goldberg Variations BWV 988, released **CC0 / public domain** by the Open
# Goldberg project (opengoldbergvariations.org; source: archive.org
# `OpenGoldbergVariations`). CC0 is why it is committable. Same provenance as
# soul-audio-core's `realmusic.*` fixtures. Every format below is derived from
# that one master, so all of them decode to the same music — which is what lets
# the matrix test assert a local decode and a remote decode agree sample for
# sample.
#
# EVERY derivation is tagged title/artist/album, deliberately: an untagged
# fixture can only ever prove "the file scanned", never "the tags reached the
# library" or "the tags survived the wire". Formats where the muxer drops a tag
# are handled explicitly below rather than silently.
#
# Run:  bash testdata/generate-meta.sh   (needs ffmpeg with libmp3lame +
# libtwolame + libopus + libvorbis). Fixtures are committed; CI never runs ffmpeg.
set -euo pipefail
cd "$(dirname "$0")"

TITLE="Aria"
ARTIST="Kimiko Ishizaka"
ALBUM="Open Goldberg Variations"
# Written into every container ffmpeg will take them for. `-map_metadata -1`
# first drops whatever the master carried, so the tags below are the only
# source of truth and a stale master can't leak an old value through.
TAGS=(-map_metadata -1 -metadata "title=$TITLE" -metadata "artist=$ARTIST" -metadata "album=$ALBUM")

ff() { ffmpeg -y -hide_banner -loglevel error "$@"; }

# Length in bytes of the ID3v2 tag at the head of $1 (0 if there is none).
# Header is "ID3" + version(2) + flags(1) + a 4-byte SYNCSAFE size (7 bits per
# byte, high bit always clear) covering everything after the 10-byte header.
id3v2_len() {
  [ "$(dd if="$1" bs=1 count=3 2>/dev/null)" = "ID3" ] || { echo 0; return; }
  local b
  read -r -a b <<<"$(od -An -tu1 -j6 -N4 "$1")"
  echo $((10 + (b[0] << 21) + (b[1] << 14) + (b[2] << 7) + b[3]))
}

# Prepend $1's ID3v2 tag to the raw stream $2, in place.
# ID3v2 is a *prefix* tag that MPEG-audio parsers are required to skip, so
# concatenation is the whole mechanism — which is how the committed
# `tagged.mp1`/`tagged.mp2` fixtures carry tags too.
graft_id3v2() {
  local donor=$1 target=$2 len
  len=$(id3v2_len "$donor")
  [ "$len" -gt 0 ] || { echo "no ID3v2 tag in $donor to graft" >&2; exit 1; }
  dd if="$donor" of="$target.id3" bs=1 count="$len" status=none
  cat "$target.id3" "$target" > "$target.tagged"
  mv "$target.tagged" "$target"
  rm -f "$target.id3"
}

if [ ! -f meta.wav ]; then
  echo "meta.wav (the Aria master) is missing — it is committed, not generated." >&2
  exit 1
fi

# --- Lossless -------------------------------------------------------------
# meta.wav itself is never rewritten — it is the committed master (already
# tagged), and re-encoding it would make every other fixture's provenance
# depend on a file this script had just churned.
ff -i meta.wav "${TAGS[@]}" -c:a flac                             meta.flac
ff -i meta.wav "${TAGS[@]}" -c:a wavpack                          meta.wv
ff -i meta.wav "${TAGS[@]}" -c:a alac -movflags +faststart        meta-alac.m4a
ff -i meta.wav "${TAGS[@]}" -c:a alac                             meta.caf
# FLAC inside an Ogg container (.oga) — a different demux path from native .flac.
ff -i meta.wav "${TAGS[@]}" -c:a flac -f oga                      meta-oggflac.oga
# AIFF, under both committed extensions. ffmpeg's AIFF muxer writes tags as an
# ID3 chunk; `-write_id3v2 1` is what makes artist/album land, not just title.
ff -i meta.wav "${TAGS[@]}" -c:a pcm_s16be -write_id3v2 1         meta.aiff
cp meta.aiff meta.aif

# --- Lossy ----------------------------------------------------------------
ff -i meta.wav "${TAGS[@]}" -c:a libmp3lame -b:a 192k             meta.mp3
# MPEG-1 Layer II. Its only tag carrier is ID3v2 — but ffmpeg's `mp2` muxer
# exposes no options at all (no `-write_id3v2`), and the `mp3` muxer refuses a
# Layer II stream ("Exactly one MP3 audio stream is required"). So encode raw,
# then graft on the ID3v2 header ffmpeg just wrote into `meta.mp3`, which
# carries exactly these tags.
ff -i meta.wav "${TAGS[@]}" -c:a mp2 -b:a 192k                    meta.mp2
graft_id3v2 meta.mp3 meta.mp2
ff -i meta.wav "${TAGS[@]}" -c:a libvorbis -q:a 5                 meta.ogg
# Opus is always 48 kHz internally — ffmpeg resamples the 44.1 kHz master.
ff -i meta.wav "${TAGS[@]}" -c:a libopus -b:a 96k                 meta.opus
ff -i meta.wav "${TAGS[@]}" -c:a libopus -b:a 96k                 meta.mka
ff -i meta.wav "${TAGS[@]}" -c:a libopus -b:a 96k                 meta.webm
# AAC-LC in two framings: MP4 (.m4a) and raw ADTS (.aac). ADTS has no metadata
# of its own, so tags ride in a prepended ID3v2 header — the `-write_id3v2 1`
# is the whole reason `meta.aac` has readable tags at all.
ff -i meta.wav "${TAGS[@]}" -c:a aac -b:a 192k -movflags +faststart meta-aac.m4a
ff -i meta.wav "${TAGS[@]}" -c:a aac -b:a 192k -f adts -write_id3v2 1 meta.aac
# `meta-id3.aac` is the same ADTS stream; kept as a separate fixture because
# `remote_e2e.rs` names it explicitly as its ID3-tagged-ADTS case.
cp meta.aac meta-id3.aac

# --- Deliberately NOT regenerated ----------------------------------------
# meta.dsf / meta.dff — DSD. ffmpeg has no DSD *encoder*, so these cannot be
#   derived from the PCM master; they are generated by soul-audio-core's
#   `testdata/generate_dsd.py` and committed as-is. (DSDIFF also has no tag
#   chunk anyone writes, which is why the matrix test treats `.dff` as a
#   filename-fallback case rather than a tagged one.)
# meta.mp1 — MPEG-1 Layer I. ffmpeg ships NO Layer I encoder (see `generate.sh`,
#   which builds `tone.mp1` by concatenating a committed conformance vector), so
#   there is no way to encode the Aria as Layer I here. Layer I therefore has
#   signal + tag coverage via `tone.mp1` / `tagged.mp1`, but no real-music
#   variant; the matrix test marks it as such instead of pretending otherwise.

echo "generated the Aria corpus:"
ls -la meta.flac meta.wv meta-alac.m4a meta.caf meta-oggflac.oga \
       meta.aiff meta.aif meta.mp3 meta.mp2 meta.ogg meta.opus meta.mka \
       meta.webm meta-aac.m4a meta.aac meta-id3.aac
