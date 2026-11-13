//! Settings page. Its one section, "Sources", manages the host's watched-folder
//! set (`soul_player_host::PlayerHandle::{add_folder, remove_folder,
//! watched_folders}`); edits refresh the library + list but never disturb
//! playback. Add uses the native folder dialog — Rune's IME is pre-release, so
//! there is no text entry here.
#![forbid(unsafe_code)]

use rune::{Align, Cursor, Cx, El, FileDialogOptions, ScrollSpec, Tx, col, el, row, text};
use soul_player_host::ReplayGainMode;

use crate::theme::Palette;
use crate::ui::{fx, radius, space, type_};
use crate::views::sidebar::{NAV_DEFS, nav_label};
use crate::views::slider::slider;
use crate::{
    State, ThemePref, apply_and_save, apply_watch, set_locale_pref, set_nav_visible,
    set_output_device, set_replaygain_mode, set_replaygain_preamp, set_theme_pref,
};

/// Preamp slider bounds, in dB (a conventional ReplayGain preamp range).
const RG_PREAMP_MIN: f32 = -15.0;
const RG_PREAMP_MAX: f32 = 15.0;

fn basename(p: &std::path::Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string_lossy().into_owned())
}

/// `+ Add Source` — native folder dialog, then `host.add_folder` folded in via
/// `apply_watch`. Mirrors `views::open_folder`'s dialog pattern.
fn add_source_button(p: &Palette) -> El {
    el().key("add-source")
        .padding(space::MD)
        .radius(radius::MD)
        .fill(p.primary)
        .center()
        .cursor(Cursor::Pointer)
        .hover(p.bg.alpha(fx::HOVER_FILL))
        .press(p.bg.alpha(fx::PRESS_FILL))
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
        .hover(p.fg.alpha(fx::HOVER))
        .press(p.fg.alpha(fx::PRESS))
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
        .padding(space::SM)
        .child(text(name.clone()).color(p.fg).grow(1.0));
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
            .hover(p.fg.alpha(fx::HOVER))
            .press(p.fg.alpha(fx::PRESS))
            .child(text("✕").color(p.text_secondary))
            .on_press(move |st: &mut State| {
                if let Ok(up) = st.host.remove_folder(&remove_for) {
                    apply_and_save(st, up);
                }
            }),
    )
}

/// The Sources section: + Add Source and one row per watched folder.
fn sources_section(s: &State, p: &Palette) -> El {
    let mut sources = col().key("sources").gap(space::XXS).child(
        row()
            .flex_wrap()
            .gap(space::SM)
            .padding(space::SM)
            .center_y()
            .child(
                text(t!("settings.sources").to_string())
                    .font(type_::HEADING)
                    .color(p.fg)
                    .grow(1.0),
            )
            .child(rescan_button(p))
            .child(add_source_button(p)),
    );
    if s.watched_folders.is_empty() {
        sources = sources.child(
            text(t!("settings.noSources").to_string())
                .color(p.text_secondary)
                .padding(space::SM),
        );
    } else {
        for i in 0..s.watched_folders.len() {
            sources = sources.child(source_row(s, p, i));
        }
    }
    sources
}

/// The Updates section: the running version, plus — when the launch-time check
/// (`update::available`) found a newer signed release — an "Install & Restart"
/// button. Install runs off the UI thread because `update::install` blocks on
/// the download and then relaunches the process.
fn updates_section(s: &State, p: &Palette) -> El {
    let mut sec = col()
        .key("updates")
        .gap(space::XXS)
        .child(
            row().padding(space::SM).child(
                text(t!("settings.updates").to_string())
                    .font(type_::HEADING)
                    .color(p.fg)
                    .grow(1.0),
            ),
        )
        .child(
            text(t!("settings.version", version => env!("CARGO_PKG_VERSION")).to_string())
                .color(p.text_secondary)
                .padding(space::SM),
        );
    if let Some(version) = &s.update_available {
        sec = sec.child(
            row()
                .gap(space::MD)
                .padding(space::SM)
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
                        .hover(p.bg.alpha(fx::HOVER_FILL))
                        .press(p.bg.alpha(fx::PRESS_FILL))
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
    sec
}

/// One theme choice; the active preference is filled with the primary color.
fn theme_button(label: String, key: &str, pref: ThemePref, active: bool, p: &Palette) -> El {
    el().key(key.to_string())
        .px(space::MD)
        .py(space::SM)
        .radius(radius::MD)
        .cursor(Cursor::Pointer)
        .fill(if active { p.primary } else { p.muted })
        .hover(if active {
            p.bg.alpha(fx::HOVER_FILL)
        } else {
            p.fg.alpha(fx::HOVER)
        })
        .press(if active {
            p.bg.alpha(fx::PRESS_FILL)
        } else {
            p.fg.alpha(fx::PRESS)
        })
        .child(
            text(label)
                .font(if active {
                    type_::ITEM_STRONG
                } else {
                    type_::ITEM
                })
                .color(if active { p.bg } else { p.fg }),
        )
        .on_press(move |s: &mut State| set_theme_pref(s, pref))
}

/// A show/hide toggle for one primary nav item: filled primary when shown,
/// muted when hidden. Clicking flips it (persisted). The current now-showing
/// route can be hidden too — the Settings gear is always reachable.
fn nav_toggle(id: &'static str, visible: bool, p: &Palette) -> El {
    el().key(format!("nav-toggle-{id}"))
        .role("button")
        .label(nav_label(id))
        .focusable(true)
        .px(space::MD)
        .py(space::SM)
        .radius(radius::MD)
        .cursor(Cursor::Pointer)
        .fill(if visible { p.primary } else { p.muted })
        .hover(if visible {
            p.bg.alpha(fx::HOVER_FILL)
        } else {
            p.fg.alpha(fx::HOVER)
        })
        .press(if visible {
            p.bg.alpha(fx::PRESS_FILL)
        } else {
            p.fg.alpha(fx::PRESS)
        })
        .child(
            text(if visible {
                t!("settings.navShown").to_string()
            } else {
                t!("settings.navHidden").to_string()
            })
            .font(if visible {
                type_::ITEM_STRONG
            } else {
                type_::ITEM
            })
            .color(if visible { p.bg } else { p.fg }),
        )
        .on_press(move |s: &mut State| set_nav_visible(s, id, !s.nav_visible(id)))
}

/// The Navigation section: one show/hide toggle per primary sidebar item, so
/// the user chooses which pages appear (the original's configurable nav).
fn navigation_section(s: &State, p: &Palette) -> El {
    let mut section = col().key("navigation").gap(space::XXS).child(
        row().padding(space::SM).child(
            text(t!("settings.navigation").to_string())
                .font(type_::HEADING)
                .color(p.fg)
                .grow(1.0),
        ),
    );
    for def in NAV_DEFS {
        section = section.child(
            row()
                .key(format!("nav-row-{}", def.id))
                .center_y()
                .padding(space::SM)
                .child(
                    text(nav_label(def.id))
                        .font(type_::ITEM)
                        .color(p.fg)
                        .grow(1.0),
                )
                .child(nav_toggle(def.id, s.nav_visible(def.id), p)),
        );
    }
    section
}

/// The Appearance section: a System / Light / Dark theme override.
fn appearance_section(s: &State, p: &Palette) -> El {
    col()
        .key("appearance")
        .gap(space::XXS)
        .child(
            row().padding(space::SM).child(
                text(t!("settings.appearance").to_string())
                    .font(type_::HEADING)
                    .color(p.fg)
                    .grow(1.0),
            ),
        )
        .child(
            row()
                .flex_wrap()
                .gap(space::SM)
                .padding(space::SM)
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

/// One language choice; the active pref is filled with the primary colour. Each
/// language shows in its own native name; "System" follows the OS locale.
fn lang_button(label: String, pref: &'static str, active: bool, p: &Palette) -> El {
    el().key(format!("lang-{pref}"))
        .px(space::MD)
        .py(space::SM)
        .radius(radius::MD)
        .cursor(Cursor::Pointer)
        .fill(if active { p.primary } else { p.muted })
        .hover(if active {
            p.bg.alpha(fx::HOVER_FILL)
        } else {
            p.fg.alpha(fx::HOVER)
        })
        .press(if active {
            p.bg.alpha(fx::PRESS_FILL)
        } else {
            p.fg.alpha(fx::PRESS)
        })
        .child(
            text(label)
                .font(if active {
                    type_::ITEM_STRONG
                } else {
                    type_::ITEM
                })
                .color(if active { p.bg } else { p.fg }),
        )
        .on_press(move |s: &mut State| set_locale_pref(s, pref))
}

/// The Language section: System (follow the OS) plus each shipped locale in its
/// native name.
fn language_section(s: &State, p: &Palette) -> El {
    let cur = s.locale_pref.as_str();
    col()
        .key("language")
        .gap(space::XXS)
        .child(
            row().padding(space::SM).child(
                text(t!("settings.language").to_string())
                    .font(type_::HEADING)
                    .color(p.fg)
                    .grow(1.0),
            ),
        )
        .child(
            row()
                .flex_wrap()
                .gap(space::SM)
                .padding(space::SM)
                .child(lang_button(
                    t!("settings.langSystem").to_string(),
                    "system",
                    cur == "system",
                    p,
                ))
                .child(lang_button("English".into(), "en", cur == "en", p))
                .child(lang_button("Deutsch".into(), "de", cur == "de", p))
                .child(lang_button("Español".into(), "es", cur == "es", p))
                .child(lang_button("中文".into(), "zh", cur == "zh", p))
                .child(lang_button("Čeština".into(), "cs", cur == "cs", p))
                .child(lang_button("日本語".into(), "ja", cur == "ja", p)),
        )
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
        .px(space::MD)
        .py(space::SM)
        .radius(radius::MD)
        .cursor(Cursor::Pointer)
        .fill(if active { p.primary } else { p.muted })
        .hover(if active {
            p.bg.alpha(fx::HOVER_FILL)
        } else {
            p.fg.alpha(fx::HOVER)
        })
        .press(if active {
            p.bg.alpha(fx::PRESS_FILL)
        } else {
            p.fg.alpha(fx::PRESS)
        })
        .child(
            text(label)
                .font(if active {
                    type_::ITEM_STRONG
                } else {
                    type_::ITEM
                })
                .color(if active { p.bg } else { p.fg })
                .grow(1.0),
        )
        .on_press(move |s: &mut State| set_output_device(s, choice.clone()))
}

/// The Output section: a "System default" choice plus one button per enumerated
/// output device, stacked vertically (device names can be long, and a host may
/// expose many). The active choice (matching `State.output_device`, `None` =
/// system default) is filled. Selecting a device applies to the NEXT playback
/// session — the host does not respawn the live stream.
fn output_section(s: &State, p: &Palette) -> El {
    let mut choices = col()
        .key("output-devices")
        .gap(space::SM)
        .padding(space::SM)
        .child(device_button(
            t!("settings.systemDefault").to_string(),
            "output-default",
            None,
            s.output_device.is_none(),
            p,
        ));
    if s.output_devices.is_empty() {
        choices = choices.child(
            text(t!("settings.noOutputDevices").to_string())
                .color(p.text_secondary)
                .padding(space::SM),
        );
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
    col()
        .key("output")
        .gap(space::XXS)
        .child(
            row().padding(space::SM).child(
                text(t!("settings.output").to_string())
                    .font(type_::HEADING)
                    .color(p.fg)
                    .grow(1.0),
            ),
        )
        .child(choices)
}

/// One ReplayGain mode choice; the active mode is filled with the primary color
/// (mirrors `theme_button` / `device_button`).
fn rg_mode_button(label: String, key: &str, mode: ReplayGainMode, active: bool, p: &Palette) -> El {
    el().key(key.to_string())
        .px(space::MD)
        .py(space::SM)
        .radius(radius::MD)
        .cursor(Cursor::Pointer)
        .fill(if active { p.primary } else { p.muted })
        .hover(if active {
            p.bg.alpha(fx::HOVER_FILL)
        } else {
            p.fg.alpha(fx::HOVER)
        })
        .press(if active {
            p.bg.alpha(fx::PRESS_FILL)
        } else {
            p.fg.alpha(fx::PRESS)
        })
        .child(
            text(label)
                .font(if active {
                    type_::ITEM_STRONG
                } else {
                    type_::ITEM
                })
                .color(if active { p.bg } else { p.fg }),
        )
        .on_press(move |s: &mut State| set_replaygain_mode(s, mode))
}

/// The ReplayGain section: an Off / Track / Album mode row plus a preamp slider
/// (−15..+15 dB). Selecting a mode or moving the preamp updates the live actor
/// immediately and persists the choice. The slider commits on release (its value
/// maps the 0..1 track position onto the dB range), and the current dB is shown
/// to its right.
fn replaygain_section(s: &State, p: &Palette, cx: &Cx<State>) -> El {
    let preamp = s.replaygain_preamp;
    let value01 = ((preamp - RG_PREAMP_MIN) / (RG_PREAMP_MAX - RG_PREAMP_MIN)).clamp(0.0, 1.0);
    let modes = row()
        .flex_wrap()
        .gap(space::SM)
        .padding(space::SM)
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
    let preamp_row = row()
        .key("rg-preamp-row")
        .gap(space::MD)
        .padding(space::SM)
        .center_y()
        .child(
            text(t!("settings.preamp").to_string())
                .color(p.text_secondary)
                .w(56.0),
        )
        .child(slider(
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
        ))
        .child(
            text(format!("{preamp:+.1} dB"))
                .color(p.text_secondary)
                .w(64.0)
                .align(Align::End),
        );
    col()
        .key("replaygain")
        .gap(space::XXS)
        .child(
            row().padding(space::SM).child(
                text(t!("settings.replayGain").to_string())
                    .font(type_::HEADING)
                    .color(p.fg)
                    .grow(1.0),
            ),
        )
        .child(modes)
        .child(preamp_row)
}

/// The Settings page: a title plus scrollable sections (Sources, Appearance,
/// Language, Output, ReplayGain, Updates).
pub fn settings_page(s: &State, p: &Palette, cx: &Cx<State>) -> El {
    let content = col()
        .key("settings-content")
        .gap(space::SECTION)
        .child(sources_section(s, p))
        .child(navigation_section(s, p))
        .child(appearance_section(s, p))
        .child(language_section(s, p))
        .child(output_section(s, p))
        .child(replaygain_section(s, p, cx))
        .child(updates_section(s, p));
    col()
        .key("settings")
        .grow(1.0)
        .min_h(0.0)
        .child(
            text(t!("settings.title").to_string())
                .font(type_::HEADING)
                .color(p.fg)
                .padding(space::LG),
        )
        .child(
            content
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .grow(1.0)
                .min_h(0.0),
        )
}
