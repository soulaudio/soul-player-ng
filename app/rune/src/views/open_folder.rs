//! Shared "Open Folder" control — the Rune head's answer to the Tauri head's
//! `open_folder` command. Pops Rune's native folder picker (`Tx::open_folder`,
//! behind the `native-file-dialog` feature), scans the chosen folder into the
//! actor, and swaps the new library into `State` so the view re-renders with it.
//! Used by the sidebar (always available) and the empty-library prompt.
#![forbid(unsafe_code)]

use rune::{El, FileDialogOptions, Tx, el, text};

use crate::State;
use crate::theme::Palette;

/// A primary-filled button that opens the native folder dialog. The dialog runs
/// asynchronously; when the user picks a folder, `on_answer` runs on the UI
/// thread with `&mut State`, loads it, and refreshes `library`.
pub fn open_folder_button(label: &str, p: &Palette) -> El {
    el().key("open-folder")
        .padding(12.0)
        .radius(8.0)
        .fill(p.primary)
        .center()
        .child(text(label).color(p.bg))
        .on_press_tx(|_s: &mut State, tx: &mut Tx| {
            tx.open_folder(
                FileDialogOptions {
                    title: "Open music folder".into(),
                    ..Default::default()
                },
                |s: &mut State, result, _tx| {
                    if let Ok(Some(path)) = result
                        && s.host.open_folder(&path).is_ok()
                    {
                        s.library = s.host.views();
                    }
                },
            );
        })
}
