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

const DBUS_NAME: &str = "soul_player";
const DISPLAY_NAME: &str = "Soul Player";

/// Owned track metadata to publish (strings owned because `MediaMetadata` borrows).
pub(crate) struct TrackMeta {
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
    } else if prev.playing != cur.playing || (cur.id.is_some() && since_last >= REFRESH_INTERVAL) {
        // Same track: a play/pause flip, or a periodic refresh while loaded.
        Push::PlaybackOnly
    } else {
        Push::Nothing
    }
}

/// Spawn the media-session thread. Returns a [`Sender`] for app→OS [`Update`]s, or
/// `None` if the thread can't start. `hwnd_raw` is the window handle as an integer
/// (Windows SMTC; `None` on macOS/Linux) — an `isize` because a raw pointer isn't
/// `Send`. OS events are routed back to the main thread via `app`.
pub(crate) fn spawn(app: AppHandle<State>, hwnd_raw: Option<isize>) -> Option<Sender<Update>> {
    let (tx, rx) = std::sync::mpsc::channel::<Update>();
    std::thread::Builder::new()
        .name("media-session".into())
        .spawn(move || session_thread(app, hwnd_raw, rx))
        .ok()?;
    Some(tx)
}

/// The session thread: owns `MediaControls`, attaches the OS→app handler, and
/// applies app→OS updates until the channel closes (app exit).
fn session_thread(app: AppHandle<State>, hwnd_raw: Option<isize>, rx: Receiver<Update>) {
    let hwnd = hwnd_raw.map(|h| h as *mut c_void);
    let config = PlatformConfig {
        dbus_name: DBUS_NAME,
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
                let md = MediaMetadata {
                    title: Some(&meta.title),
                    artist: Some(&meta.artist),
                    album: Some(&meta.album),
                    cover_url: None, // cover art is a follow-up (temp-file plumbing)
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

    fn snap(id: Option<u32>, playing: bool) -> Snapshot {
        Snapshot { id, playing }
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
}
