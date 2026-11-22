# macOS platform roadmap — bit-perfect audio, test strategy, native integration

Status: **active** · Started 2026-11-21 · Spans `soul-audio-core` (`soul-audio-desktop`) and
`soul-player-ng` (`soul-player-host`, `app/rune`).

Goal: Soul Player is a first-class macOS app on every Apple desktop — bit-perfect exclusive
output that behaves correctly on any CoreAudio device class, proven by a layered test suite
that does not depend on owning the hardware, plus the native integration a Mac music player
is expected to have (Now Playing, media keys, menu bar, Dock menu, notifications).

Legend: ✅ done · 🔨 in progress · ⏳ next · 🧪 spike / needs a decision

---

## Phase 0 — CoreAudio exclusive output ✅

- `CoreAudioExclusiveSink` (hog mode, per-track nominal-rate switching, widest physical
  format, restore-on-drop), safe `coreaudio-rs` 0.11 helpers only (`forbid(unsafe_code)`).
- Device-free policy: `exclusive_policy::{choose_rate, pick_physical_format}`.
- Host wiring (`ExclusiveSink` alias on Windows + macOS); precise fall-back reasons.
- Real-device suites: `coreaudio_exclusive_suite` (12 tests, core) and
  `coreaudio_exclusive_e2e` + `native_rate_hardware_e2e` (host). Green on MacBook Pro
  speakers (Float32 44.1–96k) and a DisplayPort monitor (Int24 32–48k).

## Phase 1 — HAL abstraction + fake device world (TDD core) ✅

Nobody in the ecosystem mocks the CoreAudio HAL (cubeb-coreaudio-rs, Chromium, cpal all
test against real devices only), so device diversity is usually untested. We split the sink:

- `exclusive_engine::ExclusiveEngine<H: Hal>` — the whole state machine, platform-neutral.
- `coreaudio_exclusive::CoreAudioHal` — the thin real adapter (macOS).
- `fake_hal::FakeHal` — scriptable world: devices from profiles, CoreAudio semantics (hog
  toggle, default device moving off a hogged device), ordered call log, test-pulled
  rendering (bit-exact assertions with no clock), fault injection.

Done: `tests/exclusive_engine_contract.rs` — 20 tests, every fixture × every standard rate,
plus faults. Writing it found a real bug (flush on an unplugged device returned `Ok`).
The real-device suite (12 tests) is green through the new `CoreAudioHal` on MacBook Pro
speakers and a DisplayPort monitor.

Contract tests run against **every fixture** (Phase 2) and every fault:
hog held by another pid / refused by driver · rate change refused · physical format refused
(ladder fallback) · stream start failure · unplug mid-play (write/flush → `Closed`, no hang,
no restore attempts on a dead device) · external rate change (AirPods SCO) · default device
moving · restore + hog release on **every** exit path · sequencing (stream stopped before
rate/format change; hog released last) · bit-exact Float32 output at unity volume.

## Phase 2 — Device profiles: capture tool + fixture library ✅

- ✅ `soul-audio-desktop` example `coreaudio_device_report` (`--probe-hog`, `--out DIR`): dumps every output device
  (name, transport, nominal rate, physical-format ladder, hog support) as JSON — **the same
  format `FakeHal` loads**, so a user's bug report becomes a regression test.
- Fixtures in `crates/soul-audio-desktop/tests/fixtures/coreaudio/*.json`, each marked
  `"source": "captured"` (real device dump) or `"synthesized"` (from specs / public reports,
  to be replaced by a capture):

| Fixture | Transport | Formats / rates | Hog | Source |
|---|---|---|---|---|
| macbook-pro-speakers (Apple silicon) | built-in | Float32 44.1/48/88.2/96k | yes | captured |
| displayport-monitor | displayport | Int24/20/16 32/44.1/48k | yes | captured |
| apple-silicon-headphone-jack | built-in | ≤96k | yes | synthesized |
| intel-builtin-output | built-in | Int16/24 44.1–96k | yes | synthesized |
| usb-uac2-dac (384k) | usb | Int16/24/32 44.1–384k | yes | synthesized |
| hdmi-avr (M-series) | hdmi | 2/8ch Int16/24 @48k + AC-3 (non-LPCM, skipped) | yes | synthesized |
| airpods | bluetooth | Float32 48k fixed | refused | synthesized |
| airplay | airplay | Float32 44.1k fixed | refused | synthesized |
| aggregate / multi-output | aggregate | sum of members | no property | synthesized |
| blackhole-2ch | virtual | Float32 8k–768k | yes | captured (CI) |
| null-audio-device (CI runner) | virtual | Float32 44.1/48k only | yes | captured (CI) |
| apple-virtual-sound-device (CI runner) | built-in | Float32 48/96k only | yes | captured (CI) |

Single-Mac project (decided 2026-11-22): no further hardware captures are planned. Real
coverage = this MacBook Pro + its DisplayPort monitor + the three CI runners' devices; every
other device class is a synthesized profile from public specs, exercised through the fake HAL
and — next — a virtual device that mimics it (Phase 4 spikes). A user's
`coreaudio_device_report` dump drops straight in as a new fixture if one ever arrives.

## Phase 3 — Robustness for real-world device behaviour ✅

Driven red→green by Phase 1 faults:
- External rate/format change while playing (AirPods mic → SCO 16k; driver-initiated):
  re-assert our rate (we hold hog), else fail honestly.
- Unplug mid-play → `SinkError::Closed` promptly (exclusive engine AND shared `CpalSink`
  via a `DeviceIsAlive` listener — Apple-silicon headphone jack / AirPods / USB DACs are
  separate devices that vanish); backend `classify_pump_error` → `handle_device_lost`
  pauses + parks (never auto-resumes onto the speakers). Manual hardware check:
  `tests/coreaudio_unplug_manual.rs`. ❎ Intel Macs switch headphones via a *data source*
  on one device (`kAudioDevicePropertyDataSource` — not in coreaudio-rs's safe API).
  **Not planned:** needs `unsafe` FFI and an Intel Mac to verify; we have neither.
- Hog refused (aggregate / Bluetooth / AirPlay) → typed `Unsupported` → host falls back to
  shared with a clear reason (strict mode stops).

## Phase 4 — CI on every Apple desktop architecture ✅

First run (soul-audio-core#109) green on macos-15, macos-26 and macos-15-intel: BlackHole
**accepts hog mode** on hosted runners, the whole 13-rate ladder switches, and the loopback is
**bit-identical** at 44.1k and 96k on all three — so the loopback step now gates. The runners'
device profiles are identical across images and are checked in as captured fixtures.

- Matrix `macos-15`, `macos-26` (arm64) + `macos-15-intel` (x86_64): build, clippy, unit +
  fake-HAL contract suite (no device needed).
- Virtual-device integration: `brew install --cask blackhole-2ch` + `switchaudio-osx`,
  `sudo killall -9 coreaudiod`, assert presence via `system_profiler`; run the real-device
  suites serially against BlackHole (cubeb-coreaudio-rs does exactly this).
- ✅ Bit-perfect loopback (BlackHole out → in, sample-exact compare); blocking (the hosted
  runners grant the microphone permission).
- ⏳ **Next** (the hardware-free way to cover more devices): custom-built BlackHole with a restricted `kSampleRates` ladder (44.1/48/96/192k) to
  exercise in-family fallback on the real AUHAL path.
- ⏳ libASPL / tympan-aspl "ProfileDevice" HAL plug-in loading fixture profiles (Int24,
  Int32-384k, 48k-only, no-hog) — blocked on a signing spike (do ad-hoc-signed HAL plug-ins
  load on macOS 15/26 runners?).

## Phase 5 — Native macOS integration 🔨

Verified on this Mac with `scripts/e2e-now-playing-macos.sh` (real app ↔ MediaRemote via
`media-control`, shared AND hog-mode exclusive — exclusive output does NOT knock the app
out of Now Playing): metadata, duration, elapsed, rate, artwork, and remote
pause/play/seek/next/toggle all round-trip. Bundled, macOS attributes it to
`co.datadir.soulplayer`.

- ✅ Now Playing artwork (cover → cache file → `file://` URL; one file per track).
- ✅ Controls menu (Play/Pause, Next ⌘→, Previous ⌘←, keys-first) + Dock menu (`macos_menu`).
- ✅ Notifications via `rune-notify` (bundled `.app` only; one id per kind).
- ✅ Sleep: `coreaudiod` holds `PreventUserIdleSystemSleep` while we play (shared and
  exclusive) — no app assertion needed. App Nap does not apply to audio-producing apps.
- ✅ Artwork latency measured: cover fetch + write ≈ 0 ms; macOS shows it ~0.6 s after the
  title (souvlaki's async `NSImage` load) — expected, not a bug (an earlier "4–7 s" was a
  coarse polling artefact). Debug log `now-playing artwork published` records timings.
- ❎ Skip ±15 s — **decided 2026-11-22: no.** Enabling `skipForward/BackwardCommand` makes
  Control Center and the media keys show ±15 s *instead of* previous/next track (right for
  podcasts, wrong for a music player; Apple Music doesn't enable them).
- ❎ Repeat / like in Control Center — **decided 2026-11-22: no.** macOS's Now Playing widget
  never shows repeat/shuffle, and the heart opens add-to-playlist (no liked state to toggle).
- ⏸ Finder "Open With" / double-click / Dock drop — **waiting on Rune**
  ([anomalyco/rune#157](https://github.com/anomalyco/rune/issues/157): an `on_open_files`
  hook for the `'odoc'` Apple event; Rune takes issues, not PRs). Shipped briefly in #265 via
  our own `unsafe` crate, **removed 2026-11-22** to keep the app at zero `unsafe`. Ready to
  re-enable in a few lines: `open_files` (library files play; unknown folders are added,
  then play; opens wait for the startup scan) already handles launch arguments; the bundle
  associations and the e2e script are in git history (6ad0dcd).
- ✅ Proper macOS icon (`icons/macos/icon.icns`, 16–1024 px on Apple's grid).
- ✅ kill / Ctrl-C / closed terminal quit like ⌘Q (`signal_quit`): no false crash report.
- ✅ Output picker shows each device's transport (`device_transport`, from `system_profiler`)
  and flags AirPlay / Bluetooth as "not bit-perfect" before the user picks them.
- ✅ `scripts/demo-library/make-demo-library.sh`: one-command 26-track library (every format
  and rate, real DSD64–512 via `dsdgen`, 5.1) for machines with no music.
- ❎ Menu-bar extra — **decided 2026-11-21: no.** Native surfaces instead (Control
  Center Now Playing + Dock menu + Controls menu, like Apple Music); avoids `tray-icon`'s
  second objc2 copy. Rune `Tray` stays Linux/Windows only.

Original plan:

- **Now Playing / media keys / AirPods controls**: audit `souvlaki` on macOS (thread
  requirements, position/rate, artwork), fix publishing, add cover art, verify Control
  Center widget; pure mapping logic stays unit-tested (`media_session::map_event`).
- **Dock menu** (Play/Pause, Next, Previous, now-playing title) via Rune `MenuItem::dock()`
  and a **menu-bar Controls menu** with ⌘ shortcuts — testable headlessly through Rune's
  native-menu dispatch.
- **Menu-bar extra (tray)**: Rune's `Tray` is Linux/Windows only → macOS backend.
- **Notifications**: `rune-notify` (`UNUserNotificationCenter`, needs a bundled `.app`).
- **App Nap / power**: keep playback unthrottled while playing.
- **Headphones unplugged → pause** (default-device-change handling in shared mode).

## Phase 6 — Packaging 🔨

✅ `cargo packager -f app` builds `Soul Player.app` (`co.datadir.soulplayer`,
`public.app-category.music`). ✅ `-f dmg` builds a drag-to-Applications DMG (verified:
mounts, correct bundle id, arm64); release.yml ships it on macOS. ⏳ Developer ID signing +
notarization need the team's Apple Developer account (decision for the owner).

## Phase 7 — Hardware-in-the-loop ❎

**Not planned** (2026-11-22): there's no Apple hardware beyond one MacBook Pro. Device
diversity is covered by the fake-HAL contract suite over synthesized profiles, the CI runners'
virtual devices, and the Phase 4 virtual-device spikes.

---

## How to run the suites (macOS)

```sh
# core — unit + fake-HAL contract (any OS)
cargo test -p soul-audio-desktop --features coreaudio-exclusive
# core — real device (serial; SOUL_CA_DEVICE="<name>" to pick one)
cargo test -p soul-audio-desktop --features coreaudio-exclusive \
  --test coreaudio_exclusive_suite -- --ignored --nocapture --test-threads=1
# player — real device e2e through PlayerHandle
cargo test --manifest-path crates/soul-player-host/Cargo.toml \
  --test coreaudio_exclusive_e2e --test native_rate_hardware_e2e -- --ignored --test-threads=1
```
