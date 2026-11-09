//! Now-playing panel: cover art, title/artist, transport controls, and the
//! hand-built seek + volume sliders (`views::slider`). Rendered under the
//! track list for this milestone; the sidebar assembly is a later task.
#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::collections::HashMap;

use rune::tok::{motion, type_};
use rune::{Align, El, Image, ImageCache, SurfaceFit, Waker, col, el, row, text};
use soul_player_host::{Command, PlayerHandle, RepeatMode, TrackView};

use crate::State;
use crate::theme::Palette;
use crate::views::slider::slider;

/// Cover art side, in points (the brief's "~64-96px" range).
const COVER_SIZE: f32 = 72.0;
/// Seek/volume slider widths. The panel isn't resizable in this milestone's
/// fixed-size window, so these are plain constants rather than a layout
/// query (`Cx::layout_size`, cheat-sheet §7, is the path once the sidebar is
/// laid out and can report its own width back into the next frame).
const SEEK_WIDTH: f32 = 560.0;
const VOLUME_WIDTH: f32 = 140.0;

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

fn cover_el(s: &State, id: Option<u32>, p: &Palette, scale: f32) -> El {
    let placeholder = el()
        .key("cover")
        .size(COVER_SIZE, COVER_SIZE)
        .radius(10.0)
        .fill(p.muted);
    let Some(id) = id else { return placeholder };
    match s.art.image_for(id, &s.host, scale) {
        // Ready and error-free: show it. Pending or failed covers keep the
        // placeholder rather than a blank or broken texture.
        Some(image) if image.is_ready() && image.error().is_none() => {
            placeholder.pixels_fit(image.pixels(), SurfaceFit::Fit)
        }
        _ => placeholder,
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
                .child(text(title).font(type_::TITLE).color(p.fg))
                .child(text(artist).font(type_::BODY).color(p.text_secondary)),
        )
}

fn seek_row(s: &State, p: &Palette) -> El {
    let duration = s.duration_secs;
    let value01 = if duration > 0.0 {
        (s.position_secs / duration).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    row()
        .key("seek")
        .gap(10.0)
        .center_y()
        .child(
            text(fmt_dur(s.position_secs))
                .font(type_::MONO_CAPTION.mono())
                .color(p.text_secondary)
                .w(42.0),
        )
        .child(slider(
            "seek",
            value01,
            SEEK_WIDTH,
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
                .w(42.0)
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
    el()
        .key(key)
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
    el()
        .key("play-pause")
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

fn shuffle_button(s: &State, p: &Palette) -> El {
    el()
        .key("shuffle")
        .role("switch")
        .label("Shuffle")
        .checked(s.shuffle)
        .focusable(true)
        .size(64.0, 32.0)
        .center()
        .radius(16.0)
        .hover(p.muted)
        .press(p.border)
        .on_press(|st: &mut State| st.host.send(Command::SetShuffle(!st.shuffle)))
        .child(
            text("Shuffle")
                .font(type_::LABEL)
                .color(if s.shuffle { p.accent } else { p.text_secondary }),
        )
}

fn repeat_button(s: &State, p: &Palette) -> El {
    let (label, on) = match s.repeat {
        RepeatMode::Off => ("Repeat", false),
        RepeatMode::All => ("Repeat: All", true),
        RepeatMode::One => ("Repeat: One", true),
    };
    el()
        .key("repeat")
        .role("button")
        .label("Repeat")
        .focusable(true)
        .size(88.0, 32.0)
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
            text(label)
                .font(type_::LABEL)
                .color(if on { p.accent } else { p.text_secondary }),
        )
}

fn transport_row(s: &State, p: &Palette) -> El {
    row()
        .key("transport")
        .gap(18.0)
        .center()
        .child(shuffle_button(s, p))
        .child(transport_button("prev", "Previous", "◀◀", p, |st: &mut State| {
            st.host.send(Command::Prev)
        }))
        .child(play_pause_button(s, p))
        .child(transport_button("next", "Next", "▶▶", p, |st: &mut State| {
            st.host.send(Command::Next)
        }))
        .child(repeat_button(s, p))
}

fn volume_row(s: &State, p: &Palette) -> El {
    row()
        .key("volume")
        .gap(10.0)
        .center_y()
        .child(text("Vol").font(type_::LABEL).color(p.text_secondary).w(28.0))
        .child(slider(
            "volume",
            s.volume,
            VOLUME_WIDTH,
            true,
            p,
            |st: &mut State, v: f32| st.host.send(Command::SetVolume(v)),
        ))
        .child(
            text(format!("{:.0}%", s.volume * 100.0))
                .font(type_::MONO_CAPTION.mono())
                .color(p.text_secondary)
                .w(40.0)
                .align(Align::End),
        )
}

/// The now-playing panel: cover art + title/artist, the seek bar with
/// current/duration times, a transport row (shuffle, prev, play/pause,
/// next, repeat), and the volume bar. `scale` is the window's device-pixel
/// scale (`Cx::env().scale`), threaded down to `ImageCache::load_mime`.
pub fn player_panel(s: &State, p: &Palette, scale: f32) -> El {
    let now = s
        .now_playing_id
        .and_then(|id| s.library.iter().find(|t| t.id == id));
    col()
        .key("player")
        .w_full()
        .fill(p.card)
        .padding(20.0)
        .gap(14.0)
        .child(now_playing_row(s, now, p, scale))
        .child(seek_row(s, p))
        .child(transport_row(s, p))
        .child(volume_row(s, p))
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
    /// fixture (used by the Tauri head's `arted_flac_embedded_art_resolves`
    /// test) carries embedded art that never starts a Null-sink playback
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
