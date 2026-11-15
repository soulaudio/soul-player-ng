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
    Align, Color, Cursor, Cx, Drag, El, FileDialogOptions, Icon, Live, Phase, ScrollSpec, Side, Tx,
    WheelEvent, col, el, icon, row, text, text_input,
};
use soul_player_host::organize;
use soul_player_host::{BandKind, GAIN_LIMIT_DB, ReplayGainMode};

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
    apply_and_save, apply_watch, close_dropdown, commit_eq_edit, cycle_eq_band_kind, eq_edit_start,
    eq_num_drag_end, eq_num_drag_move, eq_num_drag_start, eq_num_scroll, export_eq, import_eq,
    organize_begin, organize_confirm, organize_dismiss, organize_ready, remove_dsp_unit,
    remove_eq_band, reorder_dsp_units, set_dsp_unit_enabled, set_eq_band_enabled, set_eq_preamp,
    set_indexing_mode, set_locale_pref, set_managed_dest, set_managed_strategy,
    set_managed_template, set_nav_visible, set_output_device, set_replaygain_mode,
    set_replaygain_preamp, set_settings_tab, set_theme_pref, toggle_dsp_expanded, unit_bands,
    unit_enabled, unit_preamp,
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

/// `+ Add Source` — a compact primary button (the original add-button metrics,
/// plus a chevron that marks it as a menu) that toggles the [`add_source_menu`]
/// (Local folder, and Remote server as a "coming soon" item) open, instead of
/// jumping straight to the folder dialog. A select-style full-width trigger reads
/// wrong for an action button, so this is a bespoke compact trigger rather than
/// `dropdown::trigger`; it still anchors the shared menu via its `ADD_SOURCE_MENU`
/// key. The visible text is a child, so the control carries an explicit a11y label.
fn add_source_trigger(p: &Palette) -> El {
    row()
        .key(ADD_SOURCE_MENU)
        .role("button")
        .label(t!("settings.addSource").to_string())
        .focusable(true)
        .center_y()
        .gap(space::XS)
        .padding(space::MD)
        .radius(radius::MD)
        .fill(p.primary)
        .cursor(Cursor::Pointer)
        .child(text(t!("settings.addSource").to_string()).color(p.bg))
        .child(icon(&icons::CHEVRON_DOWN, 16.0).color(p.bg))
        .on_press(move |s: &mut State| crate::toggle_dropdown(s, ADD_SOURCE_MENU))
}

/// The add-source menu's floating panel — **mount at the window root** via
/// [`settings_overlays`], exactly like [`dropdown::panel`], so it escapes the
/// settings scroll clip. It mirrors `panel`'s styling (anchored under the trigger,
/// focus-trapped, card surface + border + radius, modal dismiss) but carries two
/// bespoke rows rather than a uniform choice list:
/// - **Local folder…** — focusable; closes the menu, then opens the native folder
///   dialog and folds the pick in (the flow the old button used).
/// - **Remote server…** — a disabled "coming soon" item: not focusable, greyed,
///   with a trailing chip; no handler.
fn add_source_menu(s: &State, p: &Palette) -> El {
    let coming_soon = row()
        .center_y()
        .px(space::SNUG)
        .py(space::XXS)
        .radius(radius::XS)
        .fill(p.muted)
        .child(
            text(t!("settings.comingSoon").to_string())
                .font(type_::LABEL)
                .color(p.text_secondary),
        );
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
        .child(
            text(t!("settings.addLocalFolder").to_string())
                .color(p.fg)
                .grow(1.0)
                .truncate(),
        )
        .on_press_tx(|s: &mut State, tx: &mut Tx| {
            // Close the menu before the dialog so it isn't left open behind it.
            close_dropdown(s);
            tx.open_folder(
                FileDialogOptions {
                    title: t!("settings.addFolderDialog").to_string(),
                    ..Default::default()
                },
                |s: &mut State, result, _tx| {
                    if let Ok(Some(path)) = result
                        && let Ok(up) = s.host.add_folder(&path)
                    {
                        apply_and_save(s, up);
                    }
                },
            );
        });
    let remote = row()
        .key("add-source-opt-remote")
        .role("option")
        // Not focusable: it's a "coming soon" placeholder, so it can't be activated.
        .w_full()
        .center_y()
        .gap(space::SM)
        .px(space::MD)
        .py(space::SM)
        .radius(radius::SM)
        .child(icon(&icons::SERVER, 16.0).color(p.text_secondary))
        .child(
            text(t!("settings.addRemoteServer").to_string())
                .color(p.text_secondary)
                .grow(1.0)
                .truncate(),
        )
        .child(coming_soon);
    col()
        .key(format!("{ADD_SOURCE_MENU}-menu"))
        .anchored_fit(ADD_SOURCE_MENU, Side::Bottom, Align::Start, space::XS)
        .focus_group()
        .focus_return(ADD_SOURCE_MENU.to_string())
        .min_w(MENU_MIN_W)
        .fill(p.card)
        .radius(radius::MD)
        .border_sides([1.0; 4], p.border)
        .padding(space::XXS)
        .gap(space::XXS)
        .min_h(space::NONE)
        .modal(true, close_dropdown)
        .child(local)
        .child(remote)
        .child(add_music_row(s, p))
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
            .child(
                text(t!("settings.addMusic").to_string())
                    .color(p.fg)
                    .grow(1.0)
                    .truncate(),
            )
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
        // No destination yet: a disabled hint, mirroring the Remote "coming soon"
        // row — present but not focusable, with a trailing chip.
        let hint = row()
            .center_y()
            .px(space::SNUG)
            .py(space::XXS)
            .radius(radius::XS)
            .fill(p.muted)
            .child(
                text(t!("settings.setDestinationFirst").to_string())
                    .font(type_::LABEL)
                    .color(p.text_secondary),
            );
        base.child(icon(&icons::MUSIC, 16.0).color(p.text_secondary))
            .child(
                text(t!("settings.addMusic").to_string())
                    .color(p.text_secondary)
                    .grow(1.0)
                    .truncate(),
            )
            .child(hint)
    }
}

/// Minimum width of the add-source menu panel, so the two rows have room — the
/// compact trigger is narrower than the menu, so it doesn't constrain it.
const MENU_MIN_W: f32 = 220.0;

/// Re-aggregate the current watched set — picks up files added/removed on disk
/// and roots that came back online. The set is unchanged, so no save (apply_watch,
/// not apply_and_save).
fn rescan_button(p: &Palette) -> El {
    el().key("rescan-sources")
        .role("button")
        .label(t!("settings.rescan").to_string())
        .focusable(true)
        .padding(space::MD)
        .radius(radius::MD)
        .fill(p.muted)
        .center()
        .cursor(Cursor::Pointer)
        .child(text(t!("settings.rescan").to_string()).color(p.fg))
        .on_press(|s: &mut State| {
            // Reconcile the current configured set (set_folders preserves each
            // source's enabled flag), re-aggregating files/roots that changed on
            // disk. The set itself is unchanged, so no save (apply_watch).
            let roots: Vec<std::path::PathBuf> = s.sources.iter().map(|x| x.path.clone()).collect();
            if let Ok(up) = s.host.set_folders(&roots) {
                apply_watch(s, up);
            }
        })
}

/// Side of the per-source enable/disable toggle box — a small square checkbox
/// sized to seat the check glyph, consistent with the app's other inline icons.
const TOGGLE_PX: f32 = 20.0;
/// The check glyph inside the enable toggle.
const TOGGLE_ICON_PX: f32 = 14.0;

/// One watched source, richly: a leading enable/disable toggle, the source name
/// over its full path, a per-source track count, an "offline" badge if currently
/// unavailable, and a remove (✕) that drops it from the set and re-aggregates.
fn source_row(s: &State, p: &Palette, index: usize) -> El {
    let status = &s.sources[index];
    let folder = status.path.clone();
    let name = basename(&folder);
    let full_path = folder.to_string_lossy().into_owned();
    let enabled = status.enabled;
    let unavailable = !status.available;
    let toggle_for = folder.clone();
    let remove_for = folder.clone();

    // Leading enable/disable toggle: a small square, filled `primary` (with a
    // check) when enabled, `muted` when disabled. Its accessible label states the
    // action it performs (enable when off, disable when on).
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
        .on_press(move |st: &mut State| {
            if let Ok(up) = st.host.set_folder_enabled(&toggle_for, !enabled) {
                apply_and_save(st, up);
            }
        });
    if enabled {
        toggle = toggle.child(icon(&icons::CHECK, TOGGLE_ICON_PX).color(p.bg));
    }

    // Name over full path. A disabled source reads inactive, so its name dims to
    // the secondary colour. Both lines truncate and the column grows + shrinks to
    // zero so a long path can't shove the count / badge / ✕ off the card.
    let name_color = if enabled { p.fg } else { p.text_secondary };
    let identity = col()
        .grow(1.0)
        .min_w(space::NONE)
        .child(
            text(name.clone())
                .font(type_::ITEM)
                .color(name_color)
                .truncate(),
        )
        .child(
            text(full_path)
                .font(type_::CAPTION)
                .color(p.text_secondary)
                .truncate(),
        );

    let mut r = row()
        // Key by POSITION, not basename: two sources can share a final path
        // component (D:\Music and E:\Music), and duplicate sibling keys are a
        // Rune hazard. `index` is unique within the card.
        .key(format!("source-{index}"))
        .gap(space::MD)
        .center_y()
        .padding(space::SM)
        .child(toggle)
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
    if unavailable {
        r = r.child(
            el().key(format!("offline-{index}"))
                .child(text(t!("settings.offline").to_string()).color(p.text_secondary)),
        );
    }
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
                if let Ok(up) = st.host.remove_folder(&remove_for) {
                    apply_and_save(st, up);
                }
            }),
    )
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
            .gap(space::MD)
            // Wrap so the Rescan / Add Source buttons drop to their own line
            // rather than off the card when a longer locale's heading needs the
            // width; `shrink` lets the (wrapping) heading yield that width first.
            .flex_wrap()
            .child(
                heading(
                    t!("settings.musicData").to_string(),
                    t!("settings.musicDataDesc").to_string(),
                    p,
                )
                .grow(1.0)
                .shrink(),
            )
            .child(rescan_button(p))
            .child(add_source_trigger(p)),
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
    if s.sources.is_empty() {
        c = c.child(text(t!("settings.noSources").to_string()).color(p.text_secondary));
    } else {
        for i in 0..s.sources.len() {
            c = c.child(source_row(s, p, i));
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
    col()
        .w_full()
        .align(Align::Center)
        .child(icon(&icons::CHEVRON_DOWN, 20.0).color(p.primary.alpha(0.8)))
}

/// A "soon" chip for a not-yet-built stage.
fn soon_chip(p: &Palette) -> El {
    text(t!("player.signalPath.soon").to_string())
        .font(type_::LABEL)
        .color(p.text_secondary)
        .px(space::SM)
        .py(space::XXS)
        .radius(radius::SM)
        .fill(p.muted)
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

/// A not-yet-built stage card (DSP/EQ, Exclusive output): a dim header + "soon".
fn soon_card(key: &str, glyph: &'static Icon, title: String, desc: String, p: &Palette) -> El {
    card(key, p).child(stage_header(
        p.border,
        glyph,
        title,
        Some(desc),
        Some(soon_chip(p)),
        true,
        p,
    ))
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
    col()
        .key("settings-content")
        .gap(space::SM)
        .w_full()
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
        .child(soon_card(
            "sp-stage-exclusive",
            &icons::SETTINGS,
            t!("player.signalPath.exclusive").to_string(),
            t!("settings.comingSoon").to_string(),
            p,
        ))
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
    // where a dragged card will land (the queue's reorder idiom).
    let target = s.dsp_drag.map(|(_, to)| to);
    let dragging_from = s.dsp_drag.map(|(from, _)| from);
    for i in 0..s.dsp_units.len() {
        if target == Some(i) && dragging_from != Some(i) {
            card = card.child(
                el().key("dsp-drop")
                    .w_full()
                    .h(2.0)
                    .radius(1.0)
                    .fill(p.accent),
            );
        }
        card = card.child(dsp_card(i, s, p, cx));
    }
    card.child(add_dsp_control(p))
}

/// One **DSP unit card** (keyed by POSITION, never a unit id): an always-visible
/// header — expand chevron · type label · reorder grip · enable · remove — and,
/// when expanded, the unit's body (preamp + response graph + band table). The body
/// dims when the unit is disabled.
fn dsp_card(i: usize, s: &State, p: &Palette, cx: &Cx<State>) -> El {
    let enabled = unit_enabled(s, i);
    let expanded = s.dsp_expanded.get(i).copied().unwrap_or(false);
    let dragged = s.dsp_drag.map(|(from, _)| from) == Some(i);
    // Expand/collapse chevron (▸ collapsed, ▾ open).
    let chevron = text(if expanded { "▾" } else { "▸" }.to_string())
        .font(type_::LABEL)
        .color(p.text_secondary)
        .px(space::XXS)
        .key(format!("dsp-expand-{i}"))
        .role("button")
        .focusable(true)
        .cursor(Cursor::Pointer)
        .on_press(move |st: &mut State| toggle_dsp_expanded(st, i));
    // The unit type label (EQ is the only kind today).
    let label = text("EQ".to_string())
        .font(type_::ITEM_STRONG)
        .color(if enabled { p.fg } else { p.text_secondary });
    // Drag-reorder grip — same dy→slot mapping as the queue (via `dsp_drop_index`).
    let grip = el()
        .key(format!("dsp-grip-{i}"))
        .role("button")
        .size(space::MD, DSP_ROW_H)
        .center()
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .child(icon(&icons::GRIP, 12.0).color(p.text_secondary.alpha(0.6)))
        .on_drag(
            move |st: &mut State, d: &mut rune::Drag, _tx| match d.phase {
                rune::Phase::Down => st.dsp_drag = Some((i, i)),
                rune::Phase::Move => {
                    if let Some((from, _)) = st.dsp_drag {
                        let to = dsp_drop_index(from, d.dy, st.dsp_units.len());
                        st.dsp_drag = Some((from, to));
                    }
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
        .child(chevron)
        .child(label)
        .child(grip)
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

/// Approximate height of one unit row (points) — maps a reorder drag's vertical
/// travel to a target slot, à la the queue.
const DSP_ROW_H: f32 = 34.0;

/// Where a dragged unit lands: its index plus one slot per [`DSP_ROW_H`] of travel.
fn dsp_drop_index(from: usize, dy: f32, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let steps = (dy / DSP_ROW_H).round() as i64;
    (from as i64 + steps).clamp(0, len as i64 - 1) as usize
}

/// The key identifying the add-DSP menu (its open state + panel anchor).
const ADD_DSP_MENU: &str = "dsp-add";

/// The DSP-type choices the add menu offers. One entry (EQ) today — built as a
/// dropdown so more unit types slot in later without reworking the control.
fn dsp_type_options() -> Vec<(String, String)> {
    vec![("eq".to_string(), "EQ".to_string())]
}

/// The "+ Add DSP" control: a compact primary button that opens the shared
/// [`dropdown`] of DSP types ([`add_dsp_menu`], mounted at the window root). The
/// single entry still routes through the dropdown so new types extend it cleanly.
fn add_dsp_control(p: &Palette) -> El {
    chip_btn(
        ADD_DSP_MENU.into(),
        "+ Add DSP".into(),
        p.primary.alpha(0.16),
        p.primary,
        move |st| crate::toggle_dropdown(st, ADD_DSP_MENU),
    )
}

/// The add-DSP dropdown panel — **mount at the window root** via
/// [`settings_overlays`]. Picking a type appends that unit (EQ today) and opens
/// its card. Nothing is pre-selected (it's an action menu, not a value picker).
fn add_dsp_menu(p: &Palette) -> El {
    dropdown::panel(
        ADD_DSP_MENU,
        &dsp_type_options(),
        "",
        None,
        |st: &mut State, _id: &str| add_dsp_unit(st),
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
    let count = row()
        .center_y()
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
    let mut list = col().w_full().gap(space::XS);
    for i in 0..n {
        list = list.child(eq_band_row(unit, i, s, p));
    }
    col()
        .key(format!("eq-editor-{unit}"))
        .w_full()
        .gap(space::SM)
        .child(header)
        .child(list)
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

/// About: the running version and, when the launch-time check (`update::available`)
/// found a newer signed release, an "Install & Restart" button. Install runs off
/// the UI thread because `update::install` blocks on the download and relaunches.
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
        );
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
                                    eprintln!("[updater] install failed: {e}");
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
    let inner = content.w_full().px(space::XL).pt(space::LG).pb(space::XXL);
    // One Live carries the thumb geometry from the viewport's scroll callback to
    // the thumb element in the same frame.
    let thumb = SETTINGS_SCROLL_THUMB.with(|t| t.clone());
    row()
        .key("settings")
        .grow(1.0)
        .min_h(0.0)
        .child(category_nav(s, p))
        .child(
            // `grow` lets the column claim the body width so every tab's cards
            // fill one consistent width and share a left edge (this engine splits
            // free space evenly across grow children, so flanking spacers would
            // only ever leave the column a third-width and drifting); `max_w`
            // caps it on wide windows — the original's `max-w-4xl` column.
            row()
                .key("settings-body")
                .grow(1.0)
                .min_h(0.0)
                .child(scrollbar::attach(
                    col()
                        .key("settings-scroll")
                        .grow(1.0)
                        .max_w(CONTENT_MAX_W)
                        .min_h(0.0)
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
            p,
        ));
    }
    if dropdown::is_open(s, ADD_SOURCE_MENU) {
        overlays.push(add_source_menu(s, p));
    }
    if dropdown::is_open(s, ADD_DSP_MENU) {
        overlays.push(add_dsp_menu(p));
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
