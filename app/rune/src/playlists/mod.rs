//! User-created playlists: an in-memory data model (`model`), persisted to an
//! on-disk per-folder store (`store`); Task 6+ adds cover selection and the
//! UI that surfaces both.
#![forbid(unsafe_code)]
// Nothing outside this module calls into the store yet (Task 7+ wires the UI
// up); its own tests exercise the full model + store, so this is not dead
// code, just not yet connected to `main.rs`.
#![allow(dead_code, unused_imports)]

pub mod model;
pub mod store;

pub use model::{Playlist, PlaylistEntry, PlaylistId, slugify};
pub use store::{PlaylistStore, PlaylistSummary, playlist_store_path};
