//! The old soul-player Day/Night palettes as Rune colors, selected by system theme.
#![forbid(unsafe_code)]

use rune::Color;

// `rune::Color` is linear sRGB (`crates/rune/src/color.rs:1`: "Colour in linear
// sRGB with straight alpha"), not gamma-encoded 0-255/255.0 values: the naive
// `r as f32 / 255.0` conversion the brief sketched renders visibly washed-out
// (confirmed by snapshot) because it skips the sRGB->linear decode. Route
// through the crate's own `color::hex` parser instead, which does the decode
// (`srgb_decode`) the same way `examples/*` author colors from hex.

pub struct Palette {
    pub bg: Color,
    pub fg: Color,
    pub card: Color,
    pub primary: Color,
    pub accent: Color,
    pub muted: Color,
    pub border: Color,
    pub text_secondary: Color,
    /// A remote source's online-health dot (Settings → Sources). The one
    /// semantic "good/connected" color in either palette — everything else
    /// here is a neutral/brand hue, but online/offline needs a color that
    /// reads as a status signal independent of the warm Day / cool Night hues.
    pub online: Color,
    /// "Some, but not all" status color — the title-bar remote-health
    /// indicator's partial state (a mix of online and offline remotes), and
    /// any future amber/caution reading. Independent of `accent` (Night's own
    /// accent happens to be amber-ish too, but for a brand hue, not a status).
    pub warning: Color,
    /// "Bad/unreachable" status color — the title-bar remote-health
    /// indicator's down state (every enabled remote offline). Paired with
    /// `online`/`warning` as the one semantic red in either palette.
    pub danger: Color,
}

fn hex(s: &str) -> Color {
    rune::color::hex(s).unwrap_or_else(|| panic!("invalid palette hex literal {s:?}"))
}

/// Day (light) — cream bg, espresso primary, terracotta accent.
fn day() -> Palette {
    Palette {
        bg: hex("#fbfaf8"),
        fg: hex("#2c2621"),
        card: hex("#f7f6f2"),
        primary: hex("#563c29"),
        accent: hex("#c07859"),
        muted: hex("#f0eeea"),
        border: hex("#e5e1dc"),
        text_secondary: hex("#6b6157"),
        online: hex("#4f8a5b"),
        warning: hex("#b5811f"),
        danger: hex("#b23a32"),
    }
}

/// Night (dark) — midnight bg, lavender primary, amber accent.
fn night() -> Palette {
    Palette {
        bg: hex("#110f1a"),
        fg: hex("#e9e9ed"),
        card: hex("#1b1825"),
        primary: hex("#ac94db"),
        accent: hex("#dd9a3c"),
        muted: hex("#221f2d"),
        border: hex("#2a2735"),
        text_secondary: hex("#9a94ab"),
        online: hex("#7fd99a"),
        warning: hex("#e3b655"),
        danger: hex("#e2726a"),
    }
}

pub fn palette(dark: bool) -> Palette {
    if dark { night() } else { day() }
}
