//! Background loudness analysis + cache for the **hybrid ReplayGain** path:
//! tracks that carry no ReplayGain tags get a gain computed lazily on first play
//! by scanning their integrated loudness, so a later play is normalized just like
//! a tagged track. The tag-based path (the loader's [`read_rg_tags`] → the pump)
//! already ships; this is the untagged half.
//!
//! Shape (mirrors the actor's loader): a single background **worker thread** (a
//! 64 MiB stack, because [`OwnedDecoder`] is large by value and overflows the
//! default debug stack) drains a job queue one track at a time — bounding CPU and
//! disk — and writes a shared cache. The pump reads that cache synchronously when
//! a track becomes current: a hit folds the computed gain into the track's
//! [`RgInfo`](crate::driver::RgInfo); a miss enqueues a **single-flight** analysis
//! job (never the same path twice, never a path already analyzed) and plays
//! preamp-only this time. Each completed analysis is posted back to the pump (like
//! the loader's decoder results) so the pump — the one event source — emits
//! [`Event::TrackLoudnessAnalyzed`](crate::types::Event::TrackLoudnessAnalyzed)
//! for the app to persist.
//!
//! Backend purity: this module holds only the in-memory cache + the analyzer. It
//! does no config-path I/O — the app persists the computed gains (keyed by path +
//! validated against mtime/size) and seeds them back next launch via
//! [`Command::SeedLoudness`](crate::types::Command::SeedLoudness).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::types::TrackLoudness;
use soul_audio_core::Decoder;
use soul_audio_desktop::open_track;
use soul_audio_loudness::{NO_LOUDNESS, R128Analyzer, REPLAYGAIN2_TARGET_LUFS};

/// The ReplayGain-2.0 track gain (dB, to the −18 LUFS target) derived from a
/// track's integrated loudness.
fn gain_from(m: &TrackLoudness) -> f32 {
    REPLAYGAIN2_TARGET_LUFS - m.integrated_lufs
}

/// One analysis job: the track id that triggered it (informational — the cache is
/// keyed by path) and the path to decode.
type Job = (u32, PathBuf);

/// The shared cache state, behind one mutex so the worker (writer) and the pump
/// (reader + enqueuer) agree on what is analyzed / in-flight.
#[derive(Default)]
struct CacheInner {
    /// Path → analysis outcome. A key present at all means "analysis finished,
    /// never re-enqueue": `Some(metrics)` is a usable analysis (the ReplayGain gain
    /// is derived from its integrated loudness); `None` is a sentinel for a track we
    /// analyzed but got nothing usable from (decode failed, >2 channels / DSD, or
    /// below the loudness gate) so we don't retry it forever.
    done: BTreeMap<PathBuf, Option<TrackLoudness>>,
    /// Paths currently queued or being analyzed — the single-flight guard.
    inflight: BTreeSet<PathBuf>,
}

/// In-memory loudness cache + handle to the background analysis worker. Cheap to
/// hold on the pump thread: a hit is one lock + map lookup; a miss enqueues once.
pub struct LoudnessCache {
    inner: Arc<Mutex<CacheInner>>,
    jobs: Sender<Job>,
    /// Playback-yield gate: while `true` the worker parks between decode chunks, so a
    /// background sweep never competes with active playback (the dropout guard). Set by the
    /// pump from the deck state + the analysis mode (see `LoudnessAnalysisMode::should_pause`).
    paused: Arc<AtomicBool>,
    /// Set on drop so a parked worker stops waiting and lets the join complete (the jobs
    /// channel closing alone would not wake a worker parked mid-decode).
    shutdown: Arc<AtomicBool>,
}

impl Drop for LoudnessCache {
    fn drop(&mut self) {
        // Release any parked worker so `analyzer.join()` can complete promptly.
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

impl LoudnessCache {
    /// Build the cache and spawn the 64 MiB analysis worker. Every successfully
    /// analyzed `(path, gain_db)` is posted on `results` for the pump to turn into
    /// a [`TrackLoudnessAnalyzed`](crate::types::Event::TrackLoudnessAnalyzed)
    /// event; failures are recorded as the `None` sentinel and post nothing.
    /// Returns the worker's [`JoinHandle`] — drop the `LoudnessCache` (closing the
    /// job channel) then join it to shut the worker down cleanly.
    pub fn spawn(results: Sender<(PathBuf, TrackLoudness)>) -> (LoudnessCache, JoinHandle<()>) {
        let (jobs_tx, jobs_rx) = mpsc::channel::<Job>();
        let inner = Arc::new(Mutex::new(CacheInner::default()));
        let worker_inner = Arc::clone(&inner);
        let paused = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));
        let (worker_paused, worker_shutdown) = (Arc::clone(&paused), Arc::clone(&shutdown));
        // Same 64 MiB rationale as the pump/loader: the worker builds an
        // `OwnedDecoder` (the big `AnyDecoder`) and decodes the whole track.
        let worker = thread::Builder::new()
            .name("loudness-analyzer".into())
            .stack_size(64 * 1024 * 1024)
            .spawn(move || {
                analysis_worker(
                    &jobs_rx,
                    &worker_inner,
                    &results,
                    &worker_paused,
                    &worker_shutdown,
                )
            })
            .expect("spawn loudness worker thread");
        (
            LoudnessCache {
                inner,
                jobs: jobs_tx,
                paused,
                shutdown,
            },
            worker,
        )
    }

    /// Pause (`true`) or resume (`false`) the background sweep worker: while paused it parks
    /// between decode chunks and before each job, yielding CPU/disk to active playback. The
    /// pump drives this from the deck state + the analysis mode. Idempotent.
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    /// Whether the sweep worker is currently gated off for playback-yield.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    /// How many paths are currently queued or being analyzed — the background sweep's
    /// remaining work, for the "Analyzing N" progress indicator. Drops to 0 when the
    /// sweep is caught up.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.inner
            .lock()
            .expect("loudness cache mutex")
            .inflight
            .len()
    }

    /// Supersede the in-flight sweep: clear the queued/analyzing marks so a worker that later
    /// pops a now-stale job skips it (see `analysis_worker`) instead of spending a decode on a
    /// track a rescan may have removed. The job already mid-decode finishes (harmless); a fresh
    /// [`enqueue`](Self::enqueue) of the current library re-marks the tracks that remain.
    pub fn clear_pending(&self) {
        self.inner
            .lock()
            .expect("loudness cache mutex")
            .inflight
            .clear();
    }

    /// The usable computed gain for `path`, if one is cached (`None` for either
    /// "not analyzed yet" or "analyzed, no usable gain" — the pump treats both the
    /// same: play preamp-only, and [`enqueue`](Self::enqueue) decides whether a
    /// fresh job is still warranted).
    #[must_use]
    pub fn lookup(&self, path: &Path) -> Option<f32> {
        self.metrics(path).map(|m| gain_from(&m))
    }

    /// The full cached loudness metrics for `path`, if a usable analysis is cached
    /// (`None` for "not analyzed yet" or "analyzed, nothing usable"). Drives the
    /// loudness/dynamic-range readouts; the ReplayGain path uses [`lookup`](Self::lookup).
    #[must_use]
    pub fn metrics(&self, path: &Path) -> Option<TrackLoudness> {
        self.inner
            .lock()
            .expect("loudness cache mutex")
            .done
            .get(path)
            .copied()
            .flatten()
    }

    /// Single-flight enqueue: schedule a background analysis of `path` iff it is
    /// neither already analyzed (in `done`) nor already queued/analyzing (in
    /// `inflight`). Returns whether a job was actually enqueued. Marks the path
    /// in-flight under the same lock so two quick calls can't double-enqueue.
    pub fn enqueue(&self, id: u32, path: PathBuf) -> bool {
        {
            let mut g = self.inner.lock().expect("loudness cache mutex");
            if g.done.contains_key(&path) || g.inflight.contains(&path) {
                return false;
            }
            g.inflight.insert(path.clone());
        }
        // If the worker is gone the send fails; roll back the in-flight mark so a
        // later attempt (after a respawn) can retry rather than wedge forever.
        if self.jobs.send((id, path.clone())).is_err() {
            self.inner
                .lock()
                .expect("loudness cache mutex")
                .inflight
                .remove(&path);
            return false;
        }
        true
    }

    /// Seed the cache from the app's persisted gains (`Command::SeedLoudness`), so
    /// a track analyzed in a past session is normalized this session with no
    /// re-scan. Entries are inserted as usable (`Some`) gains; an already-present
    /// key is left as-is (a live result or an earlier seed wins).
    pub fn seed(&self, entries: &[(PathBuf, TrackLoudness)]) {
        let mut g = self.inner.lock().expect("loudness cache mutex");
        for (path, metrics) in entries {
            g.done.entry(path.clone()).or_insert(Some(*metrics));
        }
    }
}

/// The worker loop: analyze one queued track at a time, record the outcome (clear
/// its in-flight mark), and post a usable gain back to the pump. Exits when the
/// job channel closes (the `LoudnessCache` dropped).
fn analysis_worker(
    jobs: &Receiver<Job>,
    inner: &Arc<Mutex<CacheInner>>,
    results: &Sender<(PathBuf, TrackLoudness)>,
    paused: &Arc<AtomicBool>,
    shutdown: &Arc<AtomicBool>,
) {
    for (_id, path) in jobs {
        // Superseded by a later scan (`clear_pending` emptied the in-flight set): this job is
        // no longer wanted, so skip the decode rather than spend it on a possibly-removed
        // track. A fresh enqueue of the current library re-marks the tracks that remain.
        if !inner
            .lock()
            .expect("loudness cache mutex")
            .inflight
            .contains(&path)
        {
            continue;
        }
        let outcome = analyze_track(&path, paused, shutdown);
        {
            let mut g = inner.lock().expect("loudness cache mutex");
            g.inflight.remove(&path);
            g.done.insert(path.clone(), outcome);
        }
        if let Some(metrics) = outcome {
            // A closed results channel means the pump is gone; the loop will end
            // when the job channel closes too, so just stop posting.
            if results.send((path, metrics)).is_err() {
                break;
            }
        }
    }
}

/// Block while the sweep is `paused`, polling every 100 ms, unless `shutdown` is set (so a
/// shutting-down worker is never left parked and `analyzer.join()` returns promptly). A
/// no-op when not paused.
fn park_while_paused(paused: &AtomicBool, shutdown: &AtomicBool) {
    while paused.load(Ordering::Relaxed) && !shutdown.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(100));
    }
}

/// Decode `path` through the desktop stack, canonicalize to signed 24-bit, scan
/// its integrated loudness, and return the ReplayGain-2.0 gain (dB, to the −18
/// LUFS target) — or `None` when there is no usable gain: an unopenable / mid-
/// stream-erroring file, a stream reporting native DSD, a silent / below-gate
/// track, or >2 channels (the analyzer is mono/stereo; surround is a v1 skip).
/// The canonicalization + feed loop mirrors `soul-audio-loudness`'s
/// `tests/ffmpeg_crosscheck.rs`.
///
/// Note on DSD: [`open_track`] wraps a DSD file in a decimating adapter, so its
/// `stream_info()` reports decimated multi-bit PCM (not `is_dsd()`) and we DO
/// analyze it via that PCM form. The `is_dsd()` guard is a safety net for any
/// decoder that would instead report a raw 1-bit stream, where the `24 - bits`
/// canonical shift would be meaningless.
fn analyze_track(
    path: &Path,
    paused: &Arc<AtomicBool>,
    shutdown: &Arc<AtomicBool>,
) -> Option<TrackLoudness> {
    let mut d = open_track(path).ok()?;
    let si = d.stream_info();
    if si.format.is_dsd() || si.channels == 0 || si.channels > 2 {
        return None;
    }
    let ch = si.channels as usize;
    // `decode_into` yields the decoder's native bit depth; shift into canonical
    // 24-bit (left-shift to widen, right-shift the rare >24-bit source down).
    let shift = 24i32 - i32::from(si.bits_per_sample());
    let mut a = R128Analyzer::new(si.sample_rate, si.channels as u8);
    let mut buf = vec![0i32; 8192 * ch];
    let mut canon = vec![0i32; 8192 * ch];
    loop {
        // Playback-yield: park (burning no CPU/disk) while the sweep is paused, so a
        // full-library analysis never competes with active playback. Bail if shutting
        // down so `analyzer.join()` does not block on a parked worker.
        park_while_paused(paused, shutdown);
        if shutdown.load(Ordering::Relaxed) {
            return None;
        }
        let n = d.decode_into(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        for (c, s) in canon[..n].iter_mut().zip(&buf[..n]) {
            *c = if shift >= 0 {
                *s << shift
            } else {
                *s >> (-shift)
            };
        }
        a.feed(&canon[..n]);
    }
    // No integrated loudness (silent / below gate / weird) → no usable analysis:
    // the hybrid-ReplayGain path needs a gain, and the readouts key off integrated.
    if a.integrated_lufs() == NO_LOUDNESS {
        return None;
    }
    Some(a.metrics().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cache with no worker thread: the caller holds the job receiver and drives
    /// completion by hand, so the single-flight bookkeeping is tested in isolation
    /// (no decode, no thread, no timing).
    fn headless() -> (LoudnessCache, Receiver<Job>) {
        let (jobs_tx, jobs_rx) = mpsc::channel::<Job>();
        let cache = LoudnessCache {
            inner: Arc::new(Mutex::new(CacheInner::default())),
            jobs: jobs_tx,
            paused: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(AtomicBool::new(false)),
        };
        (cache, jobs_rx)
    }

    /// Simulate the worker finishing one job: clear in-flight, record the outcome.
    fn complete(cache: &LoudnessCache, path: &Path, outcome: Option<TrackLoudness>) {
        let mut g = cache.inner.lock().unwrap();
        g.inflight.remove(path);
        g.done.insert(path.to_path_buf(), outcome);
    }

    /// A metrics value with a given integrated loudness (the field the RG gain is
    /// derived from); the rest are filler for the single-flight tests.
    fn metrics_at(integrated: f32) -> TrackLoudness {
        TrackLoudness {
            integrated_lufs: integrated,
            lra: 0.0,
            true_peak_dbtp: 0.0,
            plr: 0.0,
            dr: 0.0,
        }
    }

    #[test]
    fn enqueue_is_single_flight_then_cached() {
        let (cache, rx) = headless();
        let p = PathBuf::from("/music/untagged.flac");

        // First enqueue schedules exactly one job and marks the path in-flight.
        assert!(cache.enqueue(7, p.clone()), "first enqueue schedules a job");
        assert_eq!(rx.try_recv().ok(), Some((7, p.clone())), "one job queued");
        // A second enqueue while in-flight is suppressed (no duplicate job).
        assert!(
            !cache.enqueue(7, p.clone()),
            "in-flight path is not re-enqueued"
        );
        assert!(rx.try_recv().is_err(), "no second job");
        // Not usable yet (still analyzing).
        assert_eq!(cache.lookup(&p), None);

        // Worker completes with metrics → lookup derives the gain (−18 − integrated);
        // never re-enqueued. integrated −13.5 → gain −4.5.
        complete(&cache, &p, Some(metrics_at(-13.5)));
        assert_eq!(cache.lookup(&p), Some(-4.5), "the derived gain is cached");
        assert_eq!(
            cache.metrics(&p).map(|m| m.integrated_lufs),
            Some(-13.5),
            "the full metrics are available for the readouts"
        );
        assert!(
            !cache.enqueue(7, p.clone()),
            "an analyzed path is not re-enqueued"
        );
        assert!(
            rx.try_recv().is_err(),
            "no job for an already-analyzed path"
        );
    }

    #[test]
    fn failed_analysis_is_remembered_and_not_retried() {
        let (cache, rx) = headless();
        let p = PathBuf::from("/music/surround.wav");
        assert!(cache.enqueue(1, p.clone()));
        let _ = rx.try_recv();
        // Worker records the None sentinel (e.g. >2 channels / below gate).
        complete(&cache, &p, None);
        assert_eq!(cache.lookup(&p), None, "no usable gain");
        assert!(
            !cache.enqueue(1, p.clone()),
            "the None sentinel stops a forever-retry"
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn seed_populates_the_cache_without_enqueueing() {
        let (cache, rx) = headless();
        let a = PathBuf::from("/music/a.flac");
        let b = PathBuf::from("/music/b.flac");
        // integrated −12.0 → gain −6.0; integrated −20.5 → gain +2.5.
        cache.seed(&[
            (a.clone(), metrics_at(-12.0)),
            (b.clone(), metrics_at(-20.5)),
        ]);
        assert_eq!(cache.lookup(&a), Some(-6.0));
        assert_eq!(cache.lookup(&b), Some(2.5));
        // Seeded = analyzed → never scheduled; nothing was sent to the worker.
        assert!(
            !cache.enqueue(0, a.clone()),
            "a seeded path is not enqueued"
        );
        assert!(rx.try_recv().is_err(), "seeding queues no jobs");
    }

    #[test]
    fn set_paused_toggles_the_playback_yield_gate() {
        let (cache, _rx) = headless();
        assert!(!cache.is_paused(), "a fresh cache is not paused");
        cache.set_paused(true);
        assert!(cache.is_paused(), "paused after set_paused(true)");
        cache.set_paused(false);
        assert!(!cache.is_paused(), "resumed after set_paused(false)");
    }

    /// The park must not block when there is nothing to wait for, and must release the
    /// moment shutdown is set even while paused — otherwise `analyzer.join()` would hang on
    /// a parked worker. (Both paths return promptly; the test completing proves they do.)
    #[test]
    fn park_returns_when_idle_or_shutting_down() {
        let paused = AtomicBool::new(false);
        let shutdown = AtomicBool::new(false);
        park_while_paused(&paused, &shutdown); // not paused → immediate
        paused.store(true, Ordering::Relaxed);
        shutdown.store(true, Ordering::Relaxed);
        park_while_paused(&paused, &shutdown); // paused but shutting down → immediate
    }

    #[test]
    fn clear_pending_supersedes_the_queued_sweep() {
        let (cache, rx) = headless();
        let a = PathBuf::from("/music/a.flac");
        let b = PathBuf::from("/music/b.flac");
        assert!(cache.enqueue(1, a.clone()));
        assert!(cache.enqueue(2, b.clone()));
        let _ = (rx.try_recv(), rx.try_recv());
        assert_eq!(cache.pending(), 2, "two tracks queued");

        // A new scan supersedes: the in-flight marks clear (so a worker popping either stale
        // job skips it), dropping the pending count to 0.
        cache.clear_pending();
        assert_eq!(cache.pending(), 0, "clear_pending empties the queue marks");

        // A track that still exists re-enqueues cleanly (the stale mark no longer blocks it).
        assert!(
            cache.enqueue(1, a.clone()),
            "a surviving track re-enqueues after supersede"
        );
        assert_eq!(cache.pending(), 1);
    }

    /// Analyzing a short committed real fixture yields a finite gain without
    /// panicking. The exact LUFS of a test tone isn't the point — that it decodes,
    /// canonicalizes, feeds the analyzer and produces a sane number is. Runs on a
    /// 64 MiB stack (the `OwnedDecoder`-by-value rule).
    #[test]
    fn analyze_short_fixture_produces_a_finite_gain() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../testdata")
            .join("tone.flac");
        assert!(path.is_file(), "committed fixture {path:?} missing");
        let m = thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(move || {
                // Not paused, not shutting down: a plain full analysis.
                let paused = Arc::new(AtomicBool::new(false));
                let shutdown = Arc::new(AtomicBool::new(false));
                analyze_track(&path, &paused, &shutdown)
            })
            .expect("spawn big-stack thread")
            .join()
            .expect("analysis thread panicked");
        let m = m.expect("a tonal fixture has a defined integrated loudness");
        let gain = gain_from(&m);
        assert!(gain.is_finite(), "computed gain {gain} is finite");
        assert!(
            m.integrated_lufs.is_finite(),
            "integrated loudness is finite"
        );
        // The master tone is −6 dBFS; its gain to −18 LUFS lands in a wide sane
        // band — this only guards against a nonsense (wrong-canonicalization) value.
        assert!(
            (-40.0..=40.0).contains(&gain),
            "gain {gain} dB implausible for a −6 dBFS tone"
        );
    }

    /// A DSD fixture goes through `open_track`'s decimating adapter, so it reports
    /// PCM and analyzes via that decimated form (not the `is_dsd()` skip) — the
    /// invariant is that it produces a finite gain without panicking, exactly like
    /// any other PCM track.
    #[test]
    fn analyze_dsd_via_decimation_does_not_panic() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../testdata")
            .join("tone.dsf");
        assert!(path.is_file(), "committed DSD fixture {path:?} missing");
        let out = thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(move || {
                let paused = Arc::new(AtomicBool::new(false));
                let shutdown = Arc::new(AtomicBool::new(false));
                analyze_track(&path, &paused, &shutdown)
            })
            .expect("spawn big-stack thread")
            .join()
            .expect("analysis thread panicked");
        // Either finite metrics (decimated to PCM) or None (below gate) — never a
        // panic or a non-finite value.
        if let Some(m) = out {
            assert!(
                m.integrated_lufs.is_finite() && gain_from(&m).is_finite(),
                "decimated-DSD metrics are finite"
            );
        }
    }
}
