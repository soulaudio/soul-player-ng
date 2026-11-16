# Exclusive / bit-perfect output — design

Date: 2026-11-16
Status: approved direction (Option A), pre-implementation

## Goal

Make the Audio signal path's "Exclusive output" stage real: **bit-perfect playback via
WASAPI exclusive mode** (Windows first), bypassing the OS mixer so the DAC receives the
track's samples untouched at its native rate. Flip the signal-path "soon" row to a live
stage that tells the truth about what the device is actually doing.

## Research → chosen behavior (Option A)

Community + Microsoft consensus (Roon/Audirvana set the bar; foobar/JRiver the purist
edge):
- **Bit-perfect = exclusive + native-rate switching**: open the DAC at the *source's*
  rate, no OS mixer, no resample (any SRC breaks bit-perfect). Roon/Audirvana do this.
- **Graceful fallback, never silent, never hard-stop by default**: Microsoft says
  "always provide a user-visible option to fall back to shared mode." Real impls:
  attempt exclusive → fall back to shared if it fails → emit the reason → offer retry.
  The real failure cases are device-in-use (`AUDCLNT_E_DEVICE_IN_USE`), Bluetooth, and
  16-bit-only DACs.

**Option A (chosen):**
1. Open the DAC at the track's native rate, exclusive → bit-perfect.
2. If the device refuses that format exclusively (or is in use), **fall back to shared**
   automatically — never silent, never a hard stop.
3. The signal-path row shows the truth: "Exclusive · 96 kHz / 24-bit (bit-perfect)" vs
   "Exclusive unavailable — shared mode" + the reason, with a retry.
4. A **"strict bit-perfect"** switch (exclusive-or-stop, the foobar/purist stance) is a
   deliberate *later* option, not the default.

Rejected: always-exclusive-but-resample (defeats bit-perfect); exclusive-or-stop as the
default (fragile on Bluetooth/16-bit DACs).

## Where it lives (core vs -ng)

Output backends already live in core's **`soul-audio-desktop`** (the std playback
adapter: `CpalSink`, device enumeration, the `Sink` trait impls). The no_std DAP
(`soul-listener`) has its own I²S/DAC output, so WASAPI is **desktop-only + feature-gated**
— homed next to `CpalSink`, not scattered into -ng.

| Piece | Home |
|---|---|
| `WasapiExclusiveSink` (`Sink` impl, WASAPI exclusive, event-driven) | **core `soul-audio-desktop`**, behind a `wasapi-exclusive` feature (`[target.'cfg(windows)']`, optional `wasapi` crate) |
| Device capabilities (supported exclusive formats, native rates) | **core `soul-audio-desktop`** (new `DeviceInfo`/caps from `cpal::supported_output_configs()` + WASAPI `IsFormatSupported`) |
| True output format + `exclusive` flag on `StreamInfo` / `DecodeInfo` | **core `soul-audio-core`** (model.rs / deck.rs) — generically useful (the DAP's DAC has a format too) |
| Select exclusive sink; toggle contract; emit fallback reason | **-ng `soul-player-host`** (`sink::build_actor` / `device_sink_factory`) |
| `SignalPath` DTO fields (exclusive, output format, fallback reason) | **-ng `soul-player-backend`** (`project_signal_path`, `types.rs`) |
| Settings → Audio → Output exclusive toggle; live signal-path row | **-ng `app/rune`** |

## Core PR (soul-audio-core)

1. **Model** (`soul-audio-core`):
   - Extend `StreamInfo` (model.rs) so a sink can report the DAC's **true** output format
     (rate + sample format/bits), not just canonical-24. Today `CpalSink::stream_info`
     reports canonical-24 and the device's real `sample_format` is discarded.
   - `DecodeInfo` / `Deck::decode_info` (deck.rs) gains `exclusive: bool` + the real
     output format, sourced from the sink. `output_bits` stops being a constant.
   - The `Sink` trait (sink.rs) may gain an `exclusive_active() -> bool` / richer
     `StreamInfo` so the engine can project it. Keep the trait minimal + no_std-safe.
2. **`soul-audio-desktop`**:
   - `WasapiExclusiveSink` — a `Sink` impl opening WASAPI `AUDCLNT_SHAREMODE_EXCLUSIVE`,
     event-driven, at the requested (source) rate; probes `IsFormatSupported`; feeds the
     device's native bit depth from canonical-24 via `soul_audio_core::convert`. Returns a
     typed error (unsupported-format / device-in-use) so the caller can fall back.
   - A device-capabilities query (supported exclusive formats / native rates) for the
     picker and to choose the open format.
   - Feature `wasapi-exclusive` (optional `wasapi` crate, `cfg(windows)`); the crate stays
     shared-mode (`CpalSink`) on other platforms. **Verify the `wasapi` crate clears
     `cargo-deny` (license/bans); fall back to the `windows` crate's `Win32::Media::Audio`
     directly if not.**
   - Tier: `soul-audio-desktop` is a StdAdapter — no tier-manifest change, but the new
     feature + any Windows dep must pass `cargo xtask ci` (incl. the thumbv7em builds,
     which won't touch this Windows-gated code).
3. Gate locally (`cargo xtask ci`), PR, merge.

## -ng PR (soul-player-ng)

1. Bump core across all three workspaces.
2. **host** (`sink.rs` / `lib.rs`): an `exclusive: bool` on the output contract
   (`set_exclusive` or folded into `set_output_device`); `device_sink_factory` tries
   `WasapiExclusiveSink` when exclusive is on, **falls back to `CpalSink` (shared)** on a
   typed exclusive error, and records the fallback reason. Surface device capabilities +
   the true live output format. Persist the toggle (config.rs).
3. **backend** (`driver.rs` / `types.rs`): `SignalPath` gains `exclusive: bool`,
   `output_rate`, `output_bits`/sample format, and an optional `fallback_reason`. Drop the
   hardcoded `output_bits: 24`.
4. **app**:
   - Settings → Audio → Output: an **Exclusive** toggle (persisted). Shows the device's
     true format; disabled/annotated if the device can't do exclusive at all.
   - The now-playing signal-path `sp-row-exclusive` + settings `sp-stage-exclusive` rows
     go from static "soon" to live: "Exclusive · <rate>/<bits> (bit-perfect)" or
     "Exclusive unavailable — shared" + reason, with a retry affordance.
5. Full verify (app/host/backend), PR, merge.

## Shipped in two increments

The engine opens the sink **once per playback session** at a fixed rate and resamples
each track to it (the `Deck` resampler). True native-rate-per-track switching therefore
needs a core-engine change (rebuild the deck's sink when the decoder rate changes) — a
distinct, DAP-relevant piece of work. So Option A lands in two increments:

- **Increment 1 (this PR):** exclusive mixer-bypass at the **device's native (shared
  mix-format) rate**. A rate-matched track is fully bit-perfect (mixer bypassed, native
  bit depth); a mismatched track is still exclusive but resampled to the device rate —
  and the signal path shows that truth (`resampling: true`, `exclusive: true`). Graceful
  fallback to shared on any exclusive failure, with the reason. This delivers the core
  audiophile win (no Windows mixer SRC/volume/mixing) and validates the whole toggle →
  sink → fallback → signal-path plumbing end to end.
- **Increment 2 (next, core-engine):** **native-rate auto-switching** — the engine
  reopens the exclusive sink at each track's rate so nothing is ever resampled (the
  Roon/Audirvana behavior). Homed in `soul-audio-core` (the DAP's engine benefits too):
  `pump_thread` + `Deck` gain the ability to swap the sink when the source rate changes.

## Non-goals / later

- **Increment 2 native-rate auto-switching** (above) — the next core-engine step.
- **macOS** (CoreAudio hog mode + `kAudioDevicePropertyNominalSampleRate`) and **Linux**
  (ALSA `hw:` direct) — later sinks behind the same `Sink` seam.
- **DoP** (DSD-over-PCM) for exclusive DSD passthrough — later.
- **Live hot-swap** of the device/mode mid-stream — today it applies on the next actor
  spawn (same as device selection); keep that for now.
- **Strict bit-perfect** (exclusive-or-stop) switch — later, as an opt-in for purists.
- **Precise fallback reason surfaced in the UI** — the host records the typed
  `ExclusiveError` reason (`exclusive_status()`); increment 1's Settings card shows the
  active/fell-back/shared distinction, and wiring the exact reason string into the card
  is a small follow-up.

## Risks

- WASAPI exclusive is device-dependent: `IsFormatSupported` + `Initialize` can still
  refuse (Bluetooth, 16-bit DACs, device-in-use). The typed-error → shared-fallback path
  is the mitigation; test the fallback explicitly.
- Event-driven exclusive needs a correct buffer/period + a dedicated audio thread; the
  existing ring (`ringbuf 0.6`) + 200 ms budget pattern from `CpalSink` is the reference.
- `wasapi` crate licensing must clear `cargo-deny`; `windows` crate is the fallback.
- No external DAC on the dev machine → validate the *fallback path* and the format
  reporting locally; true bit-perfect A/B needs a DAC the user has.
