//! The app's single themed dropdown (select) component. A dropdown is a trigger
//! field showing the current value plus a floating panel of choices; picking one
//! commits it and closes. Built entirely from public `rune` primitives and the
//! app's own `theme::Palette`, so it matches the rest of the UI — unlike the
//! native `PopupMenu`, which draws an OS-styled menu. **This is the only way to
//! build a dropdown in the app** (enforced by `one_dropdown_component` in the
//! main test module): new pickers call [`trigger`] + [`panel`], never a bespoke
//! menu.
//!
//! It is a two-part component because an opened panel must escape the settings
//! scroll viewport's clip and draw above everything:
//! - [`trigger`] renders in place (inside the card) with a stable `key`.
//! - [`panel`] is mounted at the window ROOT (after the trigger in tree order)
//!   and `anchored` to the trigger's key, so rune positions it under the trigger,
//!   flips/clamps it to the window, and lets it ignore ancestor clips.
//!
//! One dropdown is open at a time: `State::open_dropdown` holds the open key,
//! `toggle_dropdown` / `close_dropdown` drive it. The panel is light-dismiss: a
//! transparent full-window backdrop behind it closes it on an outside press OR a
//! scroll (so the page is never scroll-locked), and the app key handler closes it on
//! Escape. (It is deliberately NOT `El::modal`, whose inert background blocked the
//! page scroll — wrong for a popover anchored deep in a scroll view.)
#![forbid(unsafe_code)]

use rune::{Align, Cursor, El, Side, WheelEvent, col, el, icon, row, text};

use crate::theme::Palette;
use crate::ui::{radius, space};
use crate::views::icons;
use crate::{State, close_dropdown, toggle_dropdown};

/// Gap between the trigger and its opened panel.
const GAP: f32 = 4.0;

/// Whether the dropdown with this key is the one currently open.
pub fn is_open(s: &State, key: &str) -> bool {
    s.open_dropdown.as_deref() == Some(key)
}

/// The in-place trigger: the current value and a chevron, styled as a select
/// field. Pressing it toggles its [`panel`] (mounted at the root) open or shut.
pub fn trigger(key: &str, current: String, p: &Palette) -> El {
    let key_owned = key.to_string();
    row()
        .key(key.to_string())
        .role("button")
        .focusable(true)
        .w_full()
        .center_y()
        .gap(space::SM)
        .px(space::MD)
        .py(space::SM)
        .radius(radius::SM)
        .fill(p.muted)
        .cursor(Cursor::Pointer)
        .hover(p.fg.alpha(0.06))
        .child(text(current).color(p.fg).grow(1.0).truncate())
        .child(icon(&icons::CHEVRON_DOWN, 16.0).color(p.text_secondary))
        .on_press(move |s: &mut State| toggle_dropdown(s, &key_owned))
}

/// The floating panel for an open dropdown — **mount at the window root**, after
/// the trigger, via [`crate::view`]'s overlay layer. `anchored` to `anchor_key`
/// positions it under the trigger and escapes the settings scroll viewport's
/// clip; `width` (the trigger's own width, when known) lines the panel up under
/// it. One themed row per `(id, label)` choice, the selected one tinted and
/// checked; picking calls `on_pick(id)` and closes. Light-dismiss: an outside press
/// or a scroll (via a transparent backdrop) closes it, as does Escape.
pub fn panel<F>(
    anchor_key: &str,
    choices: &[(String, String)],
    selected: &str,
    width: Option<f32>,
    on_pick: F,
    // An optional trailing "coming soon" row: present but greyed and NOT focusable,
    // so it reads as a teaser the user can't pick (mirrors the add-source menu).
    disabled_note: Option<&str>,
    p: &Palette,
) -> El
where
    F: Fn(&mut State, &str) + Clone + 'static,
{
    let mut menu = col()
        .key(format!("{anchor_key}-menu"))
        .anchored_fit(anchor_key, Side::Bottom, Align::Start, GAP)
        .focus_group()
        .focus_return(anchor_key.to_string())
        .fill(p.card)
        .radius(radius::MD)
        // Border + card surface alone separate the panel from the content; a
        // foreground-tinted drop shadow read as a glow in dark mode (p.fg is
        // light there), so there's no shadow.
        .border_sides([1.0, 1.0, 1.0, 1.0], p.border)
        .padding(space::XXS)
        .gap(2.0)
        .min_h(0.0);
    menu = match width {
        Some(w) => menu.w(w.max(140.0)),
        None => menu.min_w(160.0),
    };
    for (id, label) in choices {
        let selected_row = id == selected;
        let id_owned = id.clone();
        let pick = on_pick.clone();
        let mut opt = row()
            .key(format!("{anchor_key}-opt-{id}"))
            .role("option")
            .focusable(true)
            .focus_selected(selected_row)
            .w_full()
            .center_y()
            .gap(space::SM)
            .px(space::MD)
            .py(space::SM)
            .radius(radius::SM)
            .cursor(Cursor::Pointer)
            .hover(p.fg.alpha(0.06))
            .child(text(label.clone()).color(p.fg).grow(1.0).truncate());
        if selected_row {
            opt = opt
                .fill(p.primary.alpha(0.14))
                .child(icon(&icons::CHECK, 16.0).color(p.primary));
        }
        menu = menu.child(opt.on_press(move |s: &mut State| {
            pick(s, &id_owned);
            close_dropdown(s);
        }));
    }
    // A disabled "coming soon" row: greyed, not focusable, no handler.
    if let Some(note) = disabled_note {
        menu = menu.child(
            row()
                .key(format!("{anchor_key}-opt-soon"))
                .role("option")
                .w_full()
                .center_y()
                .px(space::MD)
                .py(space::SM)
                .radius(radius::SM)
                // No truncate: let the note's natural width set the panel, so
                // "… coming soon" shows in full rather than ellipsizing.
                .child(text(note.to_string()).color(p.text_secondary)),
        );
    }
    // Light-dismiss backdrop: a full-window, transparent catcher mounted BEHIND the
    // panel. An outside press OR any scroll closes the dropdown — the standard popover
    // behavior — instead of `El::modal`, whose inert background blocked the page from
    // scrolling (so a trigger deep in the settings scroll locked the page, and opening
    // fought the content scroll). The panel draws on top, so its options still take
    // presses; Escape is handled by the app's key handler.
    el().key(format!("{anchor_key}-overlay"))
        // Out of flow + full window, so it overlays the shell (a plain el would flow
        // below it). The backdrop fills it; the panel anchors within.
        .absolute(0.0, 0.0)
        .w_full()
        .h_full()
        .child(
            el().key(format!("{anchor_key}-backdrop"))
                .absolute(0.0, 0.0)
                .w_full()
                .h_full()
                // A fill (fully transparent) so the catcher is hit-tested across the
                // whole window — an outside press or a scroll closes the dropdown.
                .fill(p.card.alpha(0.0))
                .on_press(close_dropdown)
                .on_wheel(|s: &mut State, _ev: WheelEvent, _tx| {
                    close_dropdown(s);
                    true
                }),
        )
        .child(menu)
}
