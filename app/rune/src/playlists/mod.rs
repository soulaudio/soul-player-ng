//! User-created playlists: an in-memory data model (`model`), persisted to an
//! on-disk per-folder store (`store`), and pure cover-mosaic selection
//! (`cover`); Task 7+ adds the UI that surfaces all three.
#![forbid(unsafe_code)]
// Nothing outside this module calls into the store or cover selector yet
// (Task 7+ wires the UI up); their own tests exercise the full model, store
// and cover-selection logic, so this is not dead code, just not yet
// connected to `main.rs`.
#![allow(dead_code, unused_imports)]

pub mod cover;
pub mod model;
pub mod store;

pub use cover::{PlaylistCover, entries_fingerprint, select_cover};
pub use model::{Playlist, PlaylistEntry, PlaylistId, slugify};
pub use store::{PlaylistStore, PlaylistSummary, playlist_store_path};
