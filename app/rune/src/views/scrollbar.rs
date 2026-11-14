//! A thin themed overlay scrollbar for a vertical scroll viewport.
//!
//! Rune's engine has no built-in persistent scrollbar — it only tracks the OS
//! "show scroll bars" preference and overlays a fading scroller while scrolling.
//! This draws an always-present thumb on a viewport's right edge instead: mount
//! [`overlay`] as a child of the scroll viewport and feed its [`rune::Live`] from
//! the viewport's [`rune::El::on_scroll_offset`] through [`thumb_rect`]. The thumb
//! is bound by [`rune::El::live_rect`], so it follows the scroll in the same frame
//! without re-running the view, and it hides (zero size) when nothing overflows.
#![forbid(unsafe_code)]

use rune::{El, Live, ScrollPos, el};

use crate::theme::Palette;

/// Thumb thickness, in points.
const BAR_W: f32 = 6.0;
/// Inset from the viewport's top, bottom and right edges, in points.
const INSET: f32 = 4.0;
/// Shortest the thumb is ever drawn, so very long content stays grabbable.
const MIN_THUMB: f32 = 36.0;

/// The thumb rect `[x, y, w, h]` in the viewport's own coordinates for a scroll
/// position, or a zero rect when the content fits and there is nothing to scroll.
///
/// The thumb's length tracks the visible fraction of the content (viewport /
/// content height), clamped to [`MIN_THUMB`]; its position tracks the scroll
/// fraction, parked at the top inset at rest and reaching the bottom inset at the
/// end of travel. It sits in the right gutter, [`INSET`] in from the edge.
pub(crate) fn thumb_rect(at: ScrollPos) -> [f32; 4] {
    let vw = at.viewport[0];
    let vh = at.viewport[1];
    let max_y = at.max[1];
    // No vertical overflow (or an unsized viewport) → nothing to show.
    if max_y <= 0.5 || vh <= 0.0 || vw <= 0.0 {
        return [0.0; 4];
    }
    let content_h = vh + max_y;
    let track_h = (vh - 2.0 * INSET).max(0.0);
    let thumb_h = (track_h * vh / content_h).clamp(MIN_THUMB.min(track_h), track_h);
    let frac = (at.offset[1] / max_y).clamp(0.0, 1.0);
    let thumb_y = INSET + frac * (track_h - thumb_h);
    let thumb_x = vw - BAR_W - INSET;
    [thumb_x, thumb_y, BAR_W, thumb_h]
}

/// The scrollbar element: an absolutely-positioned thumb bound to `thumb` (set
/// from the viewport's `on_scroll_offset` via [`thumb_rect`]). Mount it as the
/// last child of the scroll viewport — `live_rect` pins it to the viewport frame,
/// so it stays put while the content scrolls underneath.
pub(crate) fn overlay(thumb: &Live<[f32; 4]>, key: &str, p: &Palette) -> El {
    el().key(key.to_string())
        .absolute(0.0, 0.0)
        .live_rect(thumb)
        .fill(p.text_secondary.alpha(0.45))
        .radius(BAR_W / 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `ScrollPos` (it is `#[non_exhaustive]`, so start from `Default`).
    fn pos(viewport: [f32; 2], max: [f32; 2], offset: [f32; 2]) -> ScrollPos {
        let mut p = ScrollPos::default();
        p.viewport = viewport;
        p.max = max;
        p.offset = offset;
        p
    }

    #[test]
    fn no_thumb_when_content_fits() {
        assert_eq!(
            thumb_rect(pos([800.0, 400.0], [0.0, 0.0], [0.0, 0.0])),
            [0.0; 4]
        );
    }

    #[test]
    fn thumb_parks_at_the_top_inset_when_scrolled_to_top() {
        // Content is twice the viewport, scrolled to the top.
        let r = thumb_rect(pos([800.0, 400.0], [0.0, 400.0], [0.0, 0.0]));
        assert_eq!(r[0], 800.0 - BAR_W - INSET, "x hugs the right gutter");
        assert_eq!(r[2], BAR_W, "bar thickness");
        assert_eq!(r[1], INSET, "parked at the top inset");
        // track = 400 - 8 = 392; content = 800; thumb = 392 * 400/800 = 196.
        assert!(
            (r[3] - 196.0).abs() < 0.5,
            "thumb ~half the track, got {}",
            r[3]
        );
    }

    #[test]
    fn thumb_reaches_the_bottom_inset_when_scrolled_to_max() {
        let r = thumb_rect(pos([800.0, 400.0], [0.0, 400.0], [0.0, 400.0]));
        // Bottom of the thumb = viewport bottom minus the inset (400 - 4).
        assert!(
            (r[1] + r[3] - (400.0 - INSET)).abs() < 0.5,
            "thumb bottom at vh - inset, got {}",
            r[1] + r[3]
        );
    }

    #[test]
    fn thumb_never_shorter_than_the_minimum() {
        // Enormous content would give a sub-pixel thumb; clamp it to MIN_THUMB.
        let r = thumb_rect(pos([800.0, 400.0], [0.0, 40_000.0], [0.0, 0.0]));
        assert!(
            (r[3] - MIN_THUMB).abs() < 0.5,
            "clamped to the minimum, got {}",
            r[3]
        );
    }
}
