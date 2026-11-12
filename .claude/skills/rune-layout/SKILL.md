---
name: rune-layout
description: Layout, spacing, radius, and typography conventions for the app/rune Rune UI. Use when adding or editing any view under app/rune/src/views (or building new UI) — picking gaps/padding/font sizes, laying out rows/cols/grids, or making something respond to width. Covers the design-token scale in app/rune/src/ui.rs and the rules for applying it.
---

# Rune layout & design tokens

The `app/rune` UI uses a small, fixed set of **design tokens** instead of magic
numbers. They live in `app/rune/src/ui.rs` as const modules: `space` (gaps &
padding), `radius` (corners), `type_` (`TextStyle` size+weight pairs). Values are
logical points and match the original soul-player's Tailwind scale.

**The rule: no raw spacing/radius/font numbers in view code.** Use a token.
`.gap(space::SM)`, `.padding(space::PANE)`, `.radius(radius::MD)`,
`.font(type_::TITLE)` — never `.gap(8.0)`.

## The scales (see `src/ui.rs` for the authoritative list)

`space` — 4 pt grid; base unit is `SM` (8). `SNUG` (6) and `COZY` (10) are the
Tailwind 1.5/2.5 half-steps the design uses.

| token | pt | typical use |
|---|---|---|
| `XXS` | 2 | title↔subtitle, hairline gaps |
| `XS` | 4 | tight icon↔text, fine insets |
| `SNUG` | 6 | compact control inset (list rows, cards, the gear) |
| `SM` | 8 | **default gap between items** |
| `COZY` | 10 | the transport-controls row |
| `MD` | 12 | control padding, medium gaps |
| `LG` | 16 | pane/section padding, grid gutter |
| `XL` | 24 | gap between major sections, nav top padding |
| `XXL` | 32 | — |

Semantic aliases (name the role): `space::PANE` (16), `space::GUTTER` (16),
`space::SECTION` (24). Prefer these where they read clearer than the raw rung.

`radius` — `XXS` 2, `XS` 4, `SM` 6 (rows/covers), `MD` 8 (cards/buttons/gear),
`LG` 12, `XL` 16 (panels), `FULL` (pills). **A circular control uses
`size * 0.5`, not a radius token** — e.g. `.size(BTN, BTN).radius(BTN * 0.5)`.

`type_` — `TextStyle` pairs: `SECTION` 11.5/600 (uppercase eyebrow), `CAPTION`
12/400 (subtitles/meta), `LABEL` 12/500 (`.mono()` for the version/time labels),
`BODY` 13/400, `ITEM` 14/400 (list row title), `ITEM_STRONG` 14/500 (card title),
`TITLE` 14/600 (panel/section title), `NAV` 20/600 (sidebar nav). Chain `.mono()`
/ `.lh()` as needed — they are `const fn`.

## Rules

1. **`gap` over margin.** The parent owns spacing *between* its children via
   `row().gap(..)` / `col().gap(..)`. Rune has no per-child outer margin, and you
   should not fake one — a component never sets its own outer spacing, so the same
   component drops into any layout cleanly. `padding` is for a component's
   *internal* space only.
2. **Tokens are theme-invariant.** Spacing, radius, and type never change between
   light and dark — only colour does (that lives in `theme::Palette`). Don't
   branch a gap/size on the theme.
3. **Centering:** `.center()` centres children on BOTH axes;
   `.center_y()` / `.align(Align::Center)` only set the **cross** axis (horizontal
   for a `col`). To vertically centre a `col`'s children, use `.center()` or
   `.justify_center()` — `center_y()` alone will pin them to the top.
4. **Element sizes are component tokens, not spacing.** A cover side, button
   diameter, or the sidebar width is a named `const` near its view (e.g.
   `COVER_SIZE`, `BTN_SIZE`, `DEFAULT_WIDTH`), not a `space::` token.
5. **A new recurring value** gets added to `src/ui.rs` (keep the 4 pt grid), not
   inlined. A genuinely one-off component dimension can stay a local `const` with
   a doc comment.

## Grids

The albums page is a wrapping card grid:
`row().flex_wrap().gap(space::GUTTER).padding(space::PANE)` with fixed-width cards.
`flex_wrap` gives **auto-fill** behaviour for free (cards keep their width and wrap;
trailing space on wide windows), which is what we want for uniform covers.

For **auto-fit** (cards stretch to fill each row), compute columns from the
container width (there is no CSS engine):

```
columns   = max(1, floor((avail + gap) / (min_card_w + gap)))
card_w    = (avail - (columns - 1) * gap) / columns   // square cover: h = card_w
```

The `+ gap` credits the one missing gutter after the last card; the naive
`floor(avail / (min + gap))` undercounts at boundaries.

## Responsiveness — width-driven, not device breakpoints

The window is resizable *and* the sidebar is resizable (240–480 pt), so a
component's width ≠ the window width. Respond to the **content pane's own width**
(`window_w − sidebar_w − paddings`), not the window — the container-query mindset.
Prefer intrinsic reflow (`flex_wrap`, `grow`, the Every-Layout sidebar shell we
already use) over hard breakpoints. If you must threshold, threshold on the pane
width, e.g. compact ≤ 640 pt, standard ≤ 1024 pt, wide above.

## Density (deferred)

Compact/comfortable density is the one axis that legitimately rescales spacing.
Not built yet. When it is, add a `Density` multiplier and resolve spacing through
it (`sp(space::SM, density)`), Zed-`DynamicSpacing`-style — threaded beside the
existing `scale`. Radius and type stay fixed.

## Verifying a layout change

- Headless geometry tests (`Runtime` + `presented()`/`find()`) in
  `main.rs`'s test module assert exact pixel dimensions — run `cargo test`.
- Snapshot a view: `cargo build` then
  `SOUL_PLAYER_FOLDER=testdata ./target/debug/soul-player-rune.exe --snap out.png
  --size 1100x720 [--theme light] [--scene albums|settings|collapsed|search]`.
  Pixel-diff against a baseline to confirm an intended change is the *only* change.
- Toolchain: `app/rune` is its own workspace on `nightly-2026-11-10` (run cargo
  from `app/rune`). Commit messages: Conventional Commits, no AI attribution;
  push with `--no-verify`.
