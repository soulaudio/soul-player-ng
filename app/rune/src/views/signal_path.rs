//! The expanded **signal-path** panel: the current track's real decode→output
//! chain shown stage by stage — Source → Decode → ReplayGain → Resample →
//! Output — opened by pressing the now-playing signal-path line. ReplayGain
//! toggles inline (the one stage that is a real on/off today); exclusive /
//! bit-perfect output is live (lit when the stream bypasses the OS mixer), and the
//! DSP/EQ stage is live too — lit when the active chain shapes the signal, dim
//! **"Bypassed"** when the path stays bit-exact through it. Rendered as a themed
//! overlay anchored to the `"signal-path"` line and `modal` (dismiss on an
//! outside press / Escape), the same mechanism as [`crate::views::dropdown`].
#![forbid(unsafe_code)]

use rune::{Align, Color, Cursor, El, Icon, Side, col, el, icon, row, text};
use soul_player_host::{DspUnit, ReplayGainMode, SignalPath, SourceCodec, SourceFormat};

use crate::theme::Palette;
use crate::ui::{radius, space, type_};
use crate::views::icons;
use crate::{State, close_signal_path, set_replaygain_mode};

/// Gap between the anchor line and the panel.
const GAP: f32 = 6.0;
/// Quality dot size on each stage card.
const DOT: f32 = 8.0;

/// kHz label: whole kHz stays integer (48, 96), fractional keeps one place.
fn khz(hz: u32) -> String {
    if hz % 1000 == 0 {
        format!("{}", hz / 1000)
    } else {
        format!("{:.1}", f64::from(hz) / 1000.0)
    }
}

/// The source codec's short display name — a technical identifier (like the
/// rate / "DSD64" format values), not a translatable UI label.
fn codec_name(c: SourceCodec) -> &'static str {
    match c {
        SourceCodec::Flac => "FLAC",
        SourceCodec::Alac => "ALAC",
        SourceCodec::Aac => "AAC",
        SourceCodec::Mp3 => "MP3",
        SourceCodec::Opus => "Opus",
        SourceCodec::Vorbis => "Vorbis",
        SourceCodec::WavPack => "WavPack",
        SourceCodec::Wav => "WAV",
        SourceCodec::Aiff => "AIFF",
        SourceCodec::Caf => "CAF",
        SourceCodec::Matroska => "Matroska",
        SourceCodec::Dsd => "DSD",
        SourceCodec::Pcm => "PCM",
    }
}

/// Source stage: native DSD (family + 1-bit MHz rate), or the PCM codec + bit
/// depth — the codec name (FLAC / ALAC / AAC / …) is the lossless-vs-lossy
/// identity a bare "PCM" can't give.
pub(crate) fn source_value(sp: &SignalPath) -> String {
    match sp.source {
        SourceFormat::Dsd { rate_hz } => format!(
            "DSD{} · {:.2} MHz",
            (rate_hz / 44_100 / 64).max(1) * 64,
            f64::from(rate_hz) / 1e6
        ),
        SourceFormat::Pcm { bits } => format!("{} · {bits}-bit", codec_name(sp.codec)),
    }
}

/// Decode stage: the PCM the decoder hands the engine.
pub(crate) fn decode_value(sp: &SignalPath) -> String {
    format!(
        "{} kHz · {}-bit · {} ch",
        khz(sp.decoded_rate),
        sp.decoded_bits,
        sp.channels
    )
}

/// Resample stage: `a → b kHz` when resampling, else a rate-exact note.
pub(crate) fn resample_value(sp: &SignalPath) -> String {
    if sp.resampling {
        format!("{} → {} kHz", khz(sp.decoded_rate), khz(sp.device_rate))
    } else {
        t!("player.signalPath.rateExact").to_string()
    }
}

/// The OS audio API the output goes through. Exclusive and shared both use this API;
/// only the mode differs. Per-platform and `const` (resolved for the build target). The
/// exclusive path is implemented only on Windows (WASAPI) today, but the shared path
/// names its API on every platform, so the output stage always states the backend. It's
/// a technical identifier (like the codec names), not translated.
pub(crate) const fn output_api() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "WASAPI"
    }
    #[cfg(target_os = "macos")]
    {
        "CoreAudio"
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        "ALSA"
    }
}

/// Output stage: the backend API plus the device rate and bit depth it receives.
fn output_value(sp: &SignalPath) -> String {
    format!(
        "{} · {} kHz · {}-bit",
        output_api(),
        khz(sp.device_rate),
        sp.output_bits
    )
}

/// Exclusive stage: name the backend API and the mode. When exclusive, the bit-perfect
/// device format the DAC receives (no OS mixer); otherwise the shared path through the OS
/// mixer. So the user can always see WHICH backend drives the output and whether it is
/// bit-perfect ("WASAPI exclusive, 96 kHz, 24-bit") or shared ("WASAPI shared").
pub(crate) fn exclusive_value(sp: &SignalPath) -> String {
    if sp.exclusive {
        format!(
            "{} exclusive · {} kHz · {}-bit",
            output_api(),
            khz(sp.device_rate),
            sp.output_bits
        )
    } else {
        format!("{} shared", output_api())
    }
}

/// ReplayGain stage value: the applied gain, or "off".
fn replay_gain_value(mode: ReplayGainMode, gain_db: f32) -> String {
    if mode == ReplayGainMode::Off {
        t!("player.signalPath.off").to_string()
    } else {
        format!("{gain_db:+.1} dB")
    }
}

/// DSP/EQ stage value: **"Active"** when any unit in the chain would actually alter
/// the signal, else **"Bypassed"**. Keys off [`DspUnit::affects`], so an enabled-but-
/// flat EQ — or a disabled unit — honestly reads as a bit-exact passthrough.
pub(crate) fn dsp_value(units: &[DspUnit]) -> String {
    if dsp_active(units) {
        t!("player.signalPath.dspActive").to_string()
    } else {
        t!("player.signalPath.dspBypassed").to_string()
    }
}

/// Whether the DSP/EQ chain is shaping the signal (any unit
/// [`affects`](DspUnit::affects) it) — drives the stage's lit/accent vs dim/clean look.
pub(crate) fn dsp_active(units: &[DspUnit]) -> bool {
    units.iter().any(DspUnit::affects)
}

/// One **stage card** in the signal-path pipeline. Each pipeline stage is its own
/// card (foobar/Roon style: Decoder → ReplayGain → DSP → Output), with a Roon-like
/// quality dot — `primary` for a clean/lossless pass, `accent` for a stage that
/// *enhances/processes* the signal (ReplayGain on, a resample) — a stage icon, the
/// name, its value, and an optional trailing control (e.g. the ReplayGain toggle).
/// A dim card is an inactive stage (bypassed / shared). `key` is stable for tests.
#[allow(clippy::too_many_arguments)]
fn stage_card(
    key: &str,
    glyph: &'static Icon,
    name: String,
    value: String,
    trailing: Option<El>,
    accent: Color,
    dim: bool,
    p: &Palette,
) -> El {
    let dot = if dim { p.border } else { accent };
    let border = if dim { p.border } else { accent.alpha(0.45) };
    let name_color = if dim { p.text_secondary } else { p.fg };
    let mut head = row()
        .w_full()
        .center_y()
        .gap(space::SM)
        .child(el().w(DOT).h(DOT).radius(DOT / 2.0).fill(dot))
        .child(icon(glyph, 15.0).color(if dim { p.text_secondary } else { accent }))
        .child(
            text(name)
                .font(type_::LABEL)
                .color(name_color)
                .grow(1.0)
                .truncate(),
        );
    if let Some(t) = trailing {
        head = head.child(t);
    }
    let mut body = col()
        .key(key.to_string())
        .w_full()
        .min_w(0.0)
        .fill(p.muted)
        .radius(radius::MD)
        .border_sides([1.0, 1.0, 1.0, 1.0], border)
        .padding(space::SM)
        .gap(space::XXS)
        .child(head);
    if !value.is_empty() {
        body = body.child(
            // Indented under the dot+icon so the value lines up with the name.
            row().w_full().pl(DOT + space::SM + 15.0 + space::SM).child(
                text(value)
                    .font(type_::BODY)
                    .color(if dim { p.text_secondary } else { p.fg })
                    .grow(1.0)
                    .truncate(),
            ),
        );
    }
    body
}

/// A flow connector between two stage cards — a centered down-chevron showing the
/// signal moving from one card into the next. Dim when it leads into a
/// not-yet-built stage.
fn connector(dim: bool, p: &Palette) -> El {
    col()
        .w_full()
        .align(Align::Center)
        .child(icon(&icons::CHEVRON_DOWN, 14.0).color(if dim {
            p.border
        } else {
            p.primary.alpha(0.7)
        }))
}

/// The ReplayGain on/off toggle chip — the one real pipeline toggle today. Off ↔
/// Track (the full Track/Album choice stays in Settings); flips `replaygain_mode`
/// via [`set_replaygain_mode`], which persists + pushes it to the host.
fn rg_toggle(mode: ReplayGainMode, p: &Palette) -> El {
    let on = mode != ReplayGainMode::Off;
    let label = if on {
        t!("player.signalPath.on")
    } else {
        t!("player.signalPath.off")
    };
    let next = if on {
        ReplayGainMode::Off
    } else {
        ReplayGainMode::Track
    };
    let mut chip = text(label.to_string())
        .font(type_::LABEL)
        .px(space::SM)
        .py(space::XXS)
        .radius(radius::SM)
        .key("sp-rg-toggle")
        .role("button")
        .focusable(true)
        .cursor(Cursor::Pointer)
        .on_press(move |s: &mut State| set_replaygain_mode(s, next));
    chip = if on {
        chip.fill(p.primary.alpha(0.16)).color(p.primary)
    } else {
        chip.fill(p.muted).color(p.text_secondary)
    };
    chip
}

/// The pipeline stage rows (the reusable body shared by the now-playing popover
/// and the Settings → Audio card): the live chain when a track is playing, then
/// the Exclusive output and DSP/EQ stages — both live, lit when active and dim when
/// the path stays bit-exact (shared / bypassed).
pub(crate) fn pipeline_rows(s: &State, p: &Palette) -> El {
    let mut rows = col().key("signal-path-stages").w_full().gap(space::XS);
    if let Some(sp) = s.signal_path {
        // Roon-style accent: `primary` = clean/lossless pass; `accent` = a stage
        // that *processes* the signal (ReplayGain applied, a resample).
        let rg_accent = if s.replaygain_mode == ReplayGainMode::Off {
            p.primary
        } else {
            p.accent
        };
        let rs_accent = if sp.resampling { p.accent } else { p.primary };
        rows = rows
            .child(stage_card(
                "sp-row-source",
                &icons::MUSIC,
                t!("player.signalPath.source").to_string(),
                source_value(&sp),
                None,
                p.primary,
                false,
                p,
            ))
            .child(connector(false, p))
            .child(stage_card(
                "sp-row-decode",
                &icons::PLAY,
                t!("player.signalPath.decode").to_string(),
                decode_value(&sp),
                None,
                p.primary,
                false,
                p,
            ))
            .child(connector(false, p))
            .child(stage_card(
                "sp-row-replaygain",
                &icons::VOLUME,
                t!("player.signalPath.replayGain").to_string(),
                replay_gain_value(s.replaygain_mode, sp.replay_gain_db),
                Some(rg_toggle(s.replaygain_mode, p)),
                rg_accent,
                false,
                p,
            ))
            .child(connector(false, p))
            .child(stage_card(
                "sp-row-resample",
                &icons::REPEAT,
                t!("player.signalPath.resample").to_string(),
                resample_value(&sp),
                None,
                rs_accent,
                false,
                p,
            ))
            .child(connector(false, p))
            .child(stage_card(
                "sp-row-output",
                &icons::VOLUME,
                t!("player.signalPath.output").to_string(),
                output_value(&sp),
                None,
                p.primary,
                false,
                p,
            ))
            .child(connector(true, p));
    } else {
        rows = rows.child(
            text(t!("player.nothingPlaying").to_string())
                .font(type_::BODY)
                .color(p.text_secondary)
                .pb(space::SM),
        );
    }
    // Exclusive (bit-perfect) output — live: lit when the current stream bypasses
    // the OS mixer (the device gets exactly these samples), dim "shared" otherwise.
    let excl_live = s.signal_path.filter(|x| x.exclusive);
    // DSP/EQ is host/app-owned live state (the active chain), not per-track — read it
    // straight from the app's `dsp_units` rather than the backend SignalPath.
    let dsp_on = dsp_active(&s.dsp_units);
    rows.child(stage_card(
        "sp-row-exclusive",
        &icons::SETTINGS,
        t!("player.signalPath.exclusive").to_string(),
        s.signal_path
            .map(|x| exclusive_value(&x))
            .unwrap_or_default(),
        None,
        p.primary,
        excl_live.is_none(), // dim unless exclusive is actually active
        p,
    ))
    .child(connector(true, p))
    // DSP/EQ: lit + accent when the active chain shapes the signal, dim "Bypassed"
    // when the path is bit-exact through it (empty chain, or every unit flat/disabled).
    .child(stage_card(
        "sp-row-dsp",
        &icons::SETTINGS,
        t!("player.signalPath.dsp").to_string(),
        dsp_value(&s.dsp_units),
        None,
        if dsp_on { p.accent } else { p.primary },
        !dsp_on,
        p,
    ))
}

/// Per-track **loudness & dynamic-range** readouts (from the hybrid-ReplayGain
/// background analysis): integrated LUFS, LRA, true-peak, PLR, and DR. Shown under
/// the pipeline; "analyzing…" until the current track has been measured.
pub(crate) fn loudness_block(s: &State, p: &Palette) -> El {
    let mut block = col()
        .key("signal-path-loudness")
        .w_full()
        .gap(space::XXS)
        .pt(space::XS)
        .child(
            text(t!("player.signalPath.loudness").to_string())
                .font(type_::SECTION)
                .color(p.text_secondary),
        );
    block = match s.signal_path.and_then(|sp| sp.loudness) {
        Some(m) => block
            .child(metric_row(
                "signal-path-integrated",
                t!("player.signalPath.integrated").to_string(),
                format!("{:.1} LUFS", m.integrated_lufs),
                p.fg,
                p,
            ))
            .child(metric_row(
                "signal-path-lra",
                t!("player.signalPath.lra").to_string(),
                format!("{:.1} LU", m.lra),
                p.fg,
                p,
            ))
            .child(metric_row(
                "signal-path-truepeak",
                t!("player.signalPath.truePeak").to_string(),
                format!("{:+.1} dBTP", m.true_peak_dbtp),
                p.fg,
                p,
            ))
            .child(metric_row(
                "signal-path-plr",
                t!("player.signalPath.plr").to_string(),
                format!("{:.1} LU", m.plr),
                p.fg,
                p,
            ))
            .child(metric_row(
                "signal-path-dr",
                t!("player.signalPath.dynamicRange").to_string(),
                format!("DR{}", m.dr as i32),
                dr_color(m.dr, p),
                p,
            )),
        None => block.child(
            text(t!("player.signalPath.analyzing").to_string())
                .key("signal-path-analyzing")
                .font(type_::CAPTION)
                .color(p.text_secondary),
        ),
    };
    // Live meter: the real-time momentary (400 ms) + short-term (3 s) loudness and sample
    // peak from the output tap, shown while a track plays — the dynamic counterpart to the
    // whole-track readouts above. Each value is "—" until the window fills.
    if s.playing {
        let live = s.host.live_loudness();
        let lufs = |v: f32| {
            if v.is_finite() {
                format!("{v:.1} LUFS")
            } else {
                "—".to_string()
            }
        };
        let peak = if live.peak_dbfs.is_finite() {
            format!("{:+.1} dBFS", live.peak_dbfs)
        } else {
            "—".to_string()
        };
        block = block
            .child(
                text(t!("player.signalPath.liveMeter").to_string())
                    .key("signal-path-live-heading")
                    .font(type_::SECTION)
                    .color(p.text_secondary),
            )
            .child(meter_row(
                "signal-path-momentary",
                t!("player.signalPath.momentary").to_string(),
                lufs(live.momentary_lufs),
                meter_frac(live.momentary_lufs),
                p,
            ))
            .child(meter_row(
                "signal-path-shortterm",
                t!("player.signalPath.shortTerm").to_string(),
                lufs(live.short_term_lufs),
                meter_frac(live.short_term_lufs),
                p,
            ))
            .child(meter_row(
                "signal-path-livepeak",
                t!("player.signalPath.peak").to_string(),
                peak,
                meter_frac(live.peak_dbfs),
                p,
            ));
    }
    block
}

/// One labelled metric row: label left, mono value right (the value colour is the
/// caller's — DR uses the banded [`dr_color`]).
fn metric_row(
    key: &'static str,
    label: String,
    value: String,
    value_color: Color,
    p: &Palette,
) -> El {
    row()
        .key(key)
        .w_full()
        .items_center()
        .child(
            text(label)
                .font(type_::CAPTION)
                .color(p.text_secondary)
                .grow(1.0),
        )
        .child(text(value).font(type_::LABEL.mono()).color(value_color))
}

// ── Live loudness meter bars ──────────────────────────────────────────────────
// The live-meter section draws each reading (momentary / short-term LUFS, sample
// peak dBFS) as a horizontal bar on a fixed -60..0 dB scale, so loudness and peak
// are read at a glance as they move during playback, with the exact number beside
// them. The bar fills via flex-grow WEIGHTS (fill : remainder = frac : 1-frac) so it
// adapts to the panel width (`min_w` 260, variable) without a fixed track width that
// could overflow a narrow panel.

/// Bottom of the meter scale in dB (LUFS / dBFS). Quiet passages and silence sit at
/// the far left; 0 (full scale) is the far right.
const METER_FLOOR_DB: f32 = -60.0;
const METER_BAR_H: f32 = 7.0;
const METER_LABEL_W: f32 = 64.0;
const METER_VALUE_W: f32 = 74.0;

/// Map a dB value (LUFS or dBFS) to the bar's 0..1 fill fraction over the fixed
/// `METER_FLOOR_DB..0` scale. Non-finite (silence / not-yet-measured) reads empty.
fn meter_frac(db: f32) -> f32 {
    if !db.is_finite() {
        return 0.0;
    }
    ((db - METER_FLOOR_DB) / -METER_FLOOR_DB).clamp(0.0, 1.0)
}

/// The bar fill colour: the calm brand colour normally, warming to the attention
/// accent as the level approaches full scale (a hot, near-clipping reading), the
/// familiar green-to-amber level cue, not a quality judgement.
fn meter_color(frac: f32, p: &Palette) -> Color {
    if frac >= 0.9 { p.accent } else { p.primary }
}

/// One live-meter row: a fixed-width `name`, a level bar that grows to fill the row
/// and is filled to `frac`, and the exact `value` text in a fixed-width cell so the
/// bar's right edge stays stable as the digits change.
fn meter_row(key: &'static str, name: String, value: String, frac: f32, p: &Palette) -> El {
    let frac = frac.clamp(0.0, 1.0);
    let track = row()
        .key(format!("{key}-track"))
        .grow(1.0)
        .h(METER_BAR_H)
        .radius(radius::FULL)
        .fill(p.muted)
        .child(
            el().key(format!("{key}-fill"))
                .grow(frac)
                .h(METER_BAR_H)
                .radius(radius::FULL)
                .fill(meter_color(frac, p)),
        )
        .child(el().grow(1.0 - frac));
    row()
        .key(key)
        .w_full()
        .items_center()
        .gap(space::SM)
        .child(
            text(name)
                .font(type_::CAPTION)
                .color(p.text_secondary)
                .w(METER_LABEL_W),
        )
        .child(track)
        .child(
            text(value)
                .font(type_::LABEL.mono())
                .color(p.fg)
                .w(METER_VALUE_W),
        )
}

/// DR colour band (TT/Dynameter convention): dynamic (≥ 14) highlighted in the
/// brand colour, compressed (< 8) flagged with the attention accent, in-between
/// neutral.
/// Colour-band for a DR (crest-factor) value: green (primary) for a dynamic >=14, warm
/// (accent) for a compressed <8 (the loudness-war warning), neutral between. Shared with the
/// library loudness/DR column. DR is an indicator, not a precise quality score.
pub(crate) fn dr_color(dr: f32, p: &Palette) -> Color {
    if dr >= 14.0 {
        p.primary
    } else if dr < 8.0 {
        p.accent
    } else {
        p.fg
    }
}

/// The signal-path popover, mounted at the window root (via [`crate::view`]'s
/// overlay layer) and anchored above the now-playing line. `None` when closed.
pub fn overlay(s: &State, p: &Palette) -> Option<El> {
    if !s.signal_path_open {
        return None;
    }
    Some(
        col()
            .key("signal-path-panel")
            .anchored_fit("signal-path", Side::Top, Align::Start, GAP)
            .focus_group()
            .focus_return("signal-path".to_string())
            .fill(p.card)
            .radius(radius::MD)
            .border_sides([1.0, 1.0, 1.0, 1.0], p.border)
            .padding(space::MD)
            .gap(space::XXS)
            .min_w(260.0)
            .modal(true, close_signal_path)
            .child(
                text(t!("player.signalPath.title").to_string())
                    .font(type_::SECTION)
                    .color(p.text_secondary),
            )
            .child(pipeline_rows(s, p))
            .child(loudness_block(s, p)),
    )
}

#[cfg(test)]
mod tests {
    use soul_player_host::EqUnit;

    use super::*;

    fn sp(
        source: SourceFormat,
        codec: SourceCodec,
        decoded_rate: u32,
        device_rate: u32,
        resampling: bool,
    ) -> SignalPath {
        SignalPath {
            source,
            codec,
            decoded_rate,
            decoded_bits: 24,
            channels: 2,
            replay_gain_db: 0.0,
            device_rate,
            resampling,
            output_bits: 24,
            exclusive: false,
            loudness: None,
        }
    }

    #[test]
    fn meter_frac_maps_the_db_scale_and_clamps() {
        assert_eq!(meter_frac(0.0), 1.0, "full scale fills the bar");
        assert_eq!(meter_frac(METER_FLOOR_DB), 0.0, "the floor is empty");
        assert!(
            (meter_frac(-30.0) - 0.5).abs() < 1e-6,
            "-30 dB is half on a -60..0 scale"
        );
        assert_eq!(
            meter_frac(f32::NEG_INFINITY),
            0.0,
            "silence / no reading reads empty"
        );
        assert_eq!(meter_frac(6.0), 1.0, "above full scale clamps to full");
        assert_eq!(meter_frac(-120.0), 0.0, "below the floor clamps to empty");
    }

    #[test]
    fn meter_color_warms_only_when_hot() {
        let p = crate::theme::palette(true);
        assert_eq!(
            meter_color(0.5, &p),
            p.primary,
            "a normal level is the calm brand colour"
        );
        assert_eq!(
            meter_color(0.95, &p),
            p.accent,
            "a near-full-scale level warms to the accent"
        );
    }

    #[test]
    fn stage_values_format_each_stage() {
        let dsd = sp(
            SourceFormat::Dsd { rate_hz: 2_822_400 },
            SourceCodec::Dsd,
            44_100,
            48_000,
            true,
        );
        assert_eq!(source_value(&dsd), "DSD64 · 2.82 MHz");
        assert_eq!(decode_value(&dsd), "44.1 kHz · 24-bit · 2 ch");
        assert_eq!(resample_value(&dsd), "44.1 → 48 kHz");
        assert_eq!(
            output_value(&dsd),
            format!("{} · 48 kHz · 24-bit", output_api())
        );

        let flac = sp(
            SourceFormat::Pcm { bits: 16 },
            SourceCodec::Flac,
            44_100,
            44_100,
            false,
        );
        assert_eq!(source_value(&flac), "FLAC · 16-bit");
        // A rate-exact path reads as such, not "a → a".
        assert_eq!(
            resample_value(&flac),
            t!("player.signalPath.rateExact").to_string()
        );
    }

    #[test]
    fn exclusive_value_reflects_bit_perfect_vs_shared() {
        let base = sp(
            SourceFormat::Pcm { bits: 24 },
            SourceCodec::Flac,
            96_000,
            96_000,
            false,
        );
        // Shared (the default): names the backend + mode, e.g. "WASAPI shared". The API is
        // platform-specific, so assert the shape, not the hardcoded name (CI runs on
        // Linux/macOS too).
        assert_eq!(exclusive_value(&base), format!("{} shared", output_api()));
        // Exclusive: names the backend + the bit-perfect format the device receives.
        let excl = SignalPath {
            exclusive: true,
            ..base
        };
        assert_eq!(
            exclusive_value(&excl),
            format!("{} exclusive · 96 kHz · 24-bit", output_api())
        );
    }

    /// An EQ unit that actually shapes the signal (a boosted preamp is enough —
    /// independent of the band layout).
    fn affecting_eq() -> DspUnit {
        DspUnit::Eq(EqUnit {
            preamp_db: 6.0,
            enabled: true,
            ..EqUnit::default()
        })
    }

    /// The DSP/EQ stage reads as **bypassed** whenever the chain can't alter the
    /// signal: empty, every unit disabled, or every unit flat. An enabled-but-flat
    /// EQ is an honest bit-exact passthrough (`DspUnit::affects`), and a boosted
    /// unit that is disabled is still a passthrough.
    #[test]
    fn dsp_stage_is_bypassed_for_empty_disabled_or_flat_chains() {
        let bypassed = t!("player.signalPath.dspBypassed").to_string();

        assert!(!dsp_active(&[]));
        assert_eq!(dsp_value(&[]), bypassed);

        // A default EQ unit is flat → alters nothing.
        let flat = [DspUnit::Eq(EqUnit::default())];
        assert!(!dsp_active(&flat));
        assert_eq!(dsp_value(&flat), bypassed);

        // A boosted unit that is disabled is a passthrough — the boost is bypassed.
        let disabled = [DspUnit::Eq(EqUnit {
            preamp_db: 6.0,
            enabled: false,
            ..EqUnit::default()
        })];
        assert!(!dsp_active(&disabled));
        assert_eq!(dsp_value(&disabled), bypassed);
    }

    /// A unit that shapes the signal lights the stage; flat / disabled units next to
    /// it don't change that reading.
    #[test]
    fn dsp_stage_is_active_when_a_unit_shapes_the_signal() {
        let active = t!("player.signalPath.dspActive").to_string();

        let one = [affecting_eq()];
        assert!(dsp_active(&one));
        assert_eq!(dsp_value(&one), active);

        // An affecting unit beside a flat one and a disabled one still reads active.
        let mixed = [
            affecting_eq(),
            DspUnit::Eq(EqUnit::default()),
            DspUnit::Eq(EqUnit {
                preamp_db: 6.0,
                enabled: false,
                ..EqUnit::default()
            }),
        ];
        assert!(dsp_active(&mixed));
        assert_eq!(dsp_value(&mixed), active);
    }

    /// The Source stage names the real codec on an otherwise-identical PCM stream:
    /// a lossless ALAC, a FLAC, and a bare PCM/WAV read differently (the
    /// lossless-vs-lossy distinction a plain "PCM · 24-bit" can't give).
    #[test]
    fn source_value_names_the_pcm_codec() {
        let alac = sp(
            SourceFormat::Pcm { bits: 24 },
            SourceCodec::Alac,
            96_000,
            96_000,
            false,
        );
        assert_eq!(source_value(&alac), "ALAC · 24-bit");
        let aac = sp(
            SourceFormat::Pcm { bits: 16 },
            SourceCodec::Aac,
            44_100,
            44_100,
            false,
        );
        assert_eq!(source_value(&aac), "AAC · 16-bit");
        // A decoder that names no codec falls back to the generic "PCM".
        let pcm = sp(
            SourceFormat::Pcm { bits: 16 },
            SourceCodec::Pcm,
            44_100,
            44_100,
            false,
        );
        assert_eq!(source_value(&pcm), "PCM · 16-bit");
    }

    #[test]
    fn replay_gain_value_is_off_or_the_applied_gain() {
        assert_eq!(
            replay_gain_value(ReplayGainMode::Off, 2.3),
            t!("player.signalPath.off").to_string()
        );
        assert_eq!(replay_gain_value(ReplayGainMode::Track, 2.34), "+2.3 dB");
        assert_eq!(replay_gain_value(ReplayGainMode::Album, -3.0), "-3.0 dB");
    }
}
