//! Sidebar "Up Next" queue — the banded play queue the backend projects (not
//! synthesized from library order). Two bands: the manual queue ("Next in
//! queue", reorderable + removable) and the real upcoming context ("Next from
//! <album>"). Rows are click-to-play-this-row (consuming those above), the
//! manual band drag-reorders and hover-removes, and a Clear empties it. Mirrors
//! the researched Spotify/Apple model; see `crates/soul-player-backend` for the
//! projection and `soul-audio-queue::play_upcoming` for the click semantics.
#![forbid(unsafe_code)]

use rune::{
    Align, Cursor, Drag, El, Phase, PointerPress, PopupItem, PopupMenu, ScrollSpec, Tx, col, el,
    icon, row, text,
};
use soul_player_host::Command;

use crate::State;
use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::views::icons;
use crate::views::player::{QUEUE_COVER, cover};

/// Width of the scroll-edge fade that eases rows into the nav above (Rune
/// suggests 16–32 pt).
const FADE_H: f32 = 32.0;
/// Nominal queue-row height (cover + vertical padding), used only to turn a
/// drag's vertical travel into a target index. Approximate is fine — it just
/// sets how far you drag to move one slot. `pub(crate)` so the drag e2e test can
/// drive a gesture of a known slot count.
pub(crate) const QUEUE_ROW_H: f32 = 44.0;

/// Where a dragged row lands: its own index plus one slot per `QUEUE_ROW_H` of
/// vertical travel, clamped into the band. Pure, so the drag handler and its
/// test share one rule.
fn drop_index(from: usize, dy: f32, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let steps = (dy / QUEUE_ROW_H).round() as i64;
    (from as i64 + steps).clamp(0, len as i64 - 1) as usize
}

/// Title + artist, the two-line text block shared by both bands' rows.
fn row_text(s: &State, p: &Palette, id: u32) -> El {
    let (title, artist) = match s.track(id) {
        Some(t) => (
            t.title
                .clone()
                .unwrap_or_else(|| t!("library.unknown").to_string()),
            t.artist.clone().unwrap_or_default(),
        ),
        None => (t!("library.unknown").to_string(), String::new()),
    };
    col()
        .key("qrow-text")
        .gap(space::XXS)
        .grow(1.0)
        .min_w(0.0)
        .child(text(title).font(type_::ITEM).color(p.fg).truncate())
        .child(
            text(artist)
                .font(type_::CAPTION)
                .color(p.text_secondary.alpha(0.7))
                .truncate(),
        )
}

/// A band's eyebrow label, with an optional trailing control (the manual band
/// carries Clear).
fn band_header(key: &str, label: String, trailing: Option<El>, p: &Palette) -> El {
    let mut r = row()
        .key(key.to_string())
        .w_full()
        .center_y()
        // Match the pane inset (the now-playing panel's `p-4`, the nav's `px-4`)
        // so the eyebrow left-aligns with everything else in the sidebar.
        .px(space::LG)
        .child(
            // `truncate` (ellipsis + shrink-to-0) so a long "Next from <album>"
            // or a longer-locale eyebrow ellipsizes instead of overflowing the
            // rail and squeezing the trailing Clear off.
            text(label)
                .font(type_::SECTION)
                .color(p.text_secondary)
                .grow(1.0)
                .truncate(),
        );
    if let Some(t) = trailing {
        r = r.child(t);
    }
    r
}

/// The thin accent drop-indicator shown at the target slot while dragging.
fn drop_line(p: &Palette) -> El {
    el().key("drop-line")
        .w_full()
        .h(2.0)
        .radius(radius::FULL)
        .fill(p.accent)
}

/// One manual-queue row: grip (drag-reorder) + cover + title/artist + remove ✕.
/// Clicking the row body plays this row now (consuming those above); right-click
/// opens Play / Remove. `i` is the backend manual-queue index; `k` the 1-based
/// upcoming offset for `PlayUpcoming`.
fn manual_row(s: &State, p: &Palette, scale: f32, i: usize, id: u32, k: u32, dragged: bool) -> El {
    // The drag grip: Down caches the item's current index, Move tracks the target
    // (drives the drop line), Up commits the reorder. The backing list is unchanged
    // until release, so `from` stays valid for the whole gesture.
    let grip = el()
        .key(format!("grip-{i}"))
        .role("button")
        .label(t!("queue.reorder").to_string())
        .size(16.0, QUEUE_ROW_H)
        .center()
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        // Hidden until the row is hovered (the original drawer reveals row
        // controls on hover), so the resting list stays clean.
        .group_opacity(0.0, 1.0)
        .hover(p.fg.alpha(fx::HOVER))
        .press(p.fg.alpha(fx::PRESS))
        .child(icon(&icons::GRIP, 14.0).color(p.text_secondary.alpha(0.6)))
        .on_drag(move |st: &mut State, d: &mut Drag, _tx| match d.phase {
            Phase::Down => {
                // The row's own index is the drag origin — a search by track id
                // would find the FIRST copy, so a track queued more than once
                // would move the wrong row.
                st.queue_drag = Some((i, i));
            }
            Phase::Move => {
                if let Some((from, _)) = st.queue_drag {
                    let to = drop_index(from, d.dy, st.up_next.len());
                    st.queue_drag = Some((from, to));
                }
            }
            Phase::Up => {
                if let Some((from, to)) = st.queue_drag.take()
                    && from != to
                {
                    st.host.send(Command::MoveInQueue {
                        from: from as u32,
                        to: to as u32,
                    });
                }
            }
        });

    let remove = el()
        .key(format!("remove-{i}"))
        .role("button")
        .label(t!("queue.remove").to_string())
        .focusable(true)
        .padding(space::XS)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        // Reveal on hover, alongside the grip.
        .group_opacity(0.0, 1.0)
        .hover(p.fg.alpha(fx::HOVER))
        .press(p.fg.alpha(fx::PRESS))
        .child(icon(&icons::X, 14.0).color(p.text_secondary))
        .on_press(move |st: &mut State| st.host.send(Command::RemoveFromQueue(i as u32)));

    // The hover "pill" lives on this inner body (cover + text + remove), not the
    // whole row — so the highlight is an inset rounded pill rather than an odd
    // edge-to-edge bar. The body starts after the grip gutter, so its cover still
    // lands at the 16 pt pane inset, left-aligned with the now-playing cover.
    let body = row()
        .key(format!("queue-manual-body-{i}"))
        .grow(1.0)
        .min_w(0.0)
        .gap(0.0)
        .center_y()
        .py(space::XS)
        .radius(radius::SM)
        .hover(p.fg.alpha(fx::HOVER))
        .press(p.fg.alpha(fx::PRESS))
        .child(cover(s, Some(id), QUEUE_COVER, radius::XS, p, scale).key(format!("qcover-m-{i}")))
        .child(row_text(s, p, id).pl(space::COZY).grow(1.0))
        .child(remove);

    let r = row()
        // Keyed by position `i`, not track id: the same track can sit in the
        // queue more than once, which would collide an id key.
        .key(format!("queue-manual-{i}"))
        // No inter-child gap: the 16 pt grip IS the left gutter, so the body's
        // cover lands at the 16 pt pane inset. `pr` insets the pill from the
        // right edge; `hover_group` reveals the grip + remove on hover.
        .gap(0.0)
        .center_y()
        .pr(space::SM)
        .cursor(Cursor::Pointer)
        .hover_group()
        .child(grip)
        .child(body)
        .on_secondary_press(move |_st: &mut State, press: PointerPress, tx: &mut Tx| {
            tx.popup_menu(
                PopupMenu::new()
                    .item(PopupItem::new("play", t!("player.play").to_string()))
                    .item(PopupItem::new("remove", t!("queue.remove").to_string())),
                press,
                move |st: &mut State, picked, _tx: &mut Tx| match picked {
                    Some("play") => st.host.send(Command::PlayUpcoming(k)),
                    Some("remove") => st.host.send(Command::RemoveFromQueue(i as u32)),
                    _ => {}
                },
            );
        })
        .on_press(move |st: &mut State| st.host.send(Command::PlayUpcoming(k)));
    // The lifted row dims while it's being dragged.
    if dragged { r.opacity(0.4) } else { r }
}

/// One context ("Next from") row: cover + title/artist, click to jump there.
/// Not reorderable/removable — it's the source remainder, not a user list.
fn context_row(s: &State, p: &Palette, scale: f32, id: u32, k: u32) -> El {
    // Inset hover pill on the body, matching the manual rows (minus the grip).
    let body = row()
        .key(format!("queue-context-body-{k}"))
        .grow(1.0)
        .min_w(0.0)
        .gap(0.0)
        .center_y()
        .py(space::XS)
        .radius(radius::SM)
        .hover(p.fg.alpha(fx::HOVER))
        .press(p.fg.alpha(fx::PRESS))
        .child(cover(s, Some(id), QUEUE_COVER, radius::XS, p, scale).key(format!("qcover-c-{k}")))
        .child(row_text(s, p, id).pl(space::COZY).grow(1.0));
    row()
        // Keyed by the upcoming-slot position `k`, not the track id: the same
        // track can appear more than once in the context (the same recording in
        // several formats), so an id key would collide. `k` is unique per row.
        .key(format!("queue-context-{k}"))
        .gap(0.0)
        .center_y()
        .pr(space::SM)
        .cursor(Cursor::Pointer)
        // A 16 pt leading spacer mirrors the manual rows' grip gutter, so both
        // bands' covers land at the pane inset, aligned with the now-playing
        // cover. Keyed by `k` (positional, no id collisions).
        .child(el().key(format!("ctx-indent-{k}")).w(space::LG))
        .child(body)
        .on_press(move |st: &mut State| st.host.send(Command::PlayUpcoming(k)))
}

/// Clear the manual queue (sits beside the "Next in queue" eyebrow).
fn clear_button(p: &Palette) -> El {
    el().key("clear-queue")
        .role("button")
        .label(t!("queue.clear").to_string())
        .focusable(true)
        .px(space::SNUG)
        .py(space::XXS)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .hover(p.fg.alpha(fx::HOVER))
        .press(p.fg.alpha(fx::PRESS))
        .child(
            text(t!("queue.clear").to_string())
                .font(type_::CAPTION)
                .color(p.text_secondary),
        )
        .on_press(|st: &mut State| st.host.send(Command::ClearQueue))
}

/// The empty state: nothing queued and nothing upcoming.
fn empty_state(p: &Palette) -> El {
    col()
        .key("queue-empty")
        .grow(1.0)
        .min_h(0.0)
        .center()
        .gap(space::XS)
        .px(space::MD)
        // `w_full` + `wrap` bound the lines to the sidebar width and let a longer
        // locale wrap onto a second line instead of overflowing the rail (it is
        // only ~288 pt wide); centre the wrapped lines to match the empty state.
        .child(
            text(t!("queue.empty").to_string())
                .font(type_::CAPTION)
                .color(p.text_secondary)
                .w_full()
                .wrap()
                .align(Align::Center),
        )
        .child(
            text(t!("queue.emptyHint").to_string())
                .font(type_::CAPTION)
                .color(p.text_secondary.alpha(0.6))
                .w_full()
                .wrap()
                .align(Align::Center),
        )
}

/// The sidebar queue: the manual "Next in queue" band over the "Next from
/// <context>" band, both projected from the player, in a scroll viewport between
/// the nav and the now-playing panel. Empty when nothing is queued or upcoming.
pub fn queue_section(s: &State, p: &Palette, scale: f32) -> El {
    if s.up_next.is_empty() && s.context_ids.is_empty() {
        return col()
            .key("queue")
            .grow(1.0)
            .min_h(0.0)
            .child(empty_state(p));
    }

    let manual_len = s.up_next.len();
    let mut content = col()
        .key("queue-content")
        .gap(space::MD)
        // No horizontal padding here: the band headers (`px LG`) and rows (grip/
        // spacer gutter + `pr LG`) own their own insets so covers line up with
        // the now-playing cover.
        .pt(space::SM)
        .pb(space::SM);

    // Manual band ("Next in queue") — hidden entirely when empty.
    if !s.up_next.is_empty() {
        let mut band = col()
            .key("queue-manual-band")
            .gap(space::XXS)
            .child(band_header(
                "queue-next-in-queue",
                t!("queue.nextInQueue").to_string(),
                Some(clear_button(p)),
                p,
            ));
        let target = s.queue_drag.map(|(_, to)| to);
        for (i, &id) in s.up_next.iter().enumerate() {
            if target == Some(i) {
                band = band.child(drop_line(p));
            }
            let dragged = s.queue_drag.is_some_and(|(from, _)| from == i);
            band = band.child(manual_row(s, p, scale, i, id, (i + 1) as u32, dragged));
        }
        content = content.child(band);
    }

    // Context band — the real shuffle-correct remainder of what's playing. Just
    // "Up next" (short, like Spotify's queue) rather than a verbose
    // "Next from <album>".
    if !s.context_ids.is_empty() {
        let mut band = col()
            .key("queue-context-band")
            .gap(space::XXS)
            .child(band_header(
                "queue-next-from",
                t!("queue.upNext").to_string(),
                None,
                p,
            ));
        for (j, &id) in s.context_ids.iter().enumerate() {
            let k = (manual_len + j + 1) as u32;
            band = band.child(context_row(s, p, scale, id, k));
        }
        content = content.child(band);
    }

    col().key("queue").grow(1.0).min_h(0.0).child(
        content
            .scroll(ScrollSpec {
                stick_bottom: false,
            })
            .scroll_edge_fade(FADE_H)
            .grow(1.0)
            .min_h(0.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_index_moves_one_slot_per_row_height_and_clamps() {
        // No travel: stays put.
        assert_eq!(drop_index(2, 0.0, 5), 2);
        // Down one+ row heights: moves down one/two, clamped to the last index.
        assert_eq!(drop_index(0, QUEUE_ROW_H * 1.2, 5), 1);
        assert_eq!(drop_index(1, QUEUE_ROW_H * 2.0, 5), 3);
        assert_eq!(drop_index(3, QUEUE_ROW_H * 9.0, 5), 4); // clamps to len-1
        // Up: negative travel moves toward the front, clamped at 0.
        assert_eq!(drop_index(3, -QUEUE_ROW_H * 2.0, 5), 1);
        assert_eq!(drop_index(1, -QUEUE_ROW_H * 9.0, 5), 0);
        // Degenerate.
        assert_eq!(drop_index(0, 100.0, 0), 0);
    }
}
