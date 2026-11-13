//! Now-playing panel: cover art, title/artist, transport controls, and the
//! hand-built seek + volume sliders (`views::slider`). Pinned at the bottom
//! of the fixed-width `views::sidebar`, so every row below is sized to fit
//! that width exactly rather than the full-window numbers Tasks 5-6 built
//! it at.
#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::collections::HashMap;

use rune::tok::motion;
use rune::{
    Align, Cursor, Cx, El, Icon, Image, ImageCache, SurfaceFit, Waker, col, el, icon, row, text,
};
use soul_player_host::{Command, PlayerHandle, RepeatMode, TrackView};

use crate::State;
use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::views::icons;
use crate::views::slider::slider;

/// Now-playing cover side, in points — the old app's `w-10` (40 px).
const COVER_SIZE: f32 = 40.0;
/// Queue-row cover side — the old app's small `TrackItem` thumbnail (`w-8`, 32 px).
pub const QUEUE_COVER: f32 = 32.0;
/// Covers decode at this size (×the device scale) — generous so the 160 pt
/// album-grid cards stay crisp, with the 40 pt now-playing and 36 pt queue
/// covers downscaling from the same cached decode.
const DECODE_PX: f32 = 160.0;
/// NowPlayingPanel padding — the old app's `p-4` (16 px).
const NP_PAD: f32 = space::LG;
/// The controls block below the now-playing row (old app's `px-4 pt-2 pb-4`).
const CTRL_PAD_X: f32 = space::LG;
const CTRL_PAD_T: f32 = space::SM;
const CTRL_PAD_B: f32 = space::LG;
/// Gap between the controls rows (old app's `space-y-3`) and eyebrow→row (`mb-3`).
const CTRL_GAP: f32 = space::MD;
/// Fixed label widths flanking the seek bar (current/duration, e.g.
/// `"12:34"`) and the volume bar (`"Vol"` / `"100%"`); the sliders
/// (`.grow(1.0)`, see `views::slider`) take whatever's left of the row
/// after these and the row gaps.
const TIME_LABEL_W: f32 = 36.0;
const VOL_PCT_W: f32 = 38.0;
/// Gap between a row's fixed labels and its slider, and between the
/// transport row's buttons — tightened from Task 5-6's full-width numbers
/// so shuffle/prev/play/next/repeat fit the sidebar's ~248pt content width.
const ROW_GAP: f32 = space::SM;
/// Transport-button side (circular, so each button's radius is half this); the
/// centre play/pause button is a touch larger.
const BTN_SIZE: f32 = 32.0;
const PLAY_SIZE: f32 = 40.0;

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
        let image = cache.load_mime(cover.bytes, &cover.mime, DECODE_PX, scale);
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

/// The muted cover box at `size` pt with a centered music-note glyph, for no
/// now-playing track, a track with no embedded art, or art that is still
/// decoding or failed — a placeholder that reads as "no art", not a blank box.
fn cover_placeholder(size: f32, corner: f32, p: &Palette) -> El {
    el().key("cover")
        .size(size, size)
        .radius(corner)
        .fill(p.muted)
        .center()
        .child(icon(&icons::MUSIC, (size * 0.5).round()).color(p.text_secondary))
}

/// A rounded cover box at `size` pt: the track's decoded art if ready, else the
/// placeholder. Shared by the now-playing panel (40 pt) and the queue rows
/// (36 pt), matching the old app's `w-10` cover and `TrackItem` thumbnail.
pub fn cover(s: &State, id: Option<u32>, size: f32, corner: f32, p: &Palette, scale: f32) -> El {
    let Some(id) = id else {
        return cover_placeholder(size, corner, p);
    };
    match s.art.image_for(id, &s.host, scale) {
        // Ready and error-free: show it. Pending or failed covers keep the
        // placeholder rather than a blank or broken texture.
        Some(image) if image.is_ready() && image.error().is_none() => el()
            .key("cover")
            .size(size, size)
            .radius(corner)
            .fill(p.muted)
            .pixels_fit(image.pixels(), SurfaceFit::Fit),
        _ => cover_placeholder(size, corner, p),
    }
}

fn now_playing_row(s: &State, now: Option<&TrackView>, p: &Palette, scale: f32) -> El {
    let Some(track) = now else {
        // Empty state, as the old app's NowPlayingPanel: a larger muted cover and
        // "nothing playing", no title/artist row and no heart.
        return row()
            .key("now-playing")
            .gap(space::MD)
            .center_y()
            .child(cover(s, None, 48.0, radius::MD, p, scale))
            .child(
                text(t!("player.nothingPlaying").to_string())
                    .font(type_::BODY)
                    .color(p.text_secondary),
            );
    };
    let title = track
        .title
        .clone()
        .unwrap_or_else(|| t!("library.unknown").to_string());
    let artist = track.artist.clone().unwrap_or_default();
    let album = track.album.clone();
    // Title/artist open the track's album detail page (the old app's TrackInfo
    // navigates to the album on click) — only when the track has an album.
    let mut text_col = col()
        .key("now-playing-text")
        .gap(space::XXS)
        .align(Align::Start)
        .grow(1.0)
        .min_w(0.0)
        .child(text(title).font(type_::TITLE).color(p.fg).truncate())
        .child(
            text(artist)
                .font(type_::CAPTION)
                .color(p.text_secondary)
                .truncate(),
        );
    if album.is_some() {
        text_col = text_col
            .cursor(Cursor::Pointer)
            .on_press(move |st: &mut State| {
                if let Some(a) = &album {
                    st.route = crate::Route::Album;
                    st.open_album = Some(a.clone());
                    // On mobile this panel lives on the menu screen; opening the
                    // album flips to the content pane.
                    st.mobile_show_content = true;
                }
            });
    }
    row()
        .key("now-playing")
        .gap(space::COZY)
        .center_y()
        .child(cover(s, Some(track.id), COVER_SIZE, radius::MD, p, scale))
        .child(text_col)
        // A muted heart on the right (old app's `Heart` button); decorative
        // here — favourites aren't a feature of this focused build.
        .child(icon(&icons::HEART, 16.0).color(p.text_secondary))
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
                .font(type_::LABEL.mono())
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
                .font(type_::LABEL.mono())
                .color(p.text_secondary)
                .w(TIME_LABEL_W)
                .align(Align::End),
        )
}

fn transport_button(
    key: &str,
    label: String,
    ic: &'static Icon,
    enabled: bool,
    p: &Palette,
    on_press: impl Fn(&mut State) + 'static,
) -> El {
    let base = el()
        .key(key)
        .role("button")
        .label(label)
        .focusable(enabled)
        .size(BTN_SIZE, BTN_SIZE)
        .center()
        .radius(BTN_SIZE * 0.5)
        .child(icon(ic, 16.0).color(p.text_secondary));
    if enabled {
        base.cursor(Cursor::Pointer)
            .hover(p.fg.alpha(fx::HOVER))
            .press(p.fg.alpha(fx::PRESS))
            .on_press(on_press)
    } else {
        // Disabled with no current track (old app's `disabled:opacity-0.5
        // cursor-not-allowed`): dimmed, no cursor/hover/press, inert.
        base.opacity(fx::DISABLED)
    }
}

fn play_pause_button(s: &State, p: &Palette) -> El {
    let enabled = s.now_playing_id.is_some();
    let ic: &'static Icon = if s.playing {
        &icons::PAUSE
    } else {
        &icons::PLAY
    };
    let base = el()
        .key("play-pause")
        .role("button")
        .label(if s.playing {
            t!("player.pause").to_string()
        } else {
            t!("player.play").to_string()
        })
        .focusable(enabled)
        .size(PLAY_SIZE, PLAY_SIZE)
        .center()
        .radius(PLAY_SIZE * 0.5)
        .fill(p.primary)
        .motion(motion::SNAPPY)
        .child(icon(ic, 18.0).color(p.bg));
    if enabled {
        base.cursor(Cursor::Pointer)
            .hover(p.bg.alpha(fx::HOVER_FILL))
            .press(p.bg.alpha(fx::PRESS_FILL))
            .on_press(|st: &mut State| {
                st.host.send(if st.playing {
                    Command::Pause
                } else {
                    Command::Play
                });
            })
    } else {
        base.opacity(fx::DISABLED)
    }
}

/// Compact icon button (32x32, matching `transport_button`'s size): the old
/// app's shuffle/repeat used text labels ("Shuffle", "Repeat: All") that fit
/// a full-width player but overflow the sidebar's ~248pt content width
/// across five transport buttons, so these render a glyph instead. The
/// descriptive `label`/`role` stay for accessibility regardless.
fn shuffle_button(s: &State, p: &Palette) -> El {
    let enabled = s.now_playing_id.is_some();
    let base = el()
        .key("shuffle")
        .role("switch")
        .label(t!("player.shuffle").to_string())
        .checked(s.shuffle)
        .focusable(enabled)
        .size(BTN_SIZE, BTN_SIZE)
        .center()
        .radius(BTN_SIZE * 0.5)
        .child(icon(&icons::SHUFFLE, 14.0).color(if s.shuffle {
            p.accent
        } else {
            p.text_secondary
        }));
    if enabled {
        base.cursor(Cursor::Pointer)
            .hover(p.fg.alpha(fx::HOVER))
            .press(p.fg.alpha(fx::PRESS))
            .on_press(|st: &mut State| st.host.send(Command::SetShuffle(!st.shuffle)))
    } else {
        base.opacity(fx::DISABLED)
    }
}

fn repeat_button(s: &State, p: &Palette) -> El {
    let (ic, label, on): (&'static Icon, String, bool) = match s.repeat {
        RepeatMode::Off => (&icons::REPEAT, t!("player.repeat").to_string(), false),
        RepeatMode::All => (&icons::REPEAT, t!("player.repeatAll").to_string(), true),
        RepeatMode::One => (&icons::REPEAT_1, t!("player.repeatOne").to_string(), true),
    };
    let enabled = s.now_playing_id.is_some();
    let base = el()
        .key("repeat")
        .role("button")
        .label(label)
        .focusable(enabled)
        .size(BTN_SIZE, BTN_SIZE)
        .center()
        .radius(BTN_SIZE * 0.5)
        .child(icon(ic, 14.0).color(if on { p.accent } else { p.text_secondary }));
    if enabled {
        base.cursor(Cursor::Pointer)
            .hover(p.fg.alpha(fx::HOVER))
            .press(p.fg.alpha(fx::PRESS))
            .on_press(|st: &mut State| {
                let next = match st.repeat {
                    RepeatMode::Off => RepeatMode::All,
                    RepeatMode::All => RepeatMode::One,
                    RepeatMode::One => RepeatMode::Off,
                };
                st.host.send(Command::SetRepeat(next));
            })
    } else {
        base.opacity(fx::DISABLED)
    }
}

/// Shuffle/prev/play/next/repeat in one row. At 32+32+48+32+32 = 176pt of
/// buttons plus 4 gaps, this must stay under the sidebar's ~248pt content
/// width (`ROW_GAP` keeps the 4 gaps to 32pt, for 208pt total).
fn transport_row(s: &State, p: &Palette) -> El {
    // With no current track every control is disabled, as the old app's
    // PlaybackControls (`disabled={!hasCurrentTrack}` on all five).
    let enabled = s.now_playing_id.is_some();
    row()
        .key("transport")
        .gap(ROW_GAP)
        .center()
        .child(shuffle_button(s, p))
        .child(transport_button(
            "prev",
            t!("player.previous").to_string(),
            &icons::SKIP_BACK,
            enabled,
            p,
            |st: &mut State| st.host.send(Command::Prev),
        ))
        .child(play_pause_button(s, p))
        .child(transport_button(
            "next",
            t!("player.next").to_string(),
            &icons::SKIP_FORWARD,
            enabled,
            p,
            |st: &mut State| st.host.send(Command::Next),
        ))
        .child(repeat_button(s, p))
}

/// The volume slider likewise fills whatever's left of the row; see
/// `seek_row`'s doc comment for why the row's own key (`"volume-row"`)
/// stays distinct from the slider's (`"volume"`).
fn volume_row(s: &State, p: &Palette, cx: &Cx<State>) -> El {
    let muted = s.muted;
    // While muted the slider and % read 0 (the old app's `displayVolume`); the
    // real level is preserved in the backend and restored on un-mute.
    let display_vol = if muted { 0.0 } else { s.volume };
    row()
        .key("volume-row")
        .gap(ROW_GAP)
        .center_y()
        .child(
            // The speaker icon is a mute toggle (the old app's VolumeControl); the
            // backend owns the mute state (and the pre-mute level), so the UI just
            // sends `ToggleMute` and reflects `Tick.muted`.
            el().key("volume-mute")
                .role("button")
                .label(
                    if muted {
                        t!("player.unmute")
                    } else {
                        t!("player.mute")
                    }
                    .to_string(),
                )
                .focusable(true)
                .padding(space::XXS)
                .radius(radius::SM)
                .cursor(Cursor::Pointer)
                .hover(p.fg.alpha(fx::HOVER))
                .press(p.fg.alpha(fx::PRESS))
                .on_press(|st: &mut State| st.host.send(Command::ToggleMute))
                .child(
                    icon(
                        if muted {
                            &icons::VOLUME_X
                        } else {
                            &icons::VOLUME
                        },
                        16.0,
                    )
                    .color(p.text_secondary),
                ),
        )
        .child(slider(
            "volume",
            display_vol,
            cx,
            true,
            p,
            |st: &mut State, v: f32| st.host.send(Command::SetVolume(v)),
        ))
        .child(
            text(format!("{:.0}", display_vol * 100.0))
                .font(type_::LABEL.mono())
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
        // No own fill: the sidebar already paints `p.card` behind us. A full-width
        // fill here would overpaint the sidebar's 1 px right border along this
        // band, breaking the divider into "only beside some containers" — so we
        // stay transparent and let the one continuous border show through.
        // NowPlayingPanel (old app's `p-4`): the uppercase eyebrow then the
        // cover + title/artist row, `mb-3` apart.
        .child(
            col()
                .key("now-playing-panel")
                .padding(NP_PAD)
                .gap(CTRL_GAP)
                .child(
                    text(t!("player.nowPlaying").to_string())
                        .font(type_::SECTION)
                        .color(p.text_secondary),
                )
                .child(now_playing_row(s, now, p, scale)),
        )
        // Controls block (old app's `px-4 pt-2 pb-4 space-y-3`): seek, transport,
        // volume.
        .child(
            col()
                .key("player-controls")
                .px(CTRL_PAD_X)
                .pt(CTRL_PAD_T)
                .pb(CTRL_PAD_B)
                .gap(CTRL_GAP)
                .child(seek_row(s, p, cx))
                .child(transport_row(s, p))
                .child(volume_row(s, p, cx)),
        )
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
