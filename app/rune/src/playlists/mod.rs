//! User-created playlists: a pure, in-memory data model (this module) that
//! Task 5 persists to disk and Task 6+ surfaces in the UI.
#![forbid(unsafe_code)]
// Nothing outside this module calls into the model yet; Task 5 (the on-disk
// store) and the later UI wire it up. Not dead code, just not yet connected.
#![allow(dead_code, unused_imports)]

pub mod model;

pub use model::{Playlist, PlaylistEntry, PlaylistId, slugify};
