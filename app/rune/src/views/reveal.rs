//! Reusable sequential fade + collapse "reveal" animation.
//!
//! Drive a `0.0..=1.0` progress per element (1 = fully shown, 0 = fully gone) with
//! [`tick`] each poll frame, and read [`opacity`] / [`height_factor`] to render it.
//! Going DOWN it fades the element out **at full height first** and only THEN
//! collapses the space — so a leaving item never overlaps the content reflowing
//! past it (the overlap the naive remove-and-reflow produces). Going UP it opens
//! the space then fades in. This mirrors the cross-framework "defer removal until
//! the exit animation finishes" pattern (Framer `AnimatePresence`, RTG `onExited`,
//! Vue `after-leave`, Angular `animationComplete`, SwiftUI `withAnimation` compl.).
//!
//! It's driven MANUALLY from the frame clock (like the title-bar `scan_anim`)
//! rather than from a per-element spring, so it animates no matter how the
//! framework commits the event that toggled visibility — a pointer/`on_press`
//! commit lands row-height changes instantly, which a spring-based fold can't
//! survive, but a progress we step ourselves every frame always glides.
//!
//! Drive [`tick`] with the per-frame delta from [`frame_delta`], NOT with the raw
//! value Rune hands `poll`: that argument is the ABSOLUTE clock (seconds since
//! launch), not a delta, so feeding it straight in would step the whole animation
//! in a single frame — a snap, not a glide.
#![forbid(unsafe_code)]

/// Seconds for a full reveal or hide (fade phase + collapse phase together).
pub const SECS: f32 = 0.22;

/// The split point of the `0..1` range between the fade half and the collapse
/// half. Above it the element is at full height and its opacity is moving; below
/// it the element is invisible and its height is moving.
const SPLIT: f32 = 0.5;

/// The largest per-frame step, in seconds, [`frame_delta`] will report. The app
/// renders on demand, so the first frame after an idle (or after launch) carries a
/// large jump in the absolute clock; capping it means the reveal resumes one
/// frame's worth and then rides the ~60 Hz wake-ups to completion, instead of
/// teleporting straight to its end. Also bounds a debugger/App-Nap hitch, matching
/// Rune's own `MAX_GAP` philosophy for motion.
const MAX_STEP: f32 = 1.0 / 30.0;

/// Turns Rune's absolute poll clock `now` (seconds since launch) into the real time
/// the current frame covered, clamped to [`MAX_STEP`] so an idle gap can't teleport
/// the animation. Keep one `last` cell per driver and pass it every frame; it holds
/// the previous reading. A non-advancing or backward clock yields `0.0`, never a
/// negative step.
pub fn frame_delta(now: f64, last: &mut f64) -> f32 {
    let dt = (now - *last).max(0.0) as f32;
    *last = now;
    dt.min(MAX_STEP)
}

/// Advance `progress` toward its target (1.0 when `shown`, else 0.0) by one frame
/// of `dt` seconds. Returns `true` while still animating, so the caller's poll can
/// request another frame and keep the glide going.
pub fn tick(progress: &mut f32, shown: bool, dt: f32) -> bool {
    let target = if shown { 1.0 } else { 0.0 };
    if *progress == target {
        return false;
    }
    let step = (dt / SECS).max(0.0);
    *progress = if target > *progress {
        (*progress + step).min(target)
    } else {
        (*progress - step).max(target)
    };
    *progress != target
}

/// Opacity for `progress`: fades over the upper half of the range (1.0 at p=1,
/// reaching 0 by p=`SPLIT`), so a leaving element is fully transparent before its
/// height starts to collapse.
pub fn opacity(progress: f32) -> f32 {
    ((progress - SPLIT) / (1.0 - SPLIT)).clamp(0.0, 1.0)
}

/// Height factor (0..1 of the element's natural height) for `progress`: full over
/// the upper half, collapsing to 0 over the lower half — the inverse phase to
/// [`opacity`], so the space only closes once the element is invisible.
pub fn height_factor(progress: f32) -> f32 {
    (progress / SPLIT).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequential_fade_then_collapse_on_the_way_down() {
        // Fully shown.
        assert_eq!(opacity(1.0), 1.0);
        assert_eq!(height_factor(1.0), 1.0);
        // Upper half: fading, still full height (no overlap while it fades).
        assert!(opacity(0.75) > 0.0 && opacity(0.75) < 1.0);
        assert_eq!(height_factor(0.75), 1.0, "height holds while fading");
        // Split: faded out, still full height.
        assert_eq!(opacity(0.5), 0.0);
        assert_eq!(height_factor(0.5), 1.0);
        // Lower half: invisible, now collapsing.
        assert_eq!(opacity(0.25), 0.0, "stays invisible while collapsing");
        assert!(height_factor(0.25) > 0.0 && height_factor(0.25) < 1.0);
        // Gone.
        assert_eq!(opacity(0.0), 0.0);
        assert_eq!(height_factor(0.0), 0.0);
    }

    #[test]
    fn tick_advances_toward_target_and_reports_animating() {
        let mut p = 1.0;
        // Hiding: steps down, reports animating until it reaches 0.
        assert!(tick(&mut p, false, SECS / 4.0));
        assert!(p < 1.0 && p > 0.0);
        // A big step clamps to the target and reports settled.
        let still = tick(&mut p, false, SECS);
        assert_eq!(p, 0.0);
        assert!(!still, "settled at the target → not animating");
        // Showing again reverses from wherever it is (interrupt-safe).
        assert!(tick(&mut p, true, SECS / 4.0));
        assert!(p > 0.0);
    }

    #[test]
    fn already_at_target_is_not_animating() {
        let mut shown = 1.0;
        assert!(!tick(&mut shown, true, SECS));
        let mut gone = 0.0;
        assert!(!tick(&mut gone, false, SECS));
    }

    #[test]
    fn frame_delta_is_a_clamped_per_frame_step_from_the_absolute_clock() {
        // Rune hands `poll` the ABSOLUTE clock, not a per-frame delta. `frame_delta`
        // turns a sequence of clock readings into the real time each frame covered.
        let mut last = 0.0;
        // First reading after launch: the clock is already seconds in (frames ran
        // before the toggle). One frame must advance at most a slice — never swallow
        // the whole 5 s and teleport the animation to its end.
        let d0 = frame_delta(5.0, &mut last);
        assert!(
            d0 > 0.0 && d0 <= MAX_STEP,
            "first step clamped, not 5 s: {d0}"
        );
        assert_eq!(last, 5.0, "remembers the clock it last saw");
        // Steady ~60 Hz frames: the delta is the real gap (well under the cap).
        let d1 = frame_delta(5.0 + 1.0 / 60.0, &mut last);
        assert!((d1 - 1.0 / 60.0).abs() < 1e-6, "tracks the real gap: {d1}");
        // A long idle gap (on-demand rendering slept) is capped to one frame.
        let d2 = frame_delta(12.0, &mut last);
        assert!(d2 <= MAX_STEP, "idle gap capped, no teleport: {d2}");
        // A repeated/non-advancing clock yields no step (never negative).
        assert_eq!(frame_delta(12.0, &mut last), 0.0);
    }
}
