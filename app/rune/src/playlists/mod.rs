//! User-created playlists: an in-memory data model (`model`), persisted to an
//! on-disk per-folder store (`store`), and pure cover-mosaic selection
//! (`cover`). Task 7 wired `PlaylistStore`/`PlaylistId` into `State` and the
//! routes; Task 8 wired the Playlists list page (list/create, the cover
//! selector) in; Task 9 wires the detail page's mutators
//! (rename/delete/remove_entry/move_entry) and `Playlist`/`PlaylistEntry` in.
#![forbid(unsafe_code)]
// `entries_fingerprint` (cover-mosaic memoization) is exercised only by
// `cover.rs`'s own tests; no view calls it yet (the list/detail pages don't
// memoize the cover decision, they just recompute it per render), so in a
// non-test build it is genuinely dead_code. Narrowing further would mean
// adding a per-item allow inside `cover.rs` itself, out of this task's touch
// scope (`mod.rs` only); this blanket allow is now scoped to just that one
// still-unreached item, everything else Task 9 wires up below is reached.
#![allow(dead_code)]

pub mod cover;
pub mod model;
pub mod store;

// As of Task 9, every mutator (`rename`/`delete`/`add_entry`/`remove_entry`/
// `move_entry`) and `Playlist`/`PlaylistEntry` are reached from non-test code
// (the detail page). `slugify` is reached in production too, but only via
// `store::create`'s own internal `super::model::slugify` path, never through
// this re-export, so the re-exported binding itself stays unused outside
// tests; `playlist_store_path` likewise, only reached through each view's own
// render tests resetting the shared on-disk store between runs.
#[allow(unused_imports)]
pub use cover::entries_fingerprint;
pub use cover::{PlaylistCover, select_cover};
pub use model::PlaylistId;
#[allow(unused_imports)]
pub use model::slugify;
pub use model::{Playlist, PlaylistEntry};
#[allow(unused_imports)]
pub use store::playlist_store_path;
pub use store::{PlaylistStore, PlaylistSummary};
