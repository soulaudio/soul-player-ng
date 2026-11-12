//! Layout design tokens — the one place for spacing, corner radius, and type
//! sizes, so views read `space::SM` / `radius::MD` / `type_::TITLE` instead of
//! scattered magic numbers (the "primitive + semantic" token tiers every design
//! system converges on).
//!
//! Values are logical points and match the original soul-player's Tailwind scale
//! — a 4 pt grid whose common rungs land on 8, plus the 1.5/2.5 half-steps
//! (`SNUG` = 6, `COZY` = 10) the design actually uses — so adopting them is a
//! rename with no pixel drift, not a redesign.
//!
//! These tokens are theme-INVARIANT: only colour swaps between light and dark
//! (see [`crate::theme::Palette`]); spacing, radius, and type never do. The one
//! axis that *would* rescale spacing is display density (compact/comfortable);
//! that is deliberately deferred — see `.claude/skills/rune-layout`.
//!
//! Built on `rune::tok` (which ships a sparse `space`/`radius`/`type_` base);
//! this completes and names it for the app.
#![allow(dead_code)] // a complete scale — not every rung has a caller yet

/// Spacing (gaps and padding), in logical points. The base unit is `SM` (8);
/// `SNUG` (6) and `COZY` (10) are the Tailwind 1.5 / 2.5 half-steps the design
/// uses between the main rungs. Prefer `gap` on the parent over per-child margin
/// (see the rune-layout skill), and the semantic aliases below where they fit.
pub mod space {
    pub const NONE: f32 = 0.0;
    /// 2 — title over subtitle; hairline gaps.
    pub const XXS: f32 = 2.0;
    /// 4 — tight icon↔text; fine insets.
    pub const XS: f32 = 4.0;
    /// 6 — compact control inset (list rows, cards, the gear).
    pub const SNUG: f32 = 6.0;
    /// 8 — the base unit; default gap between sibling items.
    pub const SM: f32 = 8.0;
    /// 10 — the transport-controls row.
    pub const COZY: f32 = 10.0;
    /// 12 — control padding; medium gaps (seek row, form fields).
    pub const MD: f32 = 12.0;
    /// 16 — pane/section padding and the grid gutter.
    pub const LG: f32 = 16.0;
    /// 24 — gap between major sections; the nav's top padding.
    pub const XL: f32 = 24.0;
    pub const XXL: f32 = 32.0;

    // ── Semantic aliases: name the role, not the number, for cross-cutting use.
    /// Page / sidebar-section inset (16).
    pub const PANE: f32 = LG;
    /// Album-grid gap (16).
    pub const GUTTER: f32 = LG;
    /// Gap between major sections (24).
    pub const SECTION: f32 = XL;
}

/// Corner radius, in logical points. Mirrors and extends `rune::tok::radius`.
/// For a *circular* control use `size * 0.5`, not a radius token.
pub mod radius {
    /// 2 — the collapsed-rail grabber pill.
    pub const XXS: f32 = 2.0;
    /// 4 — status dots, small chips.
    pub const XS: f32 = 4.0;
    /// 6 — nav items, list rows, cover thumbnails.
    pub const SM: f32 = 6.0;
    /// 8 — cards, buttons, the settings gear.
    pub const MD: f32 = 8.0;
    pub const LG: f32 = 12.0;
    /// 16 — large panels / cover art.
    pub const XL: f32 = 16.0;
    /// A pill: clamps to half the shorter side.
    pub const FULL: f32 = 9999.0;
}

/// Type tokens: `TextStyle` (size + weight; line-height derived) pairs that
/// replace inline `TextStyle::new(..)`. Sizes preserve the current design
/// exactly. Chain `.mono()` / `.lh()` etc. as needed (they are `const fn`).
pub mod type_ {
    use rune::TextStyle;

    /// 11.5 / 600 — an uppercase section eyebrow (e.g. "NOW PLAYING").
    pub const SECTION: TextStyle = TextStyle::new(11.5, 600.0);
    /// 12 / 400 — subtitles and meta lines (artist under title, timestamps).
    pub const CAPTION: TextStyle = TextStyle::new(12.0, 400.0);
    /// 12 / 500 — chips; the mono version caption base (`.mono()`).
    pub const LABEL: TextStyle = TextStyle::new(12.0, 500.0);
    /// 13 / 400 — body copy (matches `rune::tok::type_::BODY`).
    pub const BODY: TextStyle = TextStyle::new(13.0, 400.0);
    /// 14 / 400 — a queue / track-list row title.
    pub const ITEM: TextStyle = TextStyle::new(14.0, 400.0);
    /// 14 / 500 — a card or now-playing track title (medium weight).
    pub const ITEM_STRONG: TextStyle = TextStyle::new(14.0, 500.0);
    /// 16 / 500 / 24 — an album/media-card title (the old app's `text-base
    /// font-medium` card heading).
    pub const CARD_TITLE: TextStyle = TextStyle::new(16.0, 500.0).lh(24.0);
    /// 14 / 600 — the now-playing panel title.
    pub const TITLE: TextStyle = TextStyle::new(14.0, 600.0);
    /// 20 / 600 — the sidebar nav labels.
    pub const NAV: TextStyle = TextStyle::new(20.0, 600.0);
}
