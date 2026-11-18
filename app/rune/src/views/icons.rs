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

/// Lucide "activity" — the heart-rate pulse line. The title-bar scan-activity
/// indicator; it pulses (opacity) while a library scan runs.
pub static ACTIVITY: Icon = stroke_icon!("activity", r#"<path d="M22 12h-4l-3 9L9 3l-3 9H2"/>"#);

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

/// lucide `layout-grid` — the Discography grid-view toggle.
pub static GRID: Icon = stroke_icon!(
    "grid",
    r#"<rect width="7" height="7" x="3" y="3" rx="1"/><rect width="7" height="7" x="14" y="3" rx="1"/><rect width="7" height="7" x="14" y="14" rx="1"/><rect width="7" height="7" x="3" y="14" rx="1"/>"#
);
/// lucide `list` — the Discography list-view toggle.
pub static LIST: Icon = stroke_icon!(
    "list",
    r#"<line x1="8" x2="21" y1="6" y2="6"/><line x1="8" x2="21" y1="12" y2="12"/><line x1="8" x2="21" y1="18" y2="18"/><line x1="3" x2="3.01" y1="6" y2="6"/><line x1="3" x2="3.01" y1="12" y2="12"/><line x1="3" x2="3.01" y1="18" y2="18"/>"#
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

/// lucide `copy` — Duplicate profile. Source: lucide.dev/icons/copy.
pub static COPY: Icon = stroke_icon!(
    "copy",
    r#"<rect width="14" height="14" x="8" y="8" rx="2" ry="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/>"#
);

/// lucide `pencil` — Rename profile. Source: lucide.dev/icons/pencil.
pub static PENCIL: Icon = stroke_icon!(
    "pencil",
    r#"<path d="M21.174 6.812a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497z"/><path d="m15 5 4 4"/>"#
);

/// lucide `trash-2` — Delete profile. Source: lucide.dev/icons/trash-2.
pub static TRASH: Icon = stroke_icon!(
    "trash-2",
    r#"<path d="M3 6h18"/><path d="M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6"/><path d="M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2"/><line x1="10" x2="10" y1="11" y2="17"/><line x1="14" x2="14" y1="11" y2="17"/>"#
);

/// lucide `download` — Import profile. Source: lucide.dev/icons/download.
pub static DOWNLOAD: Icon = stroke_icon!(
    "download",
    r#"<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" x2="12" y1="15" y2="3"/>"#
);

/// lucide `upload` — Export profile. Source: lucide.dev/icons/upload.
pub static UPLOAD: Icon = stroke_icon!(
    "upload",
    r#"<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="17 8 12 3 7 8"/><line x1="12" x2="12" y1="3" y2="15"/>"#
);

/// lucide `folder` — the "Local folder…" choice in the add-source menu.
/// Source: lucide.dev/icons/folder.
pub static FOLDER: Icon = stroke_icon!(
    "folder",
    r#"<path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"/>"#
);

/// lucide `server` — the "Remote server…" (coming soon) choice in the add-source
/// menu. Source: lucide.dev/icons/server.
pub static SERVER: Icon = stroke_icon!(
    "server",
    r#"<rect width="20" height="8" x="2" y="2" rx="2" ry="2"/><rect width="20" height="8" x="2" y="14" rx="2" ry="2"/><line x1="6" x2="6.01" y1="6" y2="6"/><line x1="6" x2="6.01" y1="18" y2="18"/>"#
);

/// lucide `cloud` — the per-track "streams from a remote source" badge in a
/// track row / the now-playing line (`TrackView::remote_source`). A simple,
/// rounded glyph reads clearly at the small size a dense track list needs,
/// unlike `SERVER`'s finer detail. Source: lucide.dev/icons/cloud.
pub static CLOUD: Icon = stroke_icon!(
    "cloud",
    r#"<path d="M17.5 19H9a7 7 0 1 1 6.71-9h1.79a4.5 4.5 0 1 1 0 9Z"/>"#
);

/// lucide `github` — on the About page's "Report a problem" button (it files a
/// GitHub issue). Source: lucide.dev/icons/github.
pub static GITHUB: Icon = stroke_icon!(
    "github",
    r#"<path d="M15 22v-4a4.8 4.8 0 0 0-1-3.5c3 0 6-2 6-5.5.08-1.25-.27-2.48-1-3.5.28-1.15.28-2.35 0-3.5 0 0-1 0-3 1.5-2.64-.5-5.36-.5-8 0C6 2 5 2 5 2c-.3 1.15-.3 2.35 0 3.5A5.403 5.403 0 0 0 4 9c0 3.5 3 5.5 6 5.5-.39.49-.68 1.05-.85 1.65-.17.6-.22 1.23-.15 1.85v4"/><path d="M9 18c-4.51 2-5-2-7-2"/>"#
);

/// lucide `star` — on the About page's "Star this repository" button.
/// Source: lucide.dev/icons/star.
pub static STAR: Icon = stroke_icon!(
    "star",
    r#"<polygon points="12 2 15.09 8.26 22 9.27 17 14.14 18.18 21.02 12 17.77 5.82 21.02 7 14.14 2 9.27 8.91 8.26 12 2"/>"#
);

/// lucide `chevron-down` — the disclosure caret on the language dropdown.
pub static CHEVRON_DOWN: Icon = stroke_icon!("chevron-down", r#"<path d="m6 9 6 6 6-6"/>"#);

/// lucide `check` — marks the selected row in the dropdown panel.
pub static CHECK: Icon = stroke_icon!("check", r#"<path d="M20 6 9 17l-5-5"/>"#);

/// lucide `grip-vertical` (six dots) — the drag affordance on a reorderable
/// queue row. Filled circles so it reads at the 16 pt grip size.
pub static GRIP: Icon = Icon {
    name: "grip-vertical",
    view: 24.0,
    body: r#"<circle cx="9" cy="5" r="1" fill="currentColor"/><circle cx="9" cy="12" r="1" fill="currentColor"/><circle cx="9" cy="19" r="1" fill="currentColor"/><circle cx="15" cy="5" r="1" fill="currentColor"/><circle cx="15" cy="12" r="1" fill="currentColor"/><circle cx="15" cy="19" r="1" fill="currentColor"/>"#,
};

/// lucide `x` — the per-row remove-from-queue affordance.
pub static X: Icon = stroke_icon!("x", r#"<path d="M18 6 6 18"/><path d="m6 6 12 12"/>"#);

// --- Windows caption glyphs ------------------------------------------------
// Drawn over the (hidden) native `_ [] X` buttons, so they're tuned to match
// Windows 11's own symbols: a ~10 px mark with a thin, square-capped stroke
// (not lucide's rounded 2 px). Sized by `GLYPH_SIZE` in `titlebar.rs` so the
// 16-unit marks land near 10 px. `stroke-width` is deliberately light here.

/// Windows caption minimize glyph: a single thin horizontal rule.
pub static CAPTION_MIN: Icon = Icon {
    name: "caption-min",
    view: 24.0,
    body: r#"<g fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="square"><path d="M4 12h16"/></g>"#,
};

/// Windows caption maximize glyph: a near-square outline (hair of a corner
/// radius, as Windows 11 draws it).
pub static CAPTION_MAX: Icon = Icon {
    name: "caption-max",
    view: 24.0,
    body: r#"<g fill="none" stroke="currentColor" stroke-width="1.5"><rect x="4.5" y="4.5" width="15" height="15" rx="0.5"/></g>"#,
};

/// Windows caption close glyph: a thin, square-capped cross.
pub static CAPTION_CLOSE: Icon = Icon {
    name: "caption-close",
    view: 24.0,
    body: r#"<g fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="square"><path d="M5 5 19 19M19 5 5 19"/></g>"#,
};

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
