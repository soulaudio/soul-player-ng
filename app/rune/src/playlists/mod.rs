//! User-created playlists: an in-memory data model (`model`), persisted to an
//! on-disk per-folder store (`store`), and pure cover-mosaic selection
//! (`cover`). Task 7 wired `PlaylistStore`/`PlaylistId` into `State` and the
//! routes; Task 8 wires the real Playlists list page (list/create, the cover
//! selector) in; the mutators a detail page needs
//! (rename/delete/add_entry/remove_entry/move_entry) land in Task 9.
#![forbid(unsafe_code)]
// `cover`/`model`/`store` each have their own tests exercising this fully, so
// what's flagged below is not dead code, just not yet reached from non-test
// `main.rs`/`views` code. This can't be narrowed further than "the whole
// submodule tree" without editing those files directly (out of scope for
// this crate's "allow trim" convention; see the per-import allows below for
// exactly what's still unreached after Task 8).
#![allow(dead_code)]

pub mod cover;
pub mod model;
pub mod store;

// As of Task 8, the Playlists list page reaches `PlaylistId`, `PlaylistStore`
// (list/create), `PlaylistCover`, and `select_cover` from non-test code.
// `playlist_store_path` is still only reached through test code (the view's
// own render tests reset the shared on-disk store between runs); the rest of
// each module's public surface (`entries_fingerprint`'s memoization,
// `Playlist`/`PlaylistEntry`/`slugify`, and the mutators
// `rename`/`delete`/`add_entry`/`remove_entry`/`move_entry`) is wired up by
// the real detail page body in Task 9.
#[allow(unused_imports)]
pub use cover::entries_fingerprint;
pub use cover::{PlaylistCover, select_cover};
pub use model::PlaylistId;
#[allow(unused_imports)]
pub use model::{Playlist, PlaylistEntry, slugify};
#[allow(unused_imports)]
pub use store::playlist_store_path;
pub use store::{PlaylistStore, PlaylistSummary};
