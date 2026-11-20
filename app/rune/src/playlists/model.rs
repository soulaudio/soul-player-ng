//! The playlist data model: the shared no_std `soul-audio-playlist` crate's
//! `Playlist`/`PlaylistEntry`/`PlaylistId`/`slugify` (plus `move_entry`/
//! `remove_entry`), re-exported through the host so the store and the views
//! all agree on the one shared identity.
#![forbid(unsafe_code)]

pub use soul_player_host::{Playlist, PlaylistEntry, PlaylistId, slugify};
