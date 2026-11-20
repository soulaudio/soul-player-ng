//! User-created playlists: an in-memory data model (`model`), persisted to an
//! on-disk per-folder store (`store`), and pure cover-mosaic selection
//! (`cover`). Task 7 wired `PlaylistStore`/`PlaylistId` into `State` and the
//! routes; Task 8 wired the Playlists list page (list/create, the cover
//! selector) in; Task 9 wired the detail page's mutators
//! (rename/delete/remove_entry/move_entry) and `Playlist`/`PlaylistEntry` in;
//! Task 10 wires `add_entry` in, via the Add-to-playlist overlay's
//! `views::add_to_playlist::add_track_to_playlist`.
#![forbid(unsafe_code)]
// This is a module-wide inner attribute: it covers everything in `cover`,
// `model`, and `store`, not just one item. Right now it only masks
// `entries_fingerprint` (cover-mosaic memoization), which is exercised only
// by `cover.rs`'s own tests; no view calls it yet (the list/detail pages
// don't memoize the cover decision, they just recompute it per render), so
// in a non-test build it is genuinely dead_code. But the same blanket allow
// would just as silently mask any future dead item added anywhere in these
// submodules. Narrowing it to `entries_fingerprint` alone would mean adding
// a per-item allow inside `cover.rs` itself, out of this task's touch scope
// (`mod.rs` only), so it stays module-wide for now; everything else,
// including `add_entry`/`PlaylistEntry` as of Task 10, is reached.
#![allow(dead_code)]

pub mod cover;
pub mod model;
pub mod store;

// As of Task 10, every mutator (`rename`/`delete`/`add_entry`/`remove_entry`/
// `move_entry`) and `Playlist`/`PlaylistEntry` are reached from non-test code
// (the detail page, plus `add_entry` from the Add-to-playlist overlay). `slugify` is reached in production too, but only via
// `store::create`'s own internal `super::model::slugify` path, never through
// this re-export, so the re-exported binding itself stays unused outside
// tests; `playlist_store_path` likewise, only reached through each view's own
// render tests resetting the shared on-disk store between runs.
#[allow(unused_imports)]
pub use cover::entries_fingerprint;
// `select_cover` itself is now only reached internally (`choose_playlist_cover`,
// which the views now call instead) and by its own tests in `cover.rs`; the
// re-export stays for discoverability.
#[allow(unused_imports)]
pub use cover::{PlaylistCover, select_cover};
pub use model::PlaylistId;
#[allow(unused_imports)]
pub use model::slugify;
pub use model::{Playlist, PlaylistEntry};
#[allow(unused_imports)]
pub use store::playlist_store_path;
pub use store::{PlaylistStore, PlaylistSummary};
