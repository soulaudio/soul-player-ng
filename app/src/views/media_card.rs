//! The shared library card, the original soul-player's `MediaCard`. A cover tile
//! (square for albums, circular for artists) with a hover play-overlay, a
//! truncating title, and an optional muted subtitle. Clicking the card body runs
//! `on_open` (navigate); only the overlay button plays. Album and artist cards are
//! thin wrappers over this.
#![forbid(unsafe_code)]

use rune::{Align, Color, Cursor, El, col, el, icon, row, text};
use soul_player_host::Command;

use crate::State;
use crate::theme::Palette;
use crate::ui::{radius, space, type_};
use crate::views::grid;
use crate::views::icons;
use crate::views::player::{artist_cover, cover, cover_from_path, remote_badge_for};

/// The hover play-overlay button, the old app's `w-14 h-14 rounded-xl` (56 px).
const PLAY_BTN: f32 = 56.0;

/// The command a card's play-overlay issues (the old app's MediaCard
/// `handlePlayPause`): toggle pause/resume when a track of this card is the one
/// playing, else start this card's context from the top. Pure: view and test
/// share one rule. "Active" = the now-playing track belongs to this card.
pub(crate) fn card_play_command(now_playing: Option<u32>, ids: &[u32]) -> Command {
    if now_playing.is_some_and(|id| ids.contains(&id)) {
        Command::TogglePlay
    } else {
        Command::PlayContext {
            ids: ids.to_vec(),
            start: 0,
        }
    }
}

/// How many lines a square (album) card's title may wrap to before it is clamped.
pub(crate) const TITLE_LINES: usize = 2;

/// What a card renders: identity, artwork, and the ids its play-overlay acts on.
pub(crate) struct CardSpec {
    /// The card body's element key (e.g. `"album-0"`), unique within its grid.
    pub(crate) key: String,
    /// The accessible label (the title).
    pub(crate) label: String,
    /// The bold card title (album/artist name).
    pub(crate) title: String,
    /// An optional muted subtitle under the title (album → artist; artist → count).
    pub(crate) subtitle: Option<String>,
    /// A member track id to draw the cover from.
    pub(crate) cover_id: u32,
    /// A user-uploaded custom cover file (the old app's "Edit Artwork"), which
    /// wins over the `cover_id`-derived art when set: the album/artist's own
    /// custom image is drawn instead (see `crate::covers::CoverStore`). `None`
    /// (the common case) keeps the generated/embedded art.
    pub(crate) cover_override: Option<std::path::PathBuf>,
    /// Circular artwork (artists) vs a square `radius::MD` tile (albums).
    pub(crate) circular: bool,
    /// Every track id the play-overlay plays, in order.
    pub(crate) play_ids: Vec<u32>,
    /// `Some(host)` when every track behind this card streams from that same
    /// remote host (see `views::player::group_remote_source`), shows a cloud
    /// badge next to the title. `None` (a local or mixed-source card) shows
    /// nothing, the unremarked default.
    pub(crate) remote_source: Option<String>,
    /// `Some(dr)` shows a colour-banded DR pill in the cover's corner, the album's
    /// dynamic-range indicator (albums only). `None` shows nothing (artists, or an album
    /// whose tracks are all un-analyzed).
    pub(crate) dr: Option<f32>,
}

/// A colour-banded DR pill for a cover's top-left corner: `DR12`, green (dynamic) to warm
/// (compressed) via the shared band, on a dark scrim so it stays legible over any artwork.
fn dr_badge(key: &str, dr: f32, p: &Palette) -> El {
    el().key(format!("{key}-dr"))
        .absolute(space::XS, space::XS)
        .px(space::XS)
        .py(space::XXS)
        .radius(radius::SM)
        .fill(Color::BLACK.alpha(0.55))
        .child(
            text(format!("DR{}", dr.round() as i32))
                .font(type_::CAPTION.mono())
                .color(crate::views::signal_path::dr_color(dr, p)),
        )
}

/// One library card: artwork (square or circular) with a hover play-overlay, a
/// title, and an optional subtitle. The card body runs `on_open`; only the overlay
/// button plays/pauses this card's context. Mirrors the original's MediaCard split
/// of button = play, body = navigate.
pub(crate) fn media_card(
    s: &State,
    p: &Palette,
    spec: CardSpec,
    card_w: f32,
    scale: f32,
    on_open: impl Fn(&mut State) + 'static,
) -> El {
    // Circular artwork is just a cover with a half-side corner radius.
    let corner = if spec.circular {
        card_w * 0.5
    } else {
        radius::MD
    };
    let overlay_ids = spec.play_ids.clone();
    // When this card is the active, playing context the overlay shows a pause glyph.
    let show_pause = s
        .now_playing_id
        .is_some_and(|id| spec.play_ids.contains(&id))
        && s.playing;

    // The base artwork: a user-uploaded custom cover wins over everything (the
    // old app's "Edit Artwork"); else an artist card draws its avatar and an
    // album card its cover-track art. A circular card blurs its base art either
    // way, so the person silhouette + scrim overlay below still reads on top.
    let base_art = if let Some(path) = &spec.cover_override {
        // A user-uploaded cover is the real photo: show it crisp, clipped to the
        // card shape. No blur + no silhouette overlay (those dress up the seeded
        // avatar); once a real image is set the card reads as a plain photo.
        cover_from_path(s, path, card_w, corner, p, scale)
    } else if spec.circular {
        // Artist avatar: real art, else the artist-seeded generated cover,
        // softly blurred so the person silhouette reads on top (the original's
        // artist cards blur the art behind the glyph). Sigma scales with the
        // card so the look is consistent at every grid density.
        artist_cover(s, spec.cover_id, &spec.title, card_w, corner, p, scale).blur(card_w * 0.05)
    } else {
        cover(s, Some(spec.cover_id), card_w, corner, p, scale)
    };
    let mut cover_area = el()
        .key("cover-area")
        .size(card_w, card_w)
        .radius(corner)
        // Clip children to the rounded shape so the blurred artist avatar keeps a
        // crisp circle edge (the blur would otherwise bleed past the circle).
        .clip_rounded(corner)
        .shadow(2.0, 8.0, Color::BLACK.alpha(0.15))
        .child(base_art);
    // Artist cards (circular) overlay a person silhouette over a subtle scrim, so
    // the avatar reads as a profile the way the original's artist cards do; the
    // artwork stays faintly visible behind the glyph. A user-set custom cover is a
    // real photo, so it drops the silhouette + scrim and shows the image cleanly.
    if spec.circular && spec.cover_override.is_none() {
        cover_area = cover_area.child(
            el().key("artist-glyph")
                .size(card_w, card_w)
                .absolute(0.0, 0.0)
                .radius(corner)
                .center()
                .fill(Color::BLACK.alpha(0.35))
                .child(icon(&icons::USER, (card_w * 0.42).round()).color(Color::WHITE.alpha(0.9))),
        );
    }
    let cover_area = cover_area.child(
        // The old app's `opacity-0 group-hover:opacity-100` overlay: ONLY this
        // button plays/pauses; the card body navigates.
        el().key("play-overlay")
            .size(card_w, card_w)
            .absolute(0.0, 0.0)
            .center()
            .group_opacity(0.0, 1.0)
            .child(
                el().key("play-btn")
                    .role("button")
                    .label(t!("player.play").to_string())
                    .focusable(true)
                    .size(PLAY_BTN, PLAY_BTN)
                    .radius(radius::LG)
                    .fill(Color::BLACK.alpha(0.5))
                    .hover(Color::BLACK.alpha(0.3))
                    .cursor(Cursor::Pointer)
                    .center()
                    .on_press(move |st: &mut State| {
                        st.host
                            .send(card_play_command(st.now_playing_id, &overlay_ids))
                    })
                    .child(
                        icon(
                            if show_pause {
                                &icons::PAUSE
                            } else {
                                &icons::PLAY
                            },
                            32.0,
                        )
                        .color(Color::WHITE),
                    ),
            ),
    );

    // An album's DR rides in the cover's corner, so a browser can spot a compressed
    // (loudness-war) master vs a dynamic one at a glance across the grid.
    let cover_area = if let Some(dr) = spec.dr {
        cover_area.child(dr_badge(&spec.key, dr, p))
    } else {
        cover_area
    };

    // Artist (circular) cards center their caption under the avatar, like the
    // original; album (square) cards keep the title left-aligned under the cover.
    let text_align = if spec.circular {
        Align::Center
    } else {
        Align::Start
    };
    let title_el = text(spec.title)
        .font(type_::CARD_TITLE)
        .color(p.fg)
        .align(text_align)
        .group_opacity(1.0, 0.8);
    // An album title wraps to [`TITLE_LINES`] lines (a shared prefix like
    // "Genshin Impact - ..." must stay distinguishable) and reserves both so a
    // one-line title does not leave its row ragged. An artist name stays one line.
    let title_el = if spec.circular {
        title_el.truncate()
    } else {
        title_el
            .max_lines(TITLE_LINES)
            .min_h(TITLE_LINES as f32 * type_::CARD_TITLE.line_height)
    };
    // A local/mixed-source card (the common case) keeps the plain title; only a
    // card whose tracks all stream from one remote host gains the badge, same
    // "local is unremarked" rule as the track-row badge.
    let title_row: El = if let Some(host) = &spec.remote_source {
        row()
            .key("card-title")
            .gap(space::XS)
            .center_y()
            .min_w(0.0)
            .child(title_el.grow(1.0))
            .child(remote_badge_for(&format!("{}-remote", spec.key), host, p))
    } else {
        title_el
    };
    let mut text_col = col()
        .key("card-text")
        .gap(space::XXS)
        .min_w(0.0)
        .child(title_row);
    if let Some(sub) = spec.subtitle {
        text_col = text_col.child(
            text(sub)
                .font(type_::CARD_SUBTITLE)
                .color(p.text_secondary)
                .truncate()
                .align(text_align),
        );
    }

    col()
        .key(spec.key)
        .role("button")
        .label(spec.label)
        .focusable(true)
        // A flex item, not a fixed fractional width: wrap at [`MIN_CARD`] (the same
        // threshold `responsive_grid` counts columns by, so the column count agrees),
        // then grow to fill the row. A fixed `card_w` is a float that can't tile the
        // integer container, it rounds a hair too wide and wraps the last column, or
        // too narrow and leaves a thin gap; growing lets the engine distribute the
        // remainder so every window width fills exactly. Capped at `card_w + FILL_SLOP`
        // so a partial last row stays aligned with the full rows above.
        .min_w(grid::MIN_CARD)
        .max_w(card_w + grid::FILL_SLOP)
        .grow(1.0)
        // Cover→text gap only. No bottom padding: it used to stack on top of the
        // grid's row gap, making the space between rows visibly larger than the
        // gap between columns. The grid's own gap now spaces rows and columns alike.
        .gap(space::SM)
        .cursor(Cursor::Pointer)
        .hover_group()
        .child(cover_area)
        .child(text_col)
        .on_press(on_open)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_active_card_toggles_play_pause_otherwise_plays_from_the_start() {
        assert_eq!(
            card_play_command(Some(6), &[5, 6, 7]),
            Command::TogglePlay,
            "a track of this card is playing → toggle"
        );
        assert_eq!(
            card_play_command(Some(9), &[5, 6, 7]),
            Command::PlayContext {
                ids: vec![5, 6, 7],
                start: 0
            },
            "a different context is playing → play this one from the top"
        );
        assert_eq!(
            card_play_command(None, &[5, 6, 7]),
            Command::PlayContext {
                ids: vec![5, 6, 7],
                start: 0
            },
            "nothing playing → play from the top"
        );
    }
}
