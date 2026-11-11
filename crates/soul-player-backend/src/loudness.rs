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
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use soul_audio_core::Decoder;
use soul_audio_desktop::open_track;
use soul_audio_loudness::{NO_LOUDNESS, R128Analyzer, REPLAYGAIN2_TARGET_LUFS};

/// One analysis job: the track id that triggered it (informational — the cache is
/// keyed by path) and the path to decode.
type Job = (u32, PathBuf);

/// The shared cache state, behind one mutex so the worker (writer) and the pump
/// (reader + enqueuer) agree on what is analyzed / in-flight.
#[derive(Default)]
struct CacheInner {
    /// Path → analysis outcome. A key present at all means "analysis finished,
    /// never re-enqueue": `Some(gain_db)` is a usable computed gain; `None` is a
    /// sentinel for a track we analyzed but got no gain from (decode failed, >2
    /// channels / DSD, or below the loudness gate) so we don't retry it forever.
    done: BTreeMap<PathBuf, Option<f32>>,
    /// Paths currently queued or being analyzed — the single-flight guard.
    inflight: BTreeSet<PathBuf>,
}

/// In-memory loudness cache + handle to the background analysis worker. Cheap to
/// hold on the pump thread: a hit is one lock + map lookup; a miss enqueues once.
pub struct LoudnessCache {
    inner: Arc<Mutex<CacheInner>>,
    jobs: Sender<Job>,
}

impl LoudnessCache {
    /// Build the cache and spawn the 64 MiB analysis worker. Every successfully
    /// analyzed `(path, gain_db)` is posted on `results` for the pump to turn into
    /// a [`TrackLoudnessAnalyzed`](crate::types::Event::TrackLoudnessAnalyzed)
    /// event; failures are recorded as the `None` sentinel and post nothing.
    /// Returns the worker's [`JoinHandle`] — drop the `LoudnessCache` (closing the
    /// job channel) then join it to shut the worker down cleanly.
    pub fn spawn(results: Sender<(PathBuf, f32)>) -> (LoudnessCache, JoinHandle<()>) {
        let (jobs_tx, jobs_rx) = mpsc::channel::<Job>();
        let inner = Arc::new(Mutex::new(CacheInner::default()));
        let worker_inner = Arc::clone(&inner);
        // Same 64 MiB rationale as the pump/loader: the worker builds an
        // `OwnedDecoder` (the big `AnyDecoder`) and decodes the whole track.
        let worker = thread::Builder::new()
            .name("loudness-analyzer".into())
            .stack_size(64 * 1024 * 1024)
            .spawn(move || analysis_worker(&jobs_rx, &worker_inner, &results))
            .expect("spawn loudness worker thread");
        (
            LoudnessCache {
                inner,
                jobs: jobs_tx,
            },
            worker,
        )
    }

    /// The usable computed gain for `path`, if one is cached (`None` for either
    /// "not analyzed yet" or "analyzed, no usable gain" — the pump treats both the
    /// same: play preamp-only, and [`enqueue`](Self::enqueue) decides whether a
    /// fresh job is still warranted).
    #[must_use]
    pub fn lookup(&self, path: &Path) -> Option<f32> {
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
    pub fn seed(&self, entries: &[(PathBuf, f32)]) {
        let mut g = self.inner.lock().expect("loudness cache mutex");
        for (path, gain) in entries {
            g.done.entry(path.clone()).or_insert(Some(*gain));
        }
    }
}

/// The worker loop: analyze one queued track at a time, record the outcome (clear
/// its in-flight mark), and post a usable gain back to the pump. Exits when the
/// job channel closes (the `LoudnessCache` dropped).
fn analysis_worker(
    jobs: &Receiver<Job>,
    inner: &Arc<Mutex<CacheInner>>,
    results: &Sender<(PathBuf, f32)>,
) {
    for (_id, path) in jobs {
        let outcome = analyze_track(&path);
        {
            let mut g = inner.lock().expect("loudness cache mutex");
            g.inflight.remove(&path);
            g.done.insert(path.clone(), outcome);
        }
        if let Some(gain) = outcome {
            // A closed results channel means the pump is gone; the loop will end
            // when the job channel closes too, so just stop posting.
            if results.send((path, gain)).is_err() {
                break;
            }
        }
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
fn analyze_track(path: &Path) -> Option<f32> {
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
    let l = a.integrated_lufs();
    if l == NO_LOUDNESS {
        return None;
    }
    Some(a.replay_gain_db(REPLAYGAIN2_TARGET_LUFS))
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
        };
        (cache, jobs_rx)
    }

    /// Simulate the worker finishing one job: clear in-flight, record the outcome.
    fn complete(cache: &LoudnessCache, path: &Path, outcome: Option<f32>) {
        let mut g = cache.inner.lock().unwrap();
        g.inflight.remove(path);
        g.done.insert(path.to_path_buf(), outcome);
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

        // Worker completes with a gain → lookup folds it; never re-enqueued.
        complete(&cache, &p, Some(-4.5));
        assert_eq!(cache.lookup(&p), Some(-4.5), "a completed gain is cached");
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
        cache.seed(&[(a.clone(), -6.0), (b.clone(), 2.5)]);
        assert_eq!(cache.lookup(&a), Some(-6.0));
        assert_eq!(cache.lookup(&b), Some(2.5));
        // Seeded = analyzed → never scheduled; nothing was sent to the worker.
        assert!(
            !cache.enqueue(0, a.clone()),
            "a seeded path is not enqueued"
        );
        assert!(rx.try_recv().is_err(), "seeding queues no jobs");
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
        let gain = thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(move || analyze_track(&path))
            .expect("spawn big-stack thread")
            .join()
            .expect("analysis thread panicked");
        let gain = gain.expect("a tonal fixture has a defined integrated loudness");
        assert!(gain.is_finite(), "computed gain {gain} is finite");
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
            .spawn(move || analyze_track(&path))
            .expect("spawn big-stack thread")
            .join()
            .expect("analysis thread panicked");
        // Either a finite computed gain (decimated to PCM) or None (below gate) —
        // never a panic or a non-finite value.
        if let Some(gain) = out {
            assert!(gain.is_finite(), "decimated-DSD gain {gain} is finite");
        }
    }
}
