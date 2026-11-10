//! Now-playing panel: cover art, title/artist, transport controls, and the
//! hand-built seek + volume sliders (`views::slider`). Pinned at the bottom
//! of the fixed-width `views::sidebar`, so every row below is sized to fit
//! that width exactly rather than the full-window numbers Tasks 5-6 built
//! it at.
#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::collections::HashMap;

use rune::tok::{motion, type_};
use rune::{Align, Cx, El, Image, ImageCache, SurfaceFit, Waker, col, el, row, text};
use soul_player_host::{Command, PlayerHandle, RepeatMode, TrackView};

use crate::State;
use crate::theme::Palette;
use crate::views::slider::slider;

/// Cover art side, in points (the brief's "~64-96px" range).
const COVER_SIZE: f32 = 72.0;
/// The panel's own padding on every side (`player_panel`'s `.padding(..)`).
const PANEL_PADDING: f32 = 20.0;
/// Fixed label widths flanking the seek bar (current/duration, e.g.
/// `"12:34"`) and the volume bar (`"Vol"` / `"100%"`); the sliders
/// (`.grow(1.0)`, see `views::slider`) take whatever's left of the row
/// after these and the row gaps.
const TIME_LABEL_W: f32 = 36.0;
const VOL_LABEL_W: f32 = 24.0;
const VOL_PCT_W: f32 = 38.0;
/// Gap between a row's fixed labels and its slider, and between the
/// transport row's buttons — tightened from Task 5-6's full-width numbers
/// so shuffle/prev/play/next/repeat fit the sidebar's ~248pt content width.
const ROW_GAP: f32 = 8.0;

/// Cover-art decode cache (cheat-sheet §6a). Rune's pre-release has no
/// "load this track's art" helper, so this wraps `rune::ImageCache` +
/// `PlayerHandle::art` with a per-id memo — each cover decodes once, not
/// once per ~8 Hz `Tick` re-render. `cache` is `None` until `AppSpec::setup`
/// (see `main.rs`) wires the app's `Waker` into it; before that, and
/// whenever a track has no art or is still decoding, callers fall back to a
/// themed placeholder box (`cover_el` below) rather than blocking the panel.
pub struct ArtCache {
    cache: Option<ImageCache>,
    images: RefCell<HashMap<u32, Image>>,
}

impl ArtCache {
    pub fn new() -> Self {
        Self {
            cache: None,
            images: RefCell::new(HashMap::new()),
        }
    }

    /// Called once from `AppSpec::setup` with the app's `Waker`: the decode
    /// workers call it when a cover finishes, so the next frame just picks
    /// up the now-ready pixels with no further plumbing (`examples/live-values`'s
    /// pattern, cheat-sheet §5b).
    pub fn wire(&mut self, waker: Waker) {
        self.cache = Some(ImageCache::new(waker));
    }

    fn image_for(&self, id: u32, host: &PlayerHandle, scale: f32) -> Option<Image> {
        if let Some(image) = self.images.borrow().get(&id) {
            return Some(image.clone());
        }
        let cache = self.cache.as_ref()?;
        let cover = host.art(id)?;
        let image = cache.load_mime(cover.bytes, &cover.mime, COVER_SIZE, scale);
        self.images.borrow_mut().insert(id, image.clone());
        Some(image)
    }
}

impl Default for ArtCache {
    fn default() -> Self {
        Self::new()
    }
}

fn fmt_dur(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// The muted cover box with a centered music-note glyph, for no now-playing
/// track, a track with no embedded art, or art that is still decoding or
/// failed to — a placeholder that reads as "no art" rather than a blank box.
fn cover_placeholder(p: &Palette) -> El {
    el().key("cover")
        .size(COVER_SIZE, COVER_SIZE)
        .radius(10.0)
        .fill(p.muted)
        .center()
        .child(text("♪").font(type_::TITLE).color(p.text_secondary))
}

fn cover_el(s: &State, id: Option<u32>, p: &Palette, scale: f32) -> El {
    let Some(id) = id else {
        return cover_placeholder(p);
    };
    match s.art.image_for(id, &s.host, scale) {
        // Ready and error-free: show it. Pending or failed covers keep the
        // placeholder rather than a blank or broken texture.
        Some(image) if image.is_ready() && image.error().is_none() => el()
            .key("cover")
            .size(COVER_SIZE, COVER_SIZE)
            .radius(10.0)
            .fill(p.muted)
            .pixels_fit(image.pixels(), SurfaceFit::Fit),
        _ => cover_placeholder(p),
    }
}

fn now_playing_row(s: &State, now: Option<&TrackView>, p: &Palette, scale: f32) -> El {
    let title = now
        .and_then(|t| t.title.clone())
        .unwrap_or_else(|| "Nothing playing".to_string());
    let artist = now.and_then(|t| t.artist.clone()).unwrap_or_default();
    row()
        .key("now-playing")
        .gap(14.0)
        .child(cover_el(s, now.map(|t| t.id), p, scale))
        .child(
            col()
                .key("now-playing-text")
                .gap(4.0)
                .center_y()
                .grow(1.0)
                .min_w(0.0)
                .child(text(title).font(type_::TITLE).color(p.fg).truncate())
                .child(
                    text(artist)
                        .font(type_::BODY)
                        .color(p.text_secondary)
                        .truncate(),
                ),
        )
}

/// The seek slider now fills whatever's left of the row after the two fixed
/// time labels (`.grow(1.0)`, see `views::slider`), rather than a width this
/// row computes by hand — the row's own key is `"seek-row"`, distinct from
/// the slider's `"seek"` key, so `Cx::layout_size("seek")` inside `slider`
/// unambiguously reads the slider's own laid-out width, not this row's.
fn seek_row(s: &State, p: &Palette, cx: &Cx<State>) -> El {
    let duration = s.duration_secs;
    let value01 = if duration > 0.0 {
        (s.position_secs / duration).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    row()
        .key("seek-row")
        .gap(ROW_GAP)
        .center_y()
        .child(
            text(fmt_dur(s.position_secs))
                .font(type_::MONO_CAPTION.mono())
                .color(p.text_secondary)
                .w(TIME_LABEL_W),
        )
        .child(slider(
            "seek",
            value01,
            cx,
            false,
            p,
            move |st: &mut State, v: f32| {
                st.host.send(Command::Seek {
                    secs: f64::from(v) * duration,
                });
            },
        ))
        .child(
            text(fmt_dur(s.duration_secs))
                .font(type_::MONO_CAPTION.mono())
                .color(p.text_secondary)
                .w(TIME_LABEL_W)
                .align(Align::End),
        )
}

fn transport_button(
    key: &str,
    label: &str,
    glyph: &str,
    p: &Palette,
    on_press: impl Fn(&mut State) + 'static,
) -> El {
    el().key(key)
        .role("button")
        .label(label)
        .focusable(true)
        .size(32.0, 32.0)
        .center()
        .radius(16.0)
        .hover(p.muted)
        .press(p.border)
        .on_press(on_press)
        .child(text(glyph).font(type_::TITLE).color(p.fg))
}

fn play_pause_button(s: &State, p: &Palette) -> El {
    let glyph = if s.playing { "⏸" } else { "▶" };
    el().key("play-pause")
        .role("button")
        .label(if s.playing { "Pause" } else { "Play" })
        .focusable(true)
        .size(48.0, 48.0)
        .center()
        .radius(24.0)
        .fill(p.primary)
        .motion(motion::SNAPPY)
        .on_press(|st: &mut State| {
            st.host.send(if st.playing {
                Command::Pause
            } else {
                Command::Play
            });
        })
        .child(text(glyph).font(type_::TITLE).color(p.bg))
}

/// Compact icon button (32x32, matching `transport_button`'s size): the old
/// app's shuffle/repeat used text labels ("Shuffle", "Repeat: All") that fit
/// a full-width player but overflow the sidebar's ~248pt content width
/// across five transport buttons, so these render a glyph instead. The
/// descriptive `label`/`role` stay for accessibility regardless.
fn shuffle_button(s: &State, p: &Palette) -> El {
    el().key("shuffle")
        .role("switch")
        .label("Shuffle")
        .checked(s.shuffle)
        .focusable(true)
        .size(32.0, 32.0)
        .center()
        .radius(16.0)
        .hover(p.muted)
        .press(p.border)
        .on_press(|st: &mut State| st.host.send(Command::SetShuffle(!st.shuffle)))
        .child(text("⤭").font(type_::TITLE).color(if s.shuffle {
            p.accent
        } else {
            p.text_secondary
        }))
}

fn repeat_button(s: &State, p: &Palette) -> El {
    let (glyph, label, on) = match s.repeat {
        RepeatMode::Off => ("⟳", "Repeat", false),
        RepeatMode::All => ("⟳", "Repeat: All", true),
        RepeatMode::One => ("⟳¹", "Repeat: One", true),
    };
    el().key("repeat")
        .role("button")
        .label(label)
        .focusable(true)
        .size(32.0, 32.0)
        .center()
        .radius(16.0)
        .hover(p.muted)
        .press(p.border)
        .on_press(|st: &mut State| {
            let next = match st.repeat {
                RepeatMode::Off => RepeatMode::All,
                RepeatMode::All => RepeatMode::One,
                RepeatMode::One => RepeatMode::Off,
            };
            st.host.send(Command::SetRepeat(next));
        })
        .child(
            text(glyph)
                .font(type_::TITLE)
                .color(if on { p.accent } else { p.text_secondary }),
        )
}

/// Shuffle/prev/play/next/repeat in one row. At 32+32+48+32+32 = 176pt of
/// buttons plus 4 gaps, this must stay under the sidebar's ~248pt content
/// width (`ROW_GAP` keeps the 4 gaps to 32pt, for 208pt total).
fn transport_row(s: &State, p: &Palette) -> El {
    row()
        .key("transport")
        .gap(ROW_GAP)
        .center()
        .child(shuffle_button(s, p))
        .child(transport_button(
            "prev",
            "Previous",
            "◀◀",
            p,
            |st: &mut State| st.host.send(Command::Prev),
        ))
        .child(play_pause_button(s, p))
        .child(transport_button(
            "next",
            "Next",
            "▶▶",
            p,
            |st: &mut State| st.host.send(Command::Next),
        ))
        .child(repeat_button(s, p))
}

/// The volume slider likewise fills whatever's left of the row; see
/// `seek_row`'s doc comment for why the row's own key (`"volume-row"`)
/// stays distinct from the slider's (`"volume"`).
fn volume_row(s: &State, p: &Palette, cx: &Cx<State>) -> El {
    row()
        .key("volume-row")
        .gap(ROW_GAP)
        .center_y()
        .child(
            text("Vol")
                .font(type_::LABEL)
                .color(p.text_secondary)
                .w(VOL_LABEL_W),
        )
        .child(slider(
            "volume",
            s.volume,
            cx,
            true,
            p,
            |st: &mut State, v: f32| st.host.send(Command::SetVolume(v)),
        ))
        .child(
            text(format!("{:.0}%", s.volume * 100.0))
                .font(type_::MONO_CAPTION.mono())
                .color(p.text_secondary)
                .w(VOL_PCT_W)
                .align(Align::End),
        )
}

/// The now-playing panel: cover art + title/artist, the seek bar with
/// current/duration times, a transport row (shuffle, prev, play/pause,
/// next, repeat), and the volume bar. `scale` is the window's device-pixel
/// scale (`Cx::env().scale`), threaded down to `ImageCache::load_mime`.
/// `cx` is threaded down to the seek/volume sliders (`views::slider`) so
/// they can read their own real laid-out width back (`Cx::layout_size`)
/// instead of this panel computing it by hand from a fixed outer width.
pub fn player_panel(s: &State, p: &Palette, cx: &Cx<State>, scale: f32) -> El {
    let now = s.now_playing_id.and_then(|id| s.track(id));
    col()
        .key("player")
        .w_full()
        .fill(p.card)
        .padding(PANEL_PADDING)
        .gap(14.0)
        .child(now_playing_row(s, now, p, scale))
        .child(seek_row(s, p, cx))
        .child(transport_row(s, p))
        .child(volume_row(s, p, cx))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use soul_player_host::PlayerHandle;

    use super::ArtCache;

    fn testdata_dir() -> PathBuf {
        // app/rune -> repo-root/testdata
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata")
    }

    /// `ArtCache::image_for` resolves and decodes real cover-art bytes end to
    /// end (`PlayerHandle::art` -> `ImageCache::load_mime`), independent of
    /// whether the track is actually playing: `arted.flac`'s metadata-only
    /// fixture carries embedded art that never starts a Null-sink playback
    /// within a short virtual-clock window, which is why the player-panel
    /// snapshots fall back to the placeholder for it — this test proves the
    /// cache itself, not the unrelated playback race.
    #[test]
    fn image_for_decodes_a_known_embedded_cover() {
        let host = PlayerHandle::offline();
        let views = host.open_folder(&testdata_dir()).expect("scan testdata/");
        let id = views
            .iter()
            .find(|v| v.title.as_deref() == Some("Arted Track"))
            .expect("arted.flac view")
            .id;

        let mut art = ArtCache::new();
        art.wire(Arc::new(|| {}));
        let image = art
            .image_for(id, &host, 2.0)
            .expect("known embedded art should resolve");
        image.wait().expect("embedded cover should decode");
        assert!(image.is_ready());
        assert!(image.error().is_none());

        // Cached: a second call returns the same decoded handle, not a fresh
        // decode (the per-id memo `image_for` exists for).
        let again = art.image_for(id, &host, 2.0).expect("memoized lookup");
        assert_eq!(again.id(), image.id());
    }
}
