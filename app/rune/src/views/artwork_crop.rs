//! The "Edit Artwork" CROP dialogue (the old Soul Player's cover-crop step):
//! after a file is picked, the user PANS (drag) and ZOOMS (+/- buttons) the
//! picked image behind a fixed frame — a CIRCLE for an artist avatar, a SQUARE
//! for an album cover — then confirms to crop + store it.
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

use rune::{Color, Cursor, Drag, El, Phase, SurfaceFit, col, el, icon, row, text};

use crate::State;
use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::views::detail_page::ArtworkCrop;
use crate::views::icons;

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
/// One zoom step.
const ZOOM_STEP: f32 = 1.2;
/// The deepest zoom-in, as a multiple of the cover (min) scale.
const MAX_ZOOM: f32 = 6.0;
/// The cropped cover is re-encoded at this side, in pixels — generous for the
/// 176 pt detail hero and the grid cards while staying a small PNG.
const OUTPUT_PX: u32 = 512;

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

/// Multiply the scale by `factor`, clamped to `[min_scale, MAX_ZOOM × min_scale]`
/// (so the image never shrinks below frame coverage nor zooms past the ceiling),
/// then re-clamp the pan to the new scale's bounds.
fn apply_zoom(crop: &mut ArtworkCrop, factor: f32) {
    let min = min_scale(crop);
    crop.scale = (crop.scale * factor).clamp(min, min * MAX_ZOOM);
    clamp_offset(crop);
}

/// Zoom out one step (never below the cover scale).
fn zoom_out(s: &mut State) {
    if let Some(crop) = s.artwork_crop.as_mut() {
        apply_zoom(crop, 1.0 / ZOOM_STEP);
    }
}

/// Zoom in one step (never past `MAX_ZOOM` × the cover scale).
fn zoom_in(s: &mut State) {
    if let Some(crop) = s.artwork_crop.as_mut() {
        apply_zoom(crop, ZOOM_STEP);
    }
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

/// A round zoom button (`-` / `+`).
fn zoom_button(
    key: &str,
    label: String,
    glyph: &'static rune::Icon,
    p: &Palette,
    on_press: impl Fn(&mut State) + 'static,
) -> El {
    el().key(key.to_string())
        .role("button")
        .label(label)
        .focusable(true)
        .size(36.0, 36.0)
        .center()
        .radius(radius::FULL)
        .fill(p.muted)
        .cursor(Cursor::Pointer)
        .hover(p.fg.alpha(fx::HOVER))
        .press(p.fg.alpha(fx::PRESS))
        .child(icon(glyph, 18.0).color(p.text_secondary))
        .on_press(on_press)
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

/// The crop dialogue, mounted in `main.rs`'s overlay layer. `None` while
/// `State::artwork_crop` is `None` (closed).
pub fn overlay(s: &State, p: &Palette) -> Option<El> {
    let crop = s.artwork_crop.as_ref()?;

    let zoom_row = row()
        .key("artwork-crop-zoom")
        .center()
        .gap(space::MD)
        .child(zoom_button(
            "artwork-crop-zoom-out",
            t!("artwork.zoomOut").to_string(),
            &icons::MINUS,
            p,
            zoom_out,
        ))
        .child(zoom_button(
            "artwork-crop-zoom-in",
            t!("artwork.zoomIn").to_string(),
            &icons::PLUS,
            p,
            zoom_in,
        ));

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
            |st: &mut State| crate::close_artwork_crop(st),
        ))
        .child(footer_button(
            "artwork-crop-confirm",
            t!("artwork.setCover").to_string(),
            true,
            p,
            confirm,
        ));

    Some(
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
                    .modal(true, crate::close_artwork_crop)
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
            ),
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
    fn zoom_in_grows_and_zoom_out_floors_at_the_cover_scale() {
        let mut c = crop(
            ArtworkTarget::Album("A".into()),
            PathBuf::from("x"),
            400,
            300,
        );
        let floor = min_scale(&c);
        apply_zoom(&mut c, ZOOM_STEP);
        assert!(c.scale > floor, "zoom in grows scale");
        // Many zoom-outs can't drop below the cover scale.
        for _ in 0..20 {
            apply_zoom(&mut c, 1.0 / ZOOM_STEP);
        }
        assert!(
            (c.scale - floor).abs() < 1e-4,
            "zoom out floors at the cover scale"
        );
        // Many zoom-ins can't exceed the MAX_ZOOM ceiling.
        for _ in 0..40 {
            apply_zoom(&mut c, ZOOM_STEP);
        }
        assert!(
            (c.scale - floor * MAX_ZOOM).abs() < 1e-3,
            "zoom in caps at the ceiling"
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
