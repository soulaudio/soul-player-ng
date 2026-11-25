//! Album-shaped projections over [`TrackView`]s, pure and UI-free: the album
//! page's disc sections in (disc, track) order, the sort key behind them, and a
//! DURABLE album identity for persisted data (playlist cover dedupe) that must
//! survive a rescan, which the volatile `TrackView::album_key` does not.

use std::sync::Arc;

use soul_audio_playlist::TrackLocator;

use crate::types::TrackView;

/// One disc of an album's track list.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscSection {
    /// The disc number (`>= 1`).
    pub disc_no: u16,
    /// The disc's subtitle (first member track that carries one).
    pub subtitle: Option<Arc<str>>,
    /// `(track id, displayed row number)` in play order: the tagged track number,
    /// or the 1-based position within the disc when untagged.
    pub rows: Vec<(u32, u16)>,
    /// Sum of the disc's track durations, seconds.
    pub duration_secs: f64,
}

/// The album order key: disc (`0` reads as 1), then the tagged track number with
/// untagged (`0`) tracks after the numbered ones. Used with a STABLE sort, so ties
/// keep their incoming (library) order.
#[must_use]
pub fn disc_track_key(t: &TrackView) -> (u16, u16) {
    let track = if t.track_no == 0 {
        u16::MAX
    } else {
        t.track_no
    };
    (t.disc_no.max(1), track)
}

/// An album's tracks as disc sections, ordered by [`disc_track_key`] regardless
/// of the library's current sort axis.
#[must_use]
pub fn album_tracklist(tracks: &[&TrackView]) -> Vec<DiscSection> {
    let mut sorted: Vec<&TrackView> = tracks.to_vec();
    sorted.sort_by_key(|t| disc_track_key(t));
    let mut out: Vec<DiscSection> = Vec::new();
    for t in sorted {
        let disc = t.disc_no.max(1);
        if out.last().is_none_or(|d| d.disc_no != disc) {
            out.push(DiscSection {
                disc_no: disc,
                subtitle: None,
                rows: Vec::new(),
                duration_secs: 0.0,
            });
        }
        let sec = out.last_mut().expect("a section was pushed above");
        if sec.subtitle.is_none() {
            sec.subtitle = t.disc_subtitle.clone().filter(|s| !s.is_empty());
        }
        let position = u16::try_from(sec.rows.len() + 1).unwrap_or(u16::MAX);
        let number = if t.track_no == 0 {
            position
        } else {
            t.track_no
        };
        sec.rows.push((t.id, number));
        sec.duration_secs += t.duration_secs;
    }
    out
}

/// A durable album identity for a playlist entry: the importer's album-root rule
/// (the file's folder, or its parent when the folder is a `CD1`/`Disc 02 - x`
/// disc folder) plus the album title, prefixed by the remote server for a remote
/// locator (its scheme and host lowercased, so URL casing never splits one
/// server in two). Case and separator normalized like [`TrackLocator::key`]. Two
/// editions with one title in two folders differ; `CD1`/`CD2` of one album agree.
#[must_use]
pub fn album_identity_key(locator: &TrackLocator, album: &str) -> String {
    let (server, path) = match locator {
        TrackLocator::Local { path } => (String::new(), path.as_str()),
        TrackLocator::Remote { server, rel } => (normalize_server(server), rel.as_str()),
    };
    let norm = path.replace('\\', "/").to_lowercase();
    let mut parts: Vec<&str> = norm.split('/').collect();
    parts.pop(); // the file name
    if parts
        .last()
        .is_some_and(|d| soul_audio_tags::disc_folder::parse(d).is_some())
    {
        parts.pop();
    }
    format!("{server}\u{1f}{}\u{1f}{album}", parts.join("/"))
}

/// `server` with its scheme and host (`scheme://host:port`) lowercased; any path
/// after the host is kept as given.
fn normalize_server(server: &str) -> String {
    let host_start = server.find("://").map_or(0, |i| i + 3);
    let host_end = server[host_start..]
        .find('/')
        .map_or(server.len(), |i| host_start + i);
    let mut out = server[..host_end].to_ascii_lowercase();
    out.push_str(&server[host_end..]);
    out
}

/// The identity for an album title with no known track locator (no library
/// open, or a stale id): the title alone, so two same-title albums share it.
/// Callers use it only as a fallback when [`album_identity_key`] has no locator.
#[must_use]
pub fn unlocated_album_identity_key(album: &str) -> String {
    album_identity_key(
        &TrackLocator::Local {
            path: String::new(),
        },
        album,
    )
}

/// Whether `key` is a title-only identity ([`unlocated_album_identity_key`]):
/// no server and no album folder, so it cannot tell two same-title albums apart.
#[must_use]
pub fn is_unlocated_album_identity_key(key: &str) -> bool {
    key.starts_with("\u{1f}\u{1f}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unlocated_identity_is_the_title_alone() {
        assert_eq!(
            unlocated_album_identity_key("Box"),
            album_identity_key(
                &TrackLocator::Local {
                    path: String::new()
                },
                "Box"
            )
        );
        assert_eq!(unlocated_album_identity_key("Box"), "\u{1f}\u{1f}Box");
    }

    #[test]
    fn only_a_title_only_key_reads_as_unlocated() {
        assert!(is_unlocated_album_identity_key(
            &unlocated_album_identity_key("Box")
        ));
        let located = album_identity_key(
            &TrackLocator::Local {
                path: "d:/m/Box/01.flac".into(),
            },
            "Box",
        );
        assert!(!is_unlocated_album_identity_key(&located));
    }

    /// The server URL's scheme and host are case-insensitive, so a remote added
    /// as `HTTP://Music.Example` keeps the covers it had as `http://music.example`.
    #[test]
    fn a_remote_identity_ignores_the_server_url_case() {
        let at = |server: &str| {
            album_identity_key(
                &TrackLocator::Remote {
                    server: server.into(),
                    rel: "Box/CD1/01.flac".into(),
                },
                "Box",
            )
        };
        assert_eq!(
            at("HTTP://Music.Example:8080"),
            at("http://music.example:8080")
        );
        assert_eq!(
            at("HTTP://Music.Example:8080"),
            "http://music.example:8080\u{1f}box\u{1f}Box"
        );
    }

    fn tv(id: u32, disc: u16, track: u16, sub: Option<&str>, secs: f64) -> TrackView {
        TrackView {
            id,
            disc_no: disc,
            track_no: track,
            disc_subtitle: sub.map(Arc::from),
            duration_secs: secs,
            ..Default::default()
        }
    }

    #[test]
    fn tracklist_orders_by_disc_then_track_whatever_the_input_order() {
        // Library order is scrambled (e.g. sorted by title).
        let v = [
            tv(3, 2, 1, Some("Liyue"), 30.0),
            tv(2, 1, 6, None, 20.0),
            tv(1, 1, 5, Some("Mondstadt"), 10.0),
        ];
        let refs: Vec<&TrackView> = v.iter().collect();
        let d = album_tracklist(&refs);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].disc_no, 1);
        assert_eq!(
            d[0].rows,
            vec![(1, 5), (2, 6)],
            "row number = tagged track no"
        );
        assert_eq!(d[0].subtitle.as_deref(), Some("Mondstadt"));
        assert_eq!(d[0].duration_secs, 30.0);
        assert_eq!(d[1].disc_no, 2);
        assert_eq!(d[1].rows, vec![(3, 1)]);
        assert_eq!(d[1].subtitle.as_deref(), Some("Liyue"));
    }

    #[test]
    fn untagged_tracks_follow_numbered_ones_and_number_by_position() {
        let v = [
            tv(9, 0, 0, None, 1.0),
            tv(8, 1, 2, None, 1.0),
            tv(7, 1, 0, None, 1.0),
        ];
        let refs: Vec<&TrackView> = v.iter().collect();
        let d = album_tracklist(&refs);
        assert_eq!(d.len(), 1, "disc 0 reads as disc 1");
        // Numbered first; the two untagged keep their incoming order (9 then 7).
        assert_eq!(d[0].rows, vec![(8, 2), (9, 2), (7, 3)]);
    }

    #[test]
    fn zero_disc_literals_read_as_a_single_disc_one() {
        // Test/demo literals may carry disc_no = 0 and disc_count = 0.
        let v = [tv(2, 0, 2, None, 1.0), tv(1, 0, 1, None, 1.0)];
        assert_eq!(disc_track_key(&v[0]), (1, 2));
        let refs: Vec<&TrackView> = v.iter().collect();
        let d = album_tracklist(&refs);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].disc_no, 1);
        assert_eq!(d[0].rows, vec![(1, 1), (2, 2)]);
        assert!(album_tracklist(&[]).is_empty());
    }

    #[test]
    fn identity_key_folds_disc_folders_and_splits_same_title_folders() {
        let l = |p: &str| TrackLocator::Local { path: p.into() };
        let cd1 = album_identity_key(&l(r"D:\Music\Box\CD1\01.flac"), "Box");
        let cd2 = album_identity_key(&l("d:/music/box/Disc 02 - Liyue/01.flac"), "Box");
        assert_eq!(cd1, cd2, "disc folders of one album agree");
        let a = album_identity_key(&l("D:/Music/Hits A/01.flac"), "Greatest Hits");
        let b = album_identity_key(&l("D:/Music/Hits B/01.flac"), "Greatest Hits");
        assert_ne!(a, b, "same title in two folders differs");
        let r = TrackLocator::Remote {
            server: "http://h".into(),
            rel: "Hits A/01.flac".into(),
        };
        assert_ne!(
            album_identity_key(&r, "Greatest Hits"),
            a,
            "remote never equals local"
        );
    }
}
