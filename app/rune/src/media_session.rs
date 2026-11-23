//! OS "Now Playing" / media-session integration via `souvlaki`: publishes the
//! current track + playback state to the platform transport (Windows SMTC, macOS
//! `MPNowPlayingInfoCenter`, Linux MPRIS), and turns hardware media keys / lock-
//! screen controls back into player [`Command`]s.
//!
//! `MediaControls` is owned by one long-lived thread (dropping it tears down the
//! OS session); the app pushes metadata/playback updates to it over a channel,
//! and its event handler routes OS events straight to `host.send(..)`. This file's
//! pure core is [`map_event`]; the thread + OS wiring is below it.
#![forbid(unsafe_code)]

use souvlaki::{MediaControlEvent, SeekDirection};

use soul_player_host::Command;

/// Stepless OS seek nudge (`MediaControlEvent::Seek`), in seconds.
const SEEK_STEP_SECS: f64 = 5.0;

/// What an incoming OS media-control event resolves to.
#[derive(Debug, PartialEq)]
pub(crate) enum SessionAction {
    /// A player command to forward to the host.
    Command(Command),
    /// Bring the app window to the front (MPRIS `Raise`).
    Raise,
    /// Quit the app (MPRIS `Quit`).
    Quit,
}

/// Map an OS [`MediaControlEvent`] to a [`SessionAction`]. `position_secs` /
/// `duration_secs` are the last-known playback position + track length, needed to
/// resolve the relative seek nudges into the host's absolute `Seek { secs }`.
/// `None` for events we don't act on (e.g. `OpenUri`).
pub(crate) fn map_event(
    event: MediaControlEvent,
    position_secs: f64,
    duration_secs: f64,
) -> Option<SessionAction> {
    let cmd = |c| Some(SessionAction::Command(c));
    match event {
        MediaControlEvent::Play => cmd(Command::Play),
        MediaControlEvent::Pause => cmd(Command::Pause),
        MediaControlEvent::Toggle => cmd(Command::TogglePlay),
        MediaControlEvent::Next => cmd(Command::Next),
        MediaControlEvent::Previous => cmd(Command::Prev),
        MediaControlEvent::Stop => cmd(Command::Stop),
        MediaControlEvent::SetPosition(pos) => cmd(Command::Seek {
            secs: clamp_seek(pos.0.as_secs_f64(), duration_secs),
        }),
        MediaControlEvent::Seek(dir) => cmd(Command::Seek {
            secs: seek_by(position_secs, dir, SEEK_STEP_SECS, duration_secs),
        }),
        MediaControlEvent::SeekBy(dir, delta) => cmd(Command::Seek {
            secs: seek_by(position_secs, dir, delta.as_secs_f64(), duration_secs),
        }),
        MediaControlEvent::SetVolume(v) => cmd(Command::SetVolume(v as f32)),
        MediaControlEvent::SetShuffle(on) => cmd(Command::SetShuffle(on)),
        MediaControlEvent::Raise => Some(SessionAction::Raise),
        MediaControlEvent::Quit => Some(SessionAction::Quit),
        // We don't play arbitrary external URIs from the OS.
        MediaControlEvent::OpenUri(_) => None,
        // `MediaControlEvent` is `#[non_exhaustive]`; ignore anything new.
        _ => None,
    }
}

/// A relative seek from `position_secs` by `step` (signed by `dir`), clamped to
/// the track.
fn seek_by(position_secs: f64, dir: SeekDirection, step: f64, duration_secs: f64) -> f64 {
    let delta = match dir {
        SeekDirection::Forward => step,
        SeekDirection::Backward => -step,
    };
    clamp_seek(position_secs + delta, duration_secs)
}

/// Clamp a seek target to `[0, duration]` (or just `>= 0` when the duration is
/// unknown / zero).
fn clamp_seek(secs: f64, duration_secs: f64) -> f64 {
    let lo = secs.max(0.0);
    if duration_secs > 0.0 {
        lo.min(duration_secs)
    } else {
        lo
    }
}

// ---- app → OS feeding -----------------------------------------------------

use std::ffi::c_void;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use rune::AppHandle;
use souvlaki::{MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig};

use crate::State;

/// The MPRIS bus-name suffix: the app registers as
/// `org.mpris.MediaPlayer2.{}` with this as the suffix.
///
/// This is [`crate::desktop_id`] — the same string as the window's `app_id` and
/// the installed desktop entry's basename. MPRIS has an optional `DesktopEntry`
/// root property that would state the link outright, but souvlaki does not
/// implement it (0.9.3 serves Identity/CanQuit/CanRaise/SupportedUriSchemes/
/// SupportedMimeTypes and nothing else), so a desktop falls back to matching the
/// bus-name SUFFIX against installed desktop files. If the two disagree there is
/// nothing to match and media applets show a generic icon.
///
/// Hyphens are legal here. D-Bus forbids them in object paths and interface
/// names, but bus-name elements allow `[A-Za-z0-9_-]` (just not a leading
/// digit), and souvlaki interpolates this straight into the name.
fn dbus_name() -> &'static str {
    crate::desktop_id()
}
const DISPLAY_NAME: &str = "Soul Player";

// ---- artwork --------------------------------------------------------------------
//
// The OS transports load artwork from a URL (macOS: `NSImage initWithContentsOfURL`,
// asynchronously; MPRIS: `mpris:artUrl`; SMTC: a stream from the URI). A bare path has no
// scheme and silently fails on macOS, so the cover is written to the cache dir and passed
// as a properly percent-encoded `file://` URL. One file per track id: macOS caches by URL,
// so reusing one filename would show the previous cover.

/// The cache file name for track `id`'s cover with MIME `mime`.
pub(crate) fn artwork_file_name(id: u32, mime: &str) -> String {
    let ext = match mime {
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        _ => "jpg",
    };
    format!("cover-{id}.{ext}")
}

/// A `file://` URL for an absolute `path`, each segment percent-encoded (spaces, `#`,
/// `?`, non-ASCII album names …) so `NSURL URLWithString:` accepts it.
pub(crate) fn file_url(path: &std::path::Path) -> String {
    use std::path::Component;
    let mut url = String::from("file://");
    for c in path.components() {
        match c {
            Component::RootDir => {}
            Component::Prefix(p) => {
                // Windows drive (`C:`) → `file:///C:/…`
                url.push('/');
                url.push_str(&p.as_os_str().to_string_lossy());
                continue;
            }
            Component::Normal(seg) => {
                url.push('/');
                url.push_str(&urlencoding::encode(&seg.to_string_lossy()));
            }
            Component::CurDir | Component::ParentDir => {}
        }
    }
    url
}

/// Write track `id`'s cover into `dir` (replacing any older cover files there) and return
/// its `file://` URL; `None` when the track has no art or the write fails.
fn publish_artwork(
    dir: &std::path::Path,
    id: u32,
    cover: Option<soul_player_host::Cover>,
) -> Option<String> {
    let cover = cover?;
    std::fs::create_dir_all(dir).ok()?;
    let name = artwork_file_name(id, &cover.mime);
    // Keep only the current cover: the cache dir must not grow per track played.
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let n = e.file_name();
            let n = n.to_string_lossy();
            if n.starts_with("cover-") && n != name {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let path = dir.join(&name);
    std::fs::write(&path, &cover.bytes).ok()?;
    Some(file_url(&path))
}

/// Size of a generated Now Playing cover (Control Center / lock screen draw it large).
const GENERATED_NOW_PLAYING_PX: u32 = 600;

/// The cover to publish: the track's real art, else the same generated cover the app shows
/// for its album (or, with no album, its artist) — never an empty Now Playing tile.
pub(crate) fn now_playing_cover(
    art: Option<soul_player_host::Cover>,
    album: &str,
    artist: &str,
) -> Option<soul_player_host::Cover> {
    use soul_player_host::generated_cover as gc;
    art.or_else(|| {
        let seed = gc::album_seed(album).or_else(|| gc::artist_seed(artist))?;
        Some(soul_player_host::Cover {
            mime: "image/png".into(),
            bytes: gc::render_png(seed, GENERATED_NOW_PLAYING_PX),
        })
    })
}

/// Where now-playing covers are cached (`~/Library/Caches/soul-player/now-playing` on
/// macOS), beside the remote cache. Not "Soul Player": on Windows the cache dir is
/// `%LOCALAPPDATA%`, where `%LOCALAPPDATA%\Soul Player` is the per-user INSTALL dir — a
/// cache there survives the uninstaller (the release e2e caught that).
fn artwork_dir() -> Option<std::path::PathBuf> {
    Some(dirs::cache_dir()?.join("soul-player").join("now-playing"))
}

/// Owned track metadata to publish (strings owned because `MediaMetadata` borrows).
pub(crate) struct TrackMeta {
    /// Library id — the session thread fetches this track's cover art itself, so the
    /// tag read + file write never runs on the UI thread.
    pub id: u32,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_secs: f64,
}

/// A playback snapshot for the OS transport.
pub(crate) struct Playback {
    pub playing: bool,
    pub stopped: bool,
    pub position_secs: f64,
}

/// An app→OS update sent to the session thread.
pub(crate) enum Update {
    Track(TrackMeta),
    Playback(Playback),
}

/// A minimal now-playing snapshot for change detection.
pub(crate) struct Snapshot {
    pub id: Option<u32>,
    pub playing: bool,
    /// Playback is stopped (as opposed to merely paused). Tracked separately
    /// because `playing: false` covers both, and a Stop that leaves the track
    /// loaded flips only this — without it the OS session would sit on `Paused`
    /// until the next periodic refresh, or forever if the stop arrived while
    /// already paused. `Update::Playback` already distinguishes the two
    /// (`MediaPlayback::Stopped`); this is what gets that push SENT.
    pub stopped: bool,
}

/// What the OS session needs pushed given a change in the now-playing snapshot.
#[derive(Debug, PartialEq)]
pub(crate) enum Push {
    Nothing,
    PlaybackOnly,
    TrackAndPlayback,
}

/// Minimum interval between periodic playback refreshes while a track is loaded,
/// so the OS scrubber tracks seeks/drift without flooding the channel/D-Bus.
const REFRESH_INTERVAL: Duration = Duration::from_secs(1);

/// Decide what to push: republish everything on a track change, playback on a
/// play/pause flip, and a periodic playback refresh (>= [`REFRESH_INTERVAL`])
/// while a track is loaded. Nothing otherwise.
pub(crate) fn decide_push(prev: &Snapshot, cur: &Snapshot, since_last: Duration) -> Push {
    if prev.id != cur.id {
        Push::TrackAndPlayback
    } else if prev.playing != cur.playing
        || prev.stopped != cur.stopped
        || (cur.id.is_some() && since_last >= REFRESH_INTERVAL)
    {
        // Same track: a play/pause flip, a stop/resume, or a periodic refresh
        // while loaded.
        Push::PlaybackOnly
    } else {
        Push::Nothing
    }
}

/// Spawn the media-session thread. Returns a [`Sender`] for app→OS [`Update`]s, or
/// `None` if the thread can't start. `hwnd_raw` is the window handle as an integer
/// (Windows SMTC; `None` on macOS/Linux) — an `isize` because a raw pointer isn't
/// `Send`. OS events are routed back to the main thread via `app`.
pub(crate) fn spawn(
    app: AppHandle<State>,
    host: soul_player_host::PlayerHandle,
    hwnd_raw: Option<isize>,
) -> Option<Sender<Update>> {
    let (tx, rx) = std::sync::mpsc::channel::<Update>();
    std::thread::Builder::new()
        .name("media-session".into())
        .spawn(move || session_thread(app, host, hwnd_raw, rx))
        .ok()?;
    Some(tx)
}

/// The session thread: owns `MediaControls`, attaches the OS→app handler, and
/// applies app→OS updates until the channel closes (app exit).
fn session_thread(
    app: AppHandle<State>,
    host: soul_player_host::PlayerHandle,
    hwnd_raw: Option<isize>,
    rx: Receiver<Update>,
) {
    let hwnd = hwnd_raw.map(|h| h as *mut c_void);
    let config = PlatformConfig {
        dbus_name: dbus_name(),
        display_name: DISPLAY_NAME,
        hwnd,
    };
    let mut controls = match MediaControls::new(config) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("media session unavailable: {e:?}");
            return;
        }
    };
    // OS -> app: forward each event to the main thread, where live State resolves
    // relative seeks and dispatches the command.
    let handler_app = app.clone();
    if let Err(e) = controls.attach(move |event| {
        handler_app.update(move |s, tx| crate::apply_media_event(s, tx, event));
    }) {
        tracing::warn!("media session attach failed: {e:?}");
        return;
    }
    tracing::info!("media session active");
    while let Ok(update) = rx.recv() {
        match update {
            Update::Track(meta) => {
                let t0 = std::time::Instant::now();
                let art = now_playing_cover(host.art(meta.id), &meta.album, &meta.artist);
                let art_ms = t0.elapsed().as_millis() as u64;
                let cover_url = artwork_dir().and_then(|dir| publish_artwork(&dir, meta.id, art));
                tracing::debug!(
                    id = meta.id,
                    art_ms,
                    total_ms = t0.elapsed().as_millis() as u64,
                    has_art = cover_url.is_some(),
                    "now-playing artwork published"
                );
                let md = MediaMetadata {
                    title: Some(&meta.title),
                    artist: Some(&meta.artist),
                    album: Some(&meta.album),
                    cover_url: cover_url.as_deref(),
                    duration: (meta.duration_secs > 0.0)
                        .then(|| Duration::from_secs_f64(meta.duration_secs)),
                };
                if let Err(e) = controls.set_metadata(md) {
                    tracing::debug!("set_metadata failed: {e:?}");
                }
            }
            Update::Playback(pb) => {
                let playback = if pb.stopped {
                    MediaPlayback::Stopped
                } else {
                    let progress = Some(MediaPosition(Duration::from_secs_f64(
                        pb.position_secs.max(0.0),
                    )));
                    if pb.playing {
                        MediaPlayback::Playing { progress }
                    } else {
                        MediaPlayback::Paused { progress }
                    }
                };
                if let Err(e) = controls.set_playback(playback) {
                    tracing::debug!("set_playback failed: {e:?}");
                }
            }
        }
    }
    // Channel closed → app exiting; dropping `controls` tears down the OS session.
    tracing::info!("media session closing");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn act(event: MediaControlEvent, pos: f64, dur: f64) -> SessionAction {
        map_event(event, pos, dur).expect("event maps to an action")
    }

    #[test]
    fn transport_events_map_to_commands() {
        assert_eq!(
            act(MediaControlEvent::Toggle, 0.0, 0.0),
            SessionAction::Command(Command::TogglePlay)
        );
        assert_eq!(
            act(MediaControlEvent::Next, 0.0, 0.0),
            SessionAction::Command(Command::Next)
        );
        assert_eq!(
            act(MediaControlEvent::Previous, 0.0, 0.0),
            SessionAction::Command(Command::Prev)
        );
        assert_eq!(
            act(MediaControlEvent::Raise, 0.0, 0.0),
            SessionAction::Raise
        );
        assert_eq!(act(MediaControlEvent::Quit, 0.0, 0.0), SessionAction::Quit);
    }

    #[test]
    fn set_position_seeks_to_the_absolute_time_clamped() {
        assert_eq!(
            act(
                MediaControlEvent::SetPosition(souvlaki::MediaPosition(Duration::from_secs(30))),
                10.0,
                200.0
            ),
            SessionAction::Command(Command::Seek { secs: 30.0 })
        );
        // Past the end clamps to the duration.
        assert_eq!(
            act(
                MediaControlEvent::SetPosition(souvlaki::MediaPosition(Duration::from_secs(999))),
                10.0,
                200.0
            ),
            SessionAction::Command(Command::Seek { secs: 200.0 })
        );
    }

    #[test]
    fn relative_seeks_offset_from_the_current_position() {
        // SeekBy forward by 10 from 20 -> 30.
        assert_eq!(
            act(
                MediaControlEvent::SeekBy(SeekDirection::Forward, Duration::from_secs(10)),
                20.0,
                200.0
            ),
            SessionAction::Command(Command::Seek { secs: 30.0 })
        );
        // Stepless Seek backward from 3s can't go below 0.
        assert_eq!(
            act(MediaControlEvent::Seek(SeekDirection::Backward), 3.0, 200.0),
            SessionAction::Command(Command::Seek { secs: 0.0 })
        );
    }

    #[test]
    fn volume_and_unsupported_events() {
        assert_eq!(
            act(MediaControlEvent::SetVolume(0.5), 0.0, 0.0),
            SessionAction::Command(Command::SetVolume(0.5))
        );
        assert_eq!(
            map_event(MediaControlEvent::OpenUri("x".into()), 0.0, 0.0),
            None
        );
    }

    #[test]
    fn artwork_file_names_follow_the_mime_and_are_per_track() {
        assert_eq!(artwork_file_name(7, "image/jpeg"), "cover-7.jpg");
        assert_eq!(artwork_file_name(7, "image/png"), "cover-7.png");
        assert_eq!(
            artwork_file_name(8, "application/octet-stream"),
            "cover-8.jpg"
        );
        assert_ne!(
            artwork_file_name(1, "image/png"),
            artwork_file_name(2, "image/png"),
            "macOS caches artwork by URL, so each track needs its own file"
        );
    }

    #[cfg(unix)]
    #[test]
    fn file_urls_are_absolute_and_percent_encoded() {
        let p =
            std::path::Path::new("/Users/me/Library/Caches/Soul Player/now-playing/cover-1.jpg");
        assert_eq!(
            file_url(p),
            "file:///Users/me/Library/Caches/Soul%20Player/now-playing/cover-1.jpg"
        );
        let odd = std::path::Path::new("/tmp/AC#DC ?/Björk.png");
        assert_eq!(file_url(odd), "file:///tmp/AC%23DC%20%3F/Bj%C3%B6rk.png");
    }

    #[cfg(windows)]
    #[test]
    fn file_urls_on_windows_prefix_the_drive_and_percent_encode() {
        // The Windows drive-letter branch (`Component::Prefix`) builds `file:///C:/…` —
        // the path this platform actually runs, previously exercised only under
        // `#[cfg(unix)]`, so Windows had zero coverage of its own branch.
        let p = std::path::Path::new(r"C:\Users\me\Soul Player\now-playing\cover-1.jpg");
        assert_eq!(
            file_url(p),
            "file:///C:/Users/me/Soul%20Player/now-playing/cover-1.jpg"
        );
        // Special characters in a drive path are still percent-encoded per segment.
        let odd = std::path::Path::new(r"C:\Music\AC#DC ?\Björk.png");
        assert_eq!(
            file_url(odd),
            "file:///C:/Music/AC%23DC%20%3F/Bj%C3%B6rk.png"
        );
    }

    #[test]
    fn publish_artwork_writes_one_current_cover_and_prunes_old_ones() {
        let root = std::env::temp_dir().join(format!("soul_np_art_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let dir = &root;
        let cover = |b: u8| {
            Some(soul_player_host::Cover {
                mime: "image/png".into(),
                bytes: vec![b; 16],
            })
        };
        let u1 = publish_artwork(dir, 1, cover(1)).expect("url");
        assert!(u1.starts_with("file://") && u1.ends_with("cover-1.png"));
        let u2 = publish_artwork(dir, 2, cover(2)).expect("url");
        assert!(u2.ends_with("cover-2.png"));
        let names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["cover-2.png".to_string()], "old cover pruned");
        assert_eq!(
            std::fs::read(dir.join("cover-2.png")).unwrap(),
            vec![2u8; 16]
        );
        assert_eq!(publish_artwork(dir, 3, None), None, "no art → no URL");
    }

    /// A snapshot in the ordinary playing/paused case: a track is loaded, so it
    /// is not stopped. `snap_stopped` covers the loaded-but-stopped case.
    fn snap(id: Option<u32>, playing: bool) -> Snapshot {
        Snapshot {
            id,
            playing,
            stopped: id.is_none(),
        }
    }

    /// A snapshot for a track that is loaded but STOPPED — what MPRIS `Stop`
    /// (or a deck that reached the end) leaves behind.
    fn snap_stopped(id: Option<u32>) -> Snapshot {
        Snapshot {
            id,
            playing: false,
            stopped: true,
        }
    }

    /// Stopping a loaded track must push immediately, not wait for the periodic
    /// refresh — otherwise the OS transport keeps showing `Paused` (a different
    /// MPRIS status than the spec calls for here) for up to a second, or
    /// indefinitely when the stop arrives while already paused.
    #[test]
    fn decide_push_on_stop_of_a_loaded_track() {
        // Playing -> stopped: `playing` flips too, but the stop must be the
        // reason it pushes even when it isn't.
        assert_eq!(
            decide_push(&snap(Some(1), true), &snap_stopped(Some(1)), Duration::ZERO),
            Push::PlaybackOnly
        );
        // Paused -> stopped: `playing` is false on BOTH sides, so only the
        // `stopped` flag distinguishes them. This is the case that hung on
        // `Paused` forever.
        assert_eq!(
            decide_push(
                &snap(Some(1), false),
                &snap_stopped(Some(1)),
                Duration::ZERO
            ),
            Push::PlaybackOnly
        );
        // And back out of stopped, without a track change.
        assert_eq!(
            decide_push(&snap_stopped(Some(1)), &snap(Some(1), true), Duration::ZERO),
            Push::PlaybackOnly
        );
    }

    #[test]
    fn decide_push_republishes_on_track_change() {
        assert_eq!(
            decide_push(&snap(Some(1), true), &snap(Some(2), true), Duration::ZERO),
            Push::TrackAndPlayback
        );
        // Starting from nothing playing also counts as a track change.
        assert_eq!(
            decide_push(&snap(None, false), &snap(Some(1), true), Duration::ZERO),
            Push::TrackAndPlayback
        );
    }

    #[test]
    fn decide_push_on_play_pause_flip_only() {
        assert_eq!(
            decide_push(&snap(Some(1), true), &snap(Some(1), false), Duration::ZERO),
            Push::PlaybackOnly
        );
    }

    #[test]
    fn decide_push_refreshes_after_the_interval_but_not_before() {
        // Same track, same state, under 1s → nothing.
        assert_eq!(
            decide_push(
                &snap(Some(1), true),
                &snap(Some(1), true),
                Duration::from_millis(400)
            ),
            Push::Nothing
        );
        // Past the interval → a periodic refresh.
        assert_eq!(
            decide_push(
                &snap(Some(1), true),
                &snap(Some(1), true),
                Duration::from_secs(2)
            ),
            Push::PlaybackOnly
        );
        // No track loaded → never refreshes.
        assert_eq!(
            decide_push(
                &snap(None, false),
                &snap(None, false),
                Duration::from_secs(10)
            ),
            Push::Nothing
        );
    }

    #[test]
    fn now_playing_falls_back_to_the_generated_album_then_artist_cover() {
        use soul_player_host::{Cover, generated_cover as gc};
        let real = Cover {
            mime: "image/jpeg".into(),
            bytes: vec![1, 2, 3],
        };
        let kept = now_playing_cover(Some(real.clone()), "Album", "Artist").unwrap();
        assert_eq!(kept.bytes, real.bytes, "real art always wins");

        let by_album = now_playing_cover(None, "Kind of Blue", "Miles Davis").unwrap();
        assert_eq!(by_album.mime, "image/png");
        assert_eq!(
            by_album.bytes,
            gc::render_png(
                gc::album_seed("kind of blue").unwrap(),
                GENERATED_NOW_PLAYING_PX
            ),
            "the same generated cover the app shows for that album"
        );
        let by_artist = now_playing_cover(None, "", "Miles Davis").unwrap();
        assert_eq!(
            by_artist.bytes,
            gc::render_png(
                gc::artist_seed("Miles Davis").unwrap(),
                GENERATED_NOW_PLAYING_PX
            )
        );
        assert!(
            now_playing_cover(None, "", "").is_none(),
            "nothing to seed from"
        );
    }
}
