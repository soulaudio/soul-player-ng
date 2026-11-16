//! Desktop assembly over `soul-player-backend`: picks the audio sink, owns the
//! live `Library`, fans backend events out to subscribers, and exposes the one
//! `PlayerHandle` the Rune UI head binds to.
#![forbid(unsafe_code)]

pub mod analyzer;
pub mod eq;
mod sink;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use notify_debouncer_mini::notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{DebounceEventResult, Debouncer, new_debouncer};
use soul_player_backend::{AudioPlayer, IncrementalBuild, Library, cover_bytes};
use sysinfo::{MINIMUM_CPU_UPDATE_INTERVAL, System};

pub use analyzer::{SPECTRUM_COLS, SpectrumFrame, SpectrumShared};
pub use eq::{
    Band, BandKind, DspChain, DspKind, DspShared, DspUnit, EqUnit, FREQ_MAX, FREQ_MIN,
    GAIN_LIMIT_DB, MAX_BANDS, Profile, ProfileSet, Q_MAX, Q_MIN, band_magnitude_db, default_bands,
    eq_magnitude_db, format_autoeq, parse_autoeq,
};
pub use sink::{ExclusiveStatus, SinkMode};
/// The portable audio-profile exchange format (the `*Cfg` serde structs), re-exported
/// so a UI head reaches it through the one host contract. The desktop persists/exports
/// through these; the DAP reads the same shapes.
pub use soul_audio_dsp::format;
/// The write-path "managed library" organizer (stateless `plan`/`apply` into a
/// path-template tree), re-exported so a UI head calls it through the one host
/// contract rather than taking its own core git dependency.
pub use soul_audio_organize as organize;
pub use soul_player_backend::{
    BrowseAxis, Command, Cover, Event, PlayState, QueueState, RepeatMode, ReplayGainMode,
    SCAN_CACHE_VERSION, ScanCache, ScanPhase, ScanProgress, SignalPath, SourceCodec, SourceFormat,
    Tick, TrackLoudness, TrackView,
};

type Subscriber = Box<dyn Fn(Event) + Send>;
/// A handler for a background (watcher-triggered) library refresh (see
/// [`PlayerHandle::on_watch_update`]).
type WatchSubscriber = Box<dyn Fn(WatchUpdate) + Send>;
/// A handler for live scan progress: `Some(p)` per tick *during* a rescan, then a
/// single terminal `None` when it finishes (success or error), so the UI can show
/// and then reliably retire a progress indicator (see
/// [`PlayerHandle::on_scan_progress`]).
type ScanProgressSubscriber = Box<dyn Fn(Option<ScanProgress>) + Send>;
/// Deletions from one rescan: `(root, removed file paths)` for each root that lost
/// files since its prior cache.
type Deletions = Vec<(PathBuf, Vec<PathBuf>)>;

/// Debounce window for the live filesystem watcher: filesystem events for one
/// logical change (an editor's write-rename, an album copy's many files) are
/// coalesced into a single rescan this long after the last event in the burst.
/// Long enough to collapse a burst, short enough to feel live.
const WATCH_DEBOUNCE: Duration = Duration::from_millis(600);

/// A watcher event path that can't change the library — editor temp / partial-
/// download files and OS metadata sidecars. When a whole debounced batch is noise,
/// the watcher skips the rescan. Deliberately CONSERVATIVE: only confirmed junk is
/// noise, so a real change (including a cover-art image) is never skipped.
fn is_noise(path: &Path) -> bool {
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        let n = name.to_ascii_lowercase();
        // OS metadata sidecars and AppleDouble (`._foo`) resource forks.
        if matches!(n.as_str(), ".ds_store" | "thumbs.db" | "desktop.ini") || n.starts_with("._") {
            return true;
        }
    }
    // Temp / partial-download / editor-swap / lock extensions.
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some(
            "tmp"
                | "temp"
                | "part"
                | "partial"
                | "crdownload"
                | "download"
                | "swp"
                | "swx"
                | "swo"
                | "bak"
                | "lock"
        )
    )
}

/// Whether `path` is inside `root` (or is `root`), compared on component boundaries
/// (so `…/rock` never matches `…/rockabilly`) and case-insensitively on Windows
/// (whose filesystem is), so an event path's casing can't hide a real match.
fn path_under(path: &Path, root: &Path) -> bool {
    let mut rc = root.components();
    let mut pc = path.components();
    loop {
        match rc.next() {
            None => return true, // every component of `root` matched → `path` is within it
            Some(r) => match pc.next() {
                None => return false, // `path` is shorter than `root`
                Some(p) => {
                    let same = if cfg!(windows) {
                        p.as_os_str().eq_ignore_ascii_case(r.as_os_str())
                    } else {
                        p == r
                    };
                    if !same {
                        return false;
                    }
                }
            },
        }
    }
}

/// The configured roots that contain at least one of `paths` — the set a watcher
/// burst actually touched, so only these need re-reading (the rest are reused). An
/// empty result means no root was affected.
fn affected_roots(paths: &[PathBuf], roots: &[PathBuf]) -> std::collections::HashSet<PathBuf> {
    let mut set = std::collections::HashSet::new();
    for p in paths {
        for r in roots {
            if path_under(p, r) {
                set.insert(r.clone());
            }
        }
    }
    set
}

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

/// How hard the library scanner may work — the user-facing "indexing speed"
/// knob. Resolves to an extraction thread count ([`resolve_threads`](IndexingMode::resolve_threads))
/// each scan, so [`Automatic`](IndexingMode::Automatic) can follow the machine's
/// live CPU load.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IndexingMode {
    /// Follow the machine: scan fast when the CPU is idle, ease off when it's busy.
    /// Reads current CPU load (a brief sample) and scales parallelism to the idle
    /// headroom — the recommended default a UI should offer.
    #[default]
    Automatic,
    /// Always oversubscribe for the quickest scan (best for a big first import);
    /// may briefly load the machine.
    Maximum,
    /// Bounded, low parallelism — gentle on an older machine or a network share
    /// (a NAS), where firing many concurrent reads is counter-productive.
    Gentle,
}

impl IndexingMode {
    /// The config token persisted for this mode (stable across releases).
    #[must_use]
    pub fn as_config(self) -> &'static str {
        match self {
            IndexingMode::Automatic => "auto",
            IndexingMode::Maximum => "max",
            IndexingMode::Gentle => "gentle",
        }
    }

    /// Parse a persisted token back to a mode, defaulting to
    /// [`Automatic`](IndexingMode::Automatic) for anything unknown/missing.
    #[must_use]
    pub fn from_config(s: &str) -> Self {
        match s {
            "max" => IndexingMode::Maximum,
            "gentle" => IndexingMode::Gentle,
            _ => IndexingMode::Automatic,
        }
    }

    /// The extraction thread count to **start** a scan with for this mode *right
    /// now*. `Maximum` oversubscribes the cores (hiding I/O latency); `Gentle` stays
    /// low; `Automatic` samples the current CPU load and scales between the two. For
    /// `Automatic` this is only the *initial* `target` — a [`LoadSampler`] then keeps
    /// it tracking load for the rest of the scan (see [`ceiling`](Self::ceiling)).
    #[must_use]
    pub fn resolve_threads(self) -> usize {
        match self {
            IndexingMode::Maximum => self.ceiling(),
            IndexingMode::Gentle => 2,
            // Idle machine → up to 4× cores; busy → ~0.25× cores. "Mostly idle"
            // fallback if the load can't be read.
            IndexingMode::Automatic => {
                threads_for_idle(cores(), cpu_idle_fraction().unwrap_or(0.75))
            }
        }
    }

    /// The worker **ceiling** for this mode — how many extraction threads the pool
    /// may spawn. `Automatic` shares `Maximum`'s ceiling; its live `target` varies
    /// underneath (down to 2 on a busy machine, up to here when idle).
    #[must_use]
    pub fn ceiling(self) -> usize {
        match self {
            IndexingMode::Gentle => 2,
            IndexingMode::Maximum | IndexingMode::Automatic => {
                (cores() * 4).clamp(4, MAX_SCAN_THREADS)
            }
        }
    }
}

/// Oversubscription cap for extraction parallelism: the I/O-bound small reads scale
/// with concurrency, but a huge core count shouldn't spawn an unreasonable pool.
const MAX_SCAN_THREADS: usize = 32;

/// Logical CPU count (≥1), defaulting to 4 if the platform won't say.
fn cores() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

/// Extraction threads for a given CPU idle `fraction` (0.0 = pegged, 1.0 = idle):
/// scales `0.25×…4×` cores, clamped to `[2, MAX_SCAN_THREADS]`. Pure — both the
/// initial [`resolve_threads`](IndexingMode::resolve_threads) and the mid-scan
/// [`LoadSampler`] go through this, so they stay consistent.
fn threads_for_idle(cores: usize, fraction: f32) -> usize {
    let factor = 0.25 + 3.75 * fraction.clamp(0.0, 1.0);
    ((cores.max(1) as f32 * factor).round() as usize).clamp(2, MAX_SCAN_THREADS)
}

/// A background thread that re-samples CPU load (~every [`MINIMUM_CPU_UPDATE_INTERVAL`])
/// and writes an updated extraction `target`, so an `Automatic` scan adapts **while
/// it runs**. Dropping it stops and joins the thread.
struct LoadSampler {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl LoadSampler {
    /// Spawn a sampler that keeps `target` tracking the idle headroom, never above
    /// `ceiling`. Runs until dropped (which stops + joins it, within one sample).
    fn spawn(target: Arc<AtomicUsize>, ceiling: usize) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let cores = cores();
        let handle = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                // ~200 ms per sample; loop to keep tracking the live load.
                if let Some(idle) = cpu_idle_fraction() {
                    target.store(
                        threads_for_idle(cores, idle).min(ceiling),
                        Ordering::Relaxed,
                    );
                }
            }
        });
        LoadSampler {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for LoadSampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// The fraction of CPU that is idle right now (0.0 = pegged, 1.0 = idle), or
/// `None` if it can't be sampled. Takes two CPU readings a short interval apart
/// (sysinfo needs the gap to compute a percentage), so it costs ~one
/// [`MINIMUM_CPU_UPDATE_INTERVAL`] (~200 ms) — only paid in `Automatic` mode.
fn cpu_idle_fraction() -> Option<f32> {
    let mut sys = System::new();
    sys.refresh_cpu_usage();
    std::thread::sleep(MINIMUM_CPU_UPDATE_INTERVAL);
    sys.refresh_cpu_usage();
    let usage = sys.global_cpu_usage();
    usage
        .is_finite()
        .then(|| (1.0 - usage / 100.0).clamp(0.0, 1.0))
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
    /// The per-source incremental scan caches, keyed by root path. A rescan reuses
    /// each root's cache (re-reading only new/changed files) and refreshes it; the
    /// app persists this map at exit and seeds it at startup
    /// ([`seed_caches`](PlayerHandle::seed_caches)) so a launch is a warm diff, not
    /// a cold full read. A configured-but-unscanned root (disabled / offline) keeps
    /// its cache for when it returns; a de-configured root's cache is pruned.
    caches: Mutex<HashMap<PathBuf, ScanCache>>,
    /// The per-source baked `.soulidx` bytes, keyed by root path — the last build's
    /// output per root. A scoped (watcher-triggered) rescan re-scans only the roots
    /// whose files changed and reuses the cached bytes of the rest verbatim (each
    /// root bakes independently, so the result is byte-identical to a full rescan).
    /// Kept in lock-step with `caches`: refreshed for re-scanned roots, pruned for
    /// de-configured ones.
    source_bytes: Mutex<HashMap<PathBuf, Vec<u8>>>,
    /// Subscribers for a background (watcher-triggered) rescan's [`WatchUpdate`], so
    /// the UI refreshes its library/sources when files change on disk while the app
    /// is open. Registered via [`on_watch_update`](PlayerHandle::on_watch_update);
    /// distinct from `subscribers` (backend playback events) because a `WatchUpdate`
    /// carries the host's own [`SourceStatus`], not a backend [`Event`].
    watch_subscribers: Mutex<Vec<WatchSubscriber>>,
    /// Subscribers for live scan progress, notified on every [`ScanProgress`] tick
    /// *during* a rescan (registered via [`on_scan_progress`](PlayerHandle::on_scan_progress)).
    /// The UI drives a progress indicator from these; they fire on the scanning
    /// thread, so only an off-UI-thread rescan (the watcher, a threaded add) shows
    /// live motion — a rescan that blocks the UI thread repaints only when it ends.
    scan_progress_subscribers: Mutex<Vec<ScanProgressSubscriber>>,
    /// The live filesystem watcher ([`start_watching`](PlayerHandle::start_watching)),
    /// kept alive here so its debounce thread runs; `None` until started. Replaced
    /// (re-armed over the new root set) whenever the configured sources change.
    watcher: Mutex<Option<Debouncer<RecommendedWatcher>>>,
    /// The library-scan "indexing speed" mode, resolved to a thread count each scan
    /// (so [`IndexingMode::Automatic`] follows live CPU load). Defaults to
    /// [`Maximum`](IndexingMode::Maximum) here — no CPU sampling until the app sets
    /// the user's persisted choice ([`set_indexing_mode`](PlayerHandle::set_indexing_mode),
    /// whose own default is `Automatic`) — which also keeps the test suite fast.
    indexing_mode: Mutex<IndexingMode>,
    /// The chosen output device name (one of [`output_devices`](PlayerHandle::output_devices)),
    /// or `None` for the system default. Read when the actor is (re)spawned and
    /// passed to the sink factory; a change applies to the NEXT spawn, not the
    /// live stream (see [`set_output_device`](PlayerHandle::set_output_device)).
    output_device: Mutex<Option<String>>,
    /// Whether the user asked for bit-perfect **exclusive** output. Read when the
    /// actor is (re)spawned and passed to the sink factory; a change applies to the
    /// NEXT spawn, not the live stream (like the output device). The factory records
    /// what the request actually resolved to in [`exclusive_status`](Inner::exclusive_status).
    exclusive: Mutex<bool>,
    /// **Strict** exclusive (exclusive-or-stop): when on with `exclusive`, the next
    /// session does NOT fall back to shared if exclusive can't open — playback simply
    /// doesn't start (the purist stance). Applies to the next spawn, like `exclusive`.
    exclusive_strict: Mutex<bool>,
    /// What the last exclusive-output request resolved to (active / fell back to
    /// shared with a reason / unavailable under strict / shared by choice). Written by
    /// the sink factory on the pump thread at spawn, read by the UI to show the truth
    /// — hence an `Arc` shared with the factory.
    exclusive_status: Arc<Mutex<ExclusiveStatus>>,
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
    loudness_seed: Mutex<Vec<(PathBuf, TrackLoudness)>>,
    /// Test-support: when `Some`, [`send`](PlayerHandle::send) records commands
    /// here instead of driving the actor, so a head's command wiring can be
    /// asserted headlessly. `None` (the default) in every real handle.
    recorder: Mutex<Option<Vec<Command>>>,
    /// The live DSP chain. Shared with the device sink's [`eq::DspSink`] (created
    /// per spawn) so a change applies to the LIVE stream at its next block — like
    /// ReplayGain, not the output device. Persists across actor respawns (device
    /// changes) because the same `Arc` is handed to each new sink.
    dsp: DspShared,
    /// The latest spectrum-analyzer snapshot, written by the sink (pump thread) and
    /// polled by the UI. The same `Arc` is handed to each new sink across respawns.
    spectrum: SpectrumShared,
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
                caches: Mutex::new(HashMap::new()),
                source_bytes: Mutex::new(HashMap::new()),
                watch_subscribers: Mutex::new(Vec::new()),
                scan_progress_subscribers: Mutex::new(Vec::new()),
                watcher: Mutex::new(None),
                indexing_mode: Mutex::new(IndexingMode::Maximum),
                output_device: Mutex::new(None),
                exclusive: Mutex::new(false),
                exclusive_strict: Mutex::new(false),
                exclusive_status: Arc::new(Mutex::new(ExclusiveStatus::Shared)),
                replay_gain: Mutex::new((ReplayGainMode::Off, 0.0)),
                loudness_seed: Mutex::new(Vec::new()),
                recorder: Mutex::new(None),
                dsp: Arc::new(Mutex::new(DspChain::default())),
                spectrum: Arc::new(Mutex::new(SpectrumFrame::default())),
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
        let (up, removed) = self.rescan_compute()?;
        // Warn about genuine deletions after the scan: each scanned (hence
        // available) root reports files gone since its prior cache. An offline root
        // was never scanned, so it contributes nothing (no mass-removal when a drive
        // is unplugged). The live watcher emits these AFTER broadcasting the library
        // refresh (see [`start_watching`]), so the deletion warning always lands
        // once the list already reflects the removal.
        for (root, gone) in &removed {
            self.emit(Event::TracksRemoved {
                root: root.clone(),
                count: gone.len(),
            });
        }
        // A user source change (set_sources / set_folders / toggle) funnels through
        // here; re-arm the live watcher over the new root set if it is running. The
        // watcher's own background rescans call `rescan_compute`, not this, so they
        // never re-arm (and never recurse).
        self.rearm_watcher();
        Ok(up)
    }

    /// The enabled **and currently reachable** source roots, in config order — the
    /// set a scan actually reads. Availability is checked at the ROOT so a transient
    /// outage skips the whole source rather than purging it file-by-file.
    fn enabled_reachable_roots(&self) -> Vec<PathBuf> {
        self.inner
            .folders
            .lock()
            .expect("folders mutex")
            .iter()
            .filter(|f| f.enabled && f.path.is_dir())
            .map(|f| f.path.clone())
            .collect()
    }

    /// A full rescan of every enabled + reachable root — the entry for a source
    /// change, a manual Rescan, and the deferred startup scan. See
    /// [`rescan_build`](Self::rescan_build).
    fn rescan_compute(&self) -> anyhow::Result<(WatchUpdate, Deletions)> {
        self.rescan_build(None)
    }

    /// A **scoped** rescan: re-read only the roots in `affected` (that are enabled +
    /// reachable) and reuse every other root's cached baked bytes verbatim — the
    /// watcher's smart path. Byte-identical to a full rescan because each root bakes
    /// independently. A root in `affected` (or any root with no cached bytes to
    /// reuse) is re-scanned.
    fn rescan_scoped(
        &self,
        affected: &std::collections::HashSet<PathBuf>,
    ) -> anyhow::Result<(WatchUpdate, Deletions)> {
        self.rescan_build(Some(affected))
    }

    /// The scan itself, **without emitting any event**: (incrementally) re-read the
    /// roots this call scans — all enabled + reachable roots when `affected` is
    /// `None`, else only those roots — swap in the aggregated library, refresh the
    /// per-source caches + baked bytes, and build the [`WatchUpdate`] plus the
    /// per-root deletions. Split out from [`rescan`](Self::rescan) so the live watcher
    /// can broadcast the library refresh *before* the deletion warning (the
    /// synchronous path emits inline right after, ordering the same way).
    fn rescan_build(
        &self,
        affected: Option<&std::collections::HashSet<PathBuf>>,
    ) -> anyhow::Result<(WatchUpdate, Deletions)> {
        use std::collections::HashSet;
        let cfg = self.inner.folders.lock().expect("folders mutex").clone();
        let to_scan: Vec<PathBuf> = cfg
            .iter()
            .filter(|f| f.enabled && f.path.is_dir())
            .map(|f| f.path.clone())
            .collect();
        // Which of `to_scan` do we actually re-read? All of them for a full rescan;
        // for a scoped one, only the affected roots plus any that have no cached bytes
        // to reuse. The rest are rebuilt from their cached baked bytes.
        let rescan_mask: Vec<bool> = {
            let bytes = self.inner.source_bytes.lock().expect("source_bytes mutex");
            to_scan
                .iter()
                .map(|r| match affected {
                    None => true,
                    Some(set) => set.contains(r) || !bytes.contains_key(r),
                })
                .collect()
        };
        let rescan_roots: Vec<PathBuf> = to_scan
            .iter()
            .zip(&rescan_mask)
            .filter(|(_, m)| **m)
            .map(|(r, _)| r.clone())
            .collect();
        // Warm priors for the roots we scan: each root's cached scan state (empty =
        // a cold scan), so only new/changed files are re-read.
        let priors: Vec<ScanCache> = {
            let caches = self.inner.caches.lock().expect("caches mutex");
            rescan_roots
                .iter()
                .map(|r| caches.get(r).cloned().unwrap_or_default())
                .collect()
        };
        // Resolve the extraction parallelism for the current indexing mode. Copy the
        // mode out of the lock first: the initial sample can sleep ~200 ms and we
        // don't want to block a concurrent mode change.
        let mode = *self
            .inner
            .indexing_mode
            .lock()
            .expect("indexing_mode mutex");
        let ceiling = mode.ceiling();
        // A shared, live concurrency target the extractor re-reads per file. For
        // Automatic, a sampler keeps it tracking CPU load FOR THE DURATION of the
        // scan — a scan that starts idle but hits load mid-run throttles itself (and
        // vice-versa). Fixed modes leave it constant (no sampler).
        let target = Arc::new(AtomicUsize::new(mode.resolve_threads().min(ceiling)));
        let sampler = matches!(mode, IndexingMode::Automatic)
            .then(|| LoadSampler::spawn(Arc::clone(&target), ceiling));
        let build =
            Library::scan_many_incremental(&rescan_roots, &priors, ceiling, &target, &mut |p| {
                self.broadcast_scan_progress(Some(p))
            });
        // Terminal tick: the scan is over (whether it succeeded or errored), so retire
        // any progress indicator. Emitted on this same ordered stream *after* every
        // `Some` tick, so the UI's show→hide can't be left stuck.
        self.broadcast_scan_progress(None);
        let IncrementalBuild {
            library: scanned,
            caches: fresh,
            removed,
            bytes: fresh_bytes,
        } = build?;
        drop(sampler); // stop the sampler as soon as the scan is done

        // Assemble the final library + the per-root bytes to cache. A full rescan IS
        // the scanned library; a scoped one interleaves the freshly-baked bytes of the
        // re-scanned roots with the cached bytes of the reused ones, in `to_scan`
        // order, and rebuilds the aggregate from them.
        let (library, store_bytes): (Library, Vec<(PathBuf, Vec<u8>)>) = if affected.is_none() {
            let store = to_scan
                .iter()
                .cloned()
                .zip(fresh_bytes.iter().cloned())
                .collect();
            (scanned, store)
        } else {
            let cached = self.inner.source_bytes.lock().expect("source_bytes mutex");
            let mut fresh_iter = fresh_bytes.into_iter();
            let mut merged: Vec<Vec<u8>> = Vec::with_capacity(to_scan.len());
            let mut store: Vec<(PathBuf, Vec<u8>)> = Vec::new();
            for (root, &scanned_here) in to_scan.iter().zip(&rescan_mask) {
                if scanned_here {
                    let b = fresh_iter.next().expect("fresh bytes per re-scanned root");
                    store.push((root.clone(), b.clone()));
                    merged.push(b);
                } else {
                    // Reuse verbatim — the mask is false only when bytes exist.
                    merged.push(
                        cached
                            .get(root)
                            .cloned()
                            .expect("cached bytes for a reused root"),
                    );
                }
            }
            drop(cached);
            (Library::from_source_bytes(&to_scan, merged), store)
        };

        let configured: HashSet<&PathBuf> = cfg.iter().map(|f| &f.path).collect();
        // Refresh the re-scanned roots' caches; keep configured-but-unscanned roots
        // warm (a disabled / offline source re-aggregates when it returns); prune
        // caches for roots no longer configured.
        {
            let mut caches = self.inner.caches.lock().expect("caches mutex");
            for (root, cache) in rescan_roots.iter().zip(fresh) {
                caches.insert(root.clone(), cache);
            }
            caches.retain(|p, _| configured.contains(p));
        }
        // Same bookkeeping for the baked-bytes cache (refresh re-scanned, prune gone).
        {
            let mut bytes = self.inner.source_bytes.lock().expect("source_bytes mutex");
            for (root, b) in store_bytes {
                bytes.insert(root, b);
            }
            bytes.retain(|p, _| configured.contains(p));
        }
        let lib = Arc::new(library);
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
        Ok((WatchUpdate { views, sources }, removed))
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

    /// Seed the configured-source set from `(path, enabled)` pairs **without
    /// scanning** — for a fast first paint. The window can render the Sources list
    /// immediately ([`sources`](Self::sources) falls back to a count-0 listing of the
    /// configured set before any scan), while the actual library scan runs off the
    /// main thread afterwards via [`rescan_sources`](Self::rescan_sources). Pairs are
    /// deduped preserving order (first occurrence of a path wins). Touches only the
    /// configured set — not the library, the caches, or the watcher.
    pub fn configure_sources(&self, entries: &[(PathBuf, bool)]) {
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
    }

    /// Rescan the already-configured source set and return the refreshed
    /// [`WatchUpdate`] — the public entry the app's deferred startup scan (and a
    /// manual Rescan) runs off the main thread. Same as the internal post-mutation
    /// rescan but without changing the configured set. Emits `TracksRemoved` for any
    /// deletions and re-arms the watcher if it is running.
    pub fn rescan_sources(&self) -> anyhow::Result<WatchUpdate> {
        self.rescan()
    }

    /// Seed the per-source scan caches (as persisted by a previous run), keyed by
    /// root path, **before** [`set_sources`](Self::set_sources) — so the first scan
    /// at startup is a warm diff (only new/changed files re-read) instead of a cold
    /// full read of the whole library. The caller restores this from disk and must
    /// drop any caches written by an incompatible importer build (compare the
    /// stored version against [`SCAN_CACHE_VERSION`]) before seeding. Replaces any
    /// current cache map.
    pub fn seed_caches(&self, caches: HashMap<PathBuf, ScanCache>) {
        *self.inner.caches.lock().expect("caches mutex") = caches;
    }

    /// The current per-source scan caches, keyed by root path — persist this (with
    /// [`SCAN_CACHE_VERSION`]) after scans so the next startup is warm (see
    /// [`seed_caches`](Self::seed_caches)). Holds one warm cache per configured
    /// root that has been scanned; de-configured roots are already pruned.
    #[must_use]
    pub fn caches_snapshot(&self) -> HashMap<PathBuf, ScanCache> {
        self.inner.caches.lock().expect("caches mutex").clone()
    }

    /// Set the library-scan [`IndexingMode`] (the "indexing speed" the user picks in
    /// Settings). Applies to the **next** scan — current scans keep their resolved
    /// thread count. The app restores the persisted choice at startup; call before
    /// (or any time after) the first scan.
    pub fn set_indexing_mode(&self, mode: IndexingMode) {
        *self
            .inner
            .indexing_mode
            .lock()
            .expect("indexing_mode mutex") = mode;
    }

    /// The current [`IndexingMode`] (for seeding the Settings UI).
    #[must_use]
    pub fn indexing_mode(&self) -> IndexingMode {
        *self
            .inner
            .indexing_mode
            .lock()
            .expect("indexing_mode mutex")
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

    /// Ask for bit-perfect **exclusive** output (WASAPI exclusive mode on Windows),
    /// or shared mode. Like [`set_output_device`](Self::set_output_device), this
    /// applies to the **next** playback session — the current stream keeps its mode
    /// until the next actor spawn. If the device can't do exclusive at its native
    /// format (in use, 16-bit-only, Bluetooth) or this build/platform has no
    /// exclusive backend, the next session falls back to shared automatically; read
    /// the outcome from [`exclusive_status`](Self::exclusive_status).
    pub fn set_exclusive(&self, on: bool) {
        *self.inner.exclusive.lock().expect("exclusive mutex") = on;
    }

    /// Whether exclusive output is currently requested (for seeding the Settings
    /// toggle). Whether it is actually *active* is a property of the live stream —
    /// see the signal path's `exclusive` flag and
    /// [`exclusive_status`](Self::exclusive_status).
    #[must_use]
    pub fn exclusive_requested(&self) -> bool {
        *self.inner.exclusive.lock().expect("exclusive mutex")
    }

    /// Set **strict** exclusive (exclusive-or-stop): when on together with
    /// [`set_exclusive`](Self::set_exclusive), the next session will NOT fall back to
    /// shared if exclusive can't open — playback simply doesn't start, and
    /// [`exclusive_status`](Self::exclusive_status) reports
    /// [`ExclusiveStatus::Unavailable`]. Applies to the next spawn, like the toggle.
    pub fn set_exclusive_strict(&self, on: bool) {
        *self.inner.exclusive_strict.lock().expect("strict mutex") = on;
    }

    /// Whether strict exclusive is requested (for seeding the Settings toggle).
    #[must_use]
    pub fn exclusive_strict(&self) -> bool {
        *self.inner.exclusive_strict.lock().expect("strict mutex")
    }

    /// What the most recent device spawn's exclusive request resolved to (active at
    /// a negotiated device format / fell back to shared with a reason / shared by
    /// choice). The sink factory writes this when it opens the device; before the
    /// first device spawn it is [`ExclusiveStatus::Shared`].
    #[must_use]
    pub fn exclusive_status(&self) -> ExclusiveStatus {
        self.inner
            .exclusive_status
            .lock()
            .expect("exclusive status mutex")
            .clone()
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

    /// Set the whole DSP chain (its ordered, typed DSP units). Like
    /// [`set_replay_gain`](Self::set_replay_gain), it applies to the LIVE stream at
    /// its next block — the device sink reads the shared chain each block — and is
    /// remembered for the next spawn, without ever spawning an actor just to set it.
    /// An empty / disabled / all-flat chain is a bit-exact passthrough. Each EQ
    /// unit's bands are truncated to [`MAX_BANDS`](eq::MAX_BANDS).
    pub fn set_dsp_chain(&self, mut units: Vec<eq::DspUnit>) {
        for unit in &mut units {
            if let Some(eq) = unit.as_eq_mut() {
                eq.bands.truncate(eq::MAX_BANDS);
            }
        }
        let mut chain = self.inner.dsp.lock().expect("dsp chain mutex");
        chain.units = units;
        chain.generation = chain.generation.wrapping_add(1);
    }

    /// The live DSP chain the device sink is applying (a clone of the shared state).
    /// Symmetric with [`set_dsp_chain`](Self::set_dsp_chain) — lets callers and tests
    /// read back exactly what the engine will apply at its next block, including the
    /// `generation` counter that tells the sink to re-design.
    #[must_use]
    pub fn dsp_chain(&self) -> eq::DspChain {
        self.inner.dsp.lock().expect("dsp chain mutex").clone()
    }

    /// The latest spectrum-analyzer snapshot `(seq, columns)`: `seq` bumps each FFT
    /// (0 if nothing has played yet), `columns` are smoothed dBFS levels on a
    /// 20 Hz…20 kHz log axis. The UI polls this at frame rate; an unchanged `seq`
    /// means no new frame. Cheap — a brief lock and a fixed-size copy.
    #[must_use]
    pub fn spectrum(&self) -> (u64, [f32; SPECTRUM_COLS]) {
        let frame = self.inner.spectrum.lock().expect("spectrum mutex");
        (frame.seq, frame.cols)
    }

    /// Seed the backend's in-memory loudness cache from the app's persisted cache
    /// — `(path, computed track_gain_db)` pairs for previously-untagged tracks the
    /// hybrid-ReplayGain path analyzed in a past session. Call once at startup
    /// (before the first playback). Banked so the (lazily-spawned, or later
    /// re-spawned) actor is re-seeded on spawn, and pushed to a live actor right
    /// away if one is already running. Like [`set_replay_gain`](Self::set_replay_gain),
    /// it never spawns an actor just to seed.
    pub fn seed_loudness(&self, entries: Vec<(PathBuf, TrackLoudness)>) {
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

    /// Fan one event out to every current subscriber — the host's own path to the
    /// same fan-out the actor's forwarder uses, for events the host originates
    /// (e.g. a rescan's [`Event::TracksRemoved`] deletion warning) rather than the
    /// playing actor. Holds only the subscribers lock, briefly.
    fn emit(&self, ev: Event) {
        for sub in self
            .inner
            .subscribers
            .lock()
            .expect("subscribers mutex")
            .iter()
        {
            sub(ev.clone());
        }
    }

    /// Register a handler for background (watcher-triggered) library refreshes. The
    /// app applies the [`WatchUpdate`] on its UI thread (library + Sources list), so
    /// on-disk changes show up live. See [`start_watching`](Self::start_watching).
    pub fn on_watch_update(&self, on_update: impl Fn(WatchUpdate) + Send + 'static) {
        self.inner
            .watch_subscribers
            .lock()
            .expect("watch_subscribers mutex")
            .push(Box::new(on_update));
    }

    /// Fan a background rescan's [`WatchUpdate`] out to the watch subscribers.
    fn broadcast_watch(&self, up: WatchUpdate) {
        for sub in self
            .inner
            .watch_subscribers
            .lock()
            .expect("watch_subscribers mutex")
            .iter()
        {
            sub(up.clone());
        }
    }

    /// Register a handler for live scan progress. It fires `Some(p)` on every tick
    /// *while* a rescan runs, then exactly one terminal `None` when the rescan ends
    /// (success or error), so the UI can drive a determinate indicator and reliably
    /// retire it. Fires on the scanning thread — the app marshals onto its UI thread.
    /// The show and hide arrive on this one ordered stream, so the indicator can't be
    /// left stuck by a late tick racing a separate "done" signal.
    pub fn on_scan_progress(&self, on_progress: impl Fn(Option<ScanProgress>) + Send + 'static) {
        self.inner
            .scan_progress_subscribers
            .lock()
            .expect("scan_progress_subscribers mutex")
            .push(Box::new(on_progress));
    }

    /// Fan one scan-progress update (`Some` tick / terminal `None`) out to subscribers.
    fn broadcast_scan_progress(&self, p: Option<ScanProgress>) {
        for sub in self
            .inner
            .scan_progress_subscribers
            .lock()
            .expect("scan_progress_subscribers mutex")
            .iter()
        {
            sub(p);
        }
    }

    /// Start — or re-arm — the live filesystem watcher over the currently enabled
    /// and reachable roots. A debounced change (an album copy coalesces to one
    /// callback) triggers a background rescan; its [`WatchUpdate`] is broadcast to
    /// [`on_watch_update`](Self::on_watch_update) subscribers so the UI refreshes, and
    /// any deletions are emitted right after (so the list updates before the "N
    /// removed" warning). Idempotent — replaces any existing watcher, which is how a
    /// source add/remove re-arms it. A watcher that fails to start degrades silently
    /// (no live reindex; manual Rescan still works).
    ///
    /// The rescan is **smart**: a burst that is all noise (editor temp files, partial
    /// downloads, OS metadata) is dropped with no rescan, and an actual change
    /// re-reads only the roots it touched — the rest are reused from cache. It falls
    /// back to a full rescan whenever a change can't be attributed to a root.
    pub fn start_watching(&self) {
        let roots: Vec<PathBuf> = self.enabled_reachable_roots();
        let weak = Arc::downgrade(&self.inner);
        let mut debouncer = match new_debouncer(WATCH_DEBOUNCE, move |res: DebounceEventResult| {
            let Ok(events) = res else {
                return; // watch-backend hiccup — ignore; the next event re-triggers
            };
            let Some(inner) = weak.upgrade() else {
                return; // the handle (and app) is gone
            };
            let handle = PlayerHandle { inner };
            // Drop a pure-noise burst — it can't change the library, so don't pay for
            // a rescan (nor make the activity widget flicker) on editor/OS churn.
            let changed: Vec<PathBuf> = events
                .into_iter()
                .map(|e| e.path)
                .filter(|p| !is_noise(p))
                .collect();
            if changed.is_empty() {
                return;
            }
            // Re-read only the roots a change actually touched; reuse the rest from
            // their cached bytes. If nothing maps to a root (shouldn't happen under a
            // recursive watch), fall back to a full rescan — correctness over cleverness.
            let affected = affected_roots(&changed, &handle.enabled_reachable_roots());
            let result = if affected.is_empty() {
                handle.rescan_compute()
            } else {
                handle.rescan_scoped(&affected)
            };
            // Broadcast the library refresh BEFORE the deletion warning, so the list
            // already reflects the removal when the "N removed" notice arrives.
            if let Ok((up, removed)) = result {
                handle.broadcast_watch(up);
                for (root, gone) in removed {
                    handle.emit(Event::TracksRemoved {
                        root,
                        count: gone.len(),
                    });
                }
            }
        }) {
            Ok(d) => d,
            Err(_) => return, // no watcher backend available → no live reindex
        };
        for root in &roots {
            let _ = debouncer.watcher().watch(root, RecursiveMode::Recursive);
        }
        *self.inner.watcher.lock().expect("watcher mutex") = Some(debouncer);
    }

    /// Re-arm the watcher over the current roots, but only if it is already running —
    /// called after a source-set change so a newly added folder is watched (and a
    /// removed one dropped) without the app re-requesting it.
    fn rearm_watcher(&self) {
        let active = self.inner.watcher.lock().expect("watcher mutex").is_some();
        if active {
            self.start_watching();
        }
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
        // Snapshot the exclusive-output request the same way (applies to this spawn
        // only); share the status cell so the factory can record what it resolved to.
        let exclusive = *self.inner.exclusive.lock().expect("exclusive mutex");
        let strict = *self.inner.exclusive_strict.lock().expect("strict mutex");
        let exclusive_status = Arc::clone(&self.inner.exclusive_status);
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
        let actor = sink::build_actor(
            self.inner.mode,
            device,
            exclusive,
            strict,
            exclusive_status,
            self.inner.dsp.clone(),
            self.inner.spectrum.clone(),
            lib,
            forward,
        );
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
    fn exclusive_toggle_is_remembered_and_read_without_spawning() {
        // The exclusive-output toggle's host surface: set/read the request and read
        // the resolved status. None of these open a device (the Offline handle never
        // spawns a real sink), so this runs device-free in CI — it asserts the
        // plumbing doesn't panic and starts in the shared state.
        let h = PlayerHandle::offline();
        assert!(
            !h.exclusive_requested(),
            "defaults to shared (not requested)"
        );
        assert_eq!(
            h.exclusive_status(),
            ExclusiveStatus::Shared,
            "no device spawned yet → shared"
        );
        h.set_exclusive(true);
        assert!(h.exclusive_requested(), "the request is remembered");
        h.set_exclusive(false);
        assert!(!h.exclusive_requested(), "and can be turned back off");
        // Strict (exclusive-or-stop) is an independent remembered flag.
        assert!(!h.exclusive_strict(), "strict defaults off");
        h.set_exclusive_strict(true);
        assert!(h.exclusive_strict(), "the strict request is remembered");
        h.set_exclusive_strict(false);
        assert!(!h.exclusive_strict(), "and can be turned back off");
        assert!(
            h.inner.player.lock().unwrap().is_none(),
            "no actor is spawned just to set the toggles"
        );
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

    fn test_loudness(integrated: f32) -> TrackLoudness {
        TrackLoudness {
            integrated_lufs: integrated,
            lra: 7.0,
            true_peak_dbtp: -1.0,
            plr: 10.0,
            dr: 11.0,
        }
    }

    #[test]
    fn seed_loudness_before_open_is_remembered_without_spawning() {
        // Seeding the loudness cache must not spawn an actor (no device stream
        // before playback) and must not panic — it's banked for the next spawn.
        let h = PlayerHandle::offline();
        h.seed_loudness(vec![
            (PathBuf::from("/music/a.flac"), test_loudness(-12.0)),
            (PathBuf::from("/music/b.wav"), test_loudness(-20.5)),
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
    fn caches_snapshot_and_seed_round_trip_for_warm_startup() {
        let a = folder_with("warm_a", &["a1.wav", "a2.wav"]);
        let aroot = a.path().to_path_buf();

        // A cold scan populates the per-source cache; the snapshot holds it.
        let h = PlayerHandle::offline();
        let up = h.set_sources(&[(aroot.clone(), true)]).unwrap();
        assert_eq!(up.views.len(), 2);
        let snap = h.caches_snapshot();
        assert_eq!(
            snap.get(&aroot).map(super::ScanCache::len),
            Some(2),
            "snapshot carries the scanned root's warm cache (both files)"
        );

        // A fresh handle SEEDED with that snapshot scans the same tree → identical
        // views (the persistence → warm-startup round trip), and keeps the cache.
        let h2 = PlayerHandle::offline();
        h2.seed_caches(snap);
        let up2 = h2.set_sources(&[(aroot.clone(), true)]).unwrap();
        assert_eq!(
            up2.views.iter().map(|v| v.id).collect::<Vec<_>>(),
            up.views.iter().map(|v| v.id).collect::<Vec<_>>(),
            "warm rescan from a seeded cache lists the same tracks"
        );
        assert_eq!(
            h2.caches_snapshot().get(&aroot).map(super::ScanCache::len),
            Some(2),
            "seeded cache survives the rescan and stays warm"
        );
    }

    #[test]
    fn configure_sources_seeds_the_set_without_scanning() {
        let a = folder_with("cfg_a", &["a1.wav", "a2.wav"]);
        let aroot = a.path().to_path_buf();
        let h = PlayerHandle::offline();

        // Configure WITHOUT scanning: the source is listed (available, count 0) so the
        // window can paint the Sources list immediately, but no library is built yet.
        h.configure_sources(&[(aroot.clone(), true)]);
        let srcs = h.sources();
        assert_eq!(srcs.len(), 1, "the configured source is listed at once");
        assert_eq!(srcs[0].path, aroot);
        assert!(srcs[0].enabled && srcs[0].available);
        assert_eq!(srcs[0].track_count, 0, "no scan yet → count 0 (loading)");
        assert!(
            h.views().is_empty(),
            "configure_sources does NOT scan — the library stays empty until rescan"
        );
        assert_eq!(h.watched_folders(), vec![aroot.clone()]);

        // The deferred (off-thread) scan then folds in the real tracks + counts.
        let up = h.rescan_sources().expect("rescan");
        assert_eq!(up.views.len(), 2, "the deferred scan folds the tracks in");
        assert_eq!(up.sources.len(), 1);
        assert_eq!(up.sources[0].track_count, 2);
    }

    #[test]
    fn configure_sources_dedups_by_path_preserving_order() {
        let a = folder_with("cfg_dedup", &["a.wav"]);
        let aroot = a.path().to_path_buf();
        let h = PlayerHandle::offline();
        // Duplicate path (second disabled) must not create a second entry; first wins.
        h.configure_sources(&[(aroot.clone(), true), (aroot.clone(), false)]);
        assert_eq!(h.watched_folders(), vec![aroot.clone()]);
        let srcs = h.sources();
        assert_eq!(srcs.len(), 1);
        assert!(srcs[0].enabled, "first occurrence wins the enabled flag");
    }

    #[test]
    fn is_noise_flags_junk_but_never_audio_art_or_dirs() {
        use super::is_noise;
        // Confirmed junk → noise (a batch of only these skips the rescan).
        assert!(is_noise(Path::new("/music/.DS_Store")));
        assert!(is_noise(Path::new("/music/Thumbs.db")));
        assert!(is_noise(Path::new("/music/desktop.ini")));
        assert!(
            is_noise(Path::new("/music/._song.flac")),
            "AppleDouble sidecar"
        );
        assert!(
            is_noise(Path::new("/music/track.flac.part")),
            "partial download"
        );
        assert!(is_noise(Path::new("/music/x.crdownload")));
        assert!(is_noise(Path::new("/music/.song.flac.swp")), "vim swap");
        assert!(is_noise(Path::new("/music/album.tmp")));
        // Real, library-relevant changes → NOT noise (never skip these).
        assert!(!is_noise(Path::new("/music/album/track.flac")));
        assert!(
            !is_noise(Path::new("/music/album/cover.jpg")),
            "art can matter"
        );
        assert!(!is_noise(Path::new("/music/album")), "a directory");
    }

    #[test]
    fn affected_roots_scopes_to_the_containing_roots() {
        use super::affected_roots;
        let r1 = PathBuf::from("/music/rock");
        let r2 = PathBuf::from("/music/jazz");
        let roots = vec![r1.clone(), r2.clone()];
        // A change under rock affects only rock.
        assert_eq!(
            affected_roots(&[PathBuf::from("/music/rock/album/x.flac")], &roots),
            std::iter::once(r1.clone()).collect()
        );
        // Changes under both affect both.
        assert_eq!(
            affected_roots(
                &[
                    PathBuf::from("/music/rock/x.flac"),
                    PathBuf::from("/music/jazz/y.flac"),
                ],
                &roots,
            )
            .len(),
            2
        );
        // A change outside every root affects none (→ nothing to re-read).
        assert!(affected_roots(&[PathBuf::from("/other/z.flac")], &roots).is_empty());
        // The root path itself (e.g. the folder deleted) counts as affecting it.
        assert_eq!(
            affected_roots(std::slice::from_ref(&r1), &roots),
            std::iter::once(r1).collect()
        );
        // A sibling that merely shares a name PREFIX is not "under" the root.
        assert!(affected_roots(&[PathBuf::from("/music/rockabilly/x.flac")], &roots).is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn affected_roots_is_case_insensitive_on_windows() {
        use super::affected_roots;
        let r = PathBuf::from(r"C:\Music\Rock");
        assert_eq!(
            affected_roots(
                &[PathBuf::from(r"c:\music\ROCK\album\x.flac")],
                std::slice::from_ref(&r),
            ),
            std::iter::once(r).collect(),
            "a casing difference must not hide a real change"
        );
    }

    #[test]
    fn scoped_rescan_reuses_unaffected_roots_and_matches_a_full_rescan() {
        let a = folder_with("scoped_host_a", &["a1.wav"]);
        let b = folder_with("scoped_host_b", &["b1.wav", "b2.wav"]);
        let (aroot, broot) = (a.path().to_path_buf(), b.path().to_path_buf());
        let h = PlayerHandle::offline();

        // A full scan populates both roots (and the per-root byte cache a scoped
        // rescan reuses).
        let up = h.set_folders(&[aroot.clone(), broot.clone()]).unwrap();
        assert_eq!(up.views.len(), 3, "a1 + b1 + b2");

        // Change root A only, then rescan SCOPED to just A: B is rebuilt from its
        // cached bytes (not re-read), and the result still lists B's tracks — exactly
        // what a full rescan would produce.
        std::fs::write(aroot.join("a2.wav"), wav(48_000, 96_000)).unwrap();
        let affected: std::collections::HashSet<PathBuf> = std::iter::once(aroot.clone()).collect();
        let (scoped, removed) = h.rescan_scoped(&affected).expect("scoped rescan");
        assert_eq!(
            scoped.views.len(),
            4,
            "A's new a2 folds in AND B's two tracks are kept (reused, not dropped)"
        );
        assert!(removed.is_empty(), "nothing deleted");
        // Per-source counts match a full rescan of the same trees.
        let full = PlayerHandle::offline();
        let up_full = full.set_folders(&[aroot.clone(), broot.clone()]).unwrap();
        assert_eq!(
            scoped.views.len(),
            up_full.views.len(),
            "scoped == full track total"
        );
        assert_eq!(
            scoped
                .sources
                .iter()
                .map(|s| s.track_count)
                .collect::<Vec<_>>(),
            up_full
                .sources
                .iter()
                .map(|s| s.track_count)
                .collect::<Vec<_>>(),
            "scoped per-source counts == a full rescan"
        );
    }

    #[test]
    fn dropping_a_source_drops_its_cache() {
        let a = folder_with("drop_a", &["a.wav"]);
        let b = folder_with("drop_b", &["b.wav"]);
        let (aroot, broot) = (a.path().to_path_buf(), b.path().to_path_buf());

        let h = PlayerHandle::offline();
        h.set_folders(&[aroot.clone(), broot.clone()]).unwrap();
        assert_eq!(h.caches_snapshot().len(), 2, "both sources cached");

        // Remove b from the configured set → its cache is pruned; a stays warm.
        h.set_folders(std::slice::from_ref(&aroot)).unwrap();
        let snap = h.caches_snapshot();
        assert!(snap.contains_key(&aroot), "kept source stays cached");
        assert!(
            !snap.contains_key(&broot),
            "dropped source's cache is pruned"
        );
    }

    #[test]
    fn rescan_warns_about_deleted_tracks_on_an_available_root() {
        let a = folder_with("warn_del", &["a.wav", "b.wav"]);
        let aroot = a.path().to_path_buf();
        let h = PlayerHandle::offline();
        let events = Arc::new(Mutex::new(Vec::new()));
        {
            let ev = Arc::clone(&events);
            h.subscribe(move |e| ev.lock().expect("ev mutex").push(e));
        }

        // A cold first scan caches both files and warns about nothing.
        h.set_sources(&[(aroot.clone(), true)]).unwrap();
        assert!(
            events
                .lock()
                .expect("ev mutex")
                .iter()
                .all(|e| !matches!(e, Event::TracksRemoved { .. })),
            "a cold first scan (no prior cache) warns about no deletions"
        );

        // Delete one file, rescan warm → exactly one TracksRemoved for the root.
        std::fs::remove_file(a.path().join("a.wav")).unwrap();
        h.set_sources(&[(aroot.clone(), true)]).unwrap();
        let removals: Vec<(PathBuf, usize)> = events
            .lock()
            .expect("ev mutex")
            .iter()
            .filter_map(|e| match e {
                Event::TracksRemoved { root, count } => Some((root.clone(), *count)),
                _ => None,
            })
            .collect();
        assert_eq!(
            removals,
            vec![(aroot, 1)],
            "exactly one deletion warning, carrying the right root and count"
        );
    }

    #[test]
    fn on_scan_progress_reports_determinate_ticks_during_a_rescan() {
        let dir = folder_with("scanprog", &["a.wav", "b.wav", "c.wav"]);
        let root = dir.path().to_path_buf();
        let h = PlayerHandle::offline();

        let ticks: Arc<Mutex<Vec<Option<ScanProgress>>>> = Arc::new(Mutex::new(Vec::new()));
        {
            let t = Arc::clone(&ticks);
            h.on_scan_progress(move |p| t.lock().expect("ticks mutex").push(p));
        }

        // A synchronous cold scan: progress fires on this thread during the call.
        h.set_sources(&[(root.clone(), true)]).unwrap();
        assert_eq!(h.views().len(), 3, "three tracks scanned");

        let ticks = ticks.lock().expect("ticks mutex");
        // The stream ends with exactly one terminal `None` (scan done).
        assert!(
            ticks.last().expect("at least one update").is_none(),
            "the stream ends with a terminal None"
        );
        assert_eq!(
            ticks.iter().filter(|p| p.is_none()).count(),
            1,
            "exactly one terminal None per rescan"
        );
        let extracting: Vec<ScanProgress> = ticks
            .iter()
            .filter_map(|p| *p)
            .filter(|p| p.phase == ScanPhase::Extracting)
            .collect();
        assert!(
            !extracting.is_empty(),
            "the rescan broadcast Extracting progress to the subscriber"
        );
        for p in &extracting {
            assert!(p.done <= p.total, "done {} <= total {}", p.done, p.total);
            assert_eq!(p.total, 3, "total is the extract count");
        }
        assert_eq!(
            extracting.last().map(|p| p.done),
            Some(3),
            "extraction finishes at done == total"
        );
    }

    #[test]
    fn watcher_reindexes_and_broadcasts_on_a_new_file() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let dir = folder_with("watch_live", &["a.wav"]);
        let root = dir.path().to_path_buf();
        let h = PlayerHandle::offline();
        let (tx, rx) = mpsc::channel();
        h.on_watch_update(move |up| {
            let _ = tx.send(up.views.len());
        });
        h.set_sources(&[(root.clone(), true)]).unwrap();
        assert_eq!(h.views().len(), 1, "one track before the change");

        h.start_watching();
        // Let the platform watcher arm (FSEvents/inotify setup) before the change,
        // so the create event isn't dropped on the floor.
        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(root.join("b.wav"), wav(48_000, 96_000)).unwrap();

        // The debounced watcher should rescan and broadcast a 2-track library. Drain
        // any intermediate broadcast until the final count arrives (or time out).
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "watcher did not broadcast the new file within 20s"
            );
            match rx.recv_timeout(remaining) {
                Ok(2) => break,
                Ok(_) => continue, // an earlier broadcast in the burst — keep waiting
                Err(mpsc::RecvTimeoutError::Timeout) => panic!("watcher broadcast timed out"),
                Err(mpsc::RecvTimeoutError::Disconnected) => panic!("watch channel closed"),
            }
        }
        assert_eq!(h.views().len(), 2, "the live library now has both tracks");
    }

    #[test]
    fn start_watching_is_safe_with_no_or_unavailable_sources() {
        let h = PlayerHandle::offline();
        h.start_watching(); // no sources configured — must not panic
        let missing =
            std::env::temp_dir().join(format!("sas_watch_missing_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&missing);
        h.set_sources(&[(missing, true)]).unwrap();
        h.start_watching(); // an unavailable root — nothing to watch, must not panic
    }

    #[test]
    fn indexing_mode_resolves_bounded_thread_counts() {
        assert_eq!(
            IndexingMode::Gentle.resolve_threads(),
            2,
            "gentle is minimal parallelism"
        );
        let max = IndexingMode::Maximum.resolve_threads();
        assert!(
            (4..=32).contains(&max),
            "maximum oversubscribes but stays capped: {max}"
        );
        // Automatic samples live CPU load; whatever the load, it stays in range and
        // never above its ceiling.
        let auto = IndexingMode::Automatic.resolve_threads();
        assert!(
            (2..=32).contains(&auto),
            "automatic stays in [2, 32]: {auto}"
        );
        assert!(auto <= IndexingMode::Automatic.ceiling());
        assert_eq!(IndexingMode::Gentle.ceiling(), 2);
        assert!((4..=32).contains(&IndexingMode::Maximum.ceiling()));
    }

    #[test]
    fn threads_for_idle_is_bounded_and_monotonic() {
        // Always in [2, MAX_SCAN_THREADS], whatever the inputs (incl. out-of-range).
        for &cores in &[1usize, 2, 4, 8, 16, 64] {
            for &idle in &[-1.0_f32, 0.0, 0.25, 0.5, 0.75, 1.0, 2.0] {
                let t = threads_for_idle(cores, idle);
                assert!(
                    (2..=MAX_SCAN_THREADS).contains(&t),
                    "cores={cores} idle={idle} -> {t}"
                );
            }
        }
        // More idle headroom never means FEWER threads (monotonic non-decreasing).
        let busy = threads_for_idle(8, 0.0);
        let mid = threads_for_idle(8, 0.5);
        let idle = threads_for_idle(8, 1.0);
        assert!(
            busy <= mid && mid <= idle,
            "busy={busy} mid={mid} idle={idle}"
        );
        assert!(
            idle > busy,
            "a fully idle machine runs more threads than a pegged one"
        );
    }

    #[test]
    fn load_sampler_updates_the_target_then_stops_on_drop() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Instant;
        let ceiling = 16;
        let target = Arc::new(AtomicUsize::new(999)); // deliberately out-of-range
        {
            let _sampler = LoadSampler::spawn(Arc::clone(&target), ceiling);
            // Poll for the first real write: the ~200 ms sample interval plus thread
            // scheduling under parallel-test CPU load make a fixed sleep flaky, a
            // generous deadline doesn't.
            let deadline = Instant::now() + Duration::from_secs(5);
            let t = loop {
                let v = target.load(Ordering::Relaxed);
                if v != 999 {
                    break v;
                }
                assert!(
                    Instant::now() < deadline,
                    "the sampler never updated the target within 5s"
                );
                std::thread::sleep(Duration::from_millis(25));
            };
            assert!(
                (2..=ceiling).contains(&t),
                "the sampler wrote a bounded live target (<= ceiling), got {t}"
            );
        } // dropped here → stops + joins
        // After the sampler is gone the value is frozen (no further writes).
        let frozen = target.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            target.load(Ordering::Relaxed),
            frozen,
            "a dropped sampler stops writing the target"
        );
    }

    #[test]
    fn indexing_mode_config_tokens_round_trip() {
        for m in [
            IndexingMode::Automatic,
            IndexingMode::Maximum,
            IndexingMode::Gentle,
        ] {
            assert_eq!(
                IndexingMode::from_config(m.as_config()),
                m,
                "{m:?} round-trips"
            );
        }
        assert_eq!(
            IndexingMode::from_config("nonsense"),
            IndexingMode::Automatic,
            "an unknown/missing token defaults to Automatic"
        );
        assert_eq!(IndexingMode::default(), IndexingMode::Automatic);
    }

    #[test]
    fn set_indexing_mode_round_trips_on_the_handle() {
        let h = PlayerHandle::offline();
        assert_eq!(
            h.indexing_mode(),
            IndexingMode::Maximum,
            "the handle's own default is Maximum until the app restores the user's choice"
        );
        h.set_indexing_mode(IndexingMode::Gentle);
        assert_eq!(h.indexing_mode(), IndexingMode::Gentle);
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
