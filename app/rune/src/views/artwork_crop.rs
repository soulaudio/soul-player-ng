//! The "Edit Artwork" CROP dialogue (the old Soul Player's cover-crop step):
//! after a file is picked, the user PANS (drag) and ZOOMS (a slider) the picked
//! image behind a fixed frame — a CIRCLE for an artist avatar, a SQUARE for an
//! album cover — then confirms to crop + store it (or removes the current cover
//! via the dialogue's trash control).
//!
//! Storage stays NON-DESTRUCTIVE: confirming crops the source to a square,
//! re-encodes it to a fresh temp PNG, and copies THAT into the custom-cover
//! store (`crate::covers::CoverStore`); the music files are never touched.
//!
//! The dialogue is a centered, dim-backdrop modal mounted in `main.rs`'s overlay
//! layer (like `add_to_playlist::overlay`), driven entirely off
//! `State::artwork_crop` (`None` = closed).
#![forbid(unsafe_code)]

use std::cell::Cell;
use std::path::{Path, PathBuf};

use rune::{
    Color, Cursor, Cx, Drag, El, FileDialogOptions, Phase, SurfaceFit, Tx, col, el, icon, row, text,
};

use crate::State;
use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::views::detail_page::{ArtworkCrop, ArtworkTarget};
use crate::views::icons;
use crate::views::player::cover_from_path;
use crate::views::slider::slider;

/// The pan/zoom VIEW area side, in points (the square window the image shows
/// through).
const VIEW: f32 = 300.0;
/// The fixed crop FRAME side, in points (the bright, in-focus region).
pub(crate) const FRAME: f32 = 260.0;
/// The gap between the view edge and the frame (`VIEW == FRAME + 2 * MARGIN`).
const MARGIN: f32 = 20.0;
/// Half the view side — the view's (and frame's) centre.
const HALF: f32 = VIEW / 2.0;
/// Half the frame side — how far the frame reaches from the centre. The image
/// must always cover this on each side (`|offset| <= disp/2 - HALF_FRAME`).
const HALF_FRAME: f32 = FRAME / 2.0;
/// The deepest zoom-in, as a multiple of the cover (min) scale.
const MAX_ZOOM: f32 = 6.0;
/// The cropped cover is re-encoded at this side, in pixels — generous for the
/// 176 pt detail hero and the grid cards while staying a small PNG.
const OUTPUT_PX: u32 = 512;
/// The current-artwork preview side in the manage ('select') dialogue, in points.
const MANAGE_PREVIEW: f32 = 140.0;
/// The manage dialogue's fixed width, in points: the same outer width as the crop
/// dialogue (its `VIEW` plus the card's `LG` padding on each side), so stepping
/// Change -> crop keeps the card the same size instead of jumping.
const MANAGE_DIALOG_W: f32 = VIEW + 2.0 * space::LG;

thread_local! {
    /// The image offset at the moment a pan drag started. `Drag::dx/dy` are the
    /// pointer's TOTAL travel since the press (not a per-move delta), so — like
    /// `views::scrollbar`'s thumb — we anchor to the offset at `Phase::Down` and
    /// add the cumulative travel on each `Move`. One drag at a time per window,
    /// so a single cell suffices.
    static DRAG_ANCHOR: Cell<(f32, f32)> = const { Cell::new((0.0, 0.0)) };
}

/// The smallest scale that still fills the frame (the image's shorter side maps
/// to exactly `FRAME`). The zoom buttons never go below this, so the image can
/// never shrink inside the frame.
fn min_scale(crop: &ArtworkCrop) -> f32 {
    FRAME / (crop.src_w.min(crop.src_h).max(1) as f32)
}

/// The maximum magnitude each offset axis may take at the current scale, so the
/// displayed image still covers the frame. Zero (clamp to centre) when the
/// displayed side is no wider than the frame.
fn offset_bound(disp: f32) -> f32 {
    (disp / 2.0 - HALF_FRAME).max(0.0)
}

/// Re-clamp both offset axes to the current scale's bounds (after a zoom or a
/// pan), so a zoom-out that shrinks the image can't leave the frame uncovered.
fn clamp_offset(crop: &mut ArtworkCrop) {
    let bx = offset_bound(crop.src_w as f32 * crop.scale);
    let by = offset_bound(crop.src_h as f32 * crop.scale);
    crop.offset.0 = crop.offset.0.clamp(-bx, bx);
    crop.offset.1 = crop.offset.1.clamp(-by, by);
}

/// Map a zoom-slider value `v01` (0 = the cover scale, 1 = `MAX_ZOOM` × the cover
/// scale) to `crop`'s live scale, clamped to `[min_scale, MAX_ZOOM × min_scale]` so
/// the image never shrinks below frame coverage nor zooms past the ceiling, then
/// re-clamp the pan to the new scale's bounds.
fn apply_zoom01(crop: &mut ArtworkCrop, v01: f32) {
    let min = min_scale(crop);
    let max = min * MAX_ZOOM;
    crop.scale = (min + v01.clamp(0.0, 1.0) * (max - min)).clamp(min, max);
    clamp_offset(crop);
}

/// The slider's `on_scrub`/`on_commit`: apply the slider value to the live crop.
fn set_zoom01(s: &mut State, v01: f32) {
    if let Some(crop) = s.artwork_crop.as_mut() {
        apply_zoom01(crop, v01);
    }
}

/// The current scale as a 0..1 zoom-slider value (the inverse of [`set_zoom01`]).
fn zoom01(crop: &ArtworkCrop) -> f32 {
    let min = min_scale(crop);
    let max = min * MAX_ZOOM;
    ((crop.scale - min) / (max - min)).clamp(0.0, 1.0)
}

/// Delete `target`'s custom cover (back to the generated/embedded art) and close
/// the manage dialogue. The dialogue's Delete button calls this; it is only shown
/// when a cover is actually set.
fn delete_cover(s: &mut State, target: &ArtworkTarget) {
    let old = target.cover_path(s);
    target.clear_cover(s);
    if let Some(old) = old {
        s.art.forget_path(&old);
    }
    crate::close_artwork_edit(s);
}

/// Cancel the crop step: discard the pan/zoom and return to the manage dialogue
/// (the old app's crop "Back"), so the user lands back on Change/Delete rather
/// than all the way out.
fn crop_cancel(s: &mut State) {
    let target = s.artwork_crop.as_ref().map(|c| c.target.clone());
    s.artwork_crop = None;
    s.artwork_edit = target;
}

/// Apply one pan-drag event to the live offset: anchor at `Down`, then add the
/// cumulative pointer travel, clamped so the image keeps covering the frame.
fn pan(s: &mut State, d: &Drag) {
    let Some(crop) = s.artwork_crop.as_mut() else {
        return;
    };
    match d.phase {
        Phase::Down => DRAG_ANCHOR.with(|a| a.set(crop.offset)),
        Phase::Move | Phase::Up => {
            let (ax, ay) = DRAG_ANCHOR.with(Cell::get);
            let bx = offset_bound(crop.src_w as f32 * crop.scale);
            let by = offset_bound(crop.src_h as f32 * crop.scale);
            crop.offset.0 = (ax + d.dx).clamp(-bx, bx);
            crop.offset.1 = (ay + d.dy).clamp(-by, by);
        }
    }
}

/// Where the image's top-left sits in VIEW points, for the given crop state.
fn image_origin(crop: &ArtworkCrop) -> (f32, f32) {
    let disp_w = crop.src_w as f32 * crop.scale;
    let disp_h = crop.src_h as f32 * crop.scale;
    (
        HALF - disp_w / 2.0 + crop.offset.0,
        HALF - disp_h / 2.0 + crop.offset.1,
    )
}

/// One image layer: the decoded source drawn at its displayed size and absolute
/// position (`left`/`top` relative to the layer's own parent). Falls back to a
/// muted box while the decode is pending / the file can't be read.
fn image_layer(s: &State, crop: &ArtworkCrop, key: &str, left: f32, top: f32, p: &Palette) -> El {
    let disp_w = crop.src_w as f32 * crop.scale;
    let disp_h = crop.src_h as f32 * crop.scale;
    let base = el()
        .key(key.to_string())
        .size(disp_w, disp_h)
        .absolute(left, top);
    // `scale` is irrelevant for a raster cover (the cache decodes full-res and
    // ignores it; covers are never SVG), so a fixed hint is fine here.
    match s.art.image_for_path(&crop.source, 2.0) {
        Some(image) if image.error().is_none() => base.pixels_fit(image.pixels(), SurfaceFit::Fill),
        _ => base.fill(p.muted),
    }
}

/// A footer button in the add-to-playlist footer's shape: muted (secondary) or
/// primary-filled, labelled + focusable + pointer, with hover/press feedback.
fn footer_button(
    key: &str,
    label: String,
    primary: bool,
    p: &Palette,
    on_press: impl Fn(&mut State) + 'static,
) -> El {
    let mut b = el()
        .key(key.to_string())
        .role("button")
        .label(label.clone())
        .focusable(true)
        .px(space::MD)
        .py(space::XS)
        .radius(radius::SM)
        .cursor(Cursor::Pointer);
    if primary {
        b = b
            .fill(p.primary)
            .hover(p.bg.alpha(fx::HOVER_FILL))
            .press(p.bg.alpha(fx::PRESS_FILL))
            .child(text(label).color(p.bg));
    } else {
        b = b
            .fill(p.muted)
            .hover(p.fg.alpha(fx::HOVER))
            .press(p.fg.alpha(fx::PRESS))
            .child(text(label).color(p.fg));
    }
    b.on_press(on_press)
}

/// The pan/zoom VIEW: the dimmed full image behind a bright, framed cut-out, all
/// panning together under one `on_drag`.
fn crop_view(s: &State, crop: &ArtworkCrop, p: &Palette) -> El {
    let (left, top) = image_origin(crop);
    let circular = crop.target.is_circular();
    let corner = if circular { HALF_FRAME } else { radius::MD };
    el().key("artwork-crop-view")
        .size(VIEW, VIEW)
        .radius(radius::MD)
        .fill(p.bg)
        .clip_rounded(radius::MD)
        .cursor(Cursor::Pointer)
        // (a) the full image, bright…
        .child(image_layer(s, crop, "artwork-crop-image-dim", left, top, p))
        // …dimmed by a scrim over the whole view, so only the frame reads bright.
        .child(
            el().key("artwork-crop-scrim")
                .absolute(0.0, 0.0)
                .size(VIEW, VIEW)
                .fill(Color::BLACK.alpha(0.5)),
        )
        // (b) the bright framed cut-out: the SAME image, positioned relative to
        // the frame, clipped to the frame shape, with a 2 px ring.
        .child(
            el().key("artwork-crop-frame")
                .absolute(MARGIN, MARGIN)
                .size(FRAME, FRAME)
                .radius(corner)
                .clip_rounded(corner)
                .border(2.0, Color::WHITE.alpha(0.9))
                .child(image_layer(
                    s,
                    crop,
                    "artwork-crop-image-bright",
                    left - MARGIN,
                    top - MARGIN,
                    p,
                )),
        )
        .on_drag(|st: &mut State, d: &mut Drag, _tx| pan(st, d))
}

/// The Edit-Artwork dialogue, mounted in `main.rs`'s overlay layer: the MANAGE
/// ('select') step ([`State::artwork_edit`], Change/Delete) or, once an image is
/// chosen, the CROP step ([`State::artwork_crop`]). `None` when both are closed.
pub fn overlay(s: &State, p: &Palette, cx: &Cx<State>) -> Option<El> {
    if let Some(crop) = s.artwork_crop.as_ref() {
        return Some(crop_dialogue(s, crop, p, cx));
    }
    s.artwork_edit
        .as_ref()
        .map(|target| manage_dialogue(s, p, cx, target))
}

/// The MANAGE ('select') dialogue the hero pencil opens: a preview of the current
/// artwork, then Change (pick an image, then the crop step) and, when a cover is
/// already set, Delete, plus Cancel.
/// A full-width secondary button for the manage dialogue (Delete / Cancel) — the
/// same height + shape as [`choose_image_button`] so the stacked actions read as
/// one even column.
fn manage_button(
    key: &str,
    label: String,
    p: &Palette,
    on_press: impl Fn(&mut State) + 'static,
) -> El {
    el().key(key.to_string())
        .role("button")
        .label(label.clone())
        .focusable(true)
        .w_full()
        .py(space::SM)
        .radius(radius::SM)
        .fill(p.muted)
        .cursor(Cursor::Pointer)
        .hover(p.fg.alpha(fx::HOVER))
        .press(p.fg.alpha(fx::PRESS))
        .center()
        .child(text(label).color(p.fg))
        .on_press(on_press)
}

fn manage_dialogue(s: &State, p: &Palette, cx: &Cx<State>, target: &ArtworkTarget) -> El {
    let corner = if target.is_circular() {
        MANAGE_PREVIEW * 0.5
    } else {
        radius::MD
    };
    // Preview: the current custom cover if set, else a muted "upload" placeholder.
    let preview: El = match target.cover_path(s) {
        Some(path) => el()
            .key("artwork-edit-preview")
            .size(MANAGE_PREVIEW, MANAGE_PREVIEW)
            .radius(corner)
            .clip_rounded(corner)
            .child(cover_from_path(
                s,
                &path,
                MANAGE_PREVIEW,
                corner,
                p,
                cx.env().scale,
            )),
        None => el()
            .key("artwork-edit-preview")
            .size(MANAGE_PREVIEW, MANAGE_PREVIEW)
            .radius(corner)
            .fill(p.muted)
            .center()
            .child(icon(&icons::UPLOAD, (MANAGE_PREVIEW * 0.34).round()).color(p.text_secondary)),
    };

    let mut actions = col()
        .key("artwork-edit-actions")
        .w_full()
        .gap(space::SM)
        .child(choose_image_button(target.clone(), p));
    if target.cover_path(s).is_some() {
        let target_del = target.clone();
        actions = actions.child(manage_button(
            "artwork-edit-delete",
            t!("artwork.remove").to_string(),
            p,
            move |st: &mut State| delete_cover(st, &target_del),
        ));
    }
    actions = actions.child(manage_button(
        "artwork-edit-cancel",
        t!("common.cancel").to_string(),
        p,
        |st: &mut State| crate::close_artwork_edit(st),
    ));

    col()
        .key("artwork-edit-modal")
        .absolute(0.0, 0.0)
        .w_full()
        .h_full()
        .center()
        .fill(Color::BLACK.alpha(0.45))
        .child(
            col()
                .key("artwork-edit-dialog")
                .w(MANAGE_DIALOG_W)
                .fill(p.card)
                .radius(radius::LG)
                .border(1.0, p.border)
                .padding(space::LG)
                .gap(space::LG)
                .center()
                .modal(true, crate::close_artwork_edit)
                .child(
                    text(t!("artwork.cropTitle").to_string())
                        .font(type_::TITLE)
                        .color(p.fg),
                )
                .child(preview)
                .child(actions),
        )
}

/// The primary "Change Image" button: opens an image file dialog and, on a pick,
/// reads the image size and hands off to the CROP step (`State::artwork_crop`),
/// leaving the manage dialogue.
fn choose_image_button(target: ArtworkTarget, p: &Palette) -> El {
    el().key("artwork-edit-change")
        .role("button")
        .label(t!("artwork.change").to_string())
        .focusable(true)
        .w_full()
        .py(space::SM)
        .radius(radius::SM)
        .fill(p.primary)
        .cursor(Cursor::Pointer)
        .hover(p.bg.alpha(fx::HOVER_FILL))
        .press(p.bg.alpha(fx::PRESS_FILL))
        .center()
        .child(text(t!("artwork.change").to_string()).color(p.bg))
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
                    // Header-only size read; a file we can't measure isn't croppable.
                    let Ok((src_w, src_h)) = image::image_dimensions(&path) else {
                        return;
                    };
                    let scale = FRAME / (src_w.min(src_h).max(1) as f32);
                    s.artwork_crop = Some(ArtworkCrop {
                        target: target.clone(),
                        source: path,
                        src_w,
                        src_h,
                        scale,
                        offset: (0.0, 0.0),
                    });
                    s.artwork_edit = None; // leave manage, enter crop
                },
            );
        })
}

/// The CROP dialogue: pan/zoom the chosen image behind the frame, then Set as
/// cover (store) or Cancel (back to the manage step).
fn crop_dialogue(s: &State, crop: &ArtworkCrop, p: &Palette, cx: &Cx<State>) -> El {
    // Zoom is a continuous slider (the same hand-built `slider` as seek/volume):
    // `-` at the left, `+` at the right as static end-caps, the value mapped to
    // the live scale between the cover scale and `MAX_ZOOM`× it.
    let dragging = s.dragging_slider.as_deref() == Some("artwork-crop-zoom");
    let zoom_row = row()
        .key("artwork-crop-zoom")
        .w_full()
        .center_y()
        .gap(space::SM)
        .child(icon(&icons::MINUS, 16.0).color(p.text_secondary))
        .child(slider(
            "artwork-crop-zoom",
            zoom01(crop),
            dragging,
            cx,
            p,
            set_zoom01,
            set_zoom01,
        ))
        .child(icon(&icons::PLUS, 16.0).color(p.text_secondary));

    let footer = row()
        .key("artwork-crop-footer")
        .w_full()
        .center_y()
        .justify_between()
        .child(footer_button(
            "artwork-crop-cancel",
            t!("common.cancel").to_string(),
            false,
            p,
            crop_cancel,
        ))
        .child(footer_button(
            "artwork-crop-confirm",
            t!("artwork.setCover").to_string(),
            true,
            p,
            confirm,
        ));

    col()
        .key("artwork-crop-modal")
        .absolute(0.0, 0.0)
        .w_full()
        .h_full()
        .center()
        .fill(Color::BLACK.alpha(0.45))
        .child(
            col()
                .key("artwork-crop-dialog")
                .fill(p.card)
                .radius(radius::LG)
                .border(1.0, p.border)
                .padding(space::LG)
                .gap(space::MD)
                .center()
                .modal(true, crop_cancel)
                .child(
                    text(t!("artwork.cropTitle").to_string())
                        .font(type_::TITLE)
                        .color(p.fg),
                )
                .child(
                    text(t!("artwork.cropHint").to_string())
                        .font(type_::CAPTION)
                        .color(p.text_secondary),
                )
                .child(crop_view(s, crop, p))
                .child(zoom_row)
                .child(footer),
        )
}

/// Confirm handler: crop the source to a square per the live pan/zoom, store it
/// (non-destructively) as the target's custom cover, invalidate the art cache
/// for the old + new cover path, and close. A crop that fails (unreadable /
/// undecodable source) just closes with no change.
fn confirm(s: &mut State) {
    // Read the small fields out of `artwork_crop` first so its shared borrow
    // ends before the `&mut covers` / `&mut art` borrows below.
    let Some(crop) = s.artwork_crop.as_ref() else {
        return;
    };
    let target = crop.target.clone();
    let source = crop.source.clone();
    let (src_w, src_h, scale, offset) = (crop.src_w, crop.src_h, crop.scale, crop.offset);

    if let Some(tmp) = crop_to_temp_png(&source, src_w, src_h, scale, offset) {
        // Invalidate both the old and new cover path in the art cache, so a
        // same-extension replace (overwritten in place) re-decodes instead of
        // serving the stale image (mirrors the old direct-set path).
        let old = target.cover_path(s);
        if target.set_cover(s, &tmp) {
            if let Some(old) = old {
                s.art.forget_path(&old);
            }
            if let Some(new) = target.cover_path(s) {
                s.art.forget_path(&new);
            }
        }
    }
    crate::close_artwork_crop(s);
}

/// A process-unique temp PNG path (the crop output). Keyed by pid + an atomic
/// counter so parallel crops never collide.
fn temp_png_path() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "soul_rune_artwork_crop_{}_{}.png",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Crop `source` to the square the live pan/zoom frames, resize it to
/// [`OUTPUT_PX`], and write it as a fresh temp PNG. Returns the temp path, or
/// `None` on any decode / encode failure (the caller then just closes).
///
/// The source-pixel crop rect is the inverse of the display mapping: the frame's
/// top-left in VIEW points is `MARGIN`, so the source pixel under it is
/// `(MARGIN - image_origin) / scale`, and the frame's `FRAME` points span
/// `FRAME / scale` source pixels. The side is shrunk if rounding would push it
/// past the image edge.
fn crop_to_temp_png(
    source: &Path,
    src_w: u32,
    src_h: u32,
    scale: f32,
    offset: (f32, f32),
) -> Option<PathBuf> {
    let disp_w = src_w as f32 * scale;
    let disp_h = src_h as f32 * scale;
    let img_left = HALF - disp_w / 2.0 + offset.0;
    let img_top = HALF - disp_h / 2.0 + offset.1;

    let sx = (((MARGIN - img_left) / scale).round().max(0.0)) as u32;
    let sy = (((MARGIN - img_top) / scale).round().max(0.0)) as u32;
    let mut side = (FRAME / scale).round() as u32;
    // Shrink the side so the rect stays inside the image on both axes.
    side = side
        .min(src_w.saturating_sub(sx))
        .min(src_h.saturating_sub(sy));
    if side == 0 {
        return None;
    }

    let img = image::open(source).ok()?;
    let cropped = img.crop_imm(sx, sy, side, side);
    let out = cropped.resize_exact(OUTPUT_PX, OUTPUT_PX, image::imageops::FilterType::Lanczos3);
    let tmp = temp_png_path();
    out.save_with_format(&tmp, image::ImageFormat::Png).ok()?;
    Some(tmp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::detail_page::ArtworkTarget;

    /// A real, decodable NxM RGB PNG written to a process-unique temp path, so
    /// the crop pipeline actually decodes + crops it (unlike the 8-byte-signature
    /// stub the cover-store tests use).
    fn write_real_png(w: u32, h: u32) -> PathBuf {
        let mut img = image::RgbImage::new(w, h);
        for (x, y, px) in img.enumerate_pixels_mut() {
            *px = image::Rgb([(x % 256) as u8, (y % 256) as u8, 128]);
        }
        let path = temp_png_path();
        img.save(&path).unwrap();
        path
    }

    fn crop(target: ArtworkTarget, src: PathBuf, w: u32, h: u32) -> ArtworkCrop {
        ArtworkCrop {
            target,
            source: src,
            src_w: w,
            src_h: h,
            scale: FRAME / (w.min(h).max(1) as f32),
            offset: (0.0, 0.0),
        }
    }

    #[test]
    fn min_scale_fills_the_frame_from_the_shorter_side() {
        let c = crop(
            ArtworkTarget::Album("A".into()),
            PathBuf::from("x"),
            400,
            300,
        );
        // The shorter side (300) maps to exactly FRAME.
        assert!((min_scale(&c) - FRAME / 300.0).abs() < 1e-6);
        // At the cover scale the image covers the frame on both axes.
        assert!(300.0 * min_scale(&c) >= FRAME - 1e-3);
        assert!(400.0 * min_scale(&c) >= FRAME - 1e-3);
    }

    #[test]
    fn offset_is_clamped_so_the_image_always_covers_the_frame() {
        let mut c = crop(
            ArtworkTarget::Album("A".into()),
            PathBuf::from("x"),
            400,
            300,
        );
        // A wild pan is clamped to the bound (disp/2 - HALF_FRAME) on each axis.
        c.offset = (10_000.0, -10_000.0);
        clamp_offset(&mut c);
        let bx = offset_bound(400.0 * c.scale);
        let by = offset_bound(300.0 * c.scale);
        assert!((c.offset.0 - bx).abs() < 1e-3);
        assert!((c.offset.1 + by).abs() < 1e-3);
        // The shorter (y) side exactly fills the frame → no vertical pan room.
        assert!(by.abs() < 1e-3);
    }

    #[test]
    fn zoom_slider_maps_0_to_the_cover_scale_and_1_to_the_ceiling() {
        let mut c = crop(
            ArtworkTarget::Album("A".into()),
            PathBuf::from("x"),
            400,
            300,
        );
        let floor = min_scale(&c);
        // At the cover scale the slider reads 0.
        assert!(zoom01(&c) < 1e-6, "cover scale -> slider 0");

        // 1 -> the MAX_ZOOM ceiling (and round-trips back to 1).
        apply_zoom01(&mut c, 1.0);
        assert!(
            (c.scale - floor * MAX_ZOOM).abs() < 1e-3,
            "slider 1 -> the ceiling"
        );
        assert!((zoom01(&c) - 1.0).abs() < 1e-6, "ceiling -> slider 1");

        // 0.5 sits strictly between floor and ceiling.
        apply_zoom01(&mut c, 0.5);
        assert!(
            c.scale > floor && c.scale < floor * MAX_ZOOM,
            "slider 0.5 is between"
        );

        // 0 returns to the cover scale.
        apply_zoom01(&mut c, 0.0);
        assert!(
            (c.scale - floor).abs() < 1e-4,
            "slider 0 -> the cover scale"
        );

        // Out-of-range values are clamped to the ceiling.
        apply_zoom01(&mut c, 5.0);
        assert!(
            (c.scale - floor * MAX_ZOOM).abs() < 1e-3,
            "a value above 1 clamps to the ceiling"
        );
    }

    #[test]
    fn crop_writes_a_decodable_square_png() {
        let src = write_real_png(400, 300);
        let out = crop_to_temp_png(&src, 400, 300, FRAME / 300.0, (0.0, 0.0))
            .expect("the crop pipeline decodes + crops + encodes");
        assert_eq!(out.extension().unwrap(), "png");
        let decoded = image::open(&out).expect("the output PNG decodes");
        assert_eq!(
            (decoded.width(), decoded.height()),
            (OUTPUT_PX, OUTPUT_PX),
            "the stored cover is a square at the output size"
        );
    }
}
