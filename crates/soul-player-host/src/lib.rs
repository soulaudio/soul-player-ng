//! Desktop assembly over `soul-player-backend`: picks the audio sink, owns the
//! live `Library`, fans backend events out to subscribers, and exposes the one
//! `PlayerHandle` the Rune UI head binds to.
#![forbid(unsafe_code)]

mod sink;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use soul_player_backend::{AudioPlayer, Library, cover_bytes};

pub use sink::SinkMode;
pub use soul_player_backend::{
    BrowseAxis, Command, Cover, Event, PlayState, QueueState, RepeatMode, Tick, TrackView,
};

type Subscriber = Box<dyn Fn(Event) + Send>;

/// The result of changing the watched-folder set: the aggregated track views
/// across every *reachable* folder, plus the folders that were configured but
/// currently **unavailable** (missing / unmounted / unreadable). Unavailable
/// folders stay in the watched set and re-aggregate automatically once they come
/// back — the waist never drops a folder just because it is momentarily offline
/// (the "don't wipe the library when a drive is unplugged" rule that every mature
/// player follows).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WatchUpdate {
    /// Aggregated views across the reachable folders, in the library's order.
    pub views: Vec<TrackView>,
    /// Configured folders that were unreachable on this scan (kept in the set).
    pub unavailable: Vec<PathBuf>,
}

struct Inner {
    mode: SinkMode,
    player: Mutex<Option<AudioPlayer>>,
    library: Mutex<Option<Arc<Library>>>,
    subscribers: Mutex<Vec<Subscriber>>,
    /// The configured watched folders (deduped, in order), including any that are
    /// currently unavailable — so a folder is never lost to a transient outage.
    folders: Mutex<Vec<PathBuf>>,
}

/// The one UI contract both heads bind to. Owns the backend actor (spawned lazily
/// on first playback), the current library, and an event fan-out.
///
/// `Clone` shares the same underlying player (it's an `Arc` handle) — a UI head
/// holds several clones (one per subscriber / init closure) that all drive one
/// actor.
#[derive(Clone)]
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
                folders: Mutex::new(Vec::new()),
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

    /// Replace the watched-folder set: re-scan every **reachable** folder,
    /// aggregate them into one library (compound `(source, local)` ids), and swap
    /// it in so [`views`](Self::views) / [`art`](Self::art) reflect the union.
    ///
    /// Robustness follows what mature players do with an offline location
    /// (foobar2000 "Error!", MusicBee ignore-removable, Roon "storage offline",
    /// Navidrome keep-missing): a watched root that is not a readable directory is
    /// **skipped** (checked at the ROOT, never by purging individual files), kept
    /// in the set so it re-aggregates when it returns, and reported in
    /// [`WatchUpdate::unavailable`]. An empty or all-unavailable set yields an
    /// empty library, not an error.
    ///
    /// Does **not** change playback — managing what is watched is separate from
    /// what is playing; re-scanning re-bakes, so compound ids can shift and the
    /// caller should re-queue from the returned views if it wants.
    pub fn set_folders(&self, roots: &[PathBuf]) -> anyhow::Result<WatchUpdate> {
        // Dedup, preserving order.
        let mut set: Vec<PathBuf> = Vec::with_capacity(roots.len());
        for r in roots {
            if !set.contains(r) {
                set.push(r.clone());
            }
        }
        // Partition on ROOT availability — a watched root that isn't a readable
        // directory is treated as temporarily gone, not purged.
        let (available, unavailable): (Vec<PathBuf>, Vec<PathBuf>) =
            set.iter().cloned().partition(|r| r.is_dir());

        let lib = Arc::new(Library::scan_many(&available)?);
        let views = lib.views();
        self.set_library(lib);
        *self.inner.folders.lock().expect("folders mutex") = set;
        Ok(WatchUpdate { views, unavailable })
    }

    /// Add `dir` to the watched set (no-op if already watched) and re-aggregate.
    pub fn add_folder(&self, dir: &Path) -> anyhow::Result<WatchUpdate> {
        let mut set = self.watched_folders();
        let dir = dir.to_path_buf();
        if !set.contains(&dir) {
            set.push(dir);
        }
        self.set_folders(&set)
    }

    /// Remove `dir` from the watched set (no-op if not watched) and re-aggregate.
    pub fn remove_folder(&self, dir: &Path) -> anyhow::Result<WatchUpdate> {
        let mut set = self.watched_folders();
        set.retain(|p| p != dir);
        self.set_folders(&set)
    }

    /// The configured watched folders, in order, including any currently
    /// unavailable (so a folder is never lost to a transient outage).
    pub fn watched_folders(&self) -> Vec<PathBuf> {
        self.inner.folders.lock().expect("folders mutex").clone()
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

    /// Current library views in a chosen browse order (Title / ArtistAlbumTrack /
    /// Album / Folder) — the baked axis tables, no re-sort.
    pub fn views_by(&self, axis: BrowseAxis) -> Vec<TrackView> {
        self.inner
            .library
            .lock()
            .expect("library mutex")
            .as_ref()
            .map(|l| l.views_by(axis))
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
        // Weak, not Arc: the pump thread owns this closure, and Inner owns the
        // AudioPlayer whose cmd_tx drop is the only shutdown signal. A strong ref
        // here would be a cycle that pins the actor alive forever after the last
        // PlayerHandle drops. Upgrade per-event; skip if the handle is gone.
        let weak = Arc::downgrade(&self.inner);
        let forward = move |ev: Event| {
            let Some(inner) = weak.upgrade() else { return };
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

    #[test]
    fn subscribe_receives_now_playing_tick() {
        use soul_player_backend::PlayState;
        use std::sync::mpsc;
        use std::time::Duration;

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.wav"), wav(48_000, 96_000)).unwrap();
        std::fs::write(dir.path().join("b.wav"), wav(48_000, 96_000)).unwrap();

        let h = PlayerHandle::offline();
        let (tx, rx) = mpsc::channel::<Event>();
        h.subscribe(move |ev| {
            let _ = tx.send(ev);
        });
        h.open_folder(dir.path()).expect("open_folder");

        // Wait (bounded) for a Tick showing track 0 playing.
        let deadline = Duration::from_secs(5);
        let mut saw_playing = false;
        while let Ok(ev) = rx.recv_timeout(deadline) {
            if let Event::Tick(t) = ev
                && t.now_playing_id == Some(0)
                && t.state == PlayState::Playing
            {
                saw_playing = true;
                break;
            }
        }
        assert!(saw_playing, "never observed track 0 playing via subscribe");
    }

    // --- multi-source watched folders ---

    fn folder_with(name: &str, wavs: &[&str]) -> tempfile::TempDir {
        let d = tempfile::Builder::new().prefix(name).tempdir().unwrap();
        for w in wavs {
            std::fs::write(d.path().join(w), wav(48_000, 96_000)).unwrap();
        }
        d
    }

    #[test]
    fn set_folders_aggregates_multiple_sources() {
        let a = folder_with("src_a", &["a1.wav"]);
        let b = folder_with("src_b", &["b1.wav", "b2.wav"]);

        let h = PlayerHandle::offline();
        let up = h
            .set_folders(&[a.path().to_path_buf(), b.path().to_path_buf()])
            .unwrap();
        assert_eq!(up.views.len(), 3, "1 from a + 2 from b aggregate");
        assert!(up.unavailable.is_empty());
        assert_eq!(
            h.watched_folders(),
            vec![a.path().to_path_buf(), b.path().to_path_buf()]
        );
        assert_eq!(h.views().len(), 3, "views() reflects the union");
    }

    #[test]
    fn set_folders_skips_unavailable_and_keeps_them_for_self_heal() {
        let a = folder_with("src_ok", &["a.wav"]);
        let missing = a.path().join("does-not-exist");

        let h = PlayerHandle::offline();
        let up = h
            .set_folders(&[a.path().to_path_buf(), missing.clone()])
            .unwrap();
        assert_eq!(up.views.len(), 1, "only the reachable folder scanned");
        assert_eq!(
            up.unavailable,
            vec![missing.clone()],
            "the missing root is reported"
        );
        // Kept in the set so it re-aggregates when it returns — never dropped.
        assert_eq!(h.watched_folders(), vec![a.path().to_path_buf(), missing]);
    }

    #[test]
    fn all_unavailable_yields_empty_library_not_error() {
        let base = tempfile::tempdir().unwrap();
        let m1 = base.path().join("gone1");
        let m2 = base.path().join("gone2");
        let h = PlayerHandle::offline();
        let up = h.set_folders(&[m1.clone(), m2.clone()]).unwrap();
        assert!(up.views.is_empty());
        assert_eq!(up.unavailable, vec![m1.clone(), m2.clone()]);
        assert_eq!(h.watched_folders(), vec![m1, m2]);
    }

    #[test]
    fn add_and_remove_folder_reaggregate() {
        let a = folder_with("add_a", &["a.wav"]);
        let b = folder_with("add_b", &["b.wav"]);

        let h = PlayerHandle::offline();
        h.set_folders(&[a.path().to_path_buf()]).unwrap();
        assert_eq!(h.views().len(), 1);

        let up = h.add_folder(b.path()).unwrap();
        assert_eq!(up.views.len(), 2, "b's track added");
        assert_eq!(h.watched_folders().len(), 2);

        let up = h.remove_folder(a.path()).unwrap();
        assert_eq!(up.views.len(), 1, "only b remains");
        assert_eq!(h.watched_folders(), vec![b.path().to_path_buf()]);
    }

    #[test]
    fn set_folders_dedups_repeated_roots() {
        let a = folder_with("dedup", &["a.wav"]);
        let h = PlayerHandle::offline();
        let up = h
            .set_folders(&[a.path().to_path_buf(), a.path().to_path_buf()])
            .unwrap();
        assert_eq!(up.views.len(), 1, "a duplicate root is not scanned twice");
        assert_eq!(h.watched_folders(), vec![a.path().to_path_buf()]);
    }

    #[test]
    fn plays_a_track_located_on_a_second_source() {
        use std::sync::mpsc;
        use std::time::Duration;

        // Two sources; the 2nd track in the aggregated order belongs to source b
        // (untagged WAVs tie on sort key, so order is (source, local): a then b).
        let a = folder_with("play_a", &["a.wav"]);
        let b = folder_with("play_b", &["b.wav"]);

        let h = PlayerHandle::offline();
        let (tx, rx) = mpsc::channel::<Event>();
        h.subscribe(move |ev| {
            let _ = tx.send(ev);
        });

        let up = h
            .set_folders(&[a.path().to_path_buf(), b.path().to_path_buf()])
            .unwrap();
        assert_eq!(up.views.len(), 2);
        let ids: Vec<u32> = up.views.iter().map(|v| v.id).collect();
        let second = ids[1];
        // Start at index 1 → play the source-b track: the actor's loader must
        // resolve its compound id back to source b's root and decode the file.
        h.send(Command::PlayContext {
            ids: ids.clone(),
            start: 1,
        });

        let deadline = Duration::from_secs(5);
        let mut saw = false;
        while let Ok(ev) = rx.recv_timeout(deadline) {
            if let Event::Tick(t) = ev
                && t.now_playing_id == Some(second)
                && t.state == PlayState::Playing
            {
                saw = true;
                break;
            }
        }
        assert!(
            saw,
            "never observed the second-source track play (cross-source locate+decode)"
        );
    }
}
