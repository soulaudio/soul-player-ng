//! Settings page. Its one section, "Sources", manages the host's watched-folder
//! set (`soul_player_host::PlayerHandle::{add_folder, remove_folder,
//! watched_folders}`); edits refresh the library + list but never disturb
//! playback. Add uses the native folder dialog — Rune's IME is pre-release, so
//! there is no text entry here.
#![forbid(unsafe_code)]

use rune::tok::type_;
use rune::{El, FileDialogOptions, ScrollSpec, Tx, col, el, row, text};

use crate::theme::Palette;
use crate::{State, ThemePref, apply_and_save, apply_watch, set_theme_pref};

fn basename(p: &std::path::Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string_lossy().into_owned())
}

/// `+ Add Source` — native folder dialog, then `host.add_folder` folded in via
/// `apply_watch`. Mirrors `views::open_folder`'s dialog pattern.
fn add_source_button(p: &Palette) -> El {
    el().key("add-source")
        .padding(12.0)
        .radius(8.0)
        .fill(p.primary)
        .center()
        .child(text("+ Add Source").color(p.bg))
        .on_press_tx(|_s: &mut State, tx: &mut Tx| {
            tx.open_folder(
                FileDialogOptions {
                    title: "Add a music folder".into(),
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
        .padding(12.0)
        .radius(8.0)
        .fill(p.muted)
        .center()
        .child(text("Rescan").color(p.fg))
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
        .gap(12.0)
        .padding(8.0)
        .child(text(name.clone()).color(p.fg).grow(1.0));
    if unavailable {
        r = r.child(
            el().key(format!("offline-{name}"))
                .child(text("offline").color(p.text_secondary)),
        );
    }
    r.child(
        el().key(format!("remove-{name}"))
            .padding(6.0)
            .radius(6.0)
            .fill(p.muted)
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
    let mut sources = col().key("sources").gap(2.0).child(
        row()
            .padding(8.0)
            .child(
                text("Sources")
                    .font(type_::TITLE)
                    .color(p.primary)
                    .grow(1.0),
            )
            .child(rescan_button(p))
            .child(add_source_button(p)),
    );
    if s.watched_folders.is_empty() {
        sources = sources.child(
            text("No sources yet — add a folder to build your library.")
                .color(p.text_secondary)
                .padding(8.0),
        );
    } else {
        for i in 0..s.watched_folders.len() {
            sources = sources.child(source_row(s, p, i));
        }
    }
    sources
}

/// One theme choice; the active preference is filled with the primary color.
fn theme_button(label: &str, key: &str, pref: ThemePref, active: bool, p: &Palette) -> El {
    el().key(key.to_string())
        .padding(8.0)
        .radius(6.0)
        .fill(if active { p.primary } else { p.muted })
        .child(text(label).color(if active { p.bg } else { p.fg }))
        .on_press(move |s: &mut State| set_theme_pref(s, pref))
}

/// The Appearance section: a System / Light / Dark theme override.
fn appearance_section(s: &State, p: &Palette) -> El {
    col()
        .key("appearance")
        .gap(2.0)
        .child(
            row().padding(8.0).child(
                text("Appearance")
                    .font(type_::TITLE)
                    .color(p.primary)
                    .grow(1.0),
            ),
        )
        .child(
            row()
                .gap(8.0)
                .padding(8.0)
                .child(theme_button(
                    "System",
                    "theme-system",
                    ThemePref::System,
                    s.theme_pref == ThemePref::System,
                    p,
                ))
                .child(theme_button(
                    "Light",
                    "theme-light",
                    ThemePref::Light,
                    s.theme_pref == ThemePref::Light,
                    p,
                ))
                .child(theme_button(
                    "Dark",
                    "theme-dark",
                    ThemePref::Dark,
                    s.theme_pref == ThemePref::Dark,
                    p,
                )),
        )
}

/// The Settings page: a title plus scrollable sections (Sources, Appearance).
pub fn settings_page(s: &State, p: &Palette) -> El {
    let content = col()
        .key("settings-content")
        .gap(16.0)
        .child(sources_section(s, p))
        .child(appearance_section(s, p));
    col()
        .key("settings")
        .grow(1.0)
        .min_h(0.0)
        .child(
            text("Settings")
                .font(type_::TITLE)
                .color(p.fg)
                .padding(16.0),
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
