# Generated covers for albums and artists without art

Status: **done** (2026-11-22) · Added 2026-11-22

Shipped as `soul_player_host::generated_cover` (seeds, Oklab gradient + blob + grain, PNG) and
wired into the app's art cache (album/track covers), artist avatars, and the Now Playing
artwork. Differences from the notes below: albums are keyed by **title** (the library groups
albums by title, so every track of a compilation shares one cover; there's no album-artist
field yet); MBIDs aren't used yet; maths goes through `libm` so covers are bit-identical on
every platform (golden digests in the tests, checked on macOS/Linux/Windows CI). Renders are
memoized per seed in memory, not cached on disk (≈ms each).

## Goal

An album or artist with no cover art (no embedded picture, no folder `cover.*`, no artist image)
gets a **generated cover**: a soft multi-stop gradient in the spirit of the current ones,
with a **film-grain texture** on top so it reads as artwork instead of a flat placeholder.
Each cover is **deterministic**: the same album or artist always gets the same cover, on
every launch, machine and platform (desktop, DAP, a future iOS client).

## Design notes

- **Seed:** derive it from a stable 64-bit hash of a normalized identity string:
  - albums: `album_artist \u{1f} album`
  - artists: the artist name
  - normalize first: Unicode NFC, case-folded, whitespace trimmed and collapsed.
  - Use a fixed, specified algorithm (FNV-1a 64 or xxHash64), never `std`'s `DefaultHasher`,
    whose output is randomized per process and may change between Rust versions.
  - Prefer an MBID when the library has one: it is stable across tag spelling fixes.
- **Palette:** pick 2–3 hues from the seed within curated ranges (avoid muddy browns and
  neon clashes); vary the angle and stop positions slightly; keep contrast high enough for
  the title text drawn on cards; check the result in both light and dark themes.
- **Grain:** add per-pixel noise from a hash of `(seed, x, y)`, not a stateful RNG, so it is
  identical at any render order and resolution tier. Keep it subtle: luminance only, low
  amplitude (~3–5%), optionally a little blue-noise shaping so it doesn't band.
- **Where it renders:** a pure `generated_cover(seed, size) -> RGBA` function in a shared crate
  (unit-tested: same input → byte-identical output; golden hashes for a few seeds). Cache
  rendered sizes on disk under the art cache, keyed by `(seed, size, algo_version)`.
  - Bump `algo_version` whenever the look changes, so old caches invalidate.
- **Everywhere art shows:** album and artist cards, detail headers, the player bar and
  **Now Playing / Control Center** (it already takes a cover file URL, so a generated cover
  can be published the same way as a real one).
- **Never overrides real art:** generated covers only fill gaps. A user-set cover, embedded
  art or a folder image always wins.

## Tests

- Determinism: the same identity → the same seed → byte-identical pixels (golden digests).
- Normalization: `"Björk"` (NFC) and the NFD spelling, plus case and space variants, map to
  the same cover.
- Distinctness: a few thousand real album names produce no near-duplicate palettes (a
  minimum hue/angle distance check).
- Legibility: the contrast of the card caption over the generated cover meets the
  accessibility pass's threshold.
