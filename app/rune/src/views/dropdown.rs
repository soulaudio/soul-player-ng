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
//! `toggle_dropdown` / `close_dropdown` drive it. The panel traps focus and
//! dismisses on Escape or an outside press (`El::modal`).
#![forbid(unsafe_code)]

use rune::{Align, Cursor, El, Side, col, icon, row, text};

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
/// checked; picking calls `on_pick(id)` and closes. Dismisses on Escape or an
/// outside press (`El::modal`).
pub fn panel<F>(
    anchor_key: &str,
    choices: &[(String, String)],
    selected: &str,
    width: Option<f32>,
    on_pick: F,
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
        .border_sides([1.0, 1.0, 1.0, 1.0], p.border)
        .shadow(4.0, 16.0, p.fg.alpha(0.18))
        .padding(space::XXS)
        .gap(2.0)
        .min_h(0.0)
        .modal(true, close_dropdown);
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
    menu
}
