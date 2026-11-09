//! Sidebar queue: the tracks coming up next. Mirrors the old app's
//! `applications/shared/src/components/sidebar/QueueSection.tsx` — a
//! scrollable, bottom-aligned stack so the soonest-up track sits on the
//! bottom edge, right above the player panel, rather than pinned to the top.
#![forbid(unsafe_code)]

use rune::tok::type_;
use rune::{El, ScrollSpec, col, row, text};
use soul_player_host::Command;

use crate::State;
use crate::theme::Palette;

/// One compact queue row: title (`p.fg`) over artist (`p.text_secondary`),
/// both small and truncated so a long tag can't overflow the sidebar.
/// Display-only would satisfy the brief; clicking through to
/// `Command::Goto` is wired anyway since the natural position is cheap to
/// derive (`context_pos + 1 + index`, matching `QueueState::up_next_ids`'s
/// order — see `crates/soul-player-backend/src/driver.rs`).
fn queue_row(s: &State, p: &Palette, id: u32, natural_pos: u32) -> El {
    let (title, artist) = match s.track(id) {
        Some(t) => (
            t.title.clone().unwrap_or_else(|| "Unknown".into()),
            t.artist.clone().unwrap_or_default(),
        ),
        None => ("Unknown".into(), String::new()),
    };
    row()
        .key(format!("queue-{id}"))
        .padding(6.0)
        .child(
            col()
                .key("queue-text")
                .gap(2.0)
                .grow(1.0)
                .min_w(0.0)
                .child(text(title).font(type_::LABEL).color(p.fg).truncate())
                .child(
                    text(artist)
                        .font(type_::LABEL)
                        .color(p.text_secondary)
                        .truncate(),
                ),
        )
        .on_press(move |st: &mut State| st.host.send(Command::Goto(natural_pos)))
}

/// Upcoming-queue list for the sidebar: `State::up_next` ids resolved
/// through `State::track`, under a small "Up Next" eyebrow label. The row
/// list is `justify_end()`'d inside a scroll viewport (`.scroll` +
/// `.grow(1.0).min_h(0.0)`, cheat-sheet §7): short queues sit flush on the
/// bottom, longer ones scroll with the soonest-up row anchored there, the
/// same `justify_end`-inside-`scroll` combination the engine documents for
/// a bottom-anchored stack (`crates/rune/src/runtime/scroll/bottom.rs`).
pub fn queue_section(s: &State, p: &Palette) -> El {
    let mut rows = col().key("queue-rows").gap(2.0).justify_end();
    for (i, &id) in s.up_next.iter().enumerate() {
        let natural_pos = s.context_pos + 1 + i as u32;
        rows = rows.child(queue_row(s, p, id, natural_pos));
    }
    col()
        .key("queue")
        .gap(8.0)
        .padding(16.0)
        .grow(1.0)
        .min_h(0.0)
        .child(text("Up Next").font(type_::SECTION).color(p.text_secondary))
        .child(
            rows.scroll(ScrollSpec {
                stick_bottom: false,
            })
            .grow(1.0)
            .min_h(0.0),
        )
}
