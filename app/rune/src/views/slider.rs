//! A hand-built slider (seek, volume). Rune's pre-release ships no `Slider`
//! widget (cheat-sheet §6: confirmed absent from both `rune` and `rune-kit`),
//! so this ports `examples/settings`'s animation-speed slider pattern
//! (`examples/settings/src/main.rs:285-310`) almost verbatim: a track, a
//! filled portion proportional to the value, a thumb, `role("slider")` +
//! `range_value`/`range_step` for accessibility, keyboard stepping via
//! `on_step`, and pointer dragging via `on_drag`.
#![forbid(unsafe_code)]

use rune::tok::motion;
use rune::{Drag, El, Phase, el};

use crate::State;
use crate::theme::Palette;

/// `value01` (clamped 0..1) renders at width `w`, height 22 pt (the ported
/// example's own numbers). Dragging always commits the dropped-at value on
/// `Phase::Up`; passing `live = true` also commits on every `Phase::Move`,
/// for a control that should track the pointer continuously (volume).
/// `live = false` matches the old app's seek bar: no scrubbing feedback, the
/// seek lands once on release. Keyboard (Tab to focus, then arrows/Page
/// Up-Down/Home/End) always steps and commits immediately, as in the ported
/// example. `on_commit` is `Clone` (not boxed) so the same callback can be
/// shared between the keyboard and pointer handlers without a `dyn Fn`.
pub fn slider(
    key: &str,
    value01: f32,
    w: f32,
    live: bool,
    p: &Palette,
    on_commit: impl Fn(&mut State, f32) + Clone + 'static,
) -> El {
    let (h, k, bar) = (22.0, 18.0, 4.0);
    let travel = w - k;
    let v = value01.clamp(0.0, 1.0);
    let to_value = move |x: f32| ((x - k / 2.0) / travel).clamp(0.0, 1.0);

    let step_commit = on_commit.clone();
    let drag_commit = on_commit;
    el().key(key)
        .role("slider")
        .label(key)
        .range_value(f64::from(v), 0.0, 1.0)
        .range_step(0.01)
        .a11y_value(format!("{:.0}%", v * 100.0))
        .on_step(move |s: &mut State, steps, _| {
            step_commit(s, (v + steps as f32 * 0.01).clamp(0.0, 1.0));
        })
        .size(w, h)
        .motion(motion::SNAPPY)
        .on_drag(move |s: &mut State, d: &mut Drag, tx| match d.phase {
            Phase::Down => {
                d.data = v;
                let knob = k / 2.0 + v * travel;
                if (d.x - knob).abs() > k / 2.0 + 2.0 {
                    // Clicked the track (not the knob): jump straight there.
                    drag_commit(s, to_value(d.x));
                }
            }
            Phase::Move => {
                if live {
                    let knob = k / 2.0 + d.data * travel;
                    let grab = if (d.start_x - knob).abs() <= k / 2.0 + 2.0 {
                        d.start_x - knob
                    } else {
                        0.0
                    };
                    drag_commit(s, to_value(d.x - grab));
                    tx.instant();
                }
            }
            Phase::Up => {
                if d.cancelled() {
                    if live {
                        drag_commit(s, d.data);
                    }
                } else {
                    let knob = k / 2.0 + d.data * travel;
                    let grab = if (d.start_x - knob).abs() <= k / 2.0 + 2.0 {
                        d.start_x - knob
                    } else {
                        0.0
                    };
                    drag_commit(s, to_value(d.x - grab));
                }
            }
        })
        .child(
            el().key("track")
                .size(w, bar)
                .absolute(0.0, (h - bar) / 2.0)
                .radius(bar / 2.0)
                .fill(p.muted)
                .a11y_hidden(),
        )
        .child(
            el().key("fill")
                .size(k / 2.0 + v * travel, bar)
                .absolute(0.0, (h - bar) / 2.0)
                .radius(bar / 2.0)
                .fill(p.accent)
                .a11y_hidden(),
        )
        .child(
            el().key("knob")
                .size(k, k)
                .absolute(v * travel, (h - k) / 2.0)
                .radius(k / 2.0)
                .fill(p.fg)
                .shadow(1.0, 2.0, p.border)
                .a11y_hidden(),
        )
}
