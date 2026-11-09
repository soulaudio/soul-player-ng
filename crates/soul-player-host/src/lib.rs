//! Desktop assembly over `soul-player-backend`: picks the audio sink, owns the
//! live `Library`, fans backend events out to subscribers, and exposes the one
//! `PlayerHandle` every UI head (Tauri, Rune) binds to.
#![forbid(unsafe_code)]

mod sink;

use std::path::Path;
use std::sync::{Arc, Mutex};

use soul_player_backend::{AudioPlayer, Library, cover_bytes};

pub use sink::SinkMode;
pub use soul_player_backend::{Command, Cover, Event, QueueState, RepeatMode, Tick, TrackView};

type Subscriber = Box<dyn Fn(Event) + Send>;

struct Inner {
    mode: SinkMode,
    player: Mutex<Option<AudioPlayer>>,
    library: Mutex<Option<Arc<Library>>>,
    subscribers: Mutex<Vec<Subscriber>>,
}

/// The one UI contract both heads bind to. Owns the backend actor (spawned lazily
/// on first playback), the current library, and an event fan-out.
pub struct PlayerHandle {
    inner: Arc<Inner>,
}

impl Default for PlayerHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl PlayerHandle {
    /// Build with the sink `SinkMode::from_env()` picks (Device, or Offline when
    /// `SOUL_PLAYER_OFFLINE` is set).
    pub fn new() -> Self {
        Self::with_sink_mode(SinkMode::from_env())
    }

    /// Build forced to the headless `NullSink` (tests / device-free smoke) with
    /// no env mutation.
    pub fn offline() -> Self {
        Self::with_sink_mode(SinkMode::Offline)
    }

    /// Build with an explicit sink mode.
    pub fn with_sink_mode(mode: SinkMode) -> Self {
        Self {
            inner: Arc::new(Inner {
                mode,
                player: Mutex::new(None),
                library: Mutex::new(None),
                subscribers: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Scan `dir` into a `Library`, swap it in, and start playing from track 0.
    /// Returns the track views. Errors on scan failure or an empty folder.
    pub fn open_folder(&self, dir: &Path) -> anyhow::Result<Vec<TrackView>> {
        let lib = Arc::new(Library::scan(dir)?);
        let views = lib.views();
        if views.is_empty() {
            anyhow::bail!("no supported audio files in {}", dir.display());
        }
        self.set_library(Arc::clone(&lib));
        let ids: Vec<u32> = views.iter().map(|v| v.id).collect();
        self.send(Command::PlayContext { ids, start: 0 });
        Ok(views)
    }

    /// Register an event handler. Called on every backend event (Tick ~8 Hz +
    /// Queue on mutation), from the pump thread. Handlers must not call back into
    /// this handle. Works before or after the actor has started.
    pub fn subscribe(&self, on_event: impl Fn(Event) + Send + 'static) {
        self.inner
            .subscribers
            .lock()
            .expect("subscribers mutex")
            .push(Box::new(on_event));
    }

    /// Current library views (empty until a folder is opened).
    pub fn views(&self) -> Vec<TrackView> {
        self.inner
            .library
            .lock()
            .expect("library mutex")
            .as_ref()
            .map(|l| l.views())
            .unwrap_or_default()
    }

    /// Raw cover art for a track id (bytes + mime), or `None`.
    pub fn art(&self, id: u32) -> Option<Cover> {
        let lib = self.inner.library.lock().expect("library mutex").clone()?;
        let path = lib.path_of(id)?.to_owned();
        cover_bytes(&path)
    }

    /// Fire one transport/queue command (fire-and-forget). No-op until a folder
    /// has been opened (the actor needs a library to start).
    pub fn send(&self, cmd: Command) {
        self.ensure_actor();
        if let Some(actor) = self.inner.player.lock().expect("player mutex").as_ref() {
            actor.send(cmd);
        }
    }

    /// If `SOUL_PLAYER_FOLDER` is set, open it. The shared launch harness for both
    /// heads.
    pub fn autoload_from_env(&self) -> anyhow::Result<()> {
        if let Some(dir) = std::env::var_os("SOUL_PLAYER_FOLDER") {
            self.open_folder(Path::new(&dir))?;
        }
        Ok(())
    }

    /// Swap the library the actor resolves ids against (and cache it app-side).
    /// Locks library then player, never both at once, to avoid a lock-order
    /// inversion with `ensure_actor` (which snapshots library before locking
    /// player).
    fn set_library(&self, lib: Arc<Library>) {
        {
            *self.inner.library.lock().expect("library mutex") = Some(Arc::clone(&lib));
        }
        if let Some(actor) = self.inner.player.lock().expect("player mutex").as_ref() {
            actor.set_library(lib);
        }
    }

    /// Lazily spawn the actor once a library exists. Snapshots the library WITHOUT
    /// holding the player lock (avoids inverting `set_library`'s library→player
    /// order). The forwarder fans each event out to the current subscribers.
    fn ensure_actor(&self) {
        let lib = { self.inner.library.lock().expect("library mutex").clone() };
        let Some(lib) = lib else {
            return; // no folder opened yet
        };
        let mut guard = self.inner.player.lock().expect("player mutex");
        if guard.is_some() {
            return;
        }
        let inner = Arc::clone(&self.inner);
        let forward = move |ev: Event| {
            for sub in inner.subscribers.lock().expect("subscribers mutex").iter() {
                sub(ev.clone());
            }
        };
        *guard = Some(sink::build_actor(self.inner.mode, lib, forward));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_handle_has_empty_views_before_open() {
        let h = PlayerHandle::offline();
        assert!(h.views().is_empty());
    }

    #[test]
    fn send_before_open_is_a_noop() {
        let h = PlayerHandle::offline();
        h.send(Command::TogglePlay); // no library yet → no actor → no panic
        assert!(h.views().is_empty());
    }

    /// Minimal stereo-16-bit WAV (mirrors the backend actor tests' writer).
    fn wav(rate: u32, frames: u32) -> Vec<u8> {
        let channels: u16 = 2;
        let block_align = channels * 2;
        let byte_rate = rate * u32::from(block_align);
        let data_len = frames * u32::from(block_align);
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + data_len).to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&byte_rate.to_le_bytes());
        v.extend_from_slice(&block_align.to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data_len.to_le_bytes());
        v.resize(v.len() + data_len as usize, 0);
        v
    }

    #[test]
    fn open_folder_scans_and_exposes_views() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.wav"), wav(48_000, 96_000)).unwrap();
        std::fs::write(dir.path().join("b.wav"), wav(48_000, 96_000)).unwrap();

        let h = PlayerHandle::offline();
        let views = h.open_folder(dir.path()).expect("open_folder");
        assert_eq!(views.len(), 2, "both wavs scanned");
        assert_eq!(h.views().len(), 2, "views() reflects the opened library");
    }
}
