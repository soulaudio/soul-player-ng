//! A hand-built slider (seek, volume). Rune's pre-release ships no `Slider`
//! widget (cheat-sheet §6: confirmed absent from both `rune` and `rune-kit`),
//! so this ports `examples/settings`'s animation-speed slider pattern
//! (`examples/settings/src/main.rs:285-310`) almost verbatim: a track, a
//! filled portion proportional to the value, a thumb, `role("slider")` +
//! `range_value`/`range_step` for accessibility, keyboard stepping via
//! `on_step`, and pointer dragging via `on_drag`.
//!
//! Unlike the ported example (a fixed 180pt slider), this one fills its row
//! (`.grow(1.0)` below) instead of taking a caller-computed pixel width. The
//! track/fill/knob still need a real pixel width to place themselves (the
//! track uses `.w_full()`/percent and needs none, but the fill's width and
//! the knob's offset are `value01` fractions of the *travel* distance, which
//! is a point quantity). Two different "real width" sources cover the two
//! different needs:
//! - Rendering every frame: `Cx::layout_size(key)` reads this element's own
//!   last-committed laid-out size (`crates/rune/src/runtime/layout_read.rs`),
//!   which "settles instead of looping" within the same commit per that
//!   file's doc comment — so it already holds the real grown width by the
//!   time anything is shown, not just the frame after.
//! - Pointer drags: `Drag::w`, "in the element's own layout points... as
//!   drawn" (`crates/rune/src/el.rs:109-111`), is the width at the exact
//!   moment of the gesture, so a resize mid-drag (or any lag in the above
//!   read) still maps the pointer correctly.
#![forbid(unsafe_code)]

use rune::tok::motion;
use rune::{Cursor, Cx, Drag, El, Phase, el};

use crate::State;
use crate::theme::Palette;

/// Fallback slider width for the one frame (if any) before this key has any
/// committed layout to read back — e.g. the very first frame of a headless
/// `--snap`. Picked close to the ported example's own fixed 180pt so that
/// frame, if ever visible, still looks like a real slider rather than a
/// sliver.
const FALLBACK_W: f32 = 160.0;

/// `value01` (clamped 0..1) renders at height 22 pt (the ported example's
/// own number) and fills its row's width. Two callbacks split the gesture:
/// `on_scrub` fires on the initial track press and every `Phase::Move` — a
/// live preview, so the caller updates the rendered value as the pointer
/// moves (volume applies it straight away; the seek bar previews a scrub
/// position without seeking). `on_commit` fires once on `Phase::Up` with the
/// dropped value — the final action (the actual `Seek`). Keyboard (Tab to
/// focus, then arrows/Page Up-Down/Home/End) steps via `on_commit`, as in the
/// ported example. `dragging` (the caller passes whether this slider is the
/// one being dragged) switches the cursor to a horizontal-resize affordance
/// for the duration of the drag only. Both callbacks are `Clone` (not boxed)
/// so they can be shared across the keyboard and pointer handlers without a
/// `dyn Fn`.
pub fn slider(
    key: &str,
    value01: f32,
    dragging: bool,
    cx: &Cx<State>,
    p: &Palette,
    on_scrub: impl Fn(&mut State, f32) + Clone + 'static,
    on_commit: impl Fn(&mut State, f32) + Clone + 'static,
) -> El {
    let (h, k, bar) = (22.0, 18.0, 4.0);
    let v = value01.clamp(0.0, 1.0);
    // See the module doc: the real width this element rendered at last
    // commit, for sizing the fill/knob this frame.
    let w = cx
        .layout_size(key)
        .map(|size| size[0])
        .filter(|w| *w > 0.0)
        .unwrap_or(FALLBACK_W)
        .max(k + 1.0);
    let travel = w - k;

    let key_down = key.to_string();
    let step_commit = on_commit.clone();
    let scrub = on_scrub;
    let commit = on_commit;
    el().key(key)
        .role("slider")
        .label(key)
        .range_value(f64::from(v), 0.0, 1.0)
        .range_step(0.01)
        .a11y_value(format!("{:.0}%", v * 100.0))
        // A horizontal-drag cursor, but only while this slider is actually being
        // dragged — hovering keeps the plain pointer, so the cursor change reads
        // as "you are dragging" rather than a persistent hint.
        .cursor(if dragging {
            Cursor::ResizeColumn
        } else {
            Cursor::Default
        })
        .on_step(move |s: &mut State, steps, _| {
            step_commit(s, (v + steps as f32 * 0.01).clamp(0.0, 1.0));
        })
        .h(h)
        .grow(1.0)
        // The old app's ProgressBar hides its thumb at rest and fades it in on
        // hover/drag (`opacity: isHovering || isDragging ? 1 : 0`); `hover_group`
        // here drives the knob's `group_opacity` below.
        .hover_group()
        .motion(motion::SNAPPY)
        .on_drag(move |s: &mut State, d: &mut Drag, tx| {
            // The gesture's own width (see module doc), not the value this
            // element rendered at: correct even if a resize landed between
            // the last render and this event.
            let drag_travel = (d.w - k).max(1.0);
            let drag_to_value = move |x: f32| ((x - k / 2.0) / drag_travel).clamp(0.0, 1.0);
            // How far the pointer sits from the knob's centre at press, so dragging
            // the knob itself doesn't snap it under the pointer. Zero for a track
            // press (the value jumps to the pointer).
            let grab = move |d: &Drag| {
                let knob = k / 2.0 + d.data * drag_travel;
                if (d.start_x - knob).abs() <= k / 2.0 + 2.0 {
                    d.start_x - knob
                } else {
                    0.0
                }
            };
            match d.phase {
                Phase::Down => {
                    d.data = v;
                    s.dragging_slider = Some(key_down.clone());
                    let knob = k / 2.0 + v * drag_travel;
                    if (d.x - knob).abs() > k / 2.0 + 2.0 {
                        // Pressed the track (not the knob): preview-jump there.
                        scrub(s, drag_to_value(d.x));
                    }
                    tx.instant();
                }
                Phase::Move => {
                    scrub(s, drag_to_value(d.x - grab(d)));
                    tx.instant();
                }
                Phase::Up => {
                    s.dragging_slider = None;
                    let final_v = if d.cancelled() {
                        d.data
                    } else {
                        drag_to_value(d.x - grab(d))
                    };
                    commit(s, final_v);
                }
            }
        })
        .child(
            // Percent width: always exactly this element's own width, with
            // no dependency on the `w` read above settling.
            el().key(format!("{key}-track"))
                .w_full()
                .h(bar)
                .absolute(0.0, (h - bar) / 2.0)
                .radius(bar / 2.0)
                .fill(p.muted)
                .a11y_hidden(),
        )
        .child(
            // Filled portion is `bg-primary`, as the old app's ProgressBar.
            el().key(format!("{key}-fill"))
                .size(k / 2.0 + v * travel, bar)
                .absolute(0.0, (h - bar) / 2.0)
                .radius(bar / 2.0)
                .fill(p.primary)
                .a11y_hidden(),
        )
        .child(
            // The thumb (`bg-primary`), hidden at rest and faded in on hover/drag
            // via `group_opacity` (old app's `opacity: isHovering ? 1 : 0`).
            el().key(format!("{key}-knob"))
                .size(k, k)
                .absolute(v * travel, (h - k) / 2.0)
                .radius(k / 2.0)
                .fill(p.primary)
                .shadow(1.0, 2.0, p.border)
                .group_opacity(0.0, 1.0)
                .a11y_hidden(),
        )
}
