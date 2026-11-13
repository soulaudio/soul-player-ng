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
    Cursor, Cx, El, FileDialogOptions, Icon, PopupMenu, ScrollSpec, Tx, col, el, icon, row, text,
};
use soul_player_host::ReplayGainMode;

use crate::theme::Palette;
use crate::ui::{radius, space, type_};
use crate::views::icons;
use crate::views::slider::slider;
use crate::{
    SettingsTab, State, ThemePref, apply_and_save, apply_watch, set_locale_pref, set_output_device,
    set_replaygain_mode, set_replaygain_preamp, set_settings_tab, set_theme_pref,
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

/// `+ Add Source` — native folder dialog, then `host.add_folder` folded in via
/// `apply_watch`. Mirrors `views::open_folder`'s dialog pattern.
fn add_source_button(p: &Palette) -> El {
    el().key("add-source")
        .padding(space::MD)
        .radius(radius::MD)
        .fill(p.primary)
        .center()
        .cursor(Cursor::Pointer)
        .child(text(t!("settings.addSource").to_string()).color(p.bg))
        .on_press_tx(|_s: &mut State, tx: &mut Tx| {
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
        })
}

/// Re-aggregate the current watched set — picks up files added/removed on disk
/// and roots that came back online. The set is unchanged, so no save (apply_watch,
/// not apply_and_save).
fn rescan_button(p: &Palette) -> El {
    el().key("rescan-sources")
        .padding(space::MD)
        .radius(radius::MD)
        .fill(p.muted)
        .center()
        .cursor(Cursor::Pointer)
        .child(text(t!("settings.rescan").to_string()).color(p.fg))
        .on_press(|s: &mut State| {
            if let Ok(up) = s.host.set_folders(&s.watched_folders) {
                apply_watch(s, up);
            }
        })
}

/// One watched folder: name, an "offline" badge if currently unavailable, and a
/// remove (✕) that drops it from the set and re-aggregates.
fn source_row(s: &State, p: &Palette, index: usize) -> El {
    let folder = s.watched_folders[index].clone();
    let name = basename(&folder);
    let unavailable = s.unavailable.iter().any(|u| u == &folder);
    let remove_for = folder.clone();
    let mut r = row()
        .key(format!("source-{name}"))
        .gap(space::MD)
        .center_y()
        .padding(space::SM)
        // `truncate` (ellipsis + shrink-to-0) so a long folder name can't shove
        // the offline badge and remove ✕ off the card.
        .child(text(name.clone()).color(p.fg).grow(1.0).truncate());
    if unavailable {
        r = r.child(
            el().key(format!("offline-{name}"))
                .child(text(t!("settings.offline").to_string()).color(p.text_secondary)),
        );
    }
    r.child(
        el().key(format!("remove-{name}"))
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
        .padding(space::SM)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .fill(if active { p.primary } else { p.muted })
        .child(text(label).color(if active { p.bg } else { p.fg }))
        .on_press(move |s: &mut State| set_theme_pref(s, pref))
}

/// The language choices as `(pref id, display title)`: "System" follows the OS
/// locale (its label localized), each shipped locale in its own native name. The
/// single source for both the dropdown's current-value label and its menu — keep
/// the ids in step with `crate::LOCALES`.
fn language_options() -> Vec<(&'static str, String)> {
    vec![
        ("system", t!("settings.langSystem").to_string()),
        ("en", "English".into()),
        ("de", "Deutsch".into()),
        ("es", "Español".into()),
        ("zh", "中文".into()),
        ("cs", "Čeština".into()),
        ("ja", "日本語".into()),
    ]
}

/// The language picker as a dropdown (the original's `Select`): a full-width
/// button showing the current language and a chevron that, on press, opens a
/// native picker menu with the current value checked. Scales to any number of
/// locales without the button row wrapping — and can't overflow in a longer
/// locale since only one value shows at a time.
fn language_dropdown(s: &State, p: &Palette) -> El {
    let options = language_options();
    let current = options
        .iter()
        .find(|(id, _)| *id == s.locale_pref)
        .map(|(_, title)| title.clone())
        .unwrap_or_else(|| t!("settings.langSystem").to_string());
    row()
        .key("lang-picker")
        .role("button")
        .label(t!("settings.language").to_string())
        .focusable(true)
        .w_full()
        .center_y()
        .gap(space::SM)
        .px(space::MD)
        .py(space::SM)
        .radius(radius::SM)
        .fill(p.muted)
        .cursor(Cursor::Pointer)
        .hover(p.fg.alpha(0.06))
        .child(text(current).color(p.fg).grow(1.0).truncate())
        .child(icon(&icons::CHEVRON_DOWN, 16.0).color(p.text_secondary))
        .on_press_tx(move |s: &mut State, tx: &mut Tx| {
            let menu = PopupMenu::picker(options.clone(), &s.locale_pref);
            tx.popup_menu(menu, "lang-picker", |s: &mut State, picked, _tx| {
                if let Some(id) = picked {
                    set_locale_pref(s, id);
                }
            });
        })
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
        .padding(space::SM)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .fill(if active { p.primary } else { p.muted })
        .child(text(label).color(if active { p.bg } else { p.fg }))
        .on_press(move |s: &mut State| set_replaygain_mode(s, mode))
}

// ── Cards (one per settings group) ───────────────────────────────────────────

/// Appearance: a System / Light / Dark theme override.
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
            .child(add_source_button(p)),
    );
    if s.watched_folders.is_empty() {
        c = c.child(text(t!("settings.noSources").to_string()).color(p.text_secondary));
    } else {
        for i in 0..s.watched_folders.len() {
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
            cx,
            false,
            p,
            |st: &mut State, v: f32| {
                // Map the 0..1 slider position back onto the dB range, rounded to
                // 0.1 dB so the persisted value stays clean.
                let db = RG_PREAMP_MIN + v * (RG_PREAMP_MAX - RG_PREAMP_MIN);
                set_replaygain_preamp(st, (db * 10.0).round() / 10.0);
            },
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
