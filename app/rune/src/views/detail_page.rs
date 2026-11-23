//! The shared detail-page hero + Play-All pill — the original soul-player's
//! `DetailPageLayout` header. Album/artist/genre detail pages all build their
//! header from `detail_hero` (artwork left, eyebrow/title/subtitle/meta/actions
//! right, reflowing to stacked on a narrow pane) and their primary action from
//! `play_all_pill`, so the three pages share one grammar.
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use rune::{Color, Cursor, El, FileDialogOptions, Icon, Tx, col, el, icon, row, text};
use soul_player_host::Command;

use crate::State;
use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::views::icons;
use crate::views::player::{cover, cover_from_path, remote_badge_for};

/// Large cover on a detail hero (the old app's `w-44`).
pub(crate) const DETAIL_COVER: f32 = 176.0;
/// The genre hero's icon tile (no artwork → a glyph in a muted square).
const ICON_TILE: f32 = 120.0;

/// A hero's artwork: a square album cover, a circular artist avatar, an
/// on-disk image file (a playlist's custom cover), or a muted icon tile
/// (genres carry no art).
pub(crate) enum HeroArt {
    /// A cover drawn from a member track id; `circular` clips it to a circle.
    Cover { id: u32, circular: bool },
    /// A cover drawn from an image file on disk (a custom artist/album/playlist
    /// cover), rendered via the same path-keyed cache the Playlists list page
    /// uses. `circular` clips it to a circle (a custom artist avatar) so an
    /// overridden artist hero matches its circular grid card.
    Image { path: PathBuf, circular: bool },
    /// A centered glyph in a muted `radius::MD` tile.
    Icon(&'static Icon),
}

fn hero_artwork(
    s: &State,
    p: &Palette,
    scale: f32,
    prefix: &str,
    art: HeroArt,
    artwork: Option<ArtworkTarget>,
) -> El {
    match art {
        HeroArt::Cover { id, circular } => {
            let corner = if circular {
                DETAIL_COVER * 0.5
            } else {
                radius::MD
            };
            with_artwork_overlay(
                el().key(format!("{prefix}-cover"))
                    .size(DETAIL_COVER, DETAIL_COVER)
                    .radius(corner)
                    .clip_rounded(corner)
                    .shadow(4.0, 16.0, Color::BLACK.alpha(0.22))
                    .child(cover(s, Some(id), DETAIL_COVER, corner, p, scale)),
                s,
                p,
                corner,
                artwork,
            )
        }
        HeroArt::Image { path, circular } => {
            let corner = if circular {
                DETAIL_COVER * 0.5
            } else {
                radius::MD
            };
            with_artwork_overlay(
                el().key(format!("{prefix}-hero-image"))
                    .size(DETAIL_COVER, DETAIL_COVER)
                    .radius(corner)
                    .clip_rounded(corner)
                    .shadow(4.0, 16.0, Color::BLACK.alpha(0.22))
                    .child(cover_from_path(s, &path, DETAIL_COVER, corner, p, scale)),
                s,
                p,
                corner,
                artwork,
            )
        }
        HeroArt::Icon(glyph) => el()
            .key(format!("{prefix}-cover"))
            .size(ICON_TILE, ICON_TILE)
            .radius(radius::MD)
            .fill(p.muted)
            .center()
            .child(icon(glyph, (ICON_TILE * 0.5).round()).color(p.text_secondary)),
    }
}

/// Add the Edit/Remove-Artwork hover overlay to a hero cover when the page has an
/// artwork `target` (album + artist pages); a no-op otherwise (genre/playlist). The
/// old Soul Player reveals its "Edit Artwork" affordance ON HOVER over the cover art
/// rather than parking a button in the actions row, so the controls ride a scrim that
/// fades in on hover ([`El::group_opacity`]) while staying keyboard-reachable.
fn with_artwork_overlay(
    container: El,
    s: &State,
    p: &Palette,
    corner: f32,
    artwork: Option<ArtworkTarget>,
) -> El {
    match artwork {
        Some(target) => container.child(artwork_hover_overlay(s, p, corner, target)),
        None => container,
    }
}

/// The hover scrim over a hero cover carrying the Edit-Artwork control (and a
/// Remove control once a custom cover is set). Hidden at rest and revealed when the
/// cover is hovered; the controls keep their roles/labels/focus so the a11y sweep and
/// keyboard users reach them regardless of hover state (same pattern as a card's
/// play-overlay).
fn artwork_hover_overlay(s: &State, p: &Palette, corner: f32, target: ArtworkTarget) -> El {
    let mut controls = row()
        .key(format!("{}-artwork-controls", target.key_prefix()))
        .gap(space::SM)
        .center()
        .child(edit_artwork_control(p, target.clone()));
    if target.cover_path(s).is_some() {
        controls = controls.child(remove_artwork_control(p, target.clone()));
    }
    el().key(format!("{}-artwork-overlay", target.key_prefix()))
        .size(DETAIL_COVER, DETAIL_COVER)
        .absolute(0.0, 0.0)
        .radius(corner)
        .center()
        .fill(Color::BLACK.alpha(0.5))
        .group_opacity(0.0, 1.0)
        .child(controls)
}

/// The shared detail header: artwork left, a text block right (eyebrow, title,
/// optional subtitle, a meta line, then `actions`). Reflows to stacked when the
/// pane is too narrow (`flex_wrap`). `prefix` keys the row/cover/meta so each page
/// keeps stable keys (`album-header`, `artist-cover`, …).
///
/// `remote_source` is `Some(host)` when the viewed album/artist/genre's tracks
/// all stream from that same remote host (`views::player::group_remote_source`,
/// read straight off the already-grouped struct) — it puts the same cloud badge
/// and "streams from {host}" tooltip the grid cards and track rows show next
/// to the title. `None` (a local or mixed-source group) shows nothing.
#[allow(clippy::too_many_arguments)]
pub(crate) fn detail_hero(
    s: &State,
    p: &Palette,
    scale: f32,
    prefix: &str,
    art: HeroArt,
    eyebrow: String,
    title: String,
    remote_source: Option<String>,
    subtitle: Option<String>,
    meta: String,
    meta_icon: Option<&'static Icon>,
    actions: El,
    artwork: Option<ArtworkTarget>,
) -> El {
    let title_el = text(title).font(type_::DISPLAY).color(p.fg).truncate();
    // A local/mixed-source group (the common case) keeps the plain title; only a
    // group whose tracks all stream from one remote host gains the badge, same
    // "local is unremarked" rule as the grid cards and track rows.
    let title_row: El = if let Some(host) = &remote_source {
        row()
            .key(format!("{prefix}-title"))
            .gap(space::XS)
            .center_y()
            .min_w(0.0)
            .child(title_el.grow(1.0))
            .child(remote_badge_for(&format!("{prefix}-remote"), host, p))
    } else {
        title_el
    };
    let mut info = col()
        .key(format!("{prefix}-meta"))
        .gap(space::XS)
        .grow(1.0)
        .min_w(0.0)
        // Left-align the heading block (eyebrow/title/subtitle/meta/actions) hard
        // against the cover, like the original's detail hero. `center_y()` here was
        // `align-items: center` on a COLUMN, i.e. the CROSS (horizontal) axis, which
        // floated the whole block into the middle of the pane.
        .items_start()
        .child(text(eyebrow).font(type_::SECTION).color(p.text_secondary))
        .child(title_row);
    if let Some(sub) = subtitle {
        info = info.child(
            text(sub)
                .font(type_::CARD_TITLE)
                .color(p.text_secondary)
                .truncate(),
        );
    }
    // The meta line (track/duration counts). A `meta_icon` (the clock on
    // duration-bearing pages) sits before the text, like the original's hero.
    let meta_text = text(meta).font(type_::CAPTION).color(p.text_secondary);
    let meta_el: El = match meta_icon {
        Some(ic) => row()
            .key(format!("{prefix}-meta-line"))
            .center_y()
            .gap(space::XS)
            .child(icon(ic, 14.0).color(p.text_secondary))
            .child(meta_text),
        None => meta_text,
    };
    info = info.child(meta_el).child(actions);

    row()
        .key(format!("{prefix}-header"))
        .flex_wrap()
        .gap(space::XL)
        .pb(space::SECTION)
        .child(hero_artwork(s, p, scale, prefix, art, artwork))
        .child(info)
}

/// The "Play All" pill (the old app's `rounded-full px-6 py-3` filled Play
/// button) — plays `ids` from the top. `key` keeps each page's stable key
/// (`album-play-all`, `artist-play-all`, `genre-play-all`).
pub(crate) fn play_all_pill(key: &str, ids: Vec<u32>, p: &Palette) -> El {
    el().key(key.to_string())
        .role("button")
        .label(t!("album.playAll").to_string())
        .focusable(true)
        .px(space::XL)
        .py(space::MD)
        .radius(radius::FULL)
        .fill(p.primary)
        .cursor(Cursor::Pointer)
        .hover(p.bg.alpha(fx::HOVER_FILL))
        .press(p.bg.alpha(fx::PRESS_FILL))
        .on_press(move |st: &mut State| {
            st.host.send(Command::PlayContext {
                ids: ids.clone(),
                start: 0,
            })
        })
        .child(
            row()
                .gap(space::SM)
                .center_y()
                .child(icon(&icons::PLAY, 20.0).color(p.bg))
                .child(text(t!("album.playAll").to_string()).color(p.bg)),
        )
}

/// The size of the circular [`enqueue_all_button`] (roughly the old app's
/// secondary icon button, matched to sit comfortably beside the Play All pill).
const ENQUEUE_ALL_BTN: f32 = 44.0;

/// The secondary "add to queue" icon button beside Play All (the old app's
/// muted circular button next to its Play button): enqueues every one of
/// `ids`, in order, onto the override queue (the same queue the `+` button on
/// a Tracks row sends a single track to), just for a whole
/// album/artist/genre/playlist at once. `key` keeps each call site's stable
/// key.
pub(crate) fn enqueue_all_button(key: &str, ids: Vec<u32>, p: &Palette) -> El {
    el().key(key.to_string())
        .role("button")
        .label(t!("library.addToQueue").to_string())
        .focusable(true)
        .size(ENQUEUE_ALL_BTN, ENQUEUE_ALL_BTN)
        .center()
        .radius(radius::FULL)
        .fill(p.muted)
        .cursor(Cursor::Pointer)
        .hover(p.fg.alpha(fx::HOVER))
        .press(p.fg.alpha(fx::PRESS))
        .child(icon(&icons::LIST_PLUS, 18.0).color(p.text_secondary))
        .on_press(move |st: &mut State| {
            for &id in &ids {
                st.host.send(Command::Enqueue(id));
            }
        })
}

/// Play All plus the secondary "add to queue" button beside it: the shared
/// hero actions row for the album/artist/genre detail pages (the playlist
/// page builds its own richer action row, since it also carries rename/
/// delete/cover controls, but includes the same [`enqueue_all_button`]).
/// `play_key` is the page's stable Play All key (`album-play-all`, …); the
/// enqueue button shares one key (`detail-enqueue-all`) across pages, since
/// only one detail page is ever on screen at a time.
pub(crate) fn detail_actions(play_key: &str, ids: Vec<u32>, p: &Palette) -> El {
    row()
        .key(format!("{play_key}-actions"))
        .gap(space::SM)
        .center_y()
        .child(play_all_pill(play_key, ids.clone(), p))
        .child(enqueue_all_button("detail-enqueue-all", ids, p))
}

/// Side of the circular Edit/Remove-Artwork icon buttons (matches
/// [`ENQUEUE_ALL_BTN`] so they sit evenly beside it in the hero actions row).
const ARTWORK_BTN: f32 = ENQUEUE_ALL_BTN;

/// Which entity an "Edit Artwork" control sets a custom cover for. Covers are
/// keyed by name in [`crate::covers::CoverStore`] (an album or artist name, so
/// they survive the volatile track-id re-mint every rescan does), so the target
/// just carries the name plus which kind it is.
#[derive(Clone)]
pub(crate) enum ArtworkTarget {
    Album(String),
    Artist(String),
}

impl ArtworkTarget {
    /// This target's current custom-cover path, if one is set and the file
    /// still exists ([`crate::covers::CoverStore`] returns `None` for a
    /// vanished file). `pub(crate)` so the crop dialogue
    /// (`views::artwork_crop`) can invalidate the art cache for the same
    /// old/new paths this control does.
    pub(crate) fn cover_path(&self, s: &State) -> Option<PathBuf> {
        match self {
            ArtworkTarget::Album(name) => s.covers.album_cover_path(name),
            ArtworkTarget::Artist(name) => s.covers.artist_cover_path(name),
        }
    }

    /// Apply `src` as this target's custom cover (validated + copied by the
    /// store). Returns whether it applied. `pub(crate)` so the crop dialogue
    /// (`views::artwork_crop`) can store the CROPPED temp PNG the same way.
    pub(crate) fn set_cover(&self, s: &mut State, src: &Path) -> bool {
        match self {
            ArtworkTarget::Album(name) => s.covers.set_album_cover(name, src),
            ArtworkTarget::Artist(name) => s.covers.set_artist_cover(name, src),
        }
    }

    /// Clear this target's custom cover.
    fn clear_cover(&self, s: &mut State) {
        match self {
            ArtworkTarget::Album(name) => s.covers.clear_album_cover(name),
            ArtworkTarget::Artist(name) => s.covers.clear_artist_cover(name),
        }
    }

    /// The element-key prefix (`"album"`/`"artist"`) its controls key under,
    /// so each page keeps a stable key (`album-edit-artwork`, …).
    fn key_prefix(&self) -> &'static str {
        match self {
            ArtworkTarget::Album(_) => "album",
            ArtworkTarget::Artist(_) => "artist",
        }
    }

    /// Whether this target's crop frame is a CIRCLE (an artist avatar) rather
    /// than a SQUARE (an album cover) — matches how each kind renders
    /// everywhere else (artist heroes/cards clip to a circle, albums to a
    /// rounded square).
    pub(crate) fn is_circular(&self) -> bool {
        matches!(self, ArtworkTarget::Artist(_))
    }
}

/// An in-progress "Edit Artwork" crop (the old Soul Player's cover-crop step):
/// the picked source image plus the live pan/zoom the user is applying to it
/// behind the fixed frame. `Some` in [`crate::State::artwork_crop`] opens the
/// crop dialogue ([`crate::views::artwork_crop::overlay`]); confirming crops the
/// source to a square and stores it (non-destructively) as `target`'s custom
/// cover, cancelling drops it. `scale` maps source pixels to displayed points
/// (`disp = src * scale`) and `offset` pans the image within the frame, in
/// points, clamped so the image always covers the frame.
pub(crate) struct ArtworkCrop {
    /// Which album/artist this crop sets the cover for.
    pub target: ArtworkTarget,
    /// The absolute path to the picked source image (any decodable format).
    pub source: PathBuf,
    /// The source image's pixel dimensions (read once, cheaply, at pick time
    /// via `image::image_dimensions`).
    pub src_w: u32,
    pub src_h: u32,
    /// Source-pixels → displayed-points factor. Starts at the cover scale
    /// (`FRAME / min(src_w, src_h)`) so the image fills the frame; the zoom
    /// buttons multiply/divide it.
    pub scale: f32,
    /// Pan of the image's centre from the frame's centre, in points. Clamped so
    /// the displayed image always covers the frame on both axes.
    pub offset: (f32, f32),
}

/// A circular icon button in the hero actions row, [`ARTWORK_BTN`] wide, muted
/// like [`enqueue_all_button`]. Shared shape for the Edit/Remove-Artwork
/// controls.
fn artwork_btn(key: String, label: String, glyph: &'static Icon, p: &Palette) -> El {
    el().key(key)
        .role("button")
        .label(label.clone())
        .help(label)
        .focusable(true)
        .size(ARTWORK_BTN, ARTWORK_BTN)
        .center()
        .radius(radius::FULL)
        .fill(p.muted)
        .cursor(Cursor::Pointer)
        .hover(p.fg.alpha(fx::HOVER))
        .press(p.fg.alpha(fx::PRESS))
        .child(icon(glyph, 18.0).color(p.text_secondary))
}

/// The "Edit Artwork" control (the old app's per-album/artist cover upload):
/// opens an image-file dialog and, on a pick, opens the CROP dialogue
/// ([`crate::views::artwork_crop::overlay`]) where the user pans/zooms the
/// picked image behind a fixed frame (a circle for an artist, a square for an
/// album) before confirming. Confirming crops + stores it as `target`'s custom
/// cover via [`crate::covers::CoverStore`] (non-destructive — the music files
/// are never touched); a set cover always wins over the generated/embedded art
/// everywhere that album/artist shows. Picking no longer sets the cover
/// directly (that moved into the crop step).
fn edit_artwork_control(p: &Palette, target: ArtworkTarget) -> El {
    artwork_btn(
        format!("{}-edit-artwork", target.key_prefix()),
        t!("artwork.edit").to_string(),
        &icons::UPLOAD,
        p,
    )
    .on_press_tx(move |_s: &mut State, tx: &mut Tx| {
        let target = target.clone();
        tx.open_file(
            FileDialogOptions {
                title: t!("artwork.editDialog").to_string(),
                ..Default::default()
            }
            .filter(
                "Images (*.jpg, *.jpeg, *.png, *.webp, *.gif)",
                &["jpg", "jpeg", "png", "webp", "gif"],
            ),
            move |s: &mut State, result, _tx| {
                let Ok(Some(path)) = result else {
                    return;
                };
                // Read the picked image's pixel size cheaply (header only, no
                // full decode). A file we can't even measure isn't an image we
                // can crop, so skip it rather than opening an empty dialogue.
                let Ok((src_w, src_h)) = image::image_dimensions(&path) else {
                    return;
                };
                // Start at the cover scale so the image fills the frame; the
                // dialogue's zoom buttons take it from there.
                let scale = crate::views::artwork_crop::FRAME / (src_w.min(src_h).max(1) as f32);
                s.artwork_crop = Some(ArtworkCrop {
                    target: target.clone(),
                    source: path,
                    src_w,
                    src_h,
                    scale,
                    offset: (0.0, 0.0),
                });
            },
        );
    })
}

/// The "Remove Artwork" control: clears `target`'s custom cover, falling its
/// art back to the generated/embedded default. Only rendered when a custom
/// cover is actually set (see [`artwork_hover_overlay`]).
fn remove_artwork_control(p: &Palette, target: ArtworkTarget) -> El {
    artwork_btn(
        format!("{}-remove-artwork", target.key_prefix()),
        t!("artwork.remove").to_string(),
        &icons::X,
        p,
    )
    .on_press(move |s: &mut State| {
        let old = target.cover_path(s);
        target.clear_cover(s);
        if let Some(old) = old {
            s.art.forget_path(&old);
        }
    })
}
