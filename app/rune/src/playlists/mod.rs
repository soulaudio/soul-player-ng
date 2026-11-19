//! User-created playlists: an in-memory data model (`model`), persisted to an
//! on-disk per-folder store (`store`), and pure cover-mosaic selection
//! (`cover`); Task 7 wires `PlaylistStore`/`PlaylistId` into `State` and the
//! routes, but the real list/detail page bodies (and the mutators they'll
//! call: create/rename/delete/reorder, plus the cover selector) land in
//! Tasks 8-9.
#![forbid(unsafe_code)]
// `cover`/`model`/`store` each have their own tests exercising this fully, so
// what's flagged below is not dead code, just not yet reached from non-test
// `main.rs`/`views` code. This can't be narrowed further than "the whole
// submodule tree" without editing those files directly (out of scope for
// Task 7's "allow trim"; see its report for exactly what's still unreached).
#![allow(dead_code)]

pub mod cover;
pub mod model;
pub mod store;

// Only `PlaylistId` (State::open_playlist/navigate_to) and `PlaylistStore`
// (State::playlists) are reached from non-test code as of Task 7; the rest of
// each module's public surface (the cover-mosaic selector, the playlist
// mutators `create`/`rename`/`delete`/reorder, `PlaylistSummary`/
// `playlist_store_path`, `PlaylistEntry`/`slugify`) is wired up by the real
// list/detail page bodies in Tasks 8-9.
#[allow(unused_imports)]
pub use cover::{PlaylistCover, entries_fingerprint, select_cover};
pub use model::PlaylistId;
#[allow(unused_imports)]
pub use model::{Playlist, PlaylistEntry, slugify};
pub use store::PlaylistStore;
#[allow(unused_imports)]
pub use store::{PlaylistSummary, playlist_store_path};
