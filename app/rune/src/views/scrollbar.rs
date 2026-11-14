//! A thin themed overlay scrollbar for a vertical scroll viewport.
//!
//! Rune's engine has no built-in persistent scrollbar — it only tracks the OS
//! "show scroll bars" preference and overlays a fading scroller while scrolling.
//! This draws a thumb on a viewport's right edge instead: [`attach`] wires the
//! viewport's [`rune::El::on_scroll_offset`] to a [`rune::Live`] the thumb reads
//! through [`rune::El::live_rect`], so the thumb follows the scroll in the same
//! frame without re-running the view, and it hides (zero size) when nothing
//! overflows. The thumb is also a **drag handle**, and it is shown only while the
//! list is hovered *or* the thumb is being dragged — so it stays up for a whole
//! drag even if the pointer wanders off the list.
#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::collections::HashMap;

use rune::{El, Live, Phase, ScrollPos, Spring, el};

use crate::theme::Palette;

/// Thumb thickness, in points.
const BAR_W: f32 = 6.0;
/// Inset from the viewport's top, bottom and right edges, in points.
const INSET: f32 = 4.0;
/// Shortest the thumb is ever drawn, so very long content stays grabbable.
const MIN_THUMB: f32 = 36.0;
/// Reveal/hide fade for the thumb's opacity.
const FADE: Spring = Spring::new(0.14, 0.0);

/// Per-scrollbar visibility: whether the container is hovered, whether the thumb
/// is held in a drag, and the opacity [`Live`] those two drive — `1` while hovered
/// **or** dragging, else `0`. Driving opacity ourselves (rather than
/// `group_opacity`) is what lets a drag keep the thumb up after the pointer leaves
/// the list, since Rune's hover follows the real pointer even mid-drag.
struct VisState {
    hover: bool,
    drag: bool,
    opacity: Live<f32>,
}

thread_local! {
    /// The most recent scroll geometry per thumb key, written by each viewport's
    /// `on_scroll_offset`. The drag handler reads it to turn pointer travel into a
    /// scroll offset (it needs the live `max` and track length, which aren't on the
    /// `Drag`). At most one entry per scroll view.
    static SCROLL_GEOM: RefCell<HashMap<String, ScrollPos>> = RefCell::new(HashMap::new());
    /// Visibility state per thumb key (see [`VisState`]).
    static VIS: RefCell<HashMap<String, VisState>> = RefCell::new(HashMap::new());
}

/// The opacity [`Live`] for `key`, creating its [`VisState`] (hidden) on first use.
fn vis_opacity(key: &str) -> Live<f32> {
    VIS.with(|m| {
        m.borrow_mut()
            .entry(key.to_string())
            .or_insert_with(|| VisState {
                hover: false,
                drag: false,
                opacity: Live::new(0.0),
            })
            .opacity
            .clone()
    })
}

/// Update `key`'s hover and/or drag flag and push the resulting target opacity to
/// its [`Live`] (`1` while hovered or dragging, else `0`).
fn set_vis(key: &str, hover: Option<bool>, drag: Option<bool>) {
    VIS.with(|m| {
        let mut b = m.borrow_mut();
        let v = b.entry(key.to_string()).or_insert_with(|| VisState {
            hover: false,
            drag: false,
            opacity: Live::new(0.0),
        });
        if let Some(h) = hover {
            v.hover = h;
        }
        if let Some(d) = drag {
            v.drag = d;
        }
        v.opacity.set(if v.hover || v.drag { 1.0 } else { 0.0 });
    });
}

/// How far the thumb can travel along the track — the thumb's own movement range,
/// which maps 1:1 to the pointer and spans the whole `[0, max]` scroll range. Zero
/// when the content fits or the thumb fills the track (nothing to drag).
fn thumb_travel(at: ScrollPos) -> f32 {
    let vh = at.viewport[1];
    let max_y = at.max[1];
    if max_y <= 0.5 || vh <= 0.0 {
        return 0.0;
    }
    let content_h = vh + max_y;
    let track_h = (vh - 2.0 * INSET).max(0.0);
    let thumb_h = (track_h * vh / content_h).clamp(MIN_THUMB.min(track_h), track_h);
    (track_h - thumb_h).max(0.0)
}

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

/// The scrollbar thumb: a pill bound to `thumb` via `live_rect` (set from the
/// viewport's `on_scroll_offset` by [`attach`], which folds the scroll offset back
/// into `y` so it rides the viewport frame, not the content).
///
/// Its opacity is bound to `key`'s [`VisState`] [`Live`] (smoothed), so it fades in
/// while the list is hovered or the thumb is dragged and fades out otherwise.
pub(crate) fn overlay(thumb: &Live<[f32; 4]>, key: &str, p: &Palette) -> El {
    el().key(key.to_string())
        .absolute(0.0, 0.0)
        .live_rect(thumb)
        .fill(p.text_secondary.alpha(0.45))
        .radius(BAR_W / 2.0)
        .live_opacity(vis_opacity(key).smooth(FADE))
}

/// Attach a themed scrollbar to a vertical scroll `viewport` (keyed
/// `viewport_key`, so the drag handler can scroll it): observe the viewport's
/// hover to reveal the thumb, wire its `on_scroll_offset` to drive `thumb` through
/// [`thumb_rect`], and mount the thumb [`overlay`] as its last child. `viewport`
/// must already be a `.scroll()` viewport; `key` is the thumb's stable key.
///
/// The overlay is a **child** of the viewport, so the scroll transform shifts it
/// by the scroll offset; `attach` adds that offset back into the thumb's `y` so
/// it stays pinned to the viewport frame (`drawn = content_y − offset`) rather
/// than being dragged off the top as the content scrolls. Keeping it a child —
/// not a sibling in a wrapper — leaves the viewport's own layout (and any
/// `max_w`) exactly as the caller built it.
///
/// Visibility is driven by [`set_vis`]: the viewport's `on_hover_change` sets the
/// hover flag and the thumb's `on_drag` sets the drag flag, so the bar shows while
/// the list is hovered **or** the thumb is being dragged — it doesn't fade out if a
/// drag carries the pointer off the list.
///
/// The thumb is a **drag handle**: `on_drag` maps the pointer's travel since the
/// press (`Drag::dy`, which is independent of the thumb moving under it) to a
/// scroll offset over the whole `[0, max]` range and scrolls the viewport to it.
pub(crate) fn attach(
    viewport: El,
    viewport_key: &str,
    thumb: &Live<[f32; 4]>,
    key: &str,
    p: &Palette,
) -> El {
    let cb = thumb.clone();
    let geom_key = key.to_string();
    let drag_key = key.to_string();
    let hover_key = key.to_string();
    let vkey = viewport_key.to_string();
    viewport
        .on_hover_change_any(move |inside, _tx| set_vis(&hover_key, Some(inside), None))
        .on_scroll_offset(move |at| {
            SCROLL_GEOM.with(|m| {
                m.borrow_mut().insert(geom_key.clone(), at);
            });
            let mut r = thumb_rect(at);
            // Counteract the scroll transform applied to this child overlay so it
            // rides the viewport frame, not the content. A zero rect (nothing to
            // scroll) stays put.
            if r[2] > 0.0 {
                r[1] += at.offset[1];
            }
            cb.set(r);
        })
        .child(overlay(thumb, key, p).on_drag_any(move |d, tx| {
            // Mark the drag so the thumb stays visible across the whole gesture,
            // even if the pointer leaves the list.
            match d.phase {
                Phase::Down => set_vis(&drag_key, None, Some(true)),
                Phase::Up => set_vis(&drag_key, None, Some(false)),
                Phase::Move => {}
            }
            let Some(at) = SCROLL_GEOM.with(|m| m.borrow().get(&drag_key).copied()) else {
                return;
            };
            let max_y = at.max[1];
            let travel = thumb_travel(at);
            if max_y <= 0.5 || travel <= 0.5 {
                return; // nothing to scroll / thumb fills the track
            }
            match d.phase {
                // Anchor the drag to the scroll offset at the press; `dy` is
                // measured from here.
                Phase::Down => d.data = at.offset[1],
                Phase::Move | Phase::Up => {
                    let offset = (d.data + d.dy * max_y / travel).clamp(0.0, max_y);
                    tx.scroll_to_instant(vkey.clone(), 0.0, offset);
                }
            }
        }))
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
