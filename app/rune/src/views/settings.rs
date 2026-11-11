//! Settings page. Its one section, "Sources", manages the host's watched-folder
//! set (`soul_player_host::PlayerHandle::{add_folder, remove_folder,
//! watched_folders}`); edits refresh the library + list but never disturb
//! playback. Add uses the native folder dialog — Rune's IME is pre-release, so
//! there is no text entry here.
#![forbid(unsafe_code)]

use rune::{Cursor, El, FileDialogOptions, ScrollSpec, Tx, col, el, row, text};

use crate::theme::Palette;
use crate::ui::{radius, space, type_};
use crate::{State, ThemePref, apply_and_save, apply_watch, set_locale_pref, set_theme_pref};

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
            .padding(space::SM)
            .child(
                text(t!("settings.sources").to_string())
                    .font(type_::TITLE)
                    .color(p.primary)
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
                    .font(type_::TITLE)
                    .color(p.primary)
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
        .padding(space::SM)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .fill(if active { p.primary } else { p.muted })
        .child(text(label).color(if active { p.bg } else { p.fg }))
        .on_press(move |s: &mut State| set_theme_pref(s, pref))
}

/// The Appearance section: a System / Light / Dark theme override.
fn appearance_section(s: &State, p: &Palette) -> El {
    col()
        .key("appearance")
        .gap(space::XXS)
        .child(
            row().padding(space::SM).child(
                text(t!("settings.appearance").to_string())
                    .font(type_::TITLE)
                    .color(p.primary)
                    .grow(1.0),
            ),
        )
        .child(
            row()
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
        .padding(space::SM)
        .radius(radius::SM)
        .cursor(Cursor::Pointer)
        .fill(if active { p.primary } else { p.muted })
        .child(text(label).color(if active { p.bg } else { p.fg }))
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
                    .font(type_::TITLE)
                    .color(p.primary)
                    .grow(1.0),
            ),
        )
        .child(
            row()
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

/// The Settings page: a title plus scrollable sections (Sources, Appearance).
pub fn settings_page(s: &State, p: &Palette) -> El {
    let content = col()
        .key("settings-content")
        .gap(space::LG)
        .child(sources_section(s, p))
        .child(appearance_section(s, p))
        .child(language_section(s, p))
        .child(updates_section(s, p));
    col()
        .key("settings")
        .grow(1.0)
        .min_h(0.0)
        .child(
            text(t!("settings.title").to_string())
                .font(type_::TITLE)
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
