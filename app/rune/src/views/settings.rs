//! Settings page — a left category rail (Appearance / Music Data / Audio /
//! About) switching a content pane of grouped **cards**, mirroring the original
//! soul-player's `SettingsLayout` (a narrow nav aside + a `max-w` content column
//! of rounded `bg-muted` cards, each a heading + description per group).
//!
//! The groups manage: the Day/Night theme and UI language (Appearance), the
//! host's watched-folder set (Music Data), the output device + ReplayGain
//! (Audio), and the running version / updater (About). Add-source uses the
//! native folder dialog — Rune's IME is pre-release, so there is no text entry
//! here. The rail lives INSIDE this page (not the main sidebar), so it only
//! shows while Settings is routed.
#![forbid(unsafe_code)]

use rune::{
    Align, Color, Cursor, Cx, Drag, El, FileDialogOptions, Icon, Live, Phase, ScrollSpec, Shadow,
    Tx, WheelEvent, col, el, icon, row, text, text_input,
};
use soul_player_host::organize;
use soul_player_host::{
    BandKind, DspUnit, GAIN_LIMIT_DB, RemoteHealth, ReplayGainMode, SourceKind, SourceStatus,
};

use crate::config::AlbumYearSource;
use crate::organize_job::{scan_audio, template_for};
use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::views::dropdown;
use crate::views::icons;
use crate::views::scrollbar;
use crate::views::sidebar::{NAV_DEFS, nav_label};
use crate::views::slider::slider;
use crate::{
    NumField, OrganizePhase, SettingsTab, State, ThemePref, add_dsp_unit, add_eq_band,
    apply_and_save, apply_watch, close_dropdown, close_remote_form, commit_dsp_rename,
    commit_eq_edit, commit_profile_rename, connect_remote_source, cycle_eq_band_kind,
    delete_profile, disconnect_remote_source, duplicate_profile, eq_edit_start, eq_num_drag_end,
    eq_num_drag_move, eq_num_drag_start, eq_num_scroll, export_eq, export_profile, import_eq,
    import_profile, new_profile, open_remote_form, organize_begin, organize_confirm,
    organize_dismiss, organize_ready, reconnect_remote_now, remove_dsp_unit, remove_eq_band,
    reorder_dsp_units, scan_in_background, set_album_year, set_dsp_unit_enabled,
    set_eq_band_enabled, set_eq_preamp, set_exclusive, set_exclusive_strict, set_indexing_mode,
    set_locale_pref, set_managed_dest, set_managed_strategy, set_managed_template, set_nav_visible,
    set_output_device, set_replaygain_mode, set_replaygain_preamp, set_settings_tab,
    set_theme_pref, start_dsp_rename, start_profile_rename, switch_profile, toggle_dsp_expanded,
    toggle_eq_bands_expanded, toggle_remote_enabled, unit_bands, unit_enabled, unit_preamp,
};
use soul_player_host::IndexingMode;

/// Preamp slider bounds, in dB (a conventional ReplayGain preamp range).
const RG_PREAMP_MIN: f32 = -15.0;
const RG_PREAMP_MAX: f32 = 15.0;

/// Points of trackpad `precise` wheel travel that make one discrete number-box step,
/// so a fine two-finger scroll accumulates into steps instead of flying or stalling.
const WHEEL_NOTCH_PX: f32 = 16.0;

/// Width of the category rail (the original's narrow settings `aside`, widened
/// to seat an icon + label per row).
const NAV_WIDTH: f32 = 184.0;
/// Width cap on the content column (the original's `max-w-4xl mx-auto`).
const CONTENT_MAX_W: f32 = 860.0;

fn basename(p: &std::path::Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string_lossy().into_owned())
}

// ── Card scaffolding ─────────────────────────────────────────────────────────

/// A settings group as a card: a rounded, padded panel on the card surface —
/// mirrors the original's `bg-muted/30 rounded-lg p-6`.
fn card(key: &str, p: &Palette) -> El {
    col()
        .key(key.to_string())
        // Fill the content column so every card shares one width and left edge,
        // regardless of how wide its own controls are (the About card's lone
        // version line would otherwise shrink the whole column and drift centred).
        .w_full()
        .fill(p.card)
        .radius(radius::LG)
        .padding(space::LG)
        .gap(space::MD)
}

/// A card's heading: a bold title over a secondary-colour description line.
/// Both `wrap` so a longer-locale description reflows onto more lines instead of
/// forcing the card (or, in the Music Data header, its action buttons) to
/// overflow — the heading's intrinsic width then falls to its widest word, so a
/// `shrink`-ing parent can give the row's trailing controls their space.
fn heading(title: String, desc: String, p: &Palette) -> El {
    col()
        .gap(space::XXS)
        .child(text(title).font(type_::TITLE).color(p.fg).wrap())
        .child(
            text(desc)
                .font(type_::CAPTION)
                .color(p.text_secondary)
                .wrap(),
        )
}

// ── Category rail ────────────────────────────────────────────────────────────

/// One category row: an icon + label that switches `State::settings_tab`. The
/// active row is filled with the primary colour (like the original's
/// `bg-primary` active item); the rest tint faintly on hover. Pointer cursor
/// throughout.
fn category_item(
    key: &str,
    glyph: &'static Icon,
    label: String,
    tab: SettingsTab,
    active: bool,
    p: &Palette,
) -> El {
    let fg = if active { p.bg } else { p.text_secondary };
    let item = el()
        .key(key.to_string())
        .role("tab")
        .label(label.clone())
        .focusable(true)
        .w_full()
        .px(space::MD)
        .py(space::SM)
        .radius(radius::MD)
        .cursor(Cursor::Pointer)
        .child(
            row()
                .gap(space::SM)
                .center_y()
                .child(icon(glyph, 18.0).color(fg))
                .child(text(label).font(type_::ITEM_STRONG).color(fg)),
        )
        .on_press(move |s: &mut State| set_settings_tab(s, tab));
    if active {
        item.fill(p.primary)
    } else {
        item.hover(p.fg.alpha(0.07))
    }
}

/// The left category rail: Appearance / Music Data / Audio / About, each icon +
/// label. Music Data uses the music note and Audio the speaker; Appearance and
/// About use the palette / info glyphs added to `views::icons`.
fn category_nav(s: &State, p: &Palette) -> El {
    let tab = s.settings_tab;
    col()
        .key("settings-nav")
        .w(NAV_WIDTH)
        .h_full()
        .border_sides([0.0, 1.0, 0.0, 0.0], p.border)
        // Top inset `LG` so the first category item's box lines up with the main
        // sidebar's first nav item and the content pane's first card; sides and
        // bottom stay `MD`.
        .px(space::MD)
        .pt(space::LG)
        .pb(space::MD)
        .gap(space::XXS)
        .child(category_item(
            "cat-appearance",
            &icons::PALETTE,
            t!("settings.appearance").to_string(),
            SettingsTab::Appearance,
            tab == SettingsTab::Appearance,
            p,
        ))
        .child(category_item(
            "cat-music-data",
            &icons::MUSIC,
            t!("settings.musicData").to_string(),
            SettingsTab::MusicData,
            tab == SettingsTab::MusicData,
            p,
        ))
        .child(category_item(
            "cat-audio",
            &icons::VOLUME,
            t!("settings.audio").to_string(),
            SettingsTab::Audio,
            tab == SettingsTab::Audio,
            p,
        ))
        .child(category_item(
            "cat-about",
            &icons::INFO,
            t!("settings.about").to_string(),
            SettingsTab::About,
            tab == SettingsTab::About,
            p,
        ))
}

// ── Leaf controls ────────────────────────────────────────────────────────────

/// The key identifying the add-source menu (its open state + panel anchor).
const ADD_SOURCE_MENU: &str = "add-source";

/// `+ Add Source` — a compact primary action button that opens the [`add_source_menu`]
/// **modal dialog** (Local folder, Remote server as a "coming soon" item, and Add music
/// when a managed destination is set). A modal, not an anchored menu, so the choices
/// read as a proper dialog and stay centered + legible at any card width. The visible
/// text is a child, so the control carries an explicit a11y label. `ADD_SOURCE_MENU` is
/// still the open-state key (via `toggle_dropdown`); the overlay renders it as a modal.
fn add_source_trigger(p: &Palette) -> El {
    row()
        .key(ADD_SOURCE_MENU)
        .role("button")
        .label(t!("settings.addSource").to_string())
        .focusable(true)
        .center_y()
        .px(space::MD)
        .py(space::SM)
        .radius(radius::SM)
        .fill(p.primary)
        .cursor(Cursor::Pointer)
        .child(text(t!("settings.addSource").to_string()).color(p.bg))
        .on_press(move |s: &mut State| crate::toggle_dropdown(s, ADD_SOURCE_MENU))
}

/// An add-source option's label stacked over a one-line description, growing to fill
/// the row so any trailing chip sits at the right edge. The description makes each
/// choice self-explanatory (and, when disabled, says what to set up first).
fn source_option_text(label: String, desc: String, label_color: Color, p: &Palette) -> El {
    col()
        .grow(1.0)
        .gap(2.0)
        .child(text(label).color(label_color))
        .child(text(desc).font(type_::LABEL).color(p.text_secondary))
}

/// The add-source menu's floating panel — **mount at the window root** via
/// [`settings_overlays`], exactly like [`dropdown::panel`], so it escapes the
/// settings scroll clip. It mirrors `panel`'s styling (anchored under the trigger,
/// focus-trapped, card surface + border + radius, modal dismiss) but carries two
/// bespoke rows rather than a uniform choice list:
/// - **Local folder…** — focusable; closes the menu, then opens the native folder
///   dialog and folds the pick in (the flow the old button used).
/// - **Remote server…** — focusable; closes the menu, then opens the
///   [`remote_form_overlay`] connect form.
fn add_source_menu(s: &State, p: &Palette) -> El {
    let local = row()
        .key("add-source-opt-local")
        .role("option")
        .focusable(true)
        .w_full()
        .center_y()
        .gap(space::SM)
        .px(space::MD)
        .py(space::SM)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        // Same hover wash as `dropdown::panel`'s rows, so the menu reads identically.
        .hover(p.fg.alpha(0.06))
        .child(icon(&icons::FOLDER, 16.0).color(p.text_secondary))
        .child(source_option_text(
            t!("settings.addLocalFolder").to_string(),
            t!("settings.addLocalFolderDesc").to_string(),
            p.fg,
            p,
        ))
        .on_press_tx(|s: &mut State, tx: &mut Tx| {
            // Close the menu before the dialog so it isn't left open behind it.
            close_dropdown(s);
            tx.open_folder(
                FileDialogOptions {
                    title: t!("settings.addFolderDialog").to_string(),
                    ..Default::default()
                },
                |s: &mut State, result, _tx| {
                    if let Ok(Some(path)) = result {
                        scan_in_background(s, move |h| h.add_folder(&path), apply_and_save);
                    }
                },
            );
        });
    let remote = row()
        .key("add-source-opt-remote")
        .role("option")
        .focusable(true)
        .w_full()
        .center_y()
        .gap(space::SM)
        .px(space::MD)
        .py(space::SM)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        // Same hover wash as `dropdown::panel`'s rows, so the menu reads identically.
        .hover(p.fg.alpha(0.06))
        .child(icon(&icons::SERVER, 16.0).color(p.text_secondary))
        .child(source_option_text(
            t!("settings.addRemoteServer").to_string(),
            t!("settings.addRemoteServerDesc").to_string(),
            p.fg,
            p,
        ))
        .on_press(open_remote_form);
    // The dialog card: a title over the option rows. `modal` traps focus, makes the
    // background inert, and dismisses on Escape or an outside (scrim) press.
    let dialog = col()
        .key(format!("{ADD_SOURCE_MENU}-menu"))
        .focus_group()
        .focus_return(ADD_SOURCE_MENU.to_string())
        // Sized to fit the full (localized) rows — the labels no longer truncate, so a
        // generous max keeps even the longest translation + its trailing chip on one
        // line; the min keeps it dialog-like when the labels are short.
        .min_w(340.0)
        .max_w(540.0)
        .fill(p.card)
        .radius(radius::MD)
        .border_sides([1.0; 4], p.border)
        .padding(space::LG)
        .gap(space::XS)
        .min_h(space::NONE)
        .child(
            text(t!("settings.addSourceTitle").to_string())
                .font(type_::ITEM_STRONG)
                .color(p.fg),
        )
        .child(local)
        .child(remote)
        .child(add_music_row(s, p))
        .modal(true, close_dropdown);
    // A dimming scrim filling the window, centering the dialog.
    col()
        .key(format!("{ADD_SOURCE_MENU}-scrim"))
        .absolute(0.0, 0.0)
        .w_full()
        .h_full()
        .center()
        .fill(Color::BLACK.alpha(0.45))
        .child(dialog)
}

/// Min / max width of the remote-connect form card, echoing the add-source
/// dialog's own bounds (a small login-style card, not a wide one).
const REMOTE_FORM_MIN_W: f32 = 320.0;
const REMOTE_FORM_MAX_W: f32 = 440.0;

/// One labelled text field of the [`remote_form_overlay`]: a small caption
/// above a bordered single-line input bound to `buf` through `accessor`.
/// `key` becomes both the input's a11y key and (with a `-group` suffix) its
/// wrapping column's key; `label` is both the caption text and the input's
/// `.label(...)` (its accessible name — a `textbox` isn't named from its
/// content).
fn remote_field(
    key: &'static str,
    label: String,
    buf: &rune::TextEdit,
    accessor: fn(&mut State) -> &mut rune::TextEdit,
    p: &Palette,
) -> El {
    col()
        .key(format!("{key}-group"))
        .w_full()
        .gap(space::XXS)
        .child(
            text(label.clone())
                .font(type_::LABEL)
                .color(p.text_secondary),
        )
        .child(
            text_input(buf, type_::BODY)
                .key(key)
                .label(label)
                .single_line()
                .w_full()
                .px(space::SM)
                .py(space::XS)
                .radius(radius::SM)
                .fill(p.muted)
                .color(p.fg)
                .border_sides([1.0; 4], p.border)
                .on_edit(accessor)
                .on_submit(|s: &mut State, _tx| connect_remote_source(s)),
        )
}

/// The Connect button: like `organize_button` while idle, but while
/// `connecting` it switches to the "waiting for your browser" label and
/// drops `focusable`/`on_press`/`cursor` in favour of a dimmed, inert state —
/// the same enabled/disabled shape `player.rs`'s `transport_button` uses for
/// a control with nothing to invoke right now.
fn remote_connect_button(connecting: bool, p: &Palette) -> El {
    let label = if connecting {
        t!("settings.remoteConnecting").to_string()
    } else {
        t!("settings.remoteConnect").to_string()
    };
    let base = el()
        .key("remote-connect")
        .role("button")
        .label(label.clone())
        .focusable(!connecting)
        .padding(space::SM)
        .radius(radius::SM)
        .fill(if connecting { p.muted } else { p.primary })
        .child(text(label).color(if connecting { p.text_secondary } else { p.bg }));
    if connecting {
        base.opacity(fx::DISABLED)
    } else {
        base.cursor(Cursor::Pointer).on_press(connect_remote_source)
    }
}

/// The Remote server connect form: a small centered card over a dim scrim,
/// mounted on the root overlay layer (via [`settings_overlays`]) once
/// [`open_remote_form`] sets `State::remote_form_open`. A single labelled
/// field (server URL) over a Cancel / Connect button row — there is no
/// username/password field, since signing in happens in the system browser
/// (OAuth), not the app. Connect reads the URL and calls the host off-thread
/// (`connect_remote_source`); while that's in flight `remote_connecting`
/// swaps the Connect button for a disabled "waiting for your browser" state
/// and a status line replaces where an error would show. A failed attempt's
/// message shows inline above the buttons without closing the form. `modal`
/// traps focus, dims the background, and dismisses on Escape or an outside
/// (scrim) press — same as the add-source dialog.
pub fn remote_form_overlay(s: &State, p: &Palette) -> Option<El> {
    if !s.remote_form_open {
        return None;
    }
    let mut dialog = col()
        .key("remote-form")
        .focus_group()
        .focus_return(ADD_SOURCE_MENU.to_string())
        .min_w(REMOTE_FORM_MIN_W)
        .max_w(REMOTE_FORM_MAX_W)
        .fill(p.card)
        .radius(radius::MD)
        .border_sides([1.0; 4], p.border)
        .padding(space::LG)
        .gap(space::SM)
        .min_h(space::NONE)
        .child(
            text(t!("settings.remoteConnectTitle").to_string())
                .font(type_::ITEM_STRONG)
                .color(p.fg)
                .wrap(),
        )
        .child(
            text(t!("settings.remoteConnectHint").to_string())
                .font(type_::CAPTION)
                .color(p.text_secondary)
                .wrap(),
        )
        .child(remote_field(
            "remote-url",
            t!("settings.remoteUrlLabel").to_string(),
            &s.remote_url_buf,
            |s: &mut State| &mut s.remote_url_buf,
            p,
        ));
    if s.remote_connecting {
        dialog = dialog.child(
            text(t!("settings.remoteConnecting").to_string())
                .key("remote-connecting-status")
                .font(type_::CAPTION)
                .color(p.text_secondary)
                .wrap(),
        );
    } else if let Some(msg) = &s.remote_error {
        dialog = dialog.child(
            text(t!("settings.remoteConnectError", error => msg).to_string())
                .key("remote-error")
                .font(type_::CAPTION)
                .color(p.accent)
                .wrap(),
        );
    }
    let dialog = dialog
        .child(
            row()
                .gap(space::SM)
                .flex_wrap()
                .child(organize_button(
                    "remote-cancel",
                    t!("settings.cancel").to_string(),
                    false,
                    p,
                    close_remote_form,
                ))
                .child(remote_connect_button(s.remote_connecting, p)),
        )
        .modal(true, close_remote_form);
    Some(
        col()
            .key("remote-form-scrim")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .center()
            .fill(Color::BLACK.alpha(0.45))
            // The scrim's own dim-in keeps its fade (a backdrop easing in reads fine),
            // but `instant_changes` lands everything it HOLDS — the card and every
            // field/button inside it — at once. Without this, each nested level (card,
            // field group, button row, button) plays its OWN independent entrance fade
            // on top of its ancestors', so the opacities multiply: by the first real
            // frame the Connect button was measured at ~0.2% opacity (0.05 at the card,
            // squared again by the button row and the button itself) while the card's
            // own border/fill was already visible — reading as the card rendering
            // "cut", with its content missing, on the very first open. Landing the held
            // subtree at once makes the whole card (not just its outline) appear fully
            // formed the instant it mounts.
            .instant_changes()
            .child(dialog),
    )
}

/// The **Add music…** menu row: it runs the managed-library organizer (the
/// write-path copy/move) over a chosen set of files. It only works once a
/// managed destination is configured, so without one it renders as a disabled
/// hint (not focusable, dimmed, with a "Set a destination first" chip, like the
/// Remote "coming soon" row); with a destination it is a focusable option that
/// closes the menu and opens the organize modal (`organize_begin`).
fn add_music_row(s: &State, p: &Palette) -> El {
    let base = row()
        .key("add-source-opt-organize")
        .role("option")
        .w_full()
        .center_y()
        .gap(space::SM)
        .px(space::MD)
        .py(space::SM)
        .radius(radius::SM);
    if s.managed_dest.is_some() {
        base.focusable(true)
            .label(t!("settings.addMusic").to_string())
            .cursor(Cursor::Pointer)
            .hover(p.fg.alpha(0.06))
            .child(icon(&icons::MUSIC, 16.0).color(p.text_secondary))
            .child(source_option_text(
                t!("settings.addMusic").to_string(),
                t!("settings.addMusicDesc").to_string(),
                p.fg,
                p,
            ))
            .on_press_tx(|s: &mut State, tx: &mut Tx| {
                // Close the menu before the dialog so it isn't left open behind it.
                close_dropdown(s);
                tx.open_folder(
                    FileDialogOptions {
                        title: t!("settings.addMusicDialog").to_string(),
                        ..Default::default()
                    },
                    |s: &mut State, result, _tx| {
                        // Only proceed once a source is picked AND a destination +
                        // app handle are available (the handle lets the worker post
                        // the computed plan back onto the main thread).
                        if let Ok(Some(src_dir)) = result
                            && let (Some(handle), Some(dest)) =
                                (s.app_handle.clone(), s.managed_dest.clone())
                        {
                            // Show the scanning/planning modal immediately, then do
                            // the scan + plan off-thread so the UI never blocks.
                            organize_begin(s);
                            let template_id = s.managed_template.clone();
                            let strategy = s.managed_strategy;
                            std::thread::spawn(move || {
                                let files = scan_audio(&src_dir);
                                let plan = organize::plan(
                                    &files,
                                    &dest,
                                    &organize::Template::new(template_for(&template_id)),
                                    strategy,
                                );
                                handle.update(move |s: &mut State, _tx| {
                                    organize_ready(s, plan);
                                });
                            });
                        }
                    },
                );
            })
    } else {
        // No destination yet: dimmed + not focusable, its description explaining what to
        // set up first (clearer than a cryptic trailing chip).
        base.child(icon(&icons::MUSIC, 16.0).color(p.text_secondary))
            .child(source_option_text(
                t!("settings.addMusic").to_string(),
                t!("settings.setDestinationFirst").to_string(),
                p.text_secondary,
                p,
            ))
    }
}

/// Re-aggregate the current watched set — picks up files added/removed on disk
/// and roots that came back online. The set is unchanged, so no save (apply_watch,
/// not apply_and_save).
fn rescan_button(p: &Palette) -> El {
    el().key("rescan-sources")
        .role("button")
        .label(t!("settings.rescan").to_string())
        .focusable(true)
        .px(space::MD)
        .py(space::SM)
        .radius(radius::SM)
        .fill(p.muted)
        .center()
        .cursor(Cursor::Pointer)
        .child(text(t!("settings.rescan").to_string()).color(p.fg))
        .on_press(|s: &mut State| {
            // Reconcile the current configured set (set_folders preserves each
            // source's enabled flag), re-aggregating files/roots that changed on
            // disk. The set itself is unchanged, so no save (apply_watch).
            // ONLY local roots: `set_folders` manages the watched-FOLDER set, so a
            // remote source's synthetic path must be excluded — passing it here
            // would re-add the remote's URL as a bogus local folder (and leave the
            // real remote in place), duplicating the source.
            let roots: Vec<std::path::PathBuf> = s
                .sources
                .iter()
                .filter(|x| matches!(x.kind, SourceKind::Local { .. }))
                .map(|x| x.path.clone())
                .collect();
            // Off the UI thread: a reconcile of a big library must not freeze the
            // window, and running it in the background lets the title-bar activity
            // widget animate while it scans.
            scan_in_background(s, move |h| h.set_folders(&roots), apply_watch);
        })
}

/// Side of the per-source enable/disable toggle box — a small square checkbox
/// sized to seat the check glyph, consistent with the app's other inline icons.
const TOGGLE_PX: f32 = 20.0;
/// The check glyph inside the enable toggle.
const TOGGLE_ICON_PX: f32 = 14.0;

/// Side of the remote online/offline status dot — small, since a remote row
/// shows it ALONGSIDE the same enable/disable toggle a local row has (the
/// dot is reachability; the toggle is user intent — see `live_source_row`'s
/// leading slot). Diameter, not a radius token: a circular control uses
/// `size * 0.5` for its corner radius.
const HEALTH_DOT_PX: f32 = 10.0;

/// `url`'s host (and port, if non-default), for a remote row's title line — a
/// bare `https://` isn't informative, but the whole URL (shown in full below,
/// as the "path" line) is more than a title needs. Falls back to the whole
/// `url` if it carries no recognizable scheme.
pub(crate) fn display_host(url: &str) -> String {
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    without_scheme
        .split('/')
        .next()
        .unwrap_or(without_scheme)
        .to_string()
}

/// A small Local/Remote pill identifying a source row's [`SourceKind`] — the
/// "clear local/remote distinction" the Inc-2 program spec asks for. Styled
/// like the row's other chips (`enable`/`remove`): a filled, rounded box
/// around a short label. Remote reads in the accent color so it stands out
/// as the kind with telemetry attached.
fn source_kind_badge(index: usize, kind: &SourceKind, p: &Palette) -> El {
    let (label, color) = match kind {
        SourceKind::Local { .. } => (t!("settings.sourceLocal").to_string(), p.text_secondary),
        SourceKind::Remote { .. } => (t!("settings.sourceRemote").to_string(), p.accent),
    };
    el().key(format!("source-badge-{index}"))
        .px(space::XS)
        .py(space::XXS)
        .radius(radius::FULL)
        .fill(p.muted)
        .child(text(label).font(type_::LABEL).color(color))
}

/// A remote row's online/offline status dot — green while the last probe
/// succeeded, dim otherwise. Not rendered for a local source (locality has no
/// "online" concept); carries an accessible label since color alone isn't.
fn health_dot(index: usize, online: bool, p: &Palette) -> El {
    el().key(format!("source-online-{index}"))
        .role("status")
        .label(if online {
            t!("settings.remoteOnline").to_string()
        } else {
            t!("settings.offline").to_string()
        })
        .size(HEALTH_DOT_PX, HEALTH_DOT_PX)
        .radius(HEALTH_DOT_PX * 0.5)
        .fill(if online { p.online } else { p.text_secondary })
}

/// Round-trip latency as `"{ms}ms"`, or an em-dash placeholder when the
/// remote has never answered a probe (offline, or not yet checked) — mirrors
/// the band-table's own "—" convention for an unset numeric value. Plain text,
/// not localized: a unit abbreviation and a glyph, not prose.
pub(crate) fn latency_text(latency_ms: Option<u64>) -> String {
    match latency_ms {
        Some(ms) => format!("{ms}ms"),
        None => "—".to_string(),
    }
}

/// Seconds since the Unix epoch, for [`relative_time`]. Falls back to 0 on a
/// clock set before the epoch rather than panicking — this is display-only.
pub(crate) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `then` (a [`RemoteHealth::last_checked_unix`]) as a short, localized
/// relative time against `now` — "just now" / "%{count}m ago" / "%{count}h
/// ago" / "%{count}d ago". `None` (never probed) reads as the same em-dash
/// placeholder [`latency_text`] uses for "no value yet".
pub(crate) fn relative_time(now: u64, then: Option<u64>) -> String {
    let Some(then) = then else {
        return "—".to_string();
    };
    let secs = now.saturating_sub(then);
    if secs < 60 {
        t!("settings.justNow").to_string()
    } else if secs < 3600 {
        t!("settings.minutesAgo", count => secs / 60).to_string()
    } else if secs < 86_400 {
        t!("settings.hoursAgo", count => secs / 3600).to_string()
    } else {
        t!("settings.daysAgo", count => secs / 86_400).to_string()
    }
}

/// The compact data line under a remote source's identity — the telemetry the
/// Inc-2 program spec asks for: round-trip latency, the server's own crate
/// version, and a relative "last checked" time, read straight off
/// [`RemoteHealth`]. One row, wrapping (`flex_wrap`) so a narrow pane drops
/// items to a second line instead of overflowing, not a wall of text.
fn remote_health_line(index: usize, health: &RemoteHealth, p: &Palette) -> El {
    let version = health
        .server_version
        .clone()
        .unwrap_or_else(|| "—".to_string());
    row()
        .key(format!("source-health-{index}"))
        .gap(space::SM)
        .flex_wrap()
        .child(
            text(latency_text(health.latency_ms))
                .key(format!("source-latency-{index}"))
                .font(type_::LABEL.mono())
                .color(p.text_secondary),
        )
        .child(
            text(version)
                .key(format!("source-version-{index}"))
                .font(type_::LABEL.mono())
                .color(p.text_secondary),
        )
        .child(
            text(relative_time(now_unix(), health.last_checked_unix))
                .key(format!("source-last-checked-{index}"))
                .font(type_::CAPTION)
                .color(p.text_secondary),
        )
}

/// One row to render in the Sources card: a live source from `s.sources`
/// (local, or a currently-connected remote) — [`SourceRow::Live`] — or a
/// persisted remote URL with no live entry — [`SourceRow::Offline`] (Inc 6).
/// See [`merged_source_rows`].
enum SourceRow<'a> {
    Live(&'a SourceStatus),
    Offline(&'a str),
}

/// Normalizes a remote URL for equality comparisons only (never for
/// display or for the host, which does its own normalizing): trims a single
/// trailing `/`, the one variance a persisted URL and a live one can differ
/// by (the host normalizes the same way in
/// `add_remote_source_with_tokens`/`remove_remote_source`). Mirrors that
/// host-side `trim_end_matches('/')` so a persisted `http://h:8080` and a
/// live `http://h:8080/` are recognized as the same source here too.
fn normalize_remote_url(url: &str) -> &str {
    url.trim_end_matches('/')
}

/// `s.sources` (live: local roots + currently-connected remotes) followed by
/// every persisted remote URL (`s.remote_urls`, seeded from
/// `config::load_remotes()` by `reconnect_remotes_on_startup`) that has no
/// live entry — so a remote that failed to reconnect at launch, or is still
/// mid-reconnect, keeps showing up with an offline row and a Reconnect
/// action instead of silently vanishing from the Sources list (Inc 6). Live
/// rows keep the host's own order; offline rows are appended in the order
/// they were added, so the ordering never reshuffles as health changes.
///
/// Both comparisons are normalized ([`normalize_remote_url`]) and the
/// offline pass itself dedupes, so sources never duplicate even if
/// `s.remote_urls` and a live entry (or two `remote_urls` entries) differ
/// only by a trailing slash, or `remote_urls` somehow carries the same URL
/// twice: at most one row per distinct normalized URL, ever.
fn merged_source_rows(s: &State) -> Vec<SourceRow<'_>> {
    let mut rows: Vec<SourceRow> = s.sources.iter().map(SourceRow::Live).collect();
    let mut offline_seen: Vec<&str> = Vec::new();
    for url in &s.remote_urls {
        let normalized = normalize_remote_url(url);
        let connected = s.sources.iter().any(|st| {
            matches!(&st.kind, SourceKind::Remote { url: live } if normalize_remote_url(live) == normalized)
        });
        if connected {
            continue;
        }
        if offline_seen.contains(&normalized) {
            continue;
        }
        offline_seen.push(normalized);
        rows.push(SourceRow::Offline(url));
    }
    rows
}

/// One source row, dispatching to [`live_source_row`] or
/// [`offline_remote_row`] depending on which [`SourceRow`] variant
/// [`merged_source_rows`] produced for this position. `index` is this row's
/// position in the MERGED list, not necessarily `s.sources`' — used only to
/// key its controls uniquely within the card.
fn source_row(s: &State, p: &Palette, row: &SourceRow<'_>, index: usize) -> El {
    match row {
        SourceRow::Live(status) => live_source_row(s, p, status, index),
        SourceRow::Offline(url) => offline_remote_row(s, p, url, index),
    }
}

/// The enable/disable checkbox shared by a local root's and a remote
/// server's leading slot: a small square, filled and checked when `enabled`,
/// outline-only (muted fill, no check) when not — labelled for a11y, keyed
/// `enable-{index}` regardless of kind so a test (or a screen reader) finds
/// the same control shape on either row type. `on_toggle` carries the
/// kind-specific side effect ([`live_source_row`]'s two match arms); this
/// only renders the box and wires the press.
fn source_enable_toggle(
    index: usize,
    enabled: bool,
    p: &Palette,
    on_toggle: impl Fn(&mut State) + 'static,
) -> El {
    let mut toggle = el()
        .key(format!("enable-{index}"))
        .role("button")
        .label(if enabled {
            t!("settings.disableSource").to_string()
        } else {
            t!("settings.enableSource").to_string()
        })
        .focusable(true)
        .size(TOGGLE_PX, TOGGLE_PX)
        .radius(radius::XS)
        .center()
        .cursor(Cursor::Pointer)
        .fill(if enabled { p.primary } else { p.muted })
        .on_press(on_toggle);
    if enabled {
        toggle = toggle.child(icon(&icons::CHECK, TOGGLE_ICON_PX).color(p.bg));
    }
    toggle
}

/// A live source row (local, or a currently-connected remote): a Local/Remote
/// badge, a leading control (the enable/disable toggle for a local root; the
/// online/offline dot for a remote server), the source's name over its full
/// path/URL, a per-source track count, a remote's health/telemetry line (plus
/// an "update available" cue — see [`remote_update_available`] — when its
/// live index version has moved past the one last synced), an "offline"
/// badge if a LOCAL root is currently unreachable, and a trailing remove/
/// disconnect control that drops the source and re-aggregates.
fn live_source_row(s: &State, p: &Palette, status: &SourceStatus, index: usize) -> El {
    let enabled = status.enabled;
    let unavailable = !status.available;
    let is_remote = matches!(status.kind, SourceKind::Remote { .. });

    // A local source's title/subtitle is its basename over its full path (as
    // before); a remote has no filesystem path, so its title is the host and
    // the subtitle is the full base URL.
    let (name, full_path) = match &status.kind {
        SourceKind::Local { path } => (basename(path), path.to_string_lossy().into_owned()),
        SourceKind::Remote { url } => (display_host(url), url.clone()),
    };

    // Leading control: the enable/disable toggle, for EITHER kind now (a
    // remote keeps its health dot alongside — toggle is user intent on/off,
    // dot is live reachability, and both read as useful at once). A local
    // root's toggle flips `set_folder_enabled` + rescans; a remote's flips
    // `set_remote_enabled` (sync — no network, just excludes its already-
    // fetched blobs from the aggregate) and persists via config.
    let leading = match &status.kind {
        SourceKind::Local { path } => {
            let toggle_for = path.clone();
            source_enable_toggle(index, enabled, p, move |st: &mut State| {
                let toggle_for = toggle_for.clone();
                scan_in_background(
                    st,
                    move |h| h.set_folder_enabled(&toggle_for, !enabled),
                    apply_and_save,
                );
            })
        }
        // A remote's leading slot is JUST the toggle, same shape/position as a
        // local root — its online/offline dot lives on the health line below
        // (next to the latency/version it describes), not crammed beside the box.
        SourceKind::Remote { url } => {
            let url_for = url.clone();
            source_enable_toggle(index, enabled, p, move |st: &mut State| {
                toggle_remote_enabled(st, url_for.clone(), !enabled);
            })
        }
    };

    // Name + kind badge over full path/URL. A disabled source reads inactive,
    // so its name dims to the secondary colour. Both lines truncate and the
    // column grows + shrinks to zero so a long path/URL can't shove the
    // count / ✕ off the card.
    let name_color = if enabled { p.fg } else { p.text_secondary };
    let mut identity = col()
        .grow(1.0)
        .min_w(space::NONE)
        .child(
            row()
                .gap(space::XS)
                .center_y()
                .child(
                    text(name.clone())
                        .font(type_::ITEM)
                        .color(name_color)
                        .truncate(),
                )
                .child(source_kind_badge(index, &status.kind, p)),
        )
        .child(
            text(full_path)
                .font(type_::CAPTION)
                .color(p.text_secondary)
                .truncate(),
        );
    if let Some(health) = &status.remote {
        // The online/offline dot leads the telemetry line it describes.
        identity = identity.child(
            row()
                .gap(space::XS)
                .center_y()
                .child(health_dot(index, health.online, p))
                .child(remote_health_line(index, health, p)),
        );
        if let SourceKind::Remote { url } = &status.kind
            && health.online
            && remote_update_available(s, url, health)
        {
            let reconnecting = s.remote_reconnecting.iter().any(|u| u == url);
            identity = identity.child(
                row()
                    .key(format!("remote-update-{index}"))
                    .gap(space::SM)
                    .center_y()
                    .child(
                        text(t!("settings.updateAvailable").to_string())
                            .font(type_::CAPTION)
                            .color(p.accent),
                    )
                    .child(remote_reconnect_button(
                        index,
                        url,
                        t!("settings.syncNow").to_string(),
                        reconnecting,
                        p,
                    )),
            );
        }
    }

    let mut r = row()
        // Key by POSITION, not basename: two sources can share a final path
        // component (D:\Music and E:\Music), and duplicate sibling keys are a
        // Rune hazard. `index` is unique within the card.
        .key(format!("source-{index}"))
        .gap(space::MD)
        .center_y()
        .padding(space::SM)
        .child(leading)
        .child(identity)
        .child(
            // Keyed so tests can assert the count renders without depending on the
            // (localized) label text.
            el().key(format!("count-{index}")).child(
                text(t!("settings.trackCount", count => status.track_count).to_string())
                    .font(type_::CAPTION)
                    .color(p.text_secondary),
            ),
        );
    // The health line + dot already surface a remote's offline state; this
    // generic badge stays for an unreachable LOCAL root (an unplugged drive)
    // only, so a remote doesn't show the same signal twice.
    if unavailable && !is_remote {
        r = r.child(
            el().key(format!("offline-{index}"))
                .child(text(t!("settings.offline").to_string()).color(p.text_secondary)),
        );
    }
    match &status.kind {
        SourceKind::Local { path } => {
            let remove_for = path.clone();
            r.child(
                el().key(format!("remove-{index}"))
                    .role("button")
                    .label(t!("queue.remove").to_string())
                    .focusable(true)
                    .padding(space::SNUG)
                    .radius(radius::SM)
                    .fill(p.muted)
                    .cursor(Cursor::Pointer)
                    .child(text("✕").color(p.text_secondary))
                    .on_press(move |st: &mut State| {
                        let remove_for = remove_for.clone();
                        scan_in_background(
                            st,
                            move |h| h.remove_folder(&remove_for),
                            apply_and_save,
                        );
                    }),
            )
        }
        SourceKind::Remote { url } => r.child(remote_remove_button(index, url, p)),
    }
}

/// A persisted remote with no live entry — it failed to reconnect at
/// launch, or a manual reconnect attempt is still in flight (Inc 6): the
/// row that keeps a configured remote from silently disappearing. Shows the
/// same Remote badge and offline dot a reachable-but-currently-down remote
/// would (no latency — there has been no successful probe yet to read one
/// from), the full URL, an "offline"/"reconnecting…" status line, and
/// Reconnect + Disconnect actions. `index` is this row's position in
/// [`merged_source_rows`]' merged list (not `s.sources`'), used only to key
/// its controls uniquely.
fn offline_remote_row(s: &State, p: &Palette, url: &str, index: usize) -> El {
    let reconnecting = s.remote_reconnecting.iter().any(|u| u == url);
    let status_text = if reconnecting {
        t!("settings.reconnecting").to_string()
    } else {
        t!("settings.offline").to_string()
    };
    let kind = SourceKind::Remote {
        url: url.to_string(),
    };
    let identity = col()
        .grow(1.0)
        .min_w(space::NONE)
        .child(
            row()
                .gap(space::XS)
                .center_y()
                .child(
                    text(display_host(url))
                        .font(type_::ITEM)
                        .color(p.text_secondary)
                        .truncate(),
                )
                .child(source_kind_badge(index, &kind, p)),
        )
        .child(
            text(url.to_string())
                .font(type_::CAPTION)
                .color(p.text_secondary)
                .truncate(),
        )
        .child(
            text(status_text)
                .key(format!("remote-status-{index}"))
                .font(type_::CAPTION)
                .color(p.text_secondary),
        );

    row()
        .key(format!("source-offline-{index}"))
        .gap(space::MD)
        .center_y()
        .padding(space::SM)
        .child(health_dot(index, false, p))
        .child(identity)
        .child(remote_reconnect_button(
            index,
            url,
            t!("settings.reconnect").to_string(),
            reconnecting,
            p,
        ))
        .child(remote_remove_button(index, url, p))
}

/// Whether a connected remote's row should show the "update available" cue:
/// its live health just reported a higher `index_version` than the one
/// recorded the last time this remote's catalog was actually synced
/// (`s.remote_synced_index`, written by `record_synced_index` after a
/// connect/reconnect — never by the health poller, which only pings
/// `/v0/ping` and never re-syncs, so it must never feed this). No recorded
/// sync yet reads as "no update" rather than a false positive.
fn remote_update_available(s: &State, url: &str, health: &RemoteHealth) -> bool {
    let Some(live) = health.index_version else {
        return false;
    };
    s.remote_synced_index
        .get(url)
        .is_some_and(|&synced| live > synced)
}

/// A remote row's reconnect/sync action — fires `reconnect_remote_now`, the
/// shared silent-refresh + re-sync path an offline row's Reconnect button
/// and a connected row's "update available" Sync-now button both drive
/// (only `label` differs). While `reconnecting`, it switches to the
/// disabled/inert shape `remote_connect_button` uses, so a double press (or
/// the periodic poller bringing the remote back mid-attempt) can't start a
/// second reconnect for the same url.
fn remote_reconnect_button(
    index: usize,
    url: &str,
    label: String,
    reconnecting: bool,
    p: &Palette,
) -> El {
    let base = el()
        .key(format!("remote-reconnect-{index}"))
        .role("button")
        .label(label.clone())
        .focusable(!reconnecting)
        .padding(space::SNUG)
        .radius(radius::SM)
        .fill(p.muted)
        .child(text(label).font(type_::LABEL).color(if reconnecting {
            p.text_secondary
        } else {
            p.accent
        }));
    if reconnecting {
        base.opacity(fx::DISABLED)
    } else {
        let url = url.to_string();
        base.cursor(Cursor::Pointer)
            .on_press(move |st: &mut State| reconnect_remote_now(st, url.clone()))
    }
}

/// A remote row's Disconnect (remove) control — the remote counterpart of a
/// local row's ✕: same glyph, same keyed/focusable/labelled shape and
/// placement, but it fires `disconnect_remote_source` (which also drops the
/// config entry and the keychain refresh token — a local root has neither)
/// instead of `remove_folder`. Shared by a connected row and an offline row
/// alike: giving up on a remote should work regardless of its current
/// health.
fn remote_remove_button(index: usize, url: &str, p: &Palette) -> El {
    let url = url.to_string();
    el().key(format!("remote-remove-{index}"))
        .role("button")
        .label(t!("settings.disconnectSource").to_string())
        .focusable(true)
        .padding(space::SNUG)
        .radius(radius::SM)
        .fill(p.muted)
        .cursor(Cursor::Pointer)
        .child(text("✕").color(p.text_secondary))
        .on_press(move |st: &mut State| disconnect_remote_source(st, url.clone()))
}

/// One theme choice; the active preference is filled with the primary color.
fn theme_button(label: String, key: &str, pref: ThemePref, active: bool, p: &Palette) -> El {
    el().key(key.to_string())
        .role("button")
        .label(label.clone())
        .focusable(true)
        .padding(space::SM)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .fill(if active { p.primary } else { p.muted })
        .child(text(label).color(if active { p.bg } else { p.fg }))
        .on_press(move |s: &mut State| set_theme_pref(s, pref))
}

/// One album-year source choice; the active source is filled with the primary
/// color (mirrors `theme_button`).
fn album_year_button(
    label: String,
    key: &str,
    src: AlbumYearSource,
    active: bool,
    p: &Palette,
) -> El {
    el().key(key.to_string())
        .role("button")
        .label(label.clone())
        .focusable(true)
        .padding(space::SM)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .fill(if active { p.primary } else { p.muted })
        .child(text(label).color(if active { p.bg } else { p.fg }))
        .on_press(move |s: &mut State| set_album_year(s, src))
}

/// The key identifying the language dropdown (its open state + panel anchor).
const LANG_PICKER: &str = "lang-picker";

/// The language choices as `(pref id, display title)`: "System" follows the OS
/// locale (its label localized), each shipped locale in its own native name. The
/// single source for both the dropdown's current-value label and its panel — keep
/// the ids in step with `crate::LOCALES`.
fn language_options() -> Vec<(String, String)> {
    vec![
        ("system".into(), t!("settings.langSystem").to_string()),
        ("en".into(), "English".into()),
        ("de".into(), "Deutsch".into()),
        ("es".into(), "Español".into()),
        ("zh".into(), "中文".into()),
        ("cs".into(), "Čeština".into()),
        ("ja".into(), "日本語".into()),
    ]
}

/// The current language's display label (falls back to System).
fn current_language_label(s: &State) -> String {
    language_options()
        .into_iter()
        .find(|(id, _)| id == &s.locale_pref)
        .map(|(_, title)| title)
        .unwrap_or_else(|| t!("settings.langSystem").to_string())
}

/// The language picker: the shared themed `dropdown` trigger (the original's
/// `Select`). Its floating panel is mounted at the root by [`settings_overlays`].
fn language_dropdown(s: &State, p: &Palette) -> El {
    dropdown::trigger(LANG_PICKER, current_language_label(s), p)
}

/// One output-device choice; the active device is filled with the primary color.
/// `choice` is the device this button selects (`None` = follow the system
/// default). Full-width so a long device name has room (the whole row is the hit
/// target), mirroring `theme_button`'s selected-fill treatment.
fn device_button(
    label: String,
    key: &str,
    choice: Option<String>,
    active: bool,
    p: &Palette,
) -> El {
    el().key(key.to_string())
        .role("button")
        .label(label.clone())
        .focusable(true)
        .padding(space::SM)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .fill(if active { p.primary } else { p.muted })
        .child(
            text(label)
                .color(if active { p.bg } else { p.fg })
                .grow(1.0),
        )
        .on_press(move |s: &mut State| set_output_device(s, choice.clone()))
}

/// One ReplayGain mode choice; the active mode is filled with the primary color
/// (mirrors `theme_button` / `device_button`).
fn rg_mode_button(label: String, key: &str, mode: ReplayGainMode, active: bool, p: &Palette) -> El {
    el().key(key.to_string())
        .role("button")
        .label(label.clone())
        .focusable(true)
        .padding(space::SM)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .fill(if active { p.primary } else { p.muted })
        .child(text(label).color(if active { p.bg } else { p.fg }))
        .on_press(move |s: &mut State| set_replaygain_mode(s, mode))
}

/// One indexing-speed choice; the active mode is filled with the primary color
/// (mirrors [`rg_mode_button`]). Selecting it applies + persists via
/// [`set_indexing_mode`].
fn indexing_mode_button(
    label: String,
    key: &str,
    mode: IndexingMode,
    active: bool,
    p: &Palette,
) -> El {
    el().key(key.to_string())
        .role("button")
        .label(label.clone())
        .focusable(true)
        .padding(space::SM)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .fill(if active { p.primary } else { p.muted })
        .child(text(label).color(if active { p.bg } else { p.fg }))
        .on_press(move |s: &mut State| set_indexing_mode(s, mode))
}

/// The "Indexing speed" control for the Sources page: a heading whose description
/// is the *selected* mode's plain-language explanation, plus the three mode
/// buttons. Built idiot-proof — every option is visible and the help text tells
/// you exactly what the current choice does.
fn indexing_section(s: &State, p: &Palette) -> El {
    let mode = s.indexing_mode;
    let desc = match mode {
        IndexingMode::Automatic => t!("settings.indexingAutoDesc"),
        IndexingMode::Maximum => t!("settings.indexingMaxDesc"),
        IndexingMode::Gentle => t!("settings.indexingGentleDesc"),
    };
    col()
        .gap(space::SM)
        .child(heading(
            t!("settings.indexing").to_string(),
            desc.to_string(),
            p,
        ))
        .child(
            row()
                .gap(space::XS)
                .flex_wrap()
                .child(indexing_mode_button(
                    t!("settings.indexingModeAuto").to_string(),
                    "indexing-auto",
                    IndexingMode::Automatic,
                    mode == IndexingMode::Automatic,
                    p,
                ))
                .child(indexing_mode_button(
                    t!("settings.indexingModeMax").to_string(),
                    "indexing-max",
                    IndexingMode::Maximum,
                    mode == IndexingMode::Maximum,
                    p,
                ))
                .child(indexing_mode_button(
                    t!("settings.indexingModeGentle").to_string(),
                    "indexing-gentle",
                    IndexingMode::Gentle,
                    mode == IndexingMode::Gentle,
                    p,
                )),
        )
}

// ── Cards (one per settings group) ───────────────────────────────────────────

/// Appearance: a System / Light / Dark theme override.
/// A show/hide toggle for one primary nav item: filled primary when shown,
/// muted when hidden. Clicking flips it (persisted). The now-showing route can
/// be hidden too — the Settings gear stays in the footer, so the nav can never
/// strand the user.
fn nav_toggle(id: &'static str, visible: bool, p: &Palette) -> El {
    el().key(format!("nav-toggle-{id}"))
        .role("button")
        .label(nav_label(id))
        .focusable(true)
        .padding(space::SM)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .fill(if visible { p.primary } else { p.muted })
        .child(
            text(if visible {
                t!("settings.navShown").to_string()
            } else {
                t!("settings.navHidden").to_string()
            })
            .color(if visible { p.bg } else { p.fg }),
        )
        .on_press(move |s: &mut State| set_nav_visible(s, id, !s.nav_visible(id)))
}

/// Navigation: one show/hide toggle per primary sidebar item, so the user
/// decides which pages appear (the original's configurable nav).
fn navigation_card(s: &State, p: &Palette) -> El {
    let mut c = card("navigation", p).child(heading(
        t!("settings.navigation").to_string(),
        t!("settings.navigationDesc").to_string(),
        p,
    ));
    for def in NAV_DEFS {
        c = c.child(
            row()
                .key(format!("nav-row-{}", def.id))
                .center_y()
                .child(
                    text(nav_label(def.id))
                        .font(type_::ITEM)
                        .color(p.fg)
                        .grow(1.0),
                )
                .child(nav_toggle(def.id, s.nav_visible(def.id), p)),
        );
    }
    c
}

fn appearance_card(s: &State, p: &Palette) -> El {
    card("appearance", p)
        .child(heading(
            t!("settings.appearance").to_string(),
            t!("settings.appearanceDesc").to_string(),
            p,
        ))
        .child(
            row()
                .gap(space::SM)
                .flex_wrap()
                .child(theme_button(
                    t!("settings.themeSystem").to_string(),
                    "theme-system",
                    ThemePref::System,
                    s.theme_pref == ThemePref::System,
                    p,
                ))
                .child(theme_button(
                    t!("settings.themeLight").to_string(),
                    "theme-light",
                    ThemePref::Light,
                    s.theme_pref == ThemePref::Light,
                    p,
                ))
                .child(theme_button(
                    t!("settings.themeDark").to_string(),
                    "theme-dark",
                    ThemePref::Dark,
                    s.theme_pref == ThemePref::Dark,
                    p,
                )),
        )
        .child(
            row()
                .key("album-year-row")
                .center_y()
                .gap(space::SM)
                .flex_wrap()
                .child(
                    text(t!("settings.albumYear").to_string())
                        .font(type_::ITEM)
                        .color(p.fg)
                        .grow(1.0),
                )
                .child(
                    row()
                        .gap(space::XS)
                        .child(album_year_button(
                            t!("settings.albumYearOriginal").to_string(),
                            "album-year-original",
                            AlbumYearSource::Original,
                            s.album_year == AlbumYearSource::Original,
                            p,
                        ))
                        .child(album_year_button(
                            t!("settings.albumYearRelease").to_string(),
                            "album-year-release",
                            AlbumYearSource::Release,
                            s.album_year == AlbumYearSource::Release,
                            p,
                        )),
                ),
        )
}

/// Language: a dropdown of System (follow the OS) plus each shipped locale in its
/// native name.
fn language_card(s: &State, p: &Palette) -> El {
    card("language", p)
        .child(heading(
            t!("settings.language").to_string(),
            t!("settings.languageDesc").to_string(),
            p,
        ))
        .child(language_dropdown(s, p))
}

/// Music Data: the watched-folder set (+ Add Source / Rescan, one row per
/// folder). Named like the original soul-player so the heading doesn't clash
/// with the sidebar's "Tracks" library view.
fn music_data_card(s: &State, p: &Palette) -> El {
    let mut c = card("sources", p).child(
        row()
            .w_full()
            .gap(space::MD)
            // Keep the heading readable (a min width) and grow it to fill; group the
            // action buttons so, when the card is too narrow for all three on one line,
            // `flex_wrap` drops the WHOLE button group to its own line below the heading
            // instead of crushing the heading to a sliver and ballooning the buttons.
            // `items_start` keeps the buttons at their natural height (no cross-stretch).
            .items_start()
            .flex_wrap()
            .child(
                heading(
                    t!("settings.musicData").to_string(),
                    t!("settings.musicDataDesc").to_string(),
                    p,
                )
                .grow(1.0)
                .min_w(180.0),
            )
            .child(
                row()
                    .gap(space::SM)
                    .items_center()
                    .child(rescan_button(p))
                    .child(add_source_trigger(p)),
            ),
    );
    // A deletion warning from the most recent scan (host `Event::TracksRemoved`),
    // cleared on the next Sources refresh. `wrap` so a long folder name or locale
    // can't push it off the card.
    if let Some((root, count)) = &s.removal_notice {
        let folder = root
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or_default()
            .to_string();
        c = c.child(
            text(t!("settings.tracksRemoved", count => count, folder => folder).to_string())
                .key("removal-notice")
                .color(p.accent)
                .wrap(),
        );
    }
    let rows = merged_source_rows(s);
    if rows.is_empty() {
        c = c.child(text(t!("settings.noSources").to_string()).color(p.text_secondary));
    } else {
        for (i, row_data) in rows.iter().enumerate() {
            c = c.child(source_row(s, p, row_data, i));
        }
    }
    // How hard the scanner may work when indexing these sources.
    c.child(indexing_section(s, p))
}

// ── Managed Library (the write-path organizer: configure half) ───────────────

/// The key identifying the managed-library template picker (its open state +
/// panel anchor).
const MANAGED_TEMPLATE: &str = "managed-template";

/// The human label for a template preset id (ids come from `organize::presets()`).
/// Shown in the picker trigger + its panel.
fn preset_label(id: &str) -> String {
    match id {
        "artist-year-album" => t!("settings.preset.artist-year-album").to_string(),
        "genre-artist-album" => t!("settings.preset.genre-artist-album").to_string(),
        "flat" => t!("settings.preset.flat").to_string(),
        // "artist-album" is the default preset.
        _ => t!("settings.preset.artist-album").to_string(),
    }
}

/// A static illustrative destination path for a preset, so the user previews the
/// folder layout without any real file. Keyed to `organize::presets()`'s ids.
fn preset_example(id: &str) -> &'static str {
    match id {
        "artist-year-album" => "Daft Punk/2001 - Discovery/01 - One More Time.flac",
        "genre-artist-album" => "Electronic/Daft Punk/Discovery/01 - One More Time.flac",
        "flat" => "Daft Punk - One More Time.flac",
        _ => "Daft Punk/Discovery/01 - One More Time.flac",
    }
}

/// The template presets as `(id, human label)` for the dropdown panel — the ids
/// come from the organizer so the picker always matches the write path.
fn preset_options() -> Vec<(String, String)> {
    organize::presets()
        .iter()
        .map(|(id, _)| (id.to_string(), preset_label(id)))
        .collect()
}

/// The destination Browse button: opens the native folder dialog and persists the
/// pick. Folder dialog (not text entry) because Rune's IME is pre-release — same
/// as Add Source.
fn managed_browse_button(p: &Palette) -> El {
    el().key("managed-dest-browse")
        .role("button")
        .label(t!("settings.browse").to_string())
        .focusable(true)
        .padding(space::MD)
        .radius(radius::MD)
        .fill(p.muted)
        .center()
        .cursor(Cursor::Pointer)
        .child(text(t!("settings.browse").to_string()).color(p.fg))
        .on_press_tx(|_s: &mut State, tx: &mut Tx| {
            tx.open_folder(
                FileDialogOptions {
                    title: t!("settings.managedDestDialog").to_string(),
                    ..Default::default()
                },
                |s: &mut State, result, _tx| {
                    if let Ok(Some(path)) = result {
                        set_managed_dest(s, Some(path));
                    }
                },
            );
        })
}

/// The destination row: a label over the current path (or "Not set"), with the
/// Browse button trailing. The path column grows + shrinks to zero so a long path
/// truncates instead of shoving the button off the card.
fn managed_dest_row(s: &State, p: &Palette) -> El {
    let path_line = match &s.managed_dest {
        Some(dest) => text(dest.to_string_lossy().into_owned())
            .font(type_::BODY)
            .color(p.fg)
            .truncate(),
        None => text(t!("settings.notSet").to_string())
            .font(type_::BODY)
            .color(p.text_secondary),
    };
    row()
        .w_full()
        .gap(space::MD)
        .center_y()
        .child(
            col()
                .grow(1.0)
                .min_w(space::NONE)
                .child(
                    text(t!("settings.destination").to_string())
                        .font(type_::CAPTION)
                        .color(p.text_secondary),
                )
                .child(path_line),
        )
        .child(managed_browse_button(p))
}

/// The template section: a label, the preset picker (the shared `dropdown`), and
/// an example destination line for the selected preset (keyed `managed-example`).
fn managed_template_section(s: &State, p: &Palette) -> El {
    col()
        .w_full()
        .gap(space::XS)
        .child(
            text(t!("settings.template").to_string())
                .font(type_::CAPTION)
                .color(p.text_secondary),
        )
        .child(dropdown::trigger(
            MANAGED_TEMPLATE,
            preset_label(&s.managed_template),
            p,
        ))
        .child(
            // A static preview of where a file would land (no real file). `wrap`
            // keeps a long path inside the card instead of overflowing.
            col()
                .key("managed-example")
                .w_full()
                .gap(space::XXS)
                .child(
                    text(t!("settings.example").to_string())
                        .font(type_::CAPTION)
                        .color(p.text_secondary)
                        .wrap(),
                )
                .child(
                    text(preset_example(&s.managed_template).to_string())
                        .font(type_::BODY)
                        .color(p.fg)
                        .wrap(),
                ),
        )
}

/// One strategy choice (Copy / Move); the active one is filled with the primary
/// colour (mirrors `theme_button`).
fn strategy_button(
    label: String,
    key: &str,
    strategy: organize::Strategy,
    active: bool,
    p: &Palette,
) -> El {
    el().key(key.to_string())
        .role("button")
        .label(label.clone())
        .focusable(true)
        .padding(space::SM)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .fill(if active { p.primary } else { p.muted })
        .child(text(label).color(if active { p.bg } else { p.fg }))
        .on_press(move |s: &mut State| set_managed_strategy(s, strategy))
}

/// The strategy section: a label over the Copy / Move toggle (wraps on a narrow
/// card).
fn managed_strategy_section(s: &State, p: &Palette) -> El {
    col()
        .w_full()
        .gap(space::XS)
        .child(
            text(t!("settings.strategy").to_string())
                .font(type_::CAPTION)
                .color(p.text_secondary),
        )
        .child(
            row()
                .gap(space::SM)
                .flex_wrap()
                .child(strategy_button(
                    t!("settings.copyFiles").to_string(),
                    "managed-copy",
                    organize::Strategy::Copy,
                    s.managed_strategy == organize::Strategy::Copy,
                    p,
                ))
                .child(strategy_button(
                    t!("settings.moveFiles").to_string(),
                    "managed-move",
                    organize::Strategy::Move,
                    s.managed_strategy == organize::Strategy::Move,
                    p,
                )),
        )
}

/// Managed Library: the write-path destination + template preset + Copy/Move
/// strategy, persisted to config. Configuring only — the "Add music…" apply flow
/// that runs the copy/move is a separate task. Rendered under the Sources card on
/// the Music Data tab.
fn managed_library_card(s: &State, p: &Palette) -> El {
    card("managed-library", p)
        .child(heading(
            t!("settings.managedLibrary").to_string(),
            t!("settings.managedLibraryDesc").to_string(),
            p,
        ))
        .child(managed_dest_row(s, p))
        .child(managed_template_section(s, p))
        .child(managed_strategy_section(s, p))
}

/// Output: a "System default" choice plus one button per enumerated output
/// device, stacked vertically (device names can be long, and a host may expose
/// many). The active choice (matching `State.output_device`, `None` = system
/// default) is filled. Selecting a device applies to the NEXT playback session.
fn output_card(s: &State, p: &Palette) -> El {
    let mut choices = col()
        .key("output-devices")
        .gap(space::SM)
        .child(device_button(
            t!("settings.systemDefault").to_string(),
            "output-default",
            None,
            s.output_device.is_none(),
            p,
        ));
    if s.output_devices.is_empty() {
        choices =
            choices.child(text(t!("settings.noOutputDevices").to_string()).color(p.text_secondary));
    } else {
        for (i, name) in s.output_devices.iter().enumerate() {
            let active = s.output_device.as_deref() == Some(name.as_str());
            choices = choices.child(device_button(
                name.clone(),
                &format!("output-{i}"),
                Some(name.clone()),
                active,
                p,
            ));
        }
    }
    card("sp-stage-output", p)
        .child(stage_header(
            p.primary,
            &icons::VOLUME,
            t!("settings.output").to_string(),
            Some(t!("settings.outputDesc").to_string()),
            None,
            false,
            p,
        ))
        .child(choices)
}

/// Exclusive (bit-perfect) output: a toggle plus the live truth. When on and the
/// current stream is exclusive it's bit-perfect (primary dot + the device format);
/// when on but the device fell back to shared, that's shown honestly (accent dot);
/// off is plain shared output. Applies to the next playback session, like the
/// device choice.
/// What the Exclusive card should SAY about the current output, decided purely from
/// the toggle + live signal path + the host's resolved status — so the mapping (the
/// reason surfacing, the strict-stop case) is unit-tested without a live device. The
/// card turns this into a quality dot + i18n text.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ExclusiveDisplay {
    /// Not requested — shared by choice.
    Shared,
    /// Exclusive is active; the device format string (from the signal path).
    Active(String),
    /// Requested but fell back to shared; the device's reason.
    FellBack(String),
    /// Requested with strict mode and couldn't open — stopped; the reason.
    Stopped(String),
    /// Requested but nothing has opened a device yet (applies next session).
    Pending,
}

fn exclusive_display(
    exclusive: bool,
    signal_path: Option<soul_player_host::SignalPath>,
    status: soul_player_host::ExclusiveStatus,
) -> ExclusiveDisplay {
    use crate::views::signal_path::exclusive_value;
    use soul_player_host::ExclusiveStatus;
    if !exclusive {
        return ExclusiveDisplay::Shared;
    }
    // The live signal path is authoritative for "exclusive right now" + its format.
    if let Some(sp) = signal_path.filter(|x| x.exclusive) {
        return ExclusiveDisplay::Active(exclusive_value(&sp));
    }
    // Requested but not (yet) active — the host status carries the real reason.
    match status {
        ExclusiveStatus::Unavailable(reason) => ExclusiveDisplay::Stopped(reason),
        ExclusiveStatus::FellBack(reason) => ExclusiveDisplay::FellBack(reason),
        _ => ExclusiveDisplay::Pending,
    }
}

/// How prominently the exclusive status reads. `Active` = bit-perfect right now (the
/// one "it's working" state); `Neutral` = plain shared output; `Attention` = requested
/// but not bit-perfect (pending / fell back / stopped). The card colours the status
/// box by this, so "applied" is unmistakable from "idle" and "not applied yet".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusKind {
    Active,
    Neutral,
    Attention,
}

/// The one-line status text + its severity for an [`ExclusiveDisplay`]. Pure, so the
/// applied-vs-idle-vs-pending mapping is unit-tested without a live device.
fn exclusive_status(display: &ExclusiveDisplay) -> (String, StatusKind) {
    use crate::views::signal_path::output_api;
    match display {
        // Name the backend even in shared mode, so the output API is always visible.
        ExclusiveDisplay::Shared => (
            format!(
                "{} · {}",
                output_api(),
                t!("settings.exclusiveStatusShared")
            ),
            StatusKind::Neutral,
        ),
        ExclusiveDisplay::Active(fmt) => (
            format!("{} · {fmt}", t!("settings.exclusiveBitPerfect")),
            StatusKind::Active,
        ),
        ExclusiveDisplay::FellBack(reason) => (
            format!("{}: {reason}", t!("settings.exclusiveFellBackShort")),
            StatusKind::Attention,
        ),
        ExclusiveDisplay::Stopped(reason) => (
            format!("{}: {reason}", t!("settings.exclusiveStopped")),
            StatusKind::Attention,
        ),
        ExclusiveDisplay::Pending => (
            t!("settings.exclusivePending").to_string(),
            StatusKind::Attention,
        ),
    }
}

fn exclusive_card(s: &State, p: &Palette) -> El {
    // A dot + one-line status describing what is ACTUALLY happening. When exclusive
    // was requested but isn't active, the host status carries the real reason (device
    // in use, no exclusive format, …) — far more useful than a generic message.
    let (status, kind) = exclusive_status(&exclusive_display(
        s.exclusive,
        s.signal_path,
        s.host.exclusive_status(),
    ));
    // Colour the dot + the status box by severity so the current state reads at a
    // glance: Active is a filled-tint "working" box, Neutral is a plain shared box,
    // Attention (pending / fell back / stopped) is an accent box that draws the eye.
    let (dot, box_fill, box_fg) = match kind {
        StatusKind::Active => (p.primary, p.primary.alpha(0.16), p.primary),
        StatusKind::Neutral => (p.primary, p.muted, p.text_secondary),
        StatusKind::Attention => (p.accent, p.accent.alpha(0.16), p.accent),
    };
    let indent = 8.0 + space::SM + 18.0 + space::SM;
    let mut el = card("sp-stage-exclusive", p)
        .child(stage_header(
            dot,
            &icons::SETTINGS,
            t!("player.signalPath.exclusive").to_string(),
            Some(t!("settings.exclusiveDesc").to_string()),
            Some(exclusive_toggle(s.exclusive, p)),
            false,
            p,
        ))
        .child(
            // Indent the status box under the dot+icon so it lines up with the title.
            row().w_full().pl(indent).child(
                row()
                    .key("sp-stage-exclusive-status")
                    .grow(1.0)
                    .padding(space::SM)
                    .radius(radius::SM)
                    .fill(box_fill)
                    .child(
                        text(status)
                            .font(type_::BODY)
                            .color(box_fg)
                            .grow(1.0)
                            .wrap(),
                    ),
            ),
        );
    // Strict (exclusive-or-stop) is a sub-option — only shown when exclusive is on.
    if s.exclusive {
        el = el.child(
            row()
                .w_full()
                .pl(indent)
                .center_y()
                .gap(space::SM)
                .child(
                    text(t!("settings.exclusiveStrict").to_string())
                        .font(type_::CAPTION)
                        .color(p.text_secondary)
                        .grow(1.0)
                        .wrap(),
                )
                .child(strict_toggle(s.exclusive_strict, p)),
        );
    }
    el
}

/// The Exclusive on/off switch (a filled pill), persisted via [`set_exclusive`].
/// Filled `primary` when on, muted when off — matching the nav show/hide toggles.
fn exclusive_toggle(on: bool, p: &Palette) -> El {
    el().key("exclusive-toggle")
        .role("button")
        .label(t!("player.signalPath.exclusive").to_string())
        .focusable(true)
        .padding(space::SM)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .fill(if on { p.primary } else { p.muted })
        .child(
            text(if on {
                t!("settings.exclusiveOn").to_string()
            } else {
                t!("settings.exclusiveOff").to_string()
            })
            .color(if on { p.bg } else { p.fg }),
        )
        .on_press(move |s: &mut State| set_exclusive(s, !s.exclusive))
}

/// The strict (exclusive-or-stop) on/off switch — a smaller pill, persisted via
/// [`set_exclusive_strict`]. Filled `accent` when on (it's a stop-rather-than-fallback
/// stance, not a clean-pass), muted when off.
fn strict_toggle(on: bool, p: &Palette) -> El {
    el().key("exclusive-strict-toggle")
        .role("button")
        .label(t!("settings.exclusiveStrict").to_string())
        .focusable(true)
        .padding(space::XS)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .fill(if on { p.accent } else { p.muted })
        .child(
            text(if on {
                t!("settings.exclusiveOn").to_string()
            } else {
                t!("settings.exclusiveOff").to_string()
            })
            .font(type_::LABEL)
            .color(if on { p.bg } else { p.fg }),
        )
        .on_press(move |s: &mut State| set_exclusive_strict(s, !s.exclusive_strict))
}

// ── Signal-path pipeline (Settings → Audio is laid out AS the pipeline) ───────
//
// The whole Audio page is the signal path: one card per stage — Source →
// ReplayGain → Resample → DSP/EQ → Output → Exclusive — connected by flow arrows,
// each with a Roon-style quality dot (primary = clean/lossless pass, accent = a
// stage that processes the signal). foobar2000's chain order: Decoder →
// ReplayGain → DSP (resample/EQ) → Output.

/// Stage-card header: quality dot + stage icon + title (+ optional description and
/// trailing control). `dim` greys a not-yet-built stage. Shared by every stage
/// card so the pipeline reads consistently.
fn stage_header(
    dot: Color,
    glyph: &'static Icon,
    title: String,
    desc: Option<String>,
    trailing: Option<El>,
    dim: bool,
    p: &Palette,
) -> El {
    let dot_c = if dim { p.border } else { dot };
    let icon_c = if dim { p.text_secondary } else { dot };
    let title_c = if dim { p.text_secondary } else { p.fg };
    let mut titles = col()
        .grow(1.0)
        .min_w(0.0)
        .gap(space::XXS)
        .child(text(title).font(type_::TITLE).color(title_c).wrap());
    if let Some(d) = desc {
        titles = titles.child(text(d).font(type_::CAPTION).color(p.text_secondary).wrap());
    }
    let mut h = row()
        .w_full()
        .center_y()
        .gap(space::SM)
        .child(el().w(8.0).h(8.0).radius(4.0).fill(dot_c))
        .child(icon(glyph, 18.0).color(icon_c))
        .child(titles);
    if let Some(t) = trailing {
        h = h.child(t);
    }
    h
}

/// A flow arrow between two stage cards — a centered down-chevron showing the
/// signal moving from one card into the next.
fn pipeline_arrow(p: &Palette) -> El {
    // A vertical "wire" running card-to-card (the column around it has gap 0, so the
    // stubs meet both cards' edges) with a flow chevron riding on it — the signal
    // visibly travels from one stage into the next.
    col()
        .w_full()
        .items_center()
        .child(el().w(2.0).h(10.0).fill(p.primary.alpha(0.3)))
        .child(icon(&icons::CHEVRON_DOWN, 14.0).color(p.primary.alpha(0.75)))
        .child(el().w(2.0).h(10.0).fill(p.primary.alpha(0.3)))
}

/// A read-only stage card (Source, Resample): header + its current value.
fn readout_card(
    key: &str,
    glyph: &'static Icon,
    title: String,
    value: String,
    dot: Color,
    p: &Palette,
) -> El {
    card(key, p)
        .child(stage_header(dot, glyph, title, None, None, false, p))
        .child(
            // Indent the value under the dot+icon so it lines up with the title.
            row()
                .w_full()
                .pl(8.0 + space::SM + 18.0 + space::SM)
                .child(text(value).font(type_::BODY).color(p.fg).grow(1.0).wrap()),
        )
}

/// The Settings → Audio page, laid out as the signal-path pipeline: a stage card
/// per step, connected by flow arrows, from the source to the output device.
fn audio_pipeline(s: &State, p: &Palette, cx: &Cx<State>) -> El {
    use crate::views::signal_path::{decode_value, resample_value, source_value};
    let nothing = || t!("player.nothingPlaying").to_string();
    let source_v = s
        .signal_path
        .map(|x| format!("{}  →  {}", source_value(&x), decode_value(&x)))
        .unwrap_or_else(nothing);
    let resample_v = s
        .signal_path
        .map(|x| resample_value(&x))
        .unwrap_or_else(nothing);
    let rs_accent = if s.signal_path.map(|x| x.resampling).unwrap_or(false) {
        p.accent
    } else {
        p.primary
    };
    // The signal-path stages, flush (gap 0) so each `pipeline_arrow` wire visibly
    // bridges one card into the next.
    let pipeline = col()
        .w_full()
        .gap(0.0)
        .child(readout_card(
            "sp-stage-source",
            &icons::MUSIC,
            t!("player.signalPath.source").to_string(),
            source_v,
            p.primary,
            p,
        ))
        .child(pipeline_arrow(p))
        .child(replaygain_card(s, p, cx))
        .child(pipeline_arrow(p))
        .child(readout_card(
            "sp-stage-resample",
            &icons::REPEAT,
            t!("player.signalPath.resample").to_string(),
            resample_v,
            rs_accent,
            p,
        ))
        .child(pipeline_arrow(p))
        .child(eq_card(s, p, cx))
        .child(pipeline_arrow(p))
        .child(output_card(s, p))
        .child(pipeline_arrow(p))
        .child(exclusive_card(s, p));
    // Audio profiles belong to the whole page, not the DSP card, so they ride a bar
    // pinned to the top of the audio pipeline — switch/manage profiles while scrolled
    // anywhere down the chain. `fill(p.bg)` hides cards scrolling under it; a hairline
    // `stuck_shadow` separates it from the content only once it's actually stuck.
    let profiles = profile_bar(s, p)
        .py(space::SM)
        .fill(p.bg)
        .sticky_top(0.0)
        .stuck_shadow(Shadow {
            y: 0.5,
            color: p.border,
            ..Default::default()
        });
    col()
        .key("settings-content")
        .w_full()
        .gap(space::MD)
        .child(profiles)
        .child(pipeline)
}

/// Map a 0..1 slider position onto the ±[`GAIN_LIMIT_DB`] dB range, rounded to
/// 0.1 dB.
fn slider_to_db(v: f32) -> f32 {
    let db = -GAIN_LIMIT_DB + v * 2.0 * GAIN_LIMIT_DB;
    (db * 10.0).round() / 10.0
}

/// A dB value's 0..1 position on the ±[`GAIN_LIMIT_DB`] slider.
fn db_to_slider(db: f32) -> f32 {
    ((db + GAIN_LIMIT_DB) / (2.0 * GAIN_LIMIT_DB)).clamp(0.0, 1.0)
}

/// The profile switcher's dropdown key (its open state + panel anchor).
const PROFILE_PICKER: &str = "profile-picker";
/// The inline profile-rename text field's key (distinct from the Rename button's
/// `profile-rename`, so the two never collide while renaming).
const PROFILE_RENAME: &str = "profile-rename-field";

/// The `(id, label)` choices for the profile switcher dropdown — one per saved
/// profile, keyed by index so picking one calls `switch_profile`.
fn profile_options(s: &State) -> Vec<(String, String)> {
    s.profile_set
        .profiles()
        .iter()
        .enumerate()
        .map(|(i, p)| (i.to_string(), p.name.clone()))
        .collect()
}

/// A small square icon button for the profile bar. Disabled = greyed, not focusable,
/// no handler (used for Delete when only one profile remains). `label` is the
/// accessible name / tooltip (already localized).
fn profile_icon_btn(
    key: &str,
    glyph: &'static Icon,
    label: String,
    enabled: bool,
    p: &Palette,
    on: impl Fn(&mut State) + 'static,
) -> El {
    let tint = if enabled {
        p.text_secondary
    } else {
        p.text_secondary.alpha(0.35)
    };
    let btn = el()
        .key(key.to_string())
        .role("button")
        .label(label)
        .focusable(enabled)
        .size(28.0, 28.0)
        .center()
        .radius(radius::SM)
        .child(icon(glyph, 16.0).color(tint));
    if enabled {
        btn.cursor(Cursor::Pointer)
            .hover(p.fg.alpha(0.06))
            .on_press(move |st: &mut State| on(st))
    } else {
        btn
    }
}

/// Like [`profile_icon_btn`] but the handler gets the transaction (to open a file
/// dialog for import / export).
fn profile_icon_btn_tx(
    key: &str,
    glyph: &'static Icon,
    label: String,
    p: &Palette,
    on: impl Fn(&mut State, &mut Tx) + 'static,
) -> El {
    el().key(key.to_string())
        .role("button")
        .label(label)
        .focusable(true)
        .size(28.0, 28.0)
        .center()
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .hover(p.fg.alpha(0.06))
        .child(icon(glyph, 16.0).color(p.text_secondary))
        .on_press_tx(move |st: &mut State, tx: &mut Tx| on(st, tx))
}

/// The profile bar at the top of the DSP card: the switcher (a themed dropdown, or an
/// inline text field while renaming) on the left, and New / Duplicate / Rename /
/// Delete | Import / Export icon buttons on the right. Delete is disabled when only one
/// profile remains (there must always be at least one).
fn profile_bar(s: &State, p: &Palette) -> El {
    let switcher: El = if s.profile_rename.is_some() {
        text_input(&s.profile_rename_buf, type_::ITEM_STRONG)
            .key(PROFILE_RENAME)
            .single_line()
            .grow(1.0)
            .px(space::SM)
            .py(space::XXS)
            .radius(radius::SM)
            .fill(p.primary.alpha(0.16))
            .color(p.fg)
            .on_edit(|st: &mut State| &mut st.profile_rename_buf)
            .on_submit(|st: &mut State, _tx| commit_profile_rename(st))
    } else {
        let name = s
            .profile_set
            .active()
            .map(|pr| pr.name.clone())
            .unwrap_or_default();
        dropdown::trigger(PROFILE_PICKER, name, p).grow(1.0)
    };
    let can_delete = s.profile_set.len() > 1;
    row()
        .key("profile-bar")
        .w_full()
        .center_y()
        .gap(space::XS)
        .child(switcher)
        .child(profile_icon_btn(
            "profile-new",
            &icons::PLUS,
            t!("settings.profileNew").to_string(),
            true,
            p,
            new_profile,
        ))
        .child(profile_icon_btn(
            "profile-duplicate",
            &icons::COPY,
            t!("settings.profileDuplicate").to_string(),
            true,
            p,
            duplicate_profile,
        ))
        .child(profile_icon_btn(
            "profile-rename",
            &icons::PENCIL,
            t!("settings.profileRename").to_string(),
            true,
            p,
            start_profile_rename,
        ))
        .child(profile_icon_btn(
            "profile-delete",
            &icons::TRASH,
            t!("settings.profileDelete").to_string(),
            can_delete,
            p,
            delete_profile,
        ))
        // A hairline divider between manage and import/export.
        .child(el().w(1.0).h(16.0).radius(0.5).fill(p.border))
        .child(profile_icon_btn_tx(
            "profile-import",
            &icons::DOWNLOAD,
            t!("settings.profileImport").to_string(),
            p,
            |_st, tx| {
                tx.open_file(
                    FileDialogOptions {
                        title: t!("settings.profileImport").to_string(),
                        ..Default::default()
                    }
                    .filter("Audio profile (*.json)", &["json"]),
                    move |s: &mut State, result, _tx| {
                        if let Ok(Some(path)) = result {
                            import_profile(s, &path);
                        }
                    },
                );
            },
        ))
        .child(profile_icon_btn_tx(
            "profile-export",
            &icons::UPLOAD,
            t!("settings.profileExport").to_string(),
            p,
            |_st, tx| {
                tx.save_file(
                    FileDialogOptions {
                        title: t!("settings.profileExport").to_string(),
                        file_name: Some("profile.json".into()),
                        ..Default::default()
                    }
                    .filter("Audio profile (*.json)", &["json"]),
                    move |s: &mut State, result, _tx| {
                        if let Ok(Some(path)) = result {
                            export_profile(s, &path);
                        }
                    },
                );
            },
        ))
}

/// The **DSP chain** stage: an ordered list of expandable DSP unit cards (EQ is
/// the only kind today), each flowing into the next, plus an "+ Add DSP" control.
/// There is no master toggle — every unit card owns its own enable. The stage dot
/// goes accent when ANY enabled unit is actively shaping the signal.
fn eq_card(s: &State, p: &Palette, cx: &Cx<State>) -> El {
    let shaping = (0..s.dsp_units.len()).any(|i| {
        unit_enabled(s, i)
            && (unit_preamp(s, i).abs() > 0.05 || unit_bands(s, i).iter().any(|b| b.affects()))
    });
    let mut card = card("sp-stage-dsp", p).child(stage_header(
        if shaping { p.accent } else { p.primary },
        &icons::SETTINGS,
        t!("player.signalPath.dsp").to_string(),
        None,
        // No master toggle — each unit card has its own.
        None,
        false,
        p,
    ));
    // One expandable unit card per position, in processing order; a drop line marks
    // the GAP the dragged card will land in. Moving a card DOWN (to > from) it lands
    // *after* `to`, so the line sits before `to + 1` (or at the very end); moving it
    // UP it lands before `to`. (Drawing the line before `to` in both directions put
    // it on the wrong side when dragging downward.)
    let line_slot = s.dsp_drag.and_then(|(from, to)| match to.cmp(&from) {
        std::cmp::Ordering::Equal => None,
        std::cmp::Ordering::Greater => Some(to + 1),
        std::cmp::Ordering::Less => Some(to),
    });
    let drop_line = |p: &Palette| {
        el().key("dsp-drop")
            .w_full()
            .h(2.0)
            .radius(1.0)
            .fill(p.accent)
    };
    for i in 0..s.dsp_units.len() {
        if line_slot == Some(i) {
            card = card.child(drop_line(p));
        }
        card = card.child(dsp_card(i, s, p, cx));
    }
    if line_slot == Some(s.dsp_units.len()) {
        card = card.child(drop_line(p));
    }
    card.child(add_dsp_control(p))
}

/// A disclosure chevron sized for a card / section header (a 16 px `chevron-down`
/// icon, bigger than a text glyph): points down when `expanded`, right when
/// collapsed. Shared by the DSP card and the band-list headers.
fn expand_chevron(
    expanded: bool,
    key: String,
    p: &Palette,
    on: impl Fn(&mut State) + 'static,
) -> El {
    let caret = icon(&icons::CHEVRON_DOWN, 16.0).color(p.text_secondary);
    let caret = if expanded {
        caret
    } else {
        caret.rotate(-std::f32::consts::FRAC_PI_2)
    };
    el().key(key)
        .role("button")
        .focusable(true)
        .cursor(Cursor::Pointer)
        .px(space::XXS)
        .center()
        .child(caret)
        .on_press(move |st: &mut State| on(st))
}

/// One **DSP unit card** (keyed by POSITION, never a unit id): an always-visible
/// header — reorder grip · expand chevron · type label · enable · remove — and,
/// when expanded, the unit's body (preamp + response graph + band table). The body
/// dims when the unit is disabled.
fn dsp_card(i: usize, s: &State, p: &Palette, cx: &Cx<State>) -> El {
    let enabled = unit_enabled(s, i);
    let expanded = s.dsp_expanded.get(i).copied().unwrap_or(false);
    let dragged = s.dsp_drag.map(|(from, _)| from) == Some(i);
    // Expand/collapse chevron (points down when open, right when collapsed).
    let chevron = expand_chevron(expanded, format!("dsp-expand-{i}"), p, move |st| {
        toggle_dsp_expanded(st, i)
    });
    // The unit title: "<name> (EQ)" when the user has named it, else just its type
    // ("EQ"). Double-click it to rename the unit; while renaming it becomes an inline
    // text field bound to the shared rename buffer (mirrors the number editor).
    let name_key = format!("dsp-name-{i}");
    let label: El = if s.dsp_rename == Some(i) {
        text_input(&s.dsp_rename_buf, type_::ITEM_STRONG)
            .key(name_key)
            .single_line()
            .min_w(96.0)
            .px(space::XXS)
            .radius(radius::SM)
            .fill(p.primary.alpha(0.16))
            .color(p.fg)
            .on_edit(|st: &mut State| &mut st.dsp_rename_buf)
            .on_submit(|st: &mut State, _tx| commit_dsp_rename(st))
    } else {
        let label_text = match s.dsp_units.get(i).and_then(DspUnit::name) {
            Some(n) => format!("{n} (EQ)"),
            None => "EQ".to_string(),
        };
        text(label_text)
            .font(type_::ITEM_STRONG)
            .color(if enabled { p.fg } else { p.text_secondary })
            .key(name_key)
            .role("button")
            .focusable(true)
            .cursor(Cursor::Pointer)
            .on_double_press(move |st: &mut State| start_dsp_rename(st, i))
    };
    // Drag-reorder grip. Cards vary wildly in height (an expanded card is many times
    // a collapsed one), so a fixed row-height → slot mapping lands way off. Instead
    // snap the drop target to whichever card the pointer is actually over, via
    // `Tx::element_at_in` — exact regardless of card heights.
    let grip = el()
        .key(format!("dsp-grip-{i}"))
        .role("button")
        .size(space::MD, DSP_ROW_H)
        .center()
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .child(icon(&icons::GRIP, 12.0).color(p.text_secondary.alpha(0.6)))
        .on_drag(
            move |st: &mut State, d: &mut rune::Drag, tx: &mut Tx| match d.phase {
                rune::Phase::Down => st.dsp_drag = Some((i, i)),
                rune::Phase::Move => {
                    let [px, py] = d.window_point();
                    tx.element_at_in("sp-stage-dsp", px, py, |st: &mut State, picked, _tx| {
                        let Some((from, cur)) = st.dsp_drag else {
                            return;
                        };
                        // Snap to the card under the pointer; keep the current
                        // target when the pointer is off the cards (gaps/edges).
                        let to = picked
                            .as_ref()
                            .and_then(dsp_card_index_from_pick)
                            .unwrap_or(cur);
                        st.dsp_drag = Some((from, to));
                    });
                }
                rune::Phase::Up => {
                    if let Some((from, to)) = st.dsp_drag.take() {
                        reorder_dsp_units(st, from, to);
                    }
                }
            },
        );
    // Per-unit enable chip (the old header On/Off styling).
    let (en_fill, en_fg) = if enabled {
        (p.primary.alpha(0.16), p.primary)
    } else {
        (p.muted, p.text_secondary)
    };
    let enable = chip_btn(
        format!("dsp-en-{i}"),
        if enabled { "On" } else { "Off" }.into(),
        en_fill,
        en_fg,
        move |st| set_dsp_unit_enabled(st, i, !enabled),
    );
    let remove = chip_btn(
        format!("dsp-rm-{i}"),
        "✕".into(),
        p.muted,
        p.text_secondary,
        move |st| remove_dsp_unit(st, i),
    );
    let header = row()
        .w_full()
        .center_y()
        .gap(space::SM)
        .child(grip)
        .child(chevron)
        .child(label)
        .child(el().grow(1.0))
        .child(enable)
        .child(remove);
    let mut unit_card = col()
        .key(format!("dsp-card-{i}"))
        .w_full()
        .gap(space::SM)
        // A bordered inset panel on the page surface, so each unit reads as its own
        // card within the DSP stage card.
        .fill(p.bg)
        .radius(radius::MD)
        .border_sides([1.0; 4], p.border)
        .padding(space::MD)
        .child(header);
    if expanded {
        // Preamp slider (same ± range as a band; commits live), keyed per unit.
        let preamp = unit_preamp(s, i);
        let apply_preamp = move |st: &mut State, v: f32| set_eq_preamp(st, i, slider_to_db(v));
        let slider_key = format!("eq-preamp-{i}");
        let dragging = s.dragging_slider.as_deref() == Some(slider_key.as_str());
        let preamp_row = col()
            .key(format!("eq-preamp-row-{i}"))
            .w_full()
            .gap(space::XS)
            .child(
                row()
                    .w_full()
                    .center_y()
                    .child(
                        text(t!("settings.preamp").to_string())
                            .color(p.text_secondary)
                            .grow(1.0)
                            .shrink(),
                    )
                    .child(text(format!("{preamp:+.1} dB")).color(p.text_secondary)),
            )
            .child(row().w_full().child(slider(
                &slider_key,
                db_to_slider(preamp),
                dragging,
                cx,
                p,
                apply_preamp,
                apply_preamp,
            )));
        // Body: preamp + parametric response-curve graph + band table. Dimmed when
        // this unit is disabled.
        let body = col()
            .key(format!("eq-body-{i}"))
            .w_full()
            .gap(space::MD)
            .child(preamp_row)
            .child(crate::views::eq_graph::eq_graph(i, s, p, cx))
            .child(eq_band_editor(i, s, p));
        let body = if enabled {
            body
        } else {
            body.opacity(fx::DISABLED)
        };
        unit_card = unit_card.child(body);
    }
    if dragged {
        unit_card.opacity(0.4)
    } else {
        unit_card
    }
}

/// Height of the reorder grip (points) — just the grip's own hit area, no longer a
/// slot-mapping unit (the drop target comes from the card under the pointer).
const DSP_ROW_H: f32 = 34.0;

/// The DSP-card index a [`Picked`](rune::Picked) element sits in, read off its key
/// path (cards are keyed `dsp-card-{i}`). `None` when the pick is outside any card —
/// e.g. the pointer is in a gap between cards or past the chain's edges.
fn dsp_card_index_from_pick(picked: &rune::Picked) -> Option<usize> {
    picked.path().split('/').find_map(|seg| {
        seg.strip_prefix("dsp-card-")
            .and_then(|n| n.parse::<usize>().ok())
    })
}

/// The key identifying the add-DSP menu (its open state + panel anchor).
const ADD_DSP_MENU: &str = "dsp-add";

/// The DSP-type choices the add menu offers. One entry (EQ) today — built as a
/// dropdown so more unit types slot in later without reworking the control.
fn dsp_type_options() -> Vec<(String, String)> {
    vec![("eq".to_string(), "EQ".to_string())]
}

/// The "+ Add DSP" control: opens the shared [`dropdown`] of DSP types
/// ([`add_dsp_menu`], mounted at the window root). (The dropdown is now light-dismiss
/// — an outside press / scroll / Escape closes it — so it behaves even anchored deep
/// in the settings scroll.)
fn add_dsp_control(p: &Palette) -> El {
    // A touch taller than a row chip (py SM vs XXS) so the add action reads as the
    // chain's primary control.
    text(t!("settings.addDsp").to_string())
        .font(type_::LABEL)
        .px(space::MD)
        .py(space::SM)
        .radius(radius::SM)
        .key(ADD_DSP_MENU)
        .role("button")
        .focusable(true)
        .cursor(Cursor::Pointer)
        .fill(p.primary.alpha(0.16))
        .color(p.primary)
        .on_press(move |st: &mut State| crate::toggle_dropdown(st, ADD_DSP_MENU))
}

/// The add-DSP dropdown panel — **mount at the window root** via
/// [`settings_overlays`]. Picking a type appends that unit (EQ today) and opens its
/// card; a trailing greyed "more DSP solutions coming soon" row teases what's next.
fn add_dsp_menu(p: &Palette) -> El {
    dropdown::panel(
        ADD_DSP_MENU,
        &dsp_type_options(),
        "",
        None,
        |st: &mut State, _id: &str| add_dsp_unit(st),
        Some(t!("settings.moreDspSoon").as_ref()),
        p,
    )
}

/// A compact label for a band's shape (editor type chip).
fn kind_chip_label(kind: BandKind) -> &'static str {
    match kind {
        BandKind::LowShelf => "Lo shelf",
        BandKind::Peaking => "Bell",
        BandKind::HighShelf => "Hi shelf",
        BandKind::LowCut => "Lo cut",
        BandKind::HighCut => "Hi cut",
        BandKind::Notch => "Notch",
    }
}

/// A small chip-style button for the editor rows.
fn chip_btn(
    key: String,
    label: String,
    fill: Color,
    fg: Color,
    on: impl Fn(&mut State) + 'static,
) -> El {
    text(label)
        .font(type_::LABEL)
        .px(space::SNUG)
        .py(space::XXS)
        .radius(radius::SM)
        .key(key)
        .role("button")
        .focusable(true)
        .cursor(Cursor::Pointer)
        .fill(fill)
        .color(fg)
        .on_press(move |st: &mut State| on(st))
}

/// A chip-style button that needs the transaction (e.g. to open a file dialog).
fn chip_btn_tx(
    key: String,
    label: String,
    p: &Palette,
    on: impl Fn(&mut State, &mut Tx) + 'static,
) -> El {
    text(label)
        .font(type_::LABEL)
        .px(space::SNUG)
        .py(space::XXS)
        .radius(radius::SM)
        .key(key)
        .role("button")
        .focusable(true)
        .cursor(Cursor::Pointer)
        .fill(p.muted)
        .color(p.text_secondary)
        .on_press_tx(move |st: &mut State, tx: &mut Tx| on(st, tx))
}

/// Unit `unit`'s per-band editor: a header with the band count + Import / Export /
/// Add buttons, then a row per band to retype / nudge gain & Q / bypass / remove
/// it. The table shows directly (the card's own expand is the one level of
/// collapse — there is no inner "Bands" toggle).
fn eq_band_editor(unit: usize, s: &State, p: &Palette) -> El {
    let n = unit_bands(s, unit).len();
    let bands_open = s.eq_bands_expanded.get(unit).copied().unwrap_or(true);
    let count = row()
        .center_y()
        .gap(space::XXS)
        .child(expand_chevron(
            bands_open,
            format!("eq-bands-expand-{unit}"),
            p,
            move |st| toggle_eq_bands_expanded(st, unit),
        ))
        .child(text(format!("Bands ({n})")).font(type_::LABEL).color(p.fg));
    let can_add = n < soul_player_host::MAX_BANDS;
    let add = chip_btn(
        format!("eq-add-band-{unit}"),
        "+ Add band".into(),
        if can_add {
            p.primary.alpha(0.16)
        } else {
            p.muted
        },
        if can_add { p.primary } else { p.text_secondary },
        move |st| {
            // Add a bell in the mid-band (1 kHz, flat) — the user then drags it.
            add_eq_band(st, unit, 1000.0, 0.0);
        },
    );
    // Import / export AutoEQ (Equalizer-APO ParametricEQ) profiles via a native
    // file dialog — load headphone-correction profiles or share presets. Each acts
    // on THIS card's unit.
    let import = chip_btn_tx(
        format!("eq-import-{unit}"),
        "Import".into(),
        p,
        move |_s, tx| {
            tx.open_file(
                FileDialogOptions {
                    title: "Import EQ profile".into(),
                    ..Default::default()
                }
                .filter("ParametricEQ (*.txt)", &["txt"]),
                move |s: &mut State, result, _tx| {
                    if let Ok(Some(path)) = result {
                        import_eq(s, unit, &path);
                    }
                },
            );
        },
    );
    let export = chip_btn_tx(
        format!("eq-export-{unit}"),
        "Export".into(),
        p,
        move |_s, tx| {
            tx.save_file(
                FileDialogOptions {
                    title: "Export EQ profile".into(),
                    file_name: Some("ParametricEQ.txt".into()),
                    ..Default::default()
                }
                .filter("ParametricEQ (*.txt)", &["txt"]),
                move |s: &mut State, result, _tx| {
                    if let Ok(Some(path)) = result {
                        export_eq(s, unit, &path);
                    }
                },
            );
        },
    );
    let header = row()
        .w_full()
        .center_y()
        .gap(space::XS)
        .child(count)
        .child(el().grow(1.0))
        .child(import)
        .child(export)
        .child(add);
    let mut editor = col()
        .key(format!("eq-editor-{unit}"))
        .w_full()
        .gap(space::SM)
        .child(header);
    if bands_open {
        let mut list = col().w_full().gap(space::XS);
        for i in 0..n {
            list = list.child(eq_band_row(unit, i, s, p));
        }
        editor = editor.child(list);
    }
    editor
}

/// The key-slug for a band number's column (keeps the test keys stable).
fn field_slug(field: NumField) -> &'static str {
    match field {
        NumField::Freq => "freq",
        NumField::Gain => "gain",
        NumField::Q => "q",
    }
}

/// An Ableton-style number box for unit `unit`'s band `i` `field`: the value `num`
/// drawn mono + right-aligned in a fixed `num_w` column (so a column's decimals line
/// up), with an optional `suffix` label to its right. Drag it vertically or scroll on
/// it to change the value; double-click to type one. While that number is being
/// typed, the box becomes an inline text field bound to the shared edit buffer.
#[allow(clippy::too_many_arguments)]
fn num_field(
    unit: usize,
    i: usize,
    field: NumField,
    num: String,
    suffix: &str,
    num_w: f32,
    s: &State,
    p: &Palette,
) -> El {
    let key = format!("eq-num-{}-{unit}-{i}", field_slug(field));
    let editing = s.eq_edit == Some((unit, i, field));
    // Frequency reads as a secondary value; gain and Q are primary foreground.
    let color = match field {
        NumField::Freq => p.text_secondary,
        NumField::Gain | NumField::Q => p.fg,
    };
    let value: El = if editing {
        text_input(&s.eq_edit_buf, type_::CAPTION.mono())
            .key(key)
            .single_line()
            // Right-align the field so the digits sit exactly where the static value
            // did — no horizontal jump when the box opens for editing.
            .align(Align::End)
            .w(num_w)
            .px(space::XXS)
            .radius(radius::SM)
            .fill(p.primary.alpha(0.16))
            .color(p.fg)
            .on_edit(|st: &mut State| &mut st.eq_edit_buf)
            .on_submit(|st: &mut State, _tx| commit_eq_edit(st))
    } else {
        text(num)
            .font(type_::CAPTION.mono())
            .color(color)
            .align(Align::End)
            .w(num_w)
            .px(space::XXS)
            .radius(radius::SM)
            .key(key)
            .role("button")
            .focusable(true)
            .cursor(Cursor::ResizeRow)
            .on_double_press(move |st: &mut State| eq_edit_start(st, unit, i, field))
            .on_drag(move |st: &mut State, d: &mut Drag, _tx| match d.phase {
                Phase::Down => eq_num_drag_start(st, unit, i, field),
                Phase::Move => eq_num_drag_move(st, d.dy),
                Phase::Up => eq_num_drag_end(st),
            })
            .on_wheel(move |st: &mut State, ev: WheelEvent, _tx| {
                // Windows turns Shift+wheel into a HORIZONTAL wheel: the delta then
                // arrives in `dx` with `dy == 0`, so read whichever axis carries it
                // (otherwise Shift+scroll — the fine-tune gesture — does nothing).
                // Then mirror the volume wheel: ignore trackpad coasting, step once
                // per detent, and accumulate a trackpad's fine `precise` deltas so a
                // slow scroll still steps in BOTH directions. Negative delta = scroll
                // up = raise the value; Shift fine-tunes.
                let raw = if ev.dy != 0.0 { ev.dy } else { ev.dx };
                if ev.momentum || raw == 0.0 {
                    return true;
                }
                let fine = ev.mods.shift;
                if ev.precise {
                    st.eq_wheel_accum += raw;
                    let steps = (st.eq_wheel_accum / WHEEL_NOTCH_PX).trunc();
                    st.eq_wheel_accum -= steps * WHEEL_NOTCH_PX;
                    let up = steps < 0.0;
                    let count = (steps.abs() as i32).min(8); // guard a giant delta
                    for _ in 0..count {
                        eq_num_scroll(st, unit, i, field, up, fine);
                    }
                } else {
                    st.eq_wheel_accum = 0.0; // a real detent drops any partial travel
                    eq_num_scroll(st, unit, i, field, raw < 0.0, fine);
                }
                true
            })
    };
    let mut cell = row().center_y().gap(space::XXS).child(value);
    if !suffix.is_empty() {
        cell = cell.child(
            text(suffix.to_string())
                .font(type_::CAPTION)
                .color(p.text_secondary)
                .w(24.0),
        );
    }
    cell
}

/// One editor row: type chip · frequency · gain · Q · bypass · remove. The three
/// numbers are Ableton-style boxes (drag / scroll / double-click to type), mono and
/// right-aligned so each column's decimals line up.
fn eq_band_row(unit: usize, i: usize, s: &State, p: &Palette) -> El {
    let band = unit_bands(s, unit)[i];
    let gains = band.kind.uses_gain();
    // Frequency: whole Hz under 1 kHz, one-decimal kHz above.
    let (freq_num, freq_unit) = if band.freq >= 1000.0 {
        (format!("{:.1}", band.freq / 1000.0), "kHz")
    } else {
        (format!("{:.0}", band.freq), "Hz")
    };
    let freq = num_field(unit, i, NumField::Freq, freq_num, freq_unit, 40.0, s, p);
    // Gain: an editable box for shapes that use gain; a static dash (same column
    // metrics) for the cuts / notch that don't.
    let gain = if gains {
        num_field(
            unit,
            i,
            NumField::Gain,
            format!("{:+.1}", band.gain_db),
            "dB",
            46.0,
            s,
            p,
        )
    } else {
        row()
            .center_y()
            .gap(space::XXS)
            .child(
                text("—".to_string())
                    .font(type_::CAPTION.mono())
                    .color(p.muted)
                    .align(Align::End)
                    .px(space::XXS)
                    .w(46.0),
            )
            .child(
                text("dB".to_string())
                    .font(type_::CAPTION)
                    .color(p.muted)
                    .w(24.0),
            )
    };
    let q = row()
        .center_y()
        .gap(space::XXS)
        .child(
            text("Q".to_string())
                .font(type_::CAPTION)
                .color(p.text_secondary),
        )
        .child(num_field(
            unit,
            i,
            NumField::Q,
            format!("{:.2}", band.q),
            "",
            44.0,
            s,
            p,
        ));
    let (en_fill, en_fg) = if band.enabled {
        (p.primary.alpha(0.16), p.primary)
    } else {
        (p.muted, p.text_secondary)
    };
    let enabled = band.enabled;
    row()
        .key(format!("eq-band-row-{unit}-{i}"))
        .w_full()
        .center_y()
        .gap(space::SM)
        // Wrap the trailing controls onto a second line when the card is too narrow
        // for the whole row (e.g. a narrow window) rather than overflowing it.
        .flex_wrap()
        // Fixed-width type column so the row doesn't reshuffle when the shape
        // label changes width (e.g. "Bell" → "Hi shelf").
        .child(row().w(84.0).center_y().child(chip_btn(
            format!("eq-band-type-{unit}-{i}"),
            kind_chip_label(band.kind).to_string(),
            p.muted,
            p.fg,
            move |st| cycle_eq_band_kind(st, unit, i),
        )))
        .child(freq)
        .child(gain)
        .child(q)
        .child(el().grow(1.0))
        .child(chip_btn(
            format!("eq-band-en-{unit}-{i}"),
            if enabled { "On" } else { "Off" }.into(),
            en_fill,
            en_fg,
            move |st| set_eq_band_enabled(st, unit, i, !enabled),
        ))
        .child(chip_btn(
            format!("eq-band-rm-{unit}-{i}"),
            "✕".into(),
            p.muted,
            p.text_secondary,
            move |st| remove_eq_band(st, unit, i),
        ))
}

/// ReplayGain: an Off / Track / Album mode row plus a preamp slider (−15..+15
/// dB). Selecting a mode or moving the preamp updates the live actor immediately
/// and persists the choice. The slider commits on release (its value maps the
/// 0..1 track position onto the dB range), and the current dB is shown to its
/// right.
fn replaygain_card(s: &State, p: &Palette, cx: &Cx<State>) -> El {
    let preamp = s.replaygain_preamp;
    let value01 = ((preamp - RG_PREAMP_MIN) / (RG_PREAMP_MAX - RG_PREAMP_MIN)).clamp(0.0, 1.0);
    let modes = row()
        .gap(space::SM)
        .flex_wrap()
        .child(rg_mode_button(
            t!("settings.rgOff").to_string(),
            "rg-off",
            ReplayGainMode::Off,
            s.replaygain_mode == ReplayGainMode::Off,
            p,
        ))
        .child(rg_mode_button(
            t!("settings.rgTrack").to_string(),
            "rg-track",
            ReplayGainMode::Track,
            s.replaygain_mode == ReplayGainMode::Track,
            p,
        ))
        .child(rg_mode_button(
            t!("settings.rgAlbum").to_string(),
            "rg-album",
            ReplayGainMode::Album,
            s.replaygain_mode == ReplayGainMode::Album,
            p,
        ));
    // Map the 0..1 slider position back onto the dB range, rounded to 0.1 dB so
    // the persisted value stays clean. Used for both scrub and release (preamp
    // commits live), hence a reusable closure.
    let apply_preamp = |st: &mut State, v: f32| {
        let db = RG_PREAMP_MIN + v * (RG_PREAMP_MAX - RG_PREAMP_MIN);
        set_replaygain_preamp(st, (db * 10.0).round() / 10.0);
    };
    // Label + current dB on their own row above a full-width slider, rather than
    // label | slider | value in one row with fixed-width text cells: a longer
    // locale's label (e.g. German "Vorverstärkung") then has the whole width and
    // can't overrun a fixed cell into the slider.
    let preamp_row = col()
        .key("rg-preamp-row")
        .w_full()
        .gap(space::XS)
        .child(
            row()
                .w_full()
                .center_y()
                .child(
                    text(t!("settings.preamp").to_string())
                        .color(p.text_secondary)
                        .grow(1.0)
                        .shrink(),
                )
                .child(text(format!("{preamp:+.1} dB")).color(p.text_secondary)),
        )
        .child(row().w_full().child(slider(
            "rg-preamp",
            value01,
            s.dragging_slider.as_deref() == Some("rg-preamp"),
            cx,
            p,
            // Preamp commits live — scrub and release both apply the dB, so the
            // knob tracks the pointer and the gain updates as you drag.
            apply_preamp,
            apply_preamp,
        )));
    // Accent dot when ReplayGain is applying gain (a processing stage), else a
    // clean primary dot — the same Roon-style semantic as the signal-path flow.
    let rg_dot = if s.replaygain_mode == ReplayGainMode::Off {
        p.primary
    } else {
        p.accent
    };
    card("sp-stage-replaygain", p)
        .child(stage_header(
            rg_dot,
            &icons::VOLUME,
            t!("settings.replayGain").to_string(),
            Some(t!("settings.replayGainDesc").to_string()),
            None,
            false,
            p,
        ))
        .child(modes)
        .child(preamp_row)
}

/// A quiet, bordered action button for the About card (`p.bg` fill against the
/// card's `p.card` so it reads as a secondary action beside the primary update
/// button), with a leading glyph.
fn support_button(key: &'static str, label: String, glyph: &'static Icon, p: &Palette) -> El {
    el().key(key)
        .role("button")
        .label(label.clone())
        .focusable(true)
        .padding(space::MD)
        .radius(radius::MD)
        .fill(p.bg)
        .border(1.0, p.border)
        .center()
        .cursor(Cursor::Pointer)
        .child(
            row()
                .gap(space::SM)
                .center_y()
                .child(icon(glyph, 16.0).color(p.fg))
                .child(text(label).color(p.fg)),
        )
}

/// The About card's support actions: reveal the log folder, or open a pre-filled
/// GitHub issue carrying version + OS + a redacted recent-log tail. Both run off
/// the UI thread — launching the file manager or browser can block briefly — and
/// nothing is sent until the user submits GitHub's web form.
fn support_row(p: &Palette) -> El {
    row()
        .gap(space::SM)
        // Wrap to a second line in longer locales (e.g. Spanish) instead of
        // overflowing the card width.
        .flex_wrap()
        .child(
            support_button(
                "open-logs",
                t!("settings.openLogs").to_string(),
                &icons::FOLDER,
                p,
            )
            .on_press(|_s: &mut State| {
                std::thread::spawn(|| match crate::logging::log_dir() {
                    Some(dir) => {
                        if let Err(e) = open::that(&dir) {
                            tracing::error!("open logs folder failed: {e}");
                        }
                    }
                    None => tracing::warn!("no log directory to open"),
                });
            }),
        )
        .child(
            support_button(
                "report-problem",
                t!("settings.reportProblem").to_string(),
                &icons::GITHUB,
                p,
            )
            .on_press(|s: &mut State| {
                // Read state now; do the file read + browser launch off-thread.
                let device = s.output_device.clone();
                std::thread::spawn(move || crate::report::open_issue(device));
            }),
        )
        .child(
            support_button(
                "star-repo",
                t!("settings.starRepo").to_string(),
                &icons::STAR,
                p,
            )
            .on_press(|_s: &mut State| {
                std::thread::spawn(|| {
                    let url = format!("https://github.com/{}", crate::report::ISSUE_REPO);
                    if let Err(e) = open::that(&url) {
                        tracing::error!("open repository failed: {e}");
                    }
                });
            }),
        )
}

/// About: the running version, support actions (report a problem / open logs),
/// and, when the launch-time check (`update::available`) found a newer signed
/// release, an "Install & Restart" button. Install and the support actions run
/// off the UI thread because they block (download/relaunch, or launching a
/// browser/file manager).
fn about_card(s: &State, p: &Palette) -> El {
    let mut c = card("updates", p)
        .child(heading(
            t!("settings.about").to_string(),
            t!("settings.aboutDesc").to_string(),
            p,
        ))
        .child(
            text(t!("settings.version", version => env!("CARGO_PKG_VERSION")).to_string())
                .color(p.text_secondary),
        )
        .child(support_row(p));
    if let Some(version) = &s.update_available {
        c = c.child(
            row()
                .gap(space::MD)
                .center_y()
                .child(
                    text(t!("settings.updateAvailable", version => version).to_string())
                        .color(p.fg)
                        .grow(1.0),
                )
                .child(
                    el().key("install-update")
                        .role("button")
                        .label(t!("settings.installRestart").to_string())
                        .focusable(true)
                        .padding(space::MD)
                        .radius(radius::MD)
                        .fill(p.primary)
                        .center()
                        .cursor(Cursor::Pointer)
                        .child(text(t!("settings.installRestart").to_string()).color(p.bg))
                        .on_press(|_s: &mut State| {
                            std::thread::spawn(|| {
                                if let Err(e) = crate::update::install() {
                                    tracing::error!("update install failed: {e}");
                                }
                            });
                        }),
                ),
        );
    }
    c
}

// ── Page ─────────────────────────────────────────────────────────────────────

thread_local! {
    /// Thumb geometry for the Settings scroll viewport: written each frame by the
    /// viewport's `on_scroll_offset` and read by the thumb's `live_rect`, so the
    /// bar follows the scroll in the same frame without re-running the view. One
    /// per render thread (there is a single Settings viewport at a time).
    static SETTINGS_SCROLL_THUMB: Live<[f32; 4]> = Live::new([0.0; 4]);
}

/// The Settings page: the category rail on the left, the selected category's
/// cards centred and width-capped on the right (mirroring the original's
/// `max-w-4xl mx-auto`), scrollable with a themed scrollbar.
pub fn settings_page(s: &State, p: &Palette, cx: &Cx<State>) -> El {
    let content = match s.settings_tab {
        SettingsTab::Appearance => col()
            .key("settings-content")
            .gap(space::LG)
            .child(appearance_card(s, p))
            .child(navigation_card(s, p))
            .child(language_card(s, p)),
        SettingsTab::MusicData => col()
            .key("settings-content")
            .gap(space::LG)
            .child(music_data_card(s, p))
            .child(managed_library_card(s, p)),
        SettingsTab::Audio => audio_pipeline(s, p, cx),
        SettingsTab::About => col()
            .key("settings-content")
            .gap(space::LG)
            .child(about_card(s, p)),
    };
    // Padding lives INSIDE the scroll viewport (no fixed dead gap above the first
    // card; the bottom margin travels with the content). The top inset is `LG` —
    // the same as the category rail and the main sidebar's nav — so the first card
    // lines up with the first rail item and the first nav item.
    let inner = content
        .w_full()
        .max_w(CONTENT_MAX_W)
        .px(space::XL)
        .pt(space::LG)
        .pb(space::XXL);
    // One Live carries the thumb geometry from the viewport's scroll callback to
    // the thumb element in the same frame.
    let thumb = SETTINGS_SCROLL_THUMB.with(|t| t.clone());
    row()
        .key("settings")
        .grow(1.0)
        .min_h(0.0)
        .child(category_nav(s, p))
        .child(
            // The scroll VIEWPORT fills the whole body (so its scrollbar sits at the
            // window's far-right edge, not floating at the content column's edge);
            // `items_center` centers the `max_w`-capped content column within it, so
            // every tab's cards share one consistent width and a centered position
            // even when the window is wider than the column.
            row()
                .key("settings-body")
                .grow(1.0)
                .min_h(0.0)
                .child(scrollbar::attach(
                    col()
                        .key("settings-scroll")
                        .grow(1.0)
                        .min_h(0.0)
                        .items_center()
                        .scroll(ScrollSpec {
                            stick_bottom: false,
                        })
                        // Fade the top/bottom edges as content runs under them.
                        .scroll_edge_fade(20.0)
                        .child(inner),
                    "settings-scroll",
                    &thumb,
                    "settings-scrollbar",
                    p,
                )),
        )
}

// ── Organize modal (the "Add music → organize" apply flow) ───────────────────

/// Min / max width of the centered organize card, so it reads as a dialog on a
/// wide window and still fits a cramped one.
const ORGANIZE_CARD_MIN_W: f32 = 320.0;
const ORGANIZE_CARD_MAX_W: f32 = 460.0;

/// The shared surface of the organize modal card: a rounded, bordered panel on
/// the card colour, width-bounded so a long path truncates rather than overflows.
fn organize_card(p: &Palette) -> El {
    col()
        .key("organize-card")
        .fill(p.card)
        .radius(radius::LG)
        .border_sides([1.0; 4], p.border)
        .padding(space::LG)
        .gap(space::MD)
        .min_w(ORGANIZE_CARD_MIN_W)
        .max_w(ORGANIZE_CARD_MAX_W)
}

/// A modal title line (wraps so a longer locale reflows instead of overflowing).
fn organize_title(title: String, p: &Palette) -> El {
    text(title).font(type_::TITLE).color(p.fg).wrap()
}

/// One organize-modal action button (Confirm / Cancel / Close), mirroring
/// `theme_button`'s selected/idle fill; `primary` tints it as the affirmative
/// action.
fn organize_button(
    key: &str,
    label: String,
    primary: bool,
    p: &Palette,
    on_press: fn(&mut State),
) -> El {
    el().key(key.to_string())
        .role("button")
        .label(label.clone())
        .focusable(true)
        .padding(space::SM)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .fill(if primary { p.primary } else { p.muted })
        .child(text(label).color(if primary { p.bg } else { p.fg }))
        .on_press(on_press)
}

/// Count a plan's entries by [`organize::Action`] into
/// `(copy, moved, duplicates, conflicts)` for the Confirm summary line.
fn organize_counts(plan: &organize::OrganizePlan) -> (usize, usize, usize, usize) {
    let (mut copy, mut moved, mut dup, mut conflict) = (0, 0, 0, 0);
    for e in &plan.entries {
        match e.action {
            organize::Action::Copy => copy += 1,
            organize::Action::Move => moved += 1,
            organize::Action::SkipDuplicate => dup += 1,
            organize::Action::SkipConflict => conflict += 1,
        }
    }
    (copy, moved, dup, conflict)
}

/// The per-phase card content (without the scrim / modal wrapper).
fn organize_content(phase: &OrganizePhase, p: &Palette) -> El {
    match phase {
        OrganizePhase::Planning => organize_card(p).child(
            text(t!("settings.scanning").to_string())
                .key("organize-planning")
                .font(type_::TITLE)
                .color(p.fg)
                .wrap(),
        ),
        OrganizePhase::Confirm(plan) => {
            let (copy, moved, dup, conflict) = organize_counts(plan);
            let summary = t!(
                "settings.organizeConfirmSummary",
                copy => copy,
                moved => moved,
                dup => dup,
                conflict => conflict
            )
            .to_string();
            organize_card(p)
                .child(organize_title(
                    t!("settings.organizeConfirmTitle").to_string(),
                    p,
                ))
                .child(
                    col()
                        .key("organize-confirm")
                        .w_full()
                        .gap(space::XXS)
                        .child(text(summary).font(type_::BODY).color(p.fg).wrap())
                        .child(
                            text(plan.dest_root.to_string_lossy().into_owned())
                                .font(type_::CAPTION)
                                .color(p.text_secondary)
                                .truncate(),
                        ),
                )
                .child(
                    row()
                        .gap(space::SM)
                        .flex_wrap()
                        .child(organize_button(
                            "organize-cancel",
                            t!("settings.cancel").to_string(),
                            false,
                            p,
                            organize_dismiss,
                        ))
                        .child(organize_button(
                            "organize-confirm-go",
                            t!("settings.confirm").to_string(),
                            true,
                            p,
                            organize_confirm,
                        )),
                )
        }
        OrganizePhase::Applying {
            done,
            total,
            current,
        } => organize_card(p)
            .child(organize_title(
                t!("settings.organizeApplying").to_string(),
                p,
            ))
            .child(
                text(format!("{done} / {total}"))
                    .key("organize-progress")
                    .font(type_::BODY)
                    .color(p.fg),
            )
            .child(
                text(current.clone())
                    .font(type_::CAPTION)
                    .color(p.text_secondary)
                    .truncate(),
            ),
        OrganizePhase::Done(summary) => {
            let line = t!(
                "settings.organizeDoneSummary",
                copied => summary.copied,
                moved => summary.moved,
                skipped => summary.skipped,
                failed => summary.failed.len()
            )
            .to_string();
            organize_card(p)
                .child(organize_title(
                    t!("settings.organizeDoneTitle").to_string(),
                    p,
                ))
                .child(
                    text(line)
                        .key("organize-done")
                        .font(type_::BODY)
                        .color(p.fg)
                        .wrap(),
                )
                .child(row().child(organize_button(
                    "organize-close",
                    t!("settings.close").to_string(),
                    true,
                    p,
                    organize_dismiss,
                )))
        }
    }
}

/// The managed-library organize **modal**: a centered card over a dim scrim,
/// mounted on the root overlay layer (via [`settings_overlays`]) so it shows over
/// any route. `None` when the flow is idle (`State::organize` is `None`).
///
/// The scrim is a full-window element (`absolute` + `w_full`/`h_full`) that dims
/// the pane; the card inside is the `modal`, so rune traps focus in it, makes the
/// background inert, and dismisses on Escape or an outside (scrim) press in every
/// phase — including mid-Confirm, where an outside press simply cancels (the same
/// as the Cancel button).
pub fn organize_overlay(s: &State, p: &Palette) -> Option<El> {
    let phase = s.organize.as_ref()?;
    Some(
        col()
            .key("organize-modal")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .center()
            .fill(Color::BLACK.alpha(0.45))
            .child(organize_content(phase, p).modal(true, organize_dismiss)),
    )
}

/// Settings' root-mounted overlay layer: the open dropdown's floating panel, if
/// any. [`crate::view`] mounts this after the shell so it draws above everything
/// and escapes the content scroll viewport's clip (see [`dropdown::panel`]).
/// Empty when no settings dropdown is open.
pub fn settings_overlays(s: &State, p: &Palette, cx: &Cx<State>) -> Vec<El> {
    let mut overlays = Vec::new();
    if dropdown::is_open(s, LANG_PICKER) {
        // Match the panel width to the trigger (once it has a committed layout).
        let width = cx.layout_size(LANG_PICKER).map(|wh| wh[0]);
        overlays.push(dropdown::panel(
            LANG_PICKER,
            &language_options(),
            &s.locale_pref,
            width,
            |s: &mut State, id: &str| set_locale_pref(s, id),
            None,
            p,
        ));
    }
    if dropdown::is_open(s, ADD_SOURCE_MENU) {
        overlays.push(add_source_menu(s, p));
    }
    // The remote connect form outlives the add-source menu (opening it closes
    // the menu), so it's gated on its own flag rather than `dropdown::is_open`.
    if let Some(form) = remote_form_overlay(s, p) {
        overlays.push(form);
    }
    if dropdown::is_open(s, ADD_DSP_MENU) {
        overlays.push(add_dsp_menu(p));
    }
    if dropdown::is_open(s, PROFILE_PICKER) {
        // Match the panel width to the trigger; the active profile is pre-selected.
        let width = cx.layout_size(PROFILE_PICKER).map(|wh| wh[0]);
        let active = s.profile_set.active_index().to_string();
        overlays.push(dropdown::panel(
            PROFILE_PICKER,
            &profile_options(s),
            &active,
            width,
            |s: &mut State, id: &str| {
                if let Ok(i) = id.parse::<usize>() {
                    switch_profile(s, i);
                }
            },
            None,
            p,
        ));
    }
    if dropdown::is_open(s, MANAGED_TEMPLATE) {
        // Match the panel width to the trigger (once it has a committed layout).
        let width = cx.layout_size(MANAGED_TEMPLATE).map(|wh| wh[0]);
        overlays.push(dropdown::panel(
            MANAGED_TEMPLATE,
            &preset_options(),
            &s.managed_template,
            width,
            |s: &mut State, id: &str| set_managed_template(s, id),
            None,
            p,
        ));
    }
    // The managed-library organize modal draws over everything (and any route) —
    // last, so it stacks above any dropdown panel still mounted beneath it.
    if let Some(modal) = organize_overlay(s, p) {
        overlays.push(modal);
    }
    overlays
}

#[cfg(test)]
mod tests {
    use super::{ExclusiveDisplay, exclusive_display};
    use soul_player_host::{ExclusiveStatus, SignalPath, SourceCodec, SourceFormat};

    fn sp(exclusive: bool) -> SignalPath {
        SignalPath {
            source: SourceFormat::Pcm { bits: 24 },
            codec: SourceCodec::Flac,
            decoded_rate: 96_000,
            decoded_bits: 24,
            channels: 2,
            replay_gain_db: 0.0,
            device_rate: 96_000,
            resampling: false,
            output_bits: 24,
            exclusive,
            loudness: None,
        }
    }

    #[test]
    fn exclusive_display_maps_status_to_the_right_message() {
        use ExclusiveDisplay::*;
        // Not requested → shared by choice (the status is irrelevant).
        assert_eq!(
            exclusive_display(false, None, ExclusiveStatus::Shared),
            Shared
        );
        // Active: the live signal path says exclusive → show the backend + device format.
        // The API name is platform-specific (CI runs on Linux/macOS too), so assert via
        // the same helper instead of hardcoding "WASAPI".
        assert_eq!(
            exclusive_display(true, Some(sp(true)), ExclusiveStatus::Active),
            Active(format!(
                "{} exclusive · 96 kHz · 24-bit",
                crate::views::signal_path::output_api()
            ))
        );
        // Requested, fell back to shared → surface the DEVICE'S reason verbatim.
        assert_eq!(
            exclusive_display(
                true,
                Some(sp(false)),
                ExclusiveStatus::FellBack("device is in use by another app".into()),
            ),
            FellBack("device is in use by another app".into())
        );
        // Requested + strict, couldn't open → stopped, with the reason.
        assert_eq!(
            exclusive_display(
                true,
                None,
                ExclusiveStatus::Unavailable("no exclusive format".into()),
            ),
            Stopped("no exclusive format".into())
        );
        // Requested, but no device has spawned yet → pending (applies next session).
        assert_eq!(
            exclusive_display(true, None, ExclusiveStatus::Shared),
            Pending
        );
    }

    /// The status badge must make "applied right now" visually distinct from "idle"
    /// and "not applied yet" — the whole point of the card. Active is its own kind
    /// (prominent); shared is neutral; everything that isn't actually bit-perfect
    /// (pending / fell back / stopped) reads as attention and surfaces its detail.
    #[test]
    fn exclusive_status_badge_distinguishes_applied_from_idle() {
        use super::{StatusKind, exclusive_status};
        use ExclusiveDisplay::*;

        let (txt, kind) = exclusive_status(&Active("96 kHz · 24-bit".to_string()));
        assert_eq!(
            kind,
            StatusKind::Active,
            "live bit-perfect is the Active kind"
        );
        assert!(
            txt.contains("96 kHz · 24-bit"),
            "names the live format: {txt}"
        );

        assert_eq!(
            exclusive_status(&Shared).1,
            StatusKind::Neutral,
            "shared/off is neutral, not a working state"
        );
        assert_eq!(
            exclusive_status(&Pending).1,
            StatusKind::Attention,
            "pending is NOT applied yet → attention, never Active"
        );

        let (fb, fk) = exclusive_status(&FellBack("device in use".to_string()));
        assert_eq!(fk, StatusKind::Attention);
        assert!(
            fb.contains("device in use"),
            "surfaces the fallback reason: {fb}"
        );
        assert_eq!(
            exclusive_status(&Stopped("no format".to_string())).1,
            StatusKind::Attention
        );

        // The three kinds are genuinely distinct, so applied never looks like idle.
        assert_ne!(StatusKind::Active, StatusKind::Neutral);
        assert_ne!(StatusKind::Active, StatusKind::Attention);
    }
}
