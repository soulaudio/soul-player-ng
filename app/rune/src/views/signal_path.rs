//! The expanded **signal-path** panel: the current track's real decode→output
//! chain shown stage by stage — Source → Decode → ReplayGain → Resample →
//! Output — opened by pressing the now-playing signal-path line. ReplayGain
//! toggles inline (the one stage that is a real on/off today); exclusive /
//! bit-perfect output and a DSP/EQ chain are surfaced as **"soon"** so the whole
//! pipeline is visible even before those stages exist. Rendered as a themed
//! overlay anchored to the `"signal-path"` line and `modal` (dismiss on an
//! outside press / Escape), the same mechanism as [`crate::views::dropdown`].
#![forbid(unsafe_code)]

use rune::{Align, Cursor, El, Side, col, row, text};
use soul_player_host::{ReplayGainMode, SignalPath, SourceFormat};

use crate::theme::Palette;
use crate::ui::{radius, space, type_};
use crate::{State, close_signal_path, set_replaygain_mode};

/// Gap between the anchor line and the panel.
const GAP: f32 = 6.0;
/// Fixed width of the stage-name column, so the values line up.
const LABEL_W: f32 = 76.0;

/// kHz label: whole kHz stays integer (48, 96), fractional keeps one place.
fn khz(hz: u32) -> String {
    if hz % 1000 == 0 {
        format!("{}", hz / 1000)
    } else {
        format!("{:.1}", f64::from(hz) / 1000.0)
    }
}

/// Source stage: native DSD (family + 1-bit MHz rate) or PCM bit depth.
fn source_value(sp: &SignalPath) -> String {
    match sp.source {
        SourceFormat::Dsd { rate_hz } => format!(
            "DSD{} · {:.2} MHz",
            (rate_hz / 44_100 / 64).max(1) * 64,
            f64::from(rate_hz) / 1e6
        ),
        SourceFormat::Pcm { bits } => format!("PCM · {bits}-bit"),
    }
}

/// Decode stage: the PCM the decoder hands the engine.
fn decode_value(sp: &SignalPath) -> String {
    format!(
        "{} kHz · {}-bit · {} ch",
        khz(sp.decoded_rate),
        sp.decoded_bits,
        sp.channels
    )
}

/// Resample stage: `a → b kHz` when resampling, else a rate-exact note.
fn resample_value(sp: &SignalPath) -> String {
    if sp.resampling {
        format!("{} → {} kHz", khz(sp.decoded_rate), khz(sp.device_rate))
    } else {
        t!("player.signalPath.rateExact").to_string()
    }
}

/// Output stage: device rate + bit depth (always shared for now; exclusive is a
/// "soon" toggle on this row).
fn output_value(sp: &SignalPath) -> String {
    format!("{} kHz · {}-bit", khz(sp.device_rate), sp.output_bits)
}

/// ReplayGain stage value: the applied gain, or "off".
fn replay_gain_value(mode: ReplayGainMode, gain_db: f32) -> String {
    if mode == ReplayGainMode::Off {
        t!("player.signalPath.off").to_string()
    } else {
        format!("{gain_db:+.1} dB")
    }
}

/// A non-interactive "soon" chip marking a stage/toggle that isn't built yet.
fn soon_chip(p: &Palette) -> El {
    text(t!("player.signalPath.soon").to_string())
        .font(type_::LABEL)
        .color(p.text_secondary)
        .px(space::SM)
        .py(space::XXS)
        .radius(radius::SM)
        .fill(p.muted)
}

/// One stage row: a fixed-width name, the value, and an optional trailing control
/// (the ReplayGain toggle, or a "soon" chip). `key` is stable for tests.
fn stage_row(key: &str, name: String, value: String, trailing: Option<El>, p: &Palette) -> El {
    let mut r = row()
        .key(key.to_string())
        .w_full()
        .center_y()
        .gap(space::SM)
        .py(space::XXS)
        .child(
            text(name)
                .font(type_::LABEL)
                .color(p.text_secondary)
                .w(LABEL_W),
        )
        .child(
            text(value)
                .font(type_::BODY)
                .color(p.fg)
                .grow(1.0)
                .truncate(),
        );
    if let Some(t) = trailing {
        r = r.child(t);
    }
    r
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

/// The signal-path panel, mounted at the window root (via [`crate::view`]'s
/// overlay layer) and anchored above the now-playing line. `None` when the panel
/// is closed or nothing is playing.
pub fn overlay(s: &State, p: &Palette) -> Option<El> {
    if !s.signal_path_open {
        return None;
    }
    let sp = s.signal_path?;
    let panel = col()
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
        .child(stage_row(
            "sp-row-source",
            t!("player.signalPath.source").to_string(),
            source_value(&sp),
            None,
            p,
        ))
        .child(stage_row(
            "sp-row-decode",
            t!("player.signalPath.decode").to_string(),
            decode_value(&sp),
            None,
            p,
        ))
        .child(stage_row(
            "sp-row-replaygain",
            t!("player.signalPath.replayGain").to_string(),
            replay_gain_value(s.replaygain_mode, sp.replay_gain_db),
            Some(rg_toggle(s.replaygain_mode, p)),
            p,
        ))
        .child(stage_row(
            "sp-row-resample",
            t!("player.signalPath.resample").to_string(),
            resample_value(&sp),
            None,
            p,
        ))
        .child(stage_row(
            "sp-row-output",
            t!("player.signalPath.output").to_string(),
            output_value(&sp),
            Some(soon_chip(p)),
            p,
        ))
        // Not-yet-built stages, surfaced so the whole pipeline is visible.
        .child(stage_row(
            "sp-row-exclusive",
            t!("player.signalPath.exclusive").to_string(),
            String::new(),
            Some(soon_chip(p)),
            p,
        ))
        .child(stage_row(
            "sp-row-dsp",
            t!("player.signalPath.dsp").to_string(),
            String::new(),
            Some(soon_chip(p)),
            p,
        ));
    Some(panel)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp(
        source: SourceFormat,
        decoded_rate: u32,
        device_rate: u32,
        resampling: bool,
    ) -> SignalPath {
        SignalPath {
            source,
            decoded_rate,
            decoded_bits: 24,
            channels: 2,
            replay_gain_db: 0.0,
            device_rate,
            resampling,
            output_bits: 24,
        }
    }

    #[test]
    fn stage_values_format_each_stage() {
        let dsd = sp(
            SourceFormat::Dsd { rate_hz: 2_822_400 },
            44_100,
            48_000,
            true,
        );
        assert_eq!(source_value(&dsd), "DSD64 · 2.82 MHz");
        assert_eq!(decode_value(&dsd), "44.1 kHz · 24-bit · 2 ch");
        assert_eq!(resample_value(&dsd), "44.1 → 48 kHz");
        assert_eq!(output_value(&dsd), "48 kHz · 24-bit");

        let pcm = sp(SourceFormat::Pcm { bits: 16 }, 44_100, 44_100, false);
        assert_eq!(source_value(&pcm), "PCM · 16-bit");
        // A rate-exact path reads as such, not "a → a".
        assert_eq!(
            resample_value(&pcm),
            t!("player.signalPath.rateExact").to_string()
        );
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
