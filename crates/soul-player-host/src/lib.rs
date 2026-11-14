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
    BrowseAxis, Command, Cover, Event, PlayState, QueueState, RepeatMode, ReplayGainMode, Tick,
    TrackView,
};

type Subscriber = Box<dyn Fn(Event) + Send>;

/// The per-source status of one configured folder, as of the last scan.
///
/// A source is kept in the configured set regardless of its state — disabling it
/// or losing its root to a transient outage never drops it (the "don't wipe the
/// library when a drive is unplugged" rule that every mature player follows). The
/// flags describe what that source contributed on the most recent aggregation:
/// a disabled or [`unavailable`](Self::available) source lists with a
/// [`track_count`](Self::track_count) of 0, and re-contributes automatically once
/// it is re-enabled or its root returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceStatus {
    /// The configured root path (as given by the caller, deduped in order).
    pub path: PathBuf,
    /// Whether the user has this source enabled (disabled sources are skipped).
    pub enabled: bool,
    /// Whether the root was a readable directory on the last scan (missing /
    /// unmounted / unreadable roots are `false`, but stay in the set).
    pub available: bool,
    /// Tracks this source contributed to the aggregated library — 0 when the
    /// source is disabled or unavailable.
    pub track_count: usize,
}

/// The result of changing the configured-source set: the aggregated track views
/// across every *enabled and reachable* source, plus a [`SourceStatus`] for every
/// configured source (including disabled or currently-unavailable ones, which
/// list with 0 tracks and re-aggregate once re-enabled or back online).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WatchUpdate {
    /// Aggregated views across the enabled, reachable sources, in library order.
    pub views: Vec<TrackView>,
    /// Per-source status for the full configured set, in configured order.
    pub sources: Vec<SourceStatus>,
}

/// One configured source: a root path plus whether the user has it enabled.
/// Stored (deduped, in order) so an enabled flag — like an unavailable root —
/// survives re-scans and reconciliation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FolderConfig {
    path: PathBuf,
    enabled: bool,
}

struct Inner {
    mode: SinkMode,
    player: Mutex<Option<AudioPlayer>>,
    library: Mutex<Option<Arc<Library>>>,
    subscribers: Mutex<Vec<Subscriber>>,
    /// The configured sources (deduped, in order) with their enabled flags,
    /// including any that are currently disabled or unavailable — so a source is
    /// never lost to a transient outage or a toggle.
    folders: Mutex<Vec<FolderConfig>>,
    /// The most recent [`SourceStatus`] set (with real per-source track counts)
    /// produced by [`rescan`](PlayerHandle::rescan). Cached so
    /// [`sources`](PlayerHandle::sources) — which seeds UI state at startup —
    /// reports the true counts from the last aggregation instead of a 0
    /// placeholder, keeping the Sources list in step with the loaded library.
    last_sources: Mutex<Vec<SourceStatus>>,
    /// The chosen output device name (one of [`output_devices`](PlayerHandle::output_devices)),
    /// or `None` for the system default. Read when the actor is (re)spawned and
    /// passed to the sink factory; a change applies to the NEXT spawn, not the
    /// live stream (see [`set_output_device`](PlayerHandle::set_output_device)).
    output_device: Mutex<Option<String>>,
    /// The ReplayGain normalization choice `(mode, preamp_db)`. Remembered so a
    /// freshly-spawned actor is told the current setting (via a `SetReplayGain`
    /// command) the moment it starts — and, unlike the output device, a change
    /// also applies to the LIVE actor immediately (see
    /// [`set_replay_gain`](PlayerHandle::set_replay_gain)).
    replay_gain: Mutex<(ReplayGainMode, f32)>,
    /// The app's persisted loudness cache (`(path, computed track_gain_db)`),
    /// supplied once at startup by [`seed_loudness`](PlayerHandle::seed_loudness).
    /// Banked so a lazily- (or re-) spawned actor is re-seeded on spawn — the
    /// hybrid-ReplayGain cache for previously-untagged tracks survives the actor's
    /// lazy start just like the ReplayGain choice does.
    loudness_seed: Mutex<Vec<(PathBuf, f32)>>,
    /// Test-support: when `Some`, [`send`](PlayerHandle::send) records commands
    /// here instead of driving the actor, so a head's command wiring can be
    /// asserted headlessly. `None` (the default) in every real handle.
    recorder: Mutex<Option<Vec<Command>>>,
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

    /// Test-support: a handle that RECORDS sent commands instead of driving the
    /// actor (headless — no library, no audio). A head's command wiring (which
    /// `Command` a click/drag dispatches) can be asserted with
    /// [`recorded_commands`](PlayerHandle::recorded_commands).
    pub fn recording() -> Self {
        let h = Self::offline();
        *h.inner.recorder.lock().expect("recorder mutex") = Some(Vec::new());
        h
    }

    /// The commands sent to a [`recording`](PlayerHandle::recording) handle, in
    /// order (empty for a normal handle).
    pub fn recorded_commands(&self) -> Vec<Command> {
        self.inner
            .recorder
            .lock()
            .expect("recorder mutex")
            .clone()
            .unwrap_or_default()
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
                last_sources: Mutex::new(Vec::new()),
                output_device: Mutex::new(None),
                replay_gain: Mutex::new((ReplayGainMode::Off, 0.0)),
                loudness_seed: Mutex::new(Vec::new()),
                recorder: Mutex::new(None),
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

    /// Re-scan every **enabled and reachable** source, aggregate them into one
    /// library (compound `(source, local)` ids), swap it in so
    /// [`views`](Self::views) / [`art`](Self::art) reflect the union, and build a
    /// [`WatchUpdate`] whose [`sources`](WatchUpdate::sources) carries a
    /// [`SourceStatus`] for the *full* configured set — the single source of truth
    /// every public mutator funnels through.
    ///
    /// Robustness follows what mature players do with an offline location
    /// (foobar2000 "Error!", MusicBee ignore-removable, Roon "storage offline",
    /// Navidrome keep-missing): a root that is not a readable directory is
    /// **skipped** (checked at the ROOT, never by purging individual files), kept
    /// in the set so it re-aggregates when it returns, and flagged
    /// [`available == false`](SourceStatus::available). A disabled source is
    /// skipped the same way but flagged [`enabled == false`](SourceStatus::enabled).
    /// Either way it lists with a 0 `track_count`. An empty / all-disabled /
    /// all-unavailable set yields an empty library, not an error.
    fn rescan(&self) -> anyhow::Result<WatchUpdate> {
        let cfg = self.inner.folders.lock().expect("folders mutex").clone();
        // Only enabled, reachable roots are scanned — checked at the ROOT so a
        // transient outage is skipped, never purged file-by-file.
        let to_scan: Vec<PathBuf> = cfg
            .iter()
            .filter(|f| f.enabled && f.path.is_dir())
            .map(|f| f.path.clone())
            .collect();
        let lib = Arc::new(Library::scan_many(&to_scan)?);
        let views = lib.views();
        let counts = lib.source_counts(); // (root, n) for the scanned roots only
        self.set_library(lib);
        // Status for the FULL configured set — disabled / unavailable sources are
        // listed too, with 0 tracks, so the caller can render and re-enable them.
        let sources: Vec<SourceStatus> = cfg
            .iter()
            .map(|f| {
                let available = f.path.is_dir();
                let track_count = if f.enabled && available {
                    counts
                        .iter()
                        .find(|(r, _)| *r == f.path)
                        .map(|(_, n)| *n)
                        .unwrap_or(0)
                } else {
                    0
                };
                SourceStatus {
                    path: f.path.clone(),
                    enabled: f.enabled,
                    available,
                    track_count,
                }
            })
            .collect();
        // Cache the computed status (with real counts) so `sources()` — the
        // startup UI seed — reports true counts, not a 0 placeholder.
        *self.inner.last_sources.lock().expect("last_sources mutex") = sources.clone();
        Ok(WatchUpdate { views, sources })
    }

    /// Seed the configured-source set from `(path, enabled)` pairs, replacing any
    /// current set, then [`rescan`](Self::rescan). Pairs are deduped preserving
    /// order (the first occurrence of a path wins). Use this at startup to restore
    /// the persisted set with each source's saved enabled flag.
    ///
    /// Does **not** change playback — managing what is configured is separate from
    /// what is playing; re-scanning re-bakes, so compound ids can shift and the
    /// caller should re-queue from the returned views if it wants.
    pub fn set_sources(&self, entries: &[(PathBuf, bool)]) -> anyhow::Result<WatchUpdate> {
        let mut set: Vec<FolderConfig> = Vec::with_capacity(entries.len());
        for (p, en) in entries {
            if !set.iter().any(|f| &f.path == p) {
                set.push(FolderConfig {
                    path: p.clone(),
                    enabled: *en,
                });
            }
        }
        *self.inner.folders.lock().expect("folders mutex") = set;
        self.rescan()
    }

    /// Reconcile the configured-source set to exactly `roots` (deduped, preserving
    /// order), then [`rescan`](Self::rescan). The **enabled flag** of any path
    /// already configured is **preserved**; a newly-introduced path defaults to
    /// enabled. Paths absent from `roots` are dropped.
    ///
    /// Does **not** change playback — see [`set_sources`](Self::set_sources) for
    /// the re-bake caveat.
    pub fn set_folders(&self, roots: &[PathBuf]) -> anyhow::Result<WatchUpdate> {
        let prev = self.inner.folders.lock().expect("folders mutex").clone();
        let mut set: Vec<FolderConfig> = Vec::with_capacity(roots.len());
        for r in roots {
            if !set.iter().any(|f| &f.path == r) {
                let enabled = prev
                    .iter()
                    .find(|f| &f.path == r)
                    .map(|f| f.enabled)
                    .unwrap_or(true);
                set.push(FolderConfig {
                    path: r.clone(),
                    enabled,
                });
            }
        }
        *self.inner.folders.lock().expect("folders mutex") = set;
        self.rescan()
    }

    /// Add `dir` to the configured set (no-op if already configured, preserving
    /// its flag) and re-aggregate. A freshly-added source is enabled by default.
    pub fn add_folder(&self, dir: &Path) -> anyhow::Result<WatchUpdate> {
        let mut set = self.watched_folders();
        let dir = dir.to_path_buf();
        if !set.contains(&dir) {
            set.push(dir);
        }
        self.set_folders(&set)
    }

    /// Remove `dir` from the configured set (no-op if not configured) and
    /// re-aggregate.
    pub fn remove_folder(&self, dir: &Path) -> anyhow::Result<WatchUpdate> {
        let mut set = self.watched_folders();
        set.retain(|p| p != dir);
        self.set_folders(&set)
    }

    /// Flip the enabled flag of the configured source at `dir` (no-op if `dir`
    /// isn't configured) and [`rescan`](Self::rescan). Disabling a source drops
    /// its tracks from the library but keeps the source listed, so it can be
    /// re-enabled without re-adding it.
    pub fn set_folder_enabled(&self, dir: &Path, enabled: bool) -> anyhow::Result<WatchUpdate> {
        {
            let mut g = self.inner.folders.lock().expect("folders mutex");
            if let Some(f) = g.iter_mut().find(|f| f.path == dir) {
                f.enabled = enabled;
            }
        }
        self.rescan()
    }

    /// The configured sources as [`SourceStatus`], in order — a cheap listing that
    /// does **not** re-scan: it returns the status cached by the most recent
    /// [`rescan`](Self::rescan), so [`track_count`](SourceStatus::track_count)
    /// carries the real per-source counts from the last aggregation (ideal for
    /// seeding UI state at startup). Falls back to a count-0 listing of the
    /// configured set (with live `is_dir` availability) only before the first scan
    /// — e.g. if the most recent scan errored.
    pub fn sources(&self) -> Vec<SourceStatus> {
        let cached = self
            .inner
            .last_sources
            .lock()
            .expect("last_sources mutex")
            .clone();
        if !cached.is_empty() {
            return cached;
        }
        self.inner
            .folders
            .lock()
            .expect("folders mutex")
            .iter()
            .map(|f| SourceStatus {
                path: f.path.clone(),
                enabled: f.enabled,
                available: f.path.is_dir(),
                track_count: 0,
            })
            .collect()
    }

    /// The configured source paths, in order, including any currently disabled or
    /// unavailable (so a source is never lost to a toggle or a transient outage).
    pub fn watched_folders(&self) -> Vec<PathBuf> {
        self.inner
            .folders
            .lock()
            .expect("folders mutex")
            .iter()
            .map(|f| f.path.clone())
            .collect()
    }

    /// Choose the output device the next playback session opens, by name (one of
    /// [`output_devices`](Self::output_devices)); `None` (or an empty string,
    /// normalized to `None`) means the system default. If the named device is gone
    /// when the sink is built, playback falls back to the system default.
    ///
    /// This applies to the **next** actor spawn — the next `open_folder` /
    /// `set_folders`-then-play. A live device switch mid-playback (respawning the
    /// running actor onto the new device) is a deliberate follow-up, not done here:
    /// the current stream keeps playing on its original device until the next
    /// session starts.
    pub fn set_output_device(&self, name: Option<String>) {
        let name = name.filter(|n| !n.is_empty());
        *self.inner.output_device.lock().expect("device mutex") = name;
    }

    /// The available output device names on this host, for a device picker. Empty
    /// if the host exposes none (e.g. a headless CI machine). Delegates to
    /// [`soul_audio_desktop::output_device_names`].
    pub fn output_devices(&self) -> Vec<String> {
        soul_audio_desktop::output_device_names()
    }

    /// The current system-default output device name, if any — to label / preselect
    /// the "system default" choice. Delegates to
    /// [`soul_audio_desktop::default_output_device_name`].
    pub fn default_output_device(&self) -> Option<String> {
        soul_audio_desktop::default_output_device_name()
    }

    /// Set the ReplayGain playback normalization: `mode` picks track vs album (vs
    /// off) gain, `preamp_db` is a constant dB offset applied on top. Remembered
    /// so the NEXT playback session starts already normalized (the stored value is
    /// sent to the actor on spawn), AND — unlike the output device — applied to the
    /// LIVE actor right away if one is running, so a change in Settings is heard on
    /// the current track at its next pump cycle. Safe to call before any folder is
    /// opened: it just records the choice (no actor is spawned just to set it).
    pub fn set_replay_gain(&self, mode: ReplayGainMode, preamp_db: f32) {
        *self.inner.replay_gain.lock().expect("replay_gain mutex") = (mode, preamp_db);
        // Apply to a live actor only — never spawn one just to set the gain (that
        // would open a device stream before the user has played anything).
        if let Some(actor) = self.inner.player.lock().expect("player mutex").as_ref() {
            actor.send(Command::SetReplayGain { mode, preamp_db });
        }
    }

    /// Seed the backend's in-memory loudness cache from the app's persisted cache
    /// — `(path, computed track_gain_db)` pairs for previously-untagged tracks the
    /// hybrid-ReplayGain path analyzed in a past session. Call once at startup
    /// (before the first playback). Banked so the (lazily-spawned, or later
    /// re-spawned) actor is re-seeded on spawn, and pushed to a live actor right
    /// away if one is already running. Like [`set_replay_gain`](Self::set_replay_gain),
    /// it never spawns an actor just to seed.
    pub fn seed_loudness(&self, entries: Vec<(PathBuf, f32)>) {
        *self
            .inner
            .loudness_seed
            .lock()
            .expect("loudness seed mutex") = entries.clone();
        if let Some(actor) = self.inner.player.lock().expect("player mutex").as_ref() {
            actor.send(Command::SeedLoudness(entries));
        }
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
        // Test-support: a recording handle captures the command instead of driving
        // a live actor, so a head's command wiring can be asserted without a
        // backend. A no-op for every real handle (recorder is `None`).
        if let Some(rec) = self.inner.recorder.lock().expect("recorder mutex").as_mut() {
            rec.push(cmd);
            return;
        }
        self.ensure_actor();
        if let Some(actor) = self.inner.player.lock().expect("player mutex").as_ref() {
            actor.send(cmd);
        }
    }

    /// If `SOUL_PLAYER_FOLDER` is set, load it as the watched source set — the
    /// shared launch harness for both heads. Uses `set_folders`, not
    /// `open_folder`, so the library populates WITHOUT autoplaying (an app
    /// shouldn't start playing on its own at launch); `open_folder` keeps its
    /// play-from-0 for callers that want it.
    pub fn autoload_from_env(&self) -> anyhow::Result<()> {
        if let Some(dir) = std::env::var_os("SOUL_PLAYER_FOLDER") {
            self.set_folders(&[PathBuf::from(dir)])?;
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
        // Snapshot the chosen device for this spawn — the `Device` arm opens it (or
        // falls back to the system default); a later `set_output_device` only takes
        // effect on the NEXT spawn, never this live stream.
        let device = self
            .inner
            .output_device
            .lock()
            .expect("device mutex")
            .clone();
        // Snapshot the ReplayGain choice the same way, then tell the fresh actor so
        // the session starts already normalized. Lock ordering: this holds `guard`
        // (player) then briefly `replay_gain`; `set_replay_gain` never holds
        // `replay_gain` while taking `player`, so the two can't deadlock.
        let (rg_mode, rg_preamp) = *self.inner.replay_gain.lock().expect("replay_gain mutex");
        // Snapshot the persisted loudness cache the same way, to re-seed the fresh
        // actor so previously-analyzed untagged tracks are normalized immediately.
        let loudness_seed = self
            .inner
            .loudness_seed
            .lock()
            .expect("loudness seed mutex")
            .clone();
        let actor = sink::build_actor(self.inner.mode, device, lib, forward);
        actor.send(Command::SetReplayGain {
            mode: rg_mode,
            preamp_db: rg_preamp,
        });
        if !loudness_seed.is_empty() {
            actor.send(Command::SeedLoudness(loudness_seed));
        }
        *guard = Some(actor);
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
    fn a_recording_handle_captures_sent_commands_in_order() {
        let h = PlayerHandle::recording();
        assert!(h.recorded_commands().is_empty(), "nothing sent yet");
        h.send(Command::PlayNext(7));
        h.send(Command::ClearQueue);
        assert_eq!(
            h.recorded_commands(),
            vec![Command::PlayNext(7), Command::ClearQueue],
            "a recording handle captures commands in send order"
        );
        // A normal handle never records (recorder is None).
        let normal = PlayerHandle::offline();
        normal.send(Command::ClearQueue);
        assert!(
            normal.recorded_commands().is_empty(),
            "a normal handle records nothing"
        );
    }

    #[test]
    fn send_before_open_is_a_noop() {
        let h = PlayerHandle::offline();
        h.send(Command::TogglePlay); // no library yet → no actor → no panic
        assert!(h.views().is_empty());
    }

    #[test]
    fn output_device_selection_is_set_without_panicking() {
        // The device picker's host surface: enumerate, read the default, and store a
        // choice. None of these open a stream (the Offline handle never spawns a
        // CpalSink), so this runs device-free in CI — it just asserts the plumbing
        // doesn't panic and tolerates a host with no output devices (empty list).
        let h = PlayerHandle::offline();
        let _names = h.output_devices(); // may be empty on a headless runner
        let _default = h.default_output_device();
        h.set_output_device(Some("Some Soundcard".to_string()));
        h.set_output_device(Some(String::new())); // empty normalizes to the default
        h.set_output_device(None);
    }

    #[test]
    fn set_replay_gain_before_open_is_remembered_without_spawning() {
        // Recording the choice must not spawn an actor (no device stream before
        // playback) and must not panic — it is just stored for the next spawn.
        let h = PlayerHandle::offline();
        h.set_replay_gain(ReplayGainMode::Album, -3.0);
        assert_eq!(
            *h.inner.replay_gain.lock().unwrap(),
            (ReplayGainMode::Album, -3.0),
            "the choice is remembered"
        );
        assert!(
            h.inner.player.lock().unwrap().is_none(),
            "no actor is spawned just to set the gain"
        );
        // A live change on a running actor also just flows through (no panic):
        // open a folder (spawns the actor), then set it again.
        h.set_replay_gain(ReplayGainMode::Track, 6.0);
        assert_eq!(
            *h.inner.replay_gain.lock().unwrap(),
            (ReplayGainMode::Track, 6.0)
        );
    }

    #[test]
    fn seed_loudness_before_open_is_remembered_without_spawning() {
        // Seeding the loudness cache must not spawn an actor (no device stream
        // before playback) and must not panic — it's banked for the next spawn.
        let h = PlayerHandle::offline();
        h.seed_loudness(vec![
            (PathBuf::from("/music/a.flac"), -6.0),
            (PathBuf::from("/music/b.wav"), 2.5),
        ]);
        assert_eq!(
            h.inner.loudness_seed.lock().unwrap().len(),
            2,
            "the seed is remembered for the next spawn"
        );
        assert!(
            h.inner.player.lock().unwrap().is_none(),
            "no actor is spawned just to seed the cache"
        );
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

    /// Wait (bounded by WALL-CLOCK, so a continuous tick stream can't spin this
    /// forever) for a `Tick` the predicate accepts. Returns the matching tick.
    fn wait_for_tick(
        rx: &std::sync::mpsc::Receiver<Event>,
        pred: impl Fn(&Tick) -> bool,
    ) -> Option<Tick> {
        use std::time::{Duration, Instant};
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(Event::Tick(t)) if pred(&t) => return Some(t),
                Ok(_) => {}
                Err(_) => {}
            }
        }
        None
    }

    #[test]
    fn open_folder_plays_reports_duration_and_forwards_seek() {
        use soul_player_backend::PlayState;
        use std::sync::mpsc;

        // open_folder → play → seek → event, end to end through the handle. The
        // offline (NullSink) pump is unthrottled, so exact post-seek POSITION
        // isn't deterministically observable here (a track plays to EOF within one
        // ~8 Hz tick, and a load transitions the deck to Playing regardless of a
        // pre-load pause) — that precision is covered deterministically in the
        // backend (tests/seek_audit.rs + driver's seek_clamps_at_track_bounds).
        // What IS robustly observable, and what this asserts, is the host wiring:
        // open_folder spawns the actor, playback starts, a Tick carries the
        // correct DURATION (the seek-bar denominator), and a Seek sent through the
        // handle flows handle → actor → deck without breaking the event stream.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.wav"), wav(48_000, 48_000 * 10)).unwrap(); // 10 s

        let h = PlayerHandle::offline();
        let (tx, rx) = mpsc::channel::<Event>();
        h.subscribe(move |ev| {
            let _ = tx.send(ev);
        });
        h.open_folder(dir.path()).expect("open_folder");

        // A Tick must show track 0 playing with a ~10 s duration (decoder length,
        // or the library-tag fallback the actor applies). Duration is
        // timing-independent, so this is deterministic despite the fast pump.
        let playing = wait_for_tick(&rx, |t| {
            t.now_playing_id == Some(0) && t.state == PlayState::Playing
        });
        let t = playing.expect("never observed track 0 playing");
        let dur = t.duration_secs.expect("playing tick carries a duration");
        assert!(
            (dur - 10.0).abs() < 0.3,
            "tick duration {dur:.3}s is not ~10 s (the seek-bar denominator)"
        );

        // Forward a Seek through the handle and confirm the actor stays live
        // (still emitting events) — i.e. the handle → actor → apply_command(Seek)
        // path neither panics nor deadlocks the event fan-out.
        h.send(Command::Seek { secs: 3.0 });
        let still_alive = wait_for_tick(&rx, |_| true);
        assert!(
            still_alive.is_some(),
            "no events after Seek — the handle→actor command path stalled"
        );
    }

    // --- multi-source watched folders ---

    fn folder_with(name: &str, wavs: &[&str]) -> tempfile::TempDir {
        let d = tempfile::Builder::new().prefix(name).tempdir().unwrap();
        for w in wavs {
            std::fs::write(d.path().join(w), wav(48_000, 96_000)).unwrap();
        }
        d
    }

    /// The configured-but-unreachable roots of a `WatchUpdate`, in order — the
    /// new-shape equivalent of the removed `WatchUpdate::unavailable` field.
    fn unavailable_of(up: &WatchUpdate) -> Vec<PathBuf> {
        up.sources
            .iter()
            .filter(|s| !s.available)
            .map(|s| s.path.clone())
            .collect()
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
        assert!(unavailable_of(&up).is_empty(), "both roots are reachable");
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
            unavailable_of(&up),
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
        assert_eq!(unavailable_of(&up), vec![m1.clone(), m2.clone()]);
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

    // --- per-source status (enabled / available / track_count) ---

    #[test]
    fn source_status_reports_counts_available_and_enabled() {
        // A real root with two tracks + a missing path, seeded together as enabled
        // sources: the real one is available with its exact count, the missing one
        // is still listed (enabled) but unavailable with zero tracks.
        let a = folder_with("stat_real", &["a1.wav", "a2.wav"]);
        let real = a.path().to_path_buf();
        let missing = a.path().join("does-not-exist");

        let h = PlayerHandle::offline();
        let up = h
            .set_sources(&[(real.clone(), true), (missing.clone(), true)])
            .unwrap();
        assert_eq!(up.sources.len(), 2, "both sources are listed");

        let sa = up
            .sources
            .iter()
            .find(|s| s.path == real)
            .expect("real source listed");
        assert!(sa.enabled, "real source is enabled");
        assert!(sa.available, "real source is a readable dir");
        assert_eq!(sa.track_count, 2, "real source contributed its two tracks");

        let sm = up
            .sources
            .iter()
            .find(|s| s.path == missing)
            .expect("missing source listed");
        assert!(sm.enabled, "missing source stays enabled");
        assert!(!sm.available, "missing source is unavailable");
        assert_eq!(
            sm.track_count, 0,
            "an unavailable source contributes nothing"
        );
    }

    #[test]
    fn sources_reports_cached_counts_after_a_scan() {
        // After a scan, the cheap `sources()` listing (used to seed UI state at
        // startup) must carry the real per-source counts, not a 0 placeholder —
        // otherwise a fresh launch shows "0 tracks" until the first rescan.
        let a = folder_with("stat_cache", &["a1.wav", "a2.wav"]);
        let h = PlayerHandle::offline();
        h.set_sources(&[(a.path().to_path_buf(), true)]).unwrap();

        let got = h.sources();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].track_count, 2, "cached count, not 0");
        assert!(got[0].enabled && got[0].available);
    }

    #[test]
    fn disabled_source_contributes_no_tracks_but_stays_listed() {
        // Seed one enabled source, then disable it: the library empties, but the
        // source is never dropped — it stays listed as disabled with zero tracks.
        let a = folder_with("stat_disable", &["a1.wav", "a2.wav"]);
        let real = a.path().to_path_buf();

        let h = PlayerHandle::offline();
        h.set_sources(&[(real.clone(), true)]).unwrap();

        let up = h.set_folder_enabled(&real, false).unwrap();
        assert!(up.views.is_empty(), "a disabled source yields no views");

        let sa = up
            .sources
            .iter()
            .find(|s| s.path == real)
            .expect("source still listed");
        assert!(!sa.enabled, "the source is now disabled");
        assert_eq!(sa.track_count, 0, "a disabled source contributes no tracks");
        assert_eq!(h.sources().len(), 1, "the source is kept in the set");
    }

    #[test]
    fn set_folders_preserves_enabled_flags() {
        // Disable `a`, then reconcile to `[a, b]` via the flag-agnostic
        // `set_folders`: `a` keeps its disabled flag; the new `b` defaults enabled.
        let a = folder_with("pres_a", &["a.wav"]);
        let b = folder_with("pres_b", &["b.wav"]);

        let h = PlayerHandle::offline();
        h.set_sources(&[(a.path().to_path_buf(), true)]).unwrap();
        h.set_folder_enabled(a.path(), false).unwrap();

        let up = h
            .set_folders(&[a.path().to_path_buf(), b.path().to_path_buf()])
            .unwrap();
        assert!(
            !up.sources
                .iter()
                .find(|s| s.path == a.path())
                .expect("a listed")
                .enabled,
            "a keeps its disabled flag across set_folders"
        );
        assert!(
            up.sources
                .iter()
                .find(|s| s.path == b.path())
                .expect("b listed")
                .enabled,
            "a newly-added source defaults to enabled"
        );
    }
}
