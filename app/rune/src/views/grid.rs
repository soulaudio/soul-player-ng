//! The shared responsive grid: how many equal columns of cards fill a content
//! pane, and each card's width — the original soul-player's `grid-cols-2..8`
//! breakpoints as one pure rule, reused by every browse page and the artist
//! discography. Factored out of the album grid so the pages stay consistent.
#![forbid(unsafe_code)]

use crate::ui::space;

/// Smallest card width before the grid drops a column. Tuned so the column count
/// tracks the original's `grid-cols-2..8` viewport breakpoints and cards land
/// ~144 pt at a desktop width (its `lg` tier).
pub(crate) const MIN_CARD: f32 = 130.0;

/// Columns and per-card width for a `pane_w`-wide content pane: fill the pane
/// (minus its own `space::PANE` padding on each edge), dropping a column below
/// `min_card`, with `gap` between cards. Pure, so grid and test share one rule.
pub(crate) fn responsive_grid(pane_w: f32, min_card: f32, gap: f32) -> (usize, f32) {
    let avail = (pane_w - 2.0 * space::PANE).max(min_card);
    let cols = (((avail + gap) / (min_card + gap)).floor() as usize).max(1);
    let card_w = ((avail - (cols as f32 - 1.0) * gap) / cols as f32).floor();
    (cols, card_w)
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
