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
    Align, Cursor, Cx, El, FileDialogOptions, Icon, ScrollSpec, Side, Tx, col, el, icon, row, text,
};
use soul_player_host::ReplayGainMode;

use crate::theme::Palette;
use crate::ui::{radius, space, type_};
use crate::views::dropdown;
use crate::views::icons;
use crate::views::sidebar::{NAV_DEFS, nav_label};
use crate::views::slider::slider;
use crate::{
    SettingsTab, State, ThemePref, apply_and_save, apply_watch, close_dropdown, set_locale_pref,
    set_nav_visible, set_output_device, set_replaygain_mode, set_replaygain_preamp,
    set_settings_tab, set_theme_pref,
};

/// Preamp slider bounds, in dB (a conventional ReplayGain preamp range).
const RG_PREAMP_MIN: f32 = -15.0;
const RG_PREAMP_MAX: f32 = 15.0;

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
        .padding(space::MD)
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
fn add_source_menu(_s: &State, p: &Palette) -> El {
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
        .key(format!("enable-{name}"))
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
        .key(format!("source-{name}"))
        .gap(space::MD)
        .center_y()
        .padding(space::SM)
        .child(toggle)
        .child(identity)
        .child(
            // Keyed so tests can assert the count renders without depending on the
            // (localized) label text.
            el().key(format!("count-{name}")).child(
                text(t!("settings.trackCount", count => status.track_count).to_string())
                    .font(type_::CAPTION)
                    .color(p.text_secondary),
            ),
        );
    if unavailable {
        r = r.child(
            el().key(format!("offline-{name}"))
                .child(text(t!("settings.offline").to_string()).color(p.text_secondary)),
        );
    }
    r.child(
        el().key(format!("remove-{name}"))
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
    if s.sources.is_empty() {
        c = c.child(text(t!("settings.noSources").to_string()).color(p.text_secondary));
    } else {
        for i in 0..s.sources.len() {
            c = c.child(source_row(s, p, i));
        }
    }
    c
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
    card("output", p)
        .child(heading(
            t!("settings.output").to_string(),
            t!("settings.outputDesc").to_string(),
            p,
        ))
        .child(choices)
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
    card("replaygain", p)
        .child(heading(
            t!("settings.replayGain").to_string(),
            t!("settings.replayGainDesc").to_string(),
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

/// The Settings page: the category rail on the left, the selected category's
/// cards centred and width-capped on the right (mirroring the original's
/// `max-w-4xl mx-auto`), scrollable.
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
            .child(music_data_card(s, p)),
        SettingsTab::Audio => col()
            .key("settings-content")
            .gap(space::LG)
            .child(output_card(s, p))
            .child(replaygain_card(s, p, cx)),
        SettingsTab::About => col()
            .key("settings-content")
            .gap(space::LG)
            .child(about_card(s, p)),
    };
    row()
        .key("settings")
        .grow(1.0)
        .min_h(0.0)
        .child(category_nav(s, p))
        .child(
            row()
                .key("settings-body")
                .grow(1.0)
                .min_h(0.0)
                .padding(space::XL)
                // `grow` lets the column claim the body width so every tab's cards
                // fill one consistent width and share a left edge (this engine splits
                // free space evenly across grow children, so flanking spacers would
                // only ever leave the column a third-width and drifting); `max_w`
                // caps it on wide windows — the original's `max-w-4xl` column.
                .child(
                    content
                        .grow(1.0)
                        .max_w(CONTENT_MAX_W)
                        .min_h(0.0)
                        .scroll(ScrollSpec {
                            stick_bottom: false,
                        }),
                ),
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
    overlays
}
