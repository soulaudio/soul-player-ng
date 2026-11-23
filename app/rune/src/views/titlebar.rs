//! The app-drawn window title-bar band. Rune's `App::titlebar` (set in `main.rs`)
//! keeps the native window controls — macOS traffic lights (leading edge, over the
//! sidebar) and Windows `_ [] X` caption buttons (trailing edge) — and this draws
//! the band they sit in, over the content. `.window_drag()` moves the window
//! (double-click zooms; the run loop handles it).
//!
//! On Windows we draw our OWN caption glyphs over the native buttons: the native
//! symbols are tinted transparent (`caption_color` alpha 0) so only ours show,
//! while the native buttons underneath still take the clicks (preserving the OS
//! minimize/maximize/close wiring, Aero Snap and the resize edges). The glyphs
//! are always visible, matching native Windows 11 (which never hides its caption
//! buttons) — see the note on `bar` for why hover-to-reveal can't work here.
#![forbid(unsafe_code)]

use rune::{Align, Cursor, Cx, El, Icon, Side, Tx, col, el, icon, row, text};
use soul_player_host::{ScanPhase, ScanProgress, SourceKind, SourceStatus};

use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::views::{icons, settings};
use crate::{State, close_remote_health_popover, set_settings_tab, toggle_remote_health_popover};

/// Title-bar band height, in points.
pub(crate) const HEIGHT: f32 = 32.0;

/// Where the macOS close button's top-left sits (Electron's `trafficLightPosition`):
/// standard left padding, vertically centred for the ~12 pt lights. `main.rs` passes
/// this to `Titlebar::traffic_lights`. macOS only — they sit over the sidebar.
pub(crate) const TRAFFIC_LIGHTS: (f32, f32) = (19.0, (HEIGHT - 12.0) / 2.0);

/// Caption-glyph icon size, in points. The glyphs' marks fill ~16 of their 24-unit
/// viewbox, so this lands them near the native ~10 pt Windows 11 symbol.
const GLYPH_SIZE: f32 = 15.0;

/// Whether this platform needs the app to draw its own working window controls.
///
/// Windows gets real native buttons under our glyphs and macOS its traffic
/// lights, both placed by Rune. Linux gets neither (Rune's custom titlebar is
/// macOS/Windows-only), and on Wayland — where client-side decorations are the
/// norm and tiling compositors draw no titlebar at all — that means no window
/// controls exist unless we draw them.
///
/// Unconditional on Linux rather than probed: nothing in the winit/Rune surface
/// reports whether the compositor actually granted server-side decorations
/// (`is_decorated` only echoes what was requested), so there is no honest
/// runtime test. On a desktop that does decorate, this shows our buttons in the
/// app's own band below the system titlebar; having them twice is a far smaller
/// problem than having none.
const DRAWS_OWN_CAPTION_BUTTONS: bool = cfg!(target_os = "linux");

/// A caption button's width in points — Rune's own
/// `Titlebar::CAPTION_BUTTON_WIDTH` (Windows' and Electron's 46), so the band's
/// trailing controls measure the same whether the platform or the app drew them.
const CAPTION_BUTTON_W: f32 = 46.0;

/// Aggregate health across every ENABLED remote source — the title-bar
/// indicator's at-a-glance color, computed by [`remote_health_summary`].
/// Disabled remotes never factor in (see that function); online/offline is
/// read straight off each [`SourceStatus::remote`]'s last probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteSummary {
    /// Every enabled remote answered its last health probe.
    Healthy,
    /// At least one enabled remote is online and at least one is offline.
    Partial,
    /// Every enabled remote is offline (including "never probed yet").
    Down,
}

/// The title-bar remote-health indicator's aggregate status, over `sources`'
/// ENABLED remotes only — a disabled remote says nothing about session
/// health (the user turned it off on purpose), so it neither counts toward
/// nor against the color. Local sources are ignored entirely (they have no
/// "online" concept). Returns `None` when there are no enabled remotes at
/// all — including when there are only disabled ones, or none configured —
/// telling the caller to HIDE the indicator rather than show a trivial
/// all-or-nothing badge for zero remotes.
///
/// Pure (no clock, no I/O): a plain fold over `sources`, so it's exactly as
/// fresh as whatever snapshot the caller passes (`State::sources`, kept
/// current by the periodic poller and on-demand refreshes) and trivially
/// unit-testable without a `Runtime` or a network call.
pub(crate) fn remote_health_summary(sources: &[SourceStatus]) -> Option<RemoteSummary> {
    let mut enabled = 0usize;
    let mut online = 0usize;
    for status in sources {
        if !status.enabled || !matches!(status.kind, SourceKind::Remote { .. }) {
            continue;
        }
        enabled += 1;
        if status.remote.as_ref().is_some_and(|h| h.online) {
            online += 1;
        }
    }
    if enabled == 0 {
        None
    } else if online == enabled {
        Some(RemoteSummary::Healthy)
    } else if online == 0 {
        Some(RemoteSummary::Down)
    } else {
        Some(RemoteSummary::Partial)
    }
}

/// The title-bar back arrow shown at the leading edge on a detail route (desktop).
/// Returns to the previous view via [`crate::go_back`]; keyed `detail-back` and
/// labelled, so the a11y sweep and screen readers find it on every detail page.
fn detail_back_button(p: &Palette) -> El {
    el().key("detail-back")
        .role("button")
        .label(t!("common.back").to_string())
        .focusable(true)
        .size(28.0, 28.0)
        .center()
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .hover(p.fg.alpha(fx::HOVER))
        .press(p.fg.alpha(fx::PRESS))
        .child(text("←").font(type_::NAV).color(p.fg))
        .on_press(crate::go_back)
}

/// The window title-bar band over the content: `HEIGHT` tall, draggable. On Windows
/// it carries our own always-visible `_ [] X` glyphs at the trailing edge; the open
/// area to their left is where custom header components go later.
///
/// The glyphs don't hover-reveal: the native caption buttons are a non-client OS
/// region (`WM_NCHITTEST` → `HTMINBUTTON`/`HTMAXBUTTON`/`HTCLOSE`), so the pointer
/// over them never reaches the app's client-area hover group — a reveal would blank
/// out exactly as you reach for a button. Native Windows keeps them always shown, so
/// we do too.
pub(crate) fn bar(s: &State, p: &Palette, cx: &mut Cx<State>, mobile: bool) -> El {
    let chrome = cx.chrome_insets();
    // Hide the native Windows caption symbols — we draw our own (animatable) glyphs
    // over the buttons, which still take the clicks. alpha 0 keeps macOS unaffected.
    cx.caption_color(p.fg.alpha(0.0));
    cx.titlebar_height(HEIGHT);

    // The BAND's own committed width (previous frame) — not the window's. On desktop
    // the band sits inside the shell, right of the resizable sidebar, so its width is
    // `window − sidebar`; measuring it (rather than `app-root`) keeps the widget's
    // responsive variant and its right dock correct at every sidebar/window size.
    let band_w = cx
        .layout_size("titlebar")
        .map(|size| size[0])
        .filter(|w| *w > 0.0)
        .unwrap_or(1100.0);

    // The flexible header slot. While a scan runs it carries the right-aligned
    // activity widget; whenever at least one remote source is ENABLED it also
    // carries the remote-health indicator — the two coexist (gap between them) —
    // a leading grow spacer pushes whichever are present to the trailing edge,
    // just left of the window controls; otherwise it's the empty draggable gap.
    let remote_summary = remote_health_summary(&s.sources);
    let mut content = row()
        .key("titlebar-content")
        .grow(1.0)
        .min_w(0.0)
        .h_full()
        .items_center()
        .gap(space::SM);
    // A back arrow at the leading edge on a detail route, inline with the window
    // controls (the original's title-bar back). Desktop only: the narrow layout
    // uses the mobile top bar's own back (see `sidebar::mobile_top_bar`). The
    // title-bar band starts right of the sidebar, so this lands at the top-left
    // of the content area, exactly where the original puts it.
    let on_detail = matches!(
        s.route,
        crate::Route::Album | crate::Route::Artist | crate::Route::Genre | crate::Route::Playlist
    );
    if !mobile && on_detail {
        content = content.child(detail_back_button(p));
    }
    if s.scan_progress.is_some() || remote_summary.is_some() {
        content = content.child(el().grow(1.0));
        if let Some(prog) = &s.scan_progress {
            content = content.child(scan_activity(prog, s.scan_anim, p, band_w));
        }
        if let Some(summary) = remote_summary {
            content = content.child(remote_health_indicator(summary, p));
        }
        content = content.pr(space::SM);
    }

    let strip = row()
        .key("titlebar")
        .w_full()
        .h(HEIGHT)
        .fill(p.bg)
        .items_center()
        .pl(space::MD)
        .window_drag()
        .child(content);

    // Windows (`trailing` > 0): our three always-visible caption glyphs span the
    // trailing gutter, one per native button, which takes the clicks underneath.
    if chrome.trailing > 0.0 {
        let cell = chrome.trailing / 3.0;
        strip.child(
            row()
                .key("caption-glyphs")
                .h_full()
                .child(caption_glyph("caption-min", &icons::CAPTION_MIN, cell, p))
                .child(caption_glyph("caption-max", maximize_glyph(s), cell, p))
                .child(caption_glyph(
                    "caption-close",
                    &icons::CAPTION_CLOSE,
                    cell,
                    p,
                )),
        )
    } else if DRAWS_OWN_CAPTION_BUTTONS {
        // Linux: Rune's custom-titlebar mode is implemented for macOS and Windows
        // only ("elsewhere the native frame stays and ChromeInsets is zero"), so
        // nothing reserves a trailing gutter here and no native control exists to
        // sit under a glyph. On the modern Linux desktop that leaves the window
        // with NO controls at all: Wayland compositors mostly expect the client to
        // draw its own, and a tiling one (Hyprland, sway) draws no titlebar
        // whatsoever. So draw real buttons that carry their own presses.
        //
        // They live inside the `.window_drag()` strip; a child with its own
        // `on_press` keeps the press rather than moving the window (same rule the
        // remote-health indicator below relies on).
        strip.child(
            row()
                .key("caption-buttons")
                .h_full()
                .child(caption_button(
                    "caption-min",
                    &icons::CAPTION_MIN,
                    t!("titlebar.minimize").to_string(),
                    p,
                    |_s: &mut State, tx: &mut Tx| tx.minimize(),
                ))
                .child(caption_button(
                    "caption-max",
                    maximize_glyph(s),
                    maximize_label(s),
                    p,
                    crate::toggle_maximized_from_titlebar,
                ))
                .child(caption_button(
                    "caption-close",
                    &icons::CAPTION_CLOSE,
                    t!("titlebar.close").to_string(),
                    p,
                    |_s: &mut State, tx: &mut Tx| tx.close_window(),
                )),
        )
    } else {
        strip
    }
}

/// Remote-health indicator icon size and status-dot size, in points — sized
/// alongside the scan widget's own `SCAN_ICON` so the two read as peers in
/// the same strip.
const HEALTH_ICON: f32 = 14.0;
const HEALTH_DOT: f32 = 8.0;
/// Gap between the remote-health indicator and its opened popover.
const HEALTH_POPOVER_GAP: f32 = 6.0;

/// The title-bar remote-health indicator: a cloud glyph (the "remote" icon)
/// plus a small status dot colored by `summary` — green/amber/red for
/// healthy/partial/down. Pressing it opens [`overlay`]'s popover.
///
/// This is a CLICK target, not a hover tooltip, even though hover itself
/// would technically fire here (unlike the always-visible caption glyphs
/// above, this sits in the ordinary draggable client area, not over a real
/// OS non-client control — a child with its own `on_press` inside a
/// `.window_drag()` region keeps its press rather than moving the window).
/// Two things rule hover out anyway: the content is a multi-row list (per
/// remote: host, dot, latency, version, last-checked) that Rune's built-in
/// `El::help` tooltip can't render — it draws plain text only; and on macOS
/// `El::help` shows as AppKit's OWN tooltip, outside Rune's scene tree, so a
/// headless `Runtime::elements()` test could never see its content there —
/// unlike a click-opened, Rune-drawn popover, which renders (and is
/// testable) identically on every platform the rune UI head ships on.
fn remote_health_indicator(summary: RemoteSummary, p: &Palette) -> El {
    let (color, label) = match summary {
        RemoteSummary::Healthy => (p.online, t!("titlebar.remoteHealthy")),
        RemoteSummary::Partial => (p.warning, t!("titlebar.remotePartial")),
        RemoteSummary::Down => (p.danger, t!("titlebar.remoteDown")),
    };
    row()
        .key("remote-health-indicator")
        .role("button")
        .label(label.to_string())
        .focusable(true)
        .cursor(Cursor::Pointer)
        .h_full()
        .items_center()
        .gap(space::XXS)
        .px(space::XS)
        .radius(radius::SM)
        .hover(p.fg.alpha(0.06))
        .on_press(toggle_remote_health_popover)
        .child(icon(&icons::CLOUD, HEALTH_ICON).color(p.text_secondary))
        .child(
            el().key("remote-health-dot")
                .size(HEALTH_DOT, HEALTH_DOT)
                .radius(HEALTH_DOT * 0.5)
                .fill(color),
        )
}

/// One remote's row in the health popover: host, a status word + dot, and
/// the same latency/version/last-checked telemetry Settings → Sources shows
/// — reusing its formatters (`settings::display_host`/`latency_text`/
/// `relative_time`) rather than re-deriving them, so the two views can never
/// drift on how a latency or a relative time reads.
fn health_popover_row(i: usize, status: &SourceStatus, p: &Palette) -> El {
    let SourceKind::Remote { url } = &status.kind else {
        // `health_popover_rows` only ever calls this with a Remote source.
        return el().key(format!("remote-health-row-{i}"));
    };
    let host = settings::display_host(url);
    let online = status.enabled && status.remote.as_ref().is_some_and(|h| h.online);
    let status_word = if !status.enabled {
        t!("titlebar.remoteDisabled").to_string()
    } else if online {
        t!("settings.remoteOnline").to_string()
    } else {
        t!("settings.offline").to_string()
    };
    let latency = status.remote.as_ref().and_then(|h| h.latency_ms);
    let version = status
        .remote
        .as_ref()
        .and_then(|h| h.server_version.clone())
        .unwrap_or_else(|| "—".to_string());
    let checked = status.remote.as_ref().and_then(|h| h.last_checked_unix);
    let dot_color = if online { p.online } else { p.text_secondary };

    col()
        .key(format!("remote-health-row-{i}"))
        .role("group")
        .label(format!("{host}: {status_word}"))
        .w_full()
        .gap(space::XXS)
        .py(space::XS)
        .child(
            row()
                .gap(space::SM)
                .items_center()
                .child(
                    el().key(format!("remote-health-row-dot-{i}"))
                        .size(HEALTH_DOT, HEALTH_DOT)
                        .radius(HEALTH_DOT * 0.5)
                        .fill(dot_color),
                )
                .child(
                    text(host)
                        .font(type_::ITEM)
                        .color(p.fg)
                        .grow(1.0)
                        .truncate(),
                )
                .child(text(status_word).font(type_::CAPTION).color(dot_color)),
        )
        .child(
            row()
                .gap(space::SM)
                .flex_wrap()
                .child(
                    text(settings::latency_text(latency))
                        .key(format!("remote-health-row-latency-{i}"))
                        .font(type_::LABEL.mono())
                        .color(p.text_secondary),
                )
                .child(
                    text(version)
                        .font(type_::LABEL.mono())
                        .color(p.text_secondary),
                )
                .child(
                    text(settings::relative_time(settings::now_unix(), checked))
                        .font(type_::CAPTION)
                        .color(p.text_secondary),
                ),
        )
}

/// Every remote in `s.sources`, in order — the popover lists ALL of them
/// (enabled or not), unlike the aggregate indicator which only counts enabled
/// ones, so a disabled remote never silently disappears from view.
fn health_popover_rows(s: &State, p: &Palette) -> Vec<El> {
    s.sources
        .iter()
        .filter(|st| matches!(st.kind, SourceKind::Remote { .. }))
        .enumerate()
        .map(|(i, st)| health_popover_row(i, st, p))
        .collect()
}

/// The remote-health popover, mounted at the window root (via
/// [`crate::view`]'s overlay layer) and anchored under the indicator —
/// mirrors [`crate::views::signal_path::overlay`]'s shape (anchored + modal,
/// toggled by a `State` flag). `None` when closed, or if every remote
/// vanished from `s.sources` while it was open (nothing left to list).
pub fn overlay(s: &State, p: &Palette) -> Option<El> {
    if !s.remote_health_open {
        return None;
    }
    let rows = health_popover_rows(s, p);
    if rows.is_empty() {
        return None;
    }
    let mut panel = col()
        .key("remote-health-popover")
        .role("group")
        .label(t!("titlebar.remoteSources").to_string())
        .anchored_fit(
            "remote-health-indicator",
            Side::Bottom,
            Align::End,
            HEALTH_POPOVER_GAP,
        )
        .focus_group()
        .focus_return("remote-health-indicator".to_string())
        .fill(p.card)
        .radius(radius::MD)
        .border_sides([1.0, 1.0, 1.0, 1.0], p.border)
        .padding(space::MD)
        .gap(space::XXS)
        .min_w(260.0)
        .modal(true, close_remote_health_popover)
        .child(
            text(t!("titlebar.remoteSources").to_string())
                .font(type_::SECTION)
                .color(p.text_secondary),
        );
    for row_el in rows {
        panel = panel.child(row_el);
    }
    Some(
        panel.child(
            row()
                .key("remote-health-open-settings")
                .role("button")
                .focusable(true)
                .label(t!("titlebar.openSourcesSettings").to_string())
                .cursor(Cursor::Pointer)
                .items_center()
                .gap(space::XS)
                .py(space::XS)
                .child(
                    text(t!("titlebar.openSourcesSettings").to_string())
                        .font(type_::LABEL)
                        .color(p.primary),
                )
                .on_press(|s: &mut State| {
                    s.route = crate::Route::Settings;
                    set_settings_tab(s, crate::SettingsTab::MusicData);
                    close_remote_health_popover(s);
                }),
        ),
    )
}

/// One caption glyph cell: `width` wide, full height, glyph centred. Always
/// visible. Purely visual — the native button underneath takes the click.
fn caption_glyph(key: &'static str, glyph: &'static Icon, width: f32, p: &Palette) -> El {
    el().key(key)
        .w(width)
        .h_full()
        .center()
        .child(icon(glyph, GLYPH_SIZE).color(p.fg))
}

/// One WORKING caption button, for platforms with no native control to sit under
/// a glyph (see [`DRAWS_OWN_CAPTION_BUTTONS`]). Unlike [`caption_glyph`] — a
/// purely visual overlay on Windows' real buttons — this carries the press.
///
/// Styled as the ordinary ghost control the rest of the app uses (a foreground
/// wash on hover, one step up on press) rather than inventing a treatment: it
/// stays legible in both themes, including the close button, which a red fill
/// would not reliably do under the light palette's dark foreground.
/// The middle caption button's glyph: the restore mark while the window is
/// maximized or full screen, else maximize — the swap Windows makes on its own
/// caption button, and which the app's overlay glyph has to make too (it covers
/// the native symbol, so a static glyph pins it to "maximize" forever).
fn maximize_glyph(s: &State) -> &'static Icon {
    if s.maximized {
        &icons::CAPTION_RESTORE
    } else {
        &icons::CAPTION_MAX
    }
}

/// The matching accessible name, so the button doesn't announce "Maximize"
/// while it would actually restore.
fn maximize_label(s: &State) -> String {
    if s.maximized {
        t!("titlebar.restore").to_string()
    } else {
        t!("titlebar.maximize").to_string()
    }
}

/// `label` is the accessible name — a bare glyph has no text for a screen
/// reader to read, and unlike Windows' native caption buttons (which the OS
/// names for us) nothing else here supplies one.
fn caption_button(
    key: &'static str,
    glyph: &'static Icon,
    label: String,
    p: &Palette,
    on_press: impl Fn(&mut State, &mut Tx) + 'static,
) -> El {
    el().key(key)
        .role("button")
        .label(label)
        .focusable(true)
        .w(CAPTION_BUTTON_W)
        .h_full()
        .center()
        .cursor(Cursor::Pointer)
        .hover(p.fg.alpha(fx::HOVER))
        .press(p.fg.alpha(fx::PRESS))
        .on_press_tx(on_press)
        .child(icon(glyph, GLYPH_SIZE).color(p.fg))
}

/// Width-responsive thresholds (band width, pt) for the scan-activity widget: at or
/// above each, one more element joins the icon. Below the narrowest, only the pulsing
/// icon shows (the `smallest width → just the animated icon` case). The band is the
/// strip the widget lives in (`window − sidebar` on desktop), not the whole window.
const SCAN_WIDE: f32 = 700.0; // + phase label
const SCAN_MED: f32 = 520.0; // + progress bar
const SCAN_NARROW: f32 = 380.0; // + done/total count

/// Activity-icon size and progress-bar geometry (pt).
const SCAN_ICON: f32 = 14.0;
const SCAN_BAR_W: f32 = 84.0;
const SCAN_BAR_H: f32 = 4.0;
/// Pulse rate (rad/s) of the activity icon's opacity "breathing".
const SCAN_PULSE_SPEED: f32 = 4.2;

/// The title-bar scan-activity widget: a pulsing activity icon plus — as the band
/// widens — the phase label, a determinate progress bar, and the `done / total`
/// count. Rendered right-aligned in the header slot while a scan runs. `anim` is the
/// free-running pulse phase (seconds) the poll advances; `band_w` (the strip's own
/// width) picks the responsive variant.
fn scan_activity(prog: &ScanProgress, anim: f32, p: &Palette, band_w: f32) -> El {
    // Breathing opacity in [0.5, 1.0] so the icon reads as "working" even between
    // progress ticks (the determinate bar carries the exact amount).
    let icon_op = 0.5 + 0.5 * (0.5 + 0.5 * (anim * SCAN_PULSE_SPEED).sin());

    let mut rowel = row()
        .key("titlebar-scan")
        .h_full()
        .items_center()
        .gap(space::XS)
        .child(
            el().key("titlebar-scan-icon")
                .child(icon(&icons::ACTIVITY, SCAN_ICON).color(p.primary.alpha(icon_op))),
        );

    // Phase label (widest only).
    if band_w >= SCAN_WIDE {
        let label = match prog.phase {
            ScanPhase::Discovering => t!("titlebar.scanning"),
            ScanPhase::Extracting => t!("titlebar.indexing"),
        }
        .to_string();
        rowel = rowel.child(
            text(label)
                .key("titlebar-scan-label")
                .font(type_::CAPTION)
                .color(p.text_secondary),
        );
    }

    // Determinate progress bar (medium and up).
    if band_w >= SCAN_MED {
        let frac = if prog.total > 0 {
            (prog.done as f32 / prog.total as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        rowel = rowel.child(
            el().key("titlebar-scan-bar")
                .w(SCAN_BAR_W)
                .h(SCAN_BAR_H)
                .fill(p.muted)
                .radius(radius::FULL)
                .child(
                    el().w(SCAN_BAR_W * frac)
                        .h(SCAN_BAR_H)
                        .fill(p.primary)
                        .radius(radius::FULL),
                ),
        );
    }

    // done / total count (narrow and up).
    if band_w >= SCAN_NARROW {
        let count = format!(
            "{} / {}",
            group_thousands(prog.done),
            group_thousands(prog.total)
        );
        rowel = rowel.child(
            text(count)
                .key("titlebar-scan-count")
                .font(type_::LABEL.mono())
                .color(p.text_secondary),
        );
    }

    rowel
}

/// Group a non-negative integer with ASCII thousands separators ("1,024").
/// Deterministic and locale-independent — the title bar is no place for
/// locale-sensitive number formatting (which would also flake locale-ordered tests).
fn group_thousands(n: usize) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, &b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(b as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use soul_player_host::RemoteHealth;

    use super::*;

    /// The maximize button's glyph and name both follow the window state. The
    /// glyph can't be read back off a rendered element (Rune's `ElementInfo`
    /// carries no icon), so the decision is asserted here, at the pure helpers,
    /// and its wiring through the view is covered by `main`'s
    /// `the_maximize_button_relabels_itself_while_maximized`.
    #[test]
    fn maximize_becomes_restore_while_the_window_is_maximized() {
        let _locale = crate::config::use_test_locale("en");
        let mut s = State::new(soul_player_host::PlayerHandle::offline());

        s.maximized = false;
        assert_eq!(maximize_glyph(&s).name, "caption-max");
        assert_eq!(maximize_label(&s), "Maximize");

        s.maximized = true;
        assert_eq!(
            maximize_glyph(&s).name,
            "caption-restore",
            "a maximized window must offer RESTORE, not another maximize"
        );
        assert_eq!(maximize_label(&s), "Restore");
    }

    /// A remote `SourceStatus` with just the two fields [`remote_health_summary`]
    /// reads (`enabled`, and the probe's `online`) — everything else is a
    /// placeholder, since the pure summary never looks at it.
    fn remote(enabled: bool, online: bool) -> SourceStatus {
        SourceStatus {
            path: PathBuf::from("http://remote.test"),
            enabled,
            available: online,
            track_count: 0,
            kind: SourceKind::Remote {
                url: "http://remote.test".to_string(),
            },
            remote: Some(RemoteHealth {
                online,
                latency_ms: online.then_some(10),
                server_version: None,
                index_version: None,
                last_checked_unix: None,
            }),
        }
    }

    fn local() -> SourceStatus {
        SourceStatus {
            path: PathBuf::from("/music"),
            enabled: true,
            available: true,
            track_count: 3,
            kind: SourceKind::Local {
                path: PathBuf::from("/music"),
            },
            remote: None,
        }
    }

    /// `remote_health_summary` is a pure fold over a sources snapshot: only ENABLED
    /// remotes count, locals never affect it, and the enabled-remote online/offline mix
    /// picks the indicator state — all-online → Healthy, some-offline → Partial,
    /// all-offline → Down, no enabled remotes → hidden (None). One table over every
    /// case (was seven near-duplicate tests). Because it is called fresh per row it also
    /// shows the fold keeps no memory between snapshots (what the periodic poller relies
    /// on when it re-folds each refreshed `sources()`).
    #[test]
    fn remote_health_summary_folds_a_sources_snapshot_to_the_indicator_state() {
        let cases: Vec<(&str, Vec<SourceStatus>, Option<RemoteSummary>)> = vec![
            ("no sources at all", vec![], None),
            ("only a local source", vec![local()], None),
            (
                "one enabled online remote",
                vec![remote(true, true)],
                Some(RemoteSummary::Healthy),
            ),
            (
                "one enabled offline remote",
                vec![remote(true, false)],
                Some(RemoteSummary::Down),
            ),
            (
                "every enabled remote online",
                vec![remote(true, true), remote(true, true)],
                Some(RemoteSummary::Healthy),
            ),
            (
                "a mix of online and offline enabled remotes",
                vec![remote(true, true), remote(true, false)],
                Some(RemoteSummary::Partial),
            ),
            (
                "every enabled remote offline",
                vec![remote(true, false), remote(true, false)],
                Some(RemoteSummary::Down),
            ),
            (
                "a disabled offline remote does not drag an online one down to Partial",
                vec![remote(true, true), remote(false, false)],
                Some(RemoteSummary::Healthy),
            ),
            (
                "every remote disabled: zero enabled remotes, so the indicator hides",
                vec![remote(false, true), remote(false, false)],
                None,
            ),
            (
                "a local source never affects the remote summary",
                vec![local(), remote(true, false)],
                Some(RemoteSummary::Down),
            ),
        ];
        for (label, sources, want) in cases {
            assert_eq!(remote_health_summary(&sources), want, "case: {label}");
        }
    }
}
