//! The shared responsive grid: how many equal columns of cards fill a content
//! pane, and each card's width, the original soul-player's `grid-cols-2..8`
//! breakpoints as one pure rule, reused by every browse page and the artist
//! discography. Factored out of the album grid so the pages stay consistent.
#![forbid(unsafe_code)]

use std::borrow::Cow;
use std::rc::Rc;

use rune::{El, GrowthSpec, col, row};

use crate::State;
use crate::ui::space;

/// The per-card builder a [`virtual_grid`] caller supplies: `(state, item_index,
/// card_width) -> card element`. Boxed so the grid helper stays generic over the
/// card type; the caller captures its (memoized) item list by `Rc` inside.
pub(crate) type CardBuilder = Rc<dyn Fn(&State, usize, f32) -> El>;

/// Smallest card width before the grid drops a column. Tuned so the column count
/// tracks the original's `grid-cols-2..8` viewport breakpoints and cards land
/// ~144 pt at a desktop width (its `lg` tier).
pub(crate) const MIN_CARD: f32 = 130.0;

/// How far past the computed `card_w` a card may grow to fill a row. The cards are
/// flex items (min [`MIN_CARD`], grow to fill), not a fixed fractional width: an
/// exact-fill `card_w` is a float that cannot tile an integer container, so a
/// fixed width rounds a hair too wide and wraps the last column (a full empty
/// gap), or a hair too narrow and leaves a thin one. Letting the layout engine
/// distribute the few-point remainder fills every width exactly. The cap keeps a
/// partial last row from stretching its cards across the whole pane, they grow at
/// most this much past `card_w`, staying aligned with the rows above.
pub(crate) const FILL_SLOP: f32 = 2.0;

/// Columns and per-card width for an already-available inner width (no padding
/// subtraction): the column count is the most that fit at `min_card`, and each
/// card takes the exact remaining fraction so the cards stretch to fill. Pure.
pub(crate) fn columns(avail: f32, min_card: f32, gap: f32) -> (usize, f32) {
    let avail = avail.max(min_card);
    let cols = (((avail + gap) / (min_card + gap)).floor() as usize).max(1);
    let card_w = (avail - (cols as f32 - 1.0) * gap) / cols as f32;
    (cols, card_w)
}

/// As [`columns`], but for a `pane_w`-wide pane whose grid carries its own
/// `space::PANE` padding on each edge (the browse pages). The column count is
/// the most that fit at `min_card` (dropping a column below it), and each card
/// takes the **exact** remaining fraction so the cards STRETCH to fill the pane's
/// full width, edge to edge at every window size, resizing smoothly as the pane
/// resizes, then reflowing to more/fewer columns past `min_card`. (A floored fixed
/// width used to leave a trailing gap that grew between breakpoints.) Pure, so grid
/// and test share one rule.
pub(crate) fn responsive_grid(pane_w: f32, min_card: f32, gap: f32) -> (usize, f32) {
    columns(pane_w - 2.0 * space::PANE, min_card, gap)
}

/// A VIRTUALIZED responsive grid: the same column rule as [`responsive_grid`], but
/// the `count` items are chunked into rows of `cols` cards and only the on-screen
/// window of rows is ever built (via [`rune::El::virtual_rows`]), so a browse grid
/// stays smooth at hundreds of thousands of items instead of laying out every card.
/// `cover_aspect` is the card's cover height as a fraction of `card_w` (1.0 for a
/// square album/artist cover, 0.0 for a cover-less genre tile) and `caption_h` its
/// text height, so a row is `card_w * cover_aspect + caption_h` tall.
/// `build_card(state, item_index, card_w)` builds one card; it captures the
/// (memoized) item list by `Rc`, so it borrows nothing and indexes cheaply. Must be
/// the direct child of a `.scroll()` viewport (the `growth` contract), like Tracks.
pub(crate) fn virtual_grid(
    key: &'static str,
    pane_w: f32,
    count: usize,
    cover_aspect: f32,
    caption_h: f32,
    build_card: CardBuilder,
) -> El {
    let (cols, card_w) = responsive_grid(pane_w, MIN_CARD, space::GUTTER);
    let cols = cols.max(1);
    let rows = count.div_ceil(cols);
    // One grid row is `cols` cards side by side; its height is the cover (a fraction
    // of the card width) plus the caption, and it carries a bottom gap so rows don't
    // touch. That total is the stride `virtual_rows` windows by.
    let row_h = card_w * cover_aspect + caption_h + space::GUTTER;
    col()
        .key(key)
        .role("grid")
        .growth(GrowthSpec::default())
        .virtual_rows(
            rows,
            row_h,
            move |_s: &State, r: usize| Cow::Owned(format!("{key}-row-{r}")),
            move |s: &State, r: usize| {
                let start = r * cols;
                let end = (start + cols).min(count);
                let mut row_el = row()
                    .key(format!("{key}-row-{r}"))
                    .w_full()
                    .gap(space::GUTTER)
                    .px(space::PANE)
                    .pb(space::GUTTER);
                for pos in start..end {
                    row_el = row_el.child(build_card(s, pos, card_w));
                }
                row_el
            },
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_fills_the_pane_with_more_columns_as_it_widens() {
        // Fed the content-pane width (window minus the sidebar), the column count
        // tracks the original's grid-cols-2..8 breakpoints; cards land ~144 pt at a
        // desktop width (its `lg` tier). Values match the former albums::grid_layout.
        assert_eq!(
            responsive_grid(606.0, MIN_CARD, space::GUTTER).0,
            4,
            "md-ish → 4 cols"
        );
        let (cols, card) = responsive_grid(820.0, MIN_CARD, space::GUTTER);
        assert_eq!(cols, 5, "lg pane → 5 columns");
        assert!((card - 144.0).abs() <= 2.0, "lg cards ~144 pt, got {card}");
        assert_eq!(
            responsive_grid(1306.0, MIN_CARD, space::GUTTER).0,
            8,
            "2xl → 8 cols"
        );
        assert_eq!(
            responsive_grid(200.0, MIN_CARD, space::GUTTER).0,
            1,
            "never below 1"
        );
        assert!(responsive_grid(820.0, MIN_CARD, space::GUTTER).1 >= MIN_CARD);
    }
}
