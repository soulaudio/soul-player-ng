//! Lucide icons (the set the original soul-player uses) as Rune [`Icon`]s, so the
//! sidebar/player render the *same* glyphs at the *same* scale instead of text
//! stand-ins. Each body is the lucide element set on a 24-unit viewBox; stroke
//! icons wrap the elements in a `<g>` carrying lucide's defaults (`stroke-width:2`,
//! round caps/joins, `fill:none`) so they inherit, and paint in the tint colour
//! (`currentColor`) from `icon(..).color(..)`. Filled icons (play/pause) use
//! `fill="currentColor"` instead.
//!
//! Scales follow the original's Tailwind sizes: `w-3.5` (14) for shuffle/repeat,
//! `w-4` (16) for skip/play/pause/volume/heart/settings, `w-6`-relative for the
//! cover music note.
#![forbid(unsafe_code)]

use rune::Icon;

/// Wraps a lucide stroke body in the shared `<g>` of lucide defaults.
macro_rules! stroke_icon {
    ($name:literal, $body:literal) => {
        Icon {
            name: $name,
            view: 24.0,
            body: concat!(
                r#"<g fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">"#,
                $body,
                "</g>"
            ),
        }
    };
}

pub static SHUFFLE: Icon = stroke_icon!(
    "shuffle",
    r#"<polyline points="16 3 21 3 21 8"/><line x1="4" x2="21" y1="20" y2="3"/><polyline points="21 16 21 21 16 21"/><line x1="15" x2="21" y1="15" y2="21"/><line x1="4" x2="9" y1="4" y2="9"/>"#
);

pub static SKIP_BACK: Icon = stroke_icon!(
    "skip-back",
    r#"<polygon points="19 20 9 12 19 4 19 20"/><line x1="5" x2="5" y1="19" y2="5"/>"#
);

pub static SKIP_FORWARD: Icon = stroke_icon!(
    "skip-forward",
    r#"<polygon points="5 4 15 12 5 20 5 4"/><line x1="19" x2="19" y1="5" y2="19"/>"#
);

pub static REPEAT: Icon = stroke_icon!(
    "repeat",
    r#"<path d="m17 2 4 4-4 4"/><path d="M3 11v-1a4 4 0 0 1 4-4h14"/><path d="m7 22-4-4 4-4"/><path d="M21 13v1a4 4 0 0 1-4 4H3"/>"#
);

pub static REPEAT_1: Icon = stroke_icon!(
    "repeat-1",
    r#"<path d="m17 2 4 4-4 4"/><path d="M3 11v-1a4 4 0 0 1 4-4h14"/><path d="m7 22-4-4 4-4"/><path d="M21 13v1a4 4 0 0 1-4 4H3"/><path d="M11 10h1v4"/>"#
);

pub static VOLUME: Icon = stroke_icon!(
    "volume-2",
    r#"<polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5"/><path d="M15.54 8.46a5 5 0 0 1 0 7.07"/><path d="M19.07 4.93a10 10 0 0 1 0 14.14"/>"#
);

pub static VOLUME_X: Icon = stroke_icon!(
    "volume-x",
    r#"<polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5"/><line x1="22" x2="16" y1="9" y2="15"/><line x1="16" x2="22" y1="9" y2="15"/>"#
);

pub static HEART: Icon = stroke_icon!(
    "heart",
    r#"<path d="M19 14c1.49-1.46 3-3.21 3-5.5A5.5 5.5 0 0 0 16.5 3c-1.76 0-3 .5-4.5 2-1.5-1.5-2.74-2-4.5-2A5.5 5.5 0 0 0 2 8.5c0 2.3 1.5 4.05 3 5.5l7 7Z"/>"#
);

pub static SETTINGS: Icon = stroke_icon!(
    "settings",
    r#"<path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z"/><circle cx="12" cy="12" r="3"/>"#
);

pub static MUSIC: Icon = stroke_icon!(
    "music",
    r#"<path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/>"#
);

/// lucide `palette` — the Appearance settings category. A stroked blob with four
/// filled swatch dots (each carries its own `fill="currentColor"`, overriding the
/// group's `fill:none`, so they read as solid dots). Source: lucide.dev/icons/palette.
pub static PALETTE: Icon = stroke_icon!(
    "palette",
    r#"<path d="M12 22a1 1 0 0 1 0-20 10 9 0 0 1 10 9 5 5 0 0 1-5 5h-2.25a1.75 1.75 0 0 0-1.4 2.8l.3.4a1.75 1.75 0 0 1-1.4 2.8z"/><circle cx="13.5" cy="6.5" r=".5" fill="currentColor"/><circle cx="17.5" cy="10.5" r=".5" fill="currentColor"/><circle cx="6.5" cy="12.5" r=".5" fill="currentColor"/><circle cx="8.5" cy="7.5" r=".5" fill="currentColor"/>"#
);

/// lucide `info` — the About settings category (an "i" in a circle). Pure stroke.
/// Source: lucide.dev/icons/info.
pub static INFO: Icon = stroke_icon!(
    "info",
    r#"<circle cx="12" cy="12" r="10"/><path d="M12 16v-4"/><path d="M12 8h.01"/>"#
);

pub static PLUS: Icon = stroke_icon!("plus", r#"<path d="M5 12h14"/><path d="M12 5v14"/>"#);

/// lucide `chevron-down` — the disclosure caret on the language dropdown.
pub static CHEVRON_DOWN: Icon = stroke_icon!("chevron-down", r#"<path d="m6 9 6 6 6-6"/>"#);

/// lucide `grip-vertical` (six dots) — the drag affordance on a reorderable
/// queue row. Filled circles so it reads at the 16 pt grip size.
pub static GRIP: Icon = Icon {
    name: "grip-vertical",
    view: 24.0,
    body: r#"<circle cx="9" cy="5" r="1" fill="currentColor"/><circle cx="9" cy="12" r="1" fill="currentColor"/><circle cx="9" cy="19" r="1" fill="currentColor"/><circle cx="15" cy="5" r="1" fill="currentColor"/><circle cx="15" cy="12" r="1" fill="currentColor"/><circle cx="15" cy="19" r="1" fill="currentColor"/>"#,
};

/// lucide `x` — the per-row remove-from-queue affordance.
pub static X: Icon = stroke_icon!("x", r#"<path d="M18 6 6 18"/><path d="m6 6 12 12"/>"#);

pub static SEARCH: Icon = stroke_icon!(
    "search",
    r#"<circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/>"#
);

/// Play is drawn filled (a solid triangle on the primary button), not lucide's
/// outline — it reads better at button size.
pub static PLAY: Icon = Icon {
    name: "play",
    view: 24.0,
    body: r#"<polygon points="6 3 20 12 6 21 6 3" fill="currentColor"/>"#,
};

/// Pause, filled to match PLAY.
pub static PAUSE: Icon = Icon {
    name: "pause",
    view: 24.0,
    body: r#"<rect x="14" y="4" width="4" height="16" rx="1" fill="currentColor"/><rect x="6" y="4" width="4" height="16" rx="1" fill="currentColor"/>"#,
};
