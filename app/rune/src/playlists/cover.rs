//! Pure cover-selection logic for a playlist's thumbnail: the shared no_std
//! `soul-audio-playlist` crate's `PlaylistCover`/`select_cover`/
//! `first_distinct_album_indices`/`entries_fingerprint`/
//! `choose_playlist_cover`, re-exported through the host. Note `PlaylistCover::Custom`
//! now carries a `String` (not a `PathBuf`); callers that need a `&Path` (e.g.
//! `views::player::cover_from_path`) build one with `Path::new(&path)`.
#![forbid(unsafe_code)]

pub use soul_player_host::{
    PlaylistCover, choose_playlist_cover, entries_fingerprint, first_distinct_album_indices,
    select_cover,
};
