//! Background **acoustic-identity** job: fingerprint every library track once and
//! cache the result, so the client holds a ready-to-submit identity for each file
//! without ever blocking playback or re-decoding a track twice.
//!
//! This is the client half of the cloud `/v1/identify` flow (see
//! `docs/superpowers/specs/*-cloud-enrichment`). It computes and *persists* each
//! track's [`AcousticId`] — the compressed Chromaprint fingerprint plus its decoded
//! duration — which, paired with the tags-MBID the catalog already carries, is the
//! track's identity. The identity is submitted to the enrichment cloud only when
//! the user opts in; the audio itself never leaves the machine. Even before any
//! cloud exists the cache is useful precompute (and the basis for local duplicate
//! detection).
//!
//! Shape — a deliberate mirror of [`crate::loudness`]: a single background **worker
//! thread** on a 64 MiB stack (because [`soul_audio_desktop::open_track`]'s
//! `OwnedDecoder` is large by value and overflows the default stack) drains a job
//! queue one track at a time — bounding CPU and disk — and writes a shared,
//! single-flight cache. The caller enqueues the library's tracks; each finished
//! identity is posted back on a channel for the app to persist, and seeded back
//! next launch so an already-identified track is never re-fingerprinted.
//!
//! Backend purity: this module holds only the in-memory cache + the worker. It does
//! no config-path I/O — the app persists the identities (keyed by path + validated
//! against mtime/size) and seeds them back.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crate::fingerprint::fingerprint_file;

/// A track's acoustic identity as cached + submitted: the AcoustID-style compressed
/// fingerprint (URL-safe base64) and the decoded duration. The cloud keys on
/// `(fingerprint, duration)`; the tags-MBID is attached from the catalog at submit
/// time, not stored here.
#[derive(Clone, Debug, PartialEq)]
pub struct AcousticId {
    /// Chromaprint-compressed fingerprint, URL-safe base64 — the exact wire form.
    pub fingerprint: String,
    /// Decoded audio duration in seconds.
    pub duration_secs: f64,
}

/// One identify job: the track id that triggered it (informational — the cache is
/// keyed by path) and the path to decode + fingerprint.
type Job = (u32, PathBuf);

/// Shared cache state behind one mutex so the worker (writer) and the enqueuer agree
/// on what is identified / in-flight.
#[derive(Default)]
struct CacheInner {
    /// Path → outcome. A key present means "finished, never re-enqueue":
    /// `Some(id)` is a usable identity; `None` is the sentinel for a track we tried
    /// but got no fingerprint from (decode failed or too short) so we don't retry it.
    done: BTreeMap<PathBuf, Option<AcousticId>>,
    /// Paths currently queued or being fingerprinted — the single-flight guard.
    inflight: BTreeSet<PathBuf>,
}

/// In-memory identity cache + handle to the background fingerprint worker. A lookup
/// is one lock + map read; an enqueue schedules at most one job per path.
pub struct IdentifyCache {
    inner: Arc<Mutex<CacheInner>>,
    jobs: Sender<Job>,
}

impl IdentifyCache {
    /// Build the cache and spawn the 64 MiB fingerprint worker. Every successfully
    /// identified `(path, AcousticId)` is posted on `results` for the app to persist;
    /// a failure is recorded as the `None` sentinel and posts nothing. Returns the
    /// worker's [`JoinHandle`] — drop the `IdentifyCache` (closing the job channel)
    /// then join it to shut the worker down cleanly.
    pub fn spawn(results: Sender<(PathBuf, AcousticId)>) -> (IdentifyCache, JoinHandle<()>) {
        let (jobs_tx, jobs_rx) = mpsc::channel::<Job>();
        let inner = Arc::new(Mutex::new(CacheInner::default()));
        let worker_inner = Arc::clone(&inner);
        // Same 64 MiB rationale as the loudness analyzer: the worker builds an
        // `OwnedDecoder` (the big `AnyDecoder`) and decodes the track to fingerprint.
        let worker = thread::Builder::new()
            .name("acoustic-identify".into())
            .stack_size(64 * 1024 * 1024)
            .spawn(move || identify_worker(&jobs_rx, &worker_inner, &results))
            .expect("spawn identify worker thread");
        (
            IdentifyCache {
                inner,
                jobs: jobs_tx,
            },
            worker,
        )
    }

    /// The cached identity for `path`, if one is ready (`None` for either "not
    /// identified yet" or "identified, no usable fingerprint").
    #[must_use]
    pub fn lookup(&self, path: &Path) -> Option<AcousticId> {
        self.inner
            .lock()
            .expect("identify cache mutex")
            .done
            .get(path)
            .cloned()
            .flatten()
    }

    /// Single-flight enqueue: schedule a background identify of `path` iff it is
    /// neither already identified (in `done`) nor already queued/working (in
    /// `inflight`). Returns whether a job was actually enqueued.
    pub fn enqueue(&self, id: u32, path: PathBuf) -> bool {
        {
            let mut g = self.inner.lock().expect("identify cache mutex");
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
                .expect("identify cache mutex")
                .inflight
                .remove(&path);
            return false;
        }
        true
    }

    /// Seed the cache from the app's persisted identities, so a track identified in a
    /// past session is not re-fingerprinted this session. Entries are inserted as
    /// usable; an already-present key is left as-is (a live result or earlier seed
    /// wins).
    pub fn seed(&self, entries: &[(PathBuf, AcousticId)]) {
        let mut g = self.inner.lock().expect("identify cache mutex");
        for (path, id) in entries {
            g.done
                .entry(path.clone())
                .or_insert_with(|| Some(id.clone()));
        }
    }
}

/// The worker loop: identify one queued track at a time, record the outcome (clear
/// its in-flight mark), and post a usable identity back. Exits when the job channel
/// closes (the `IdentifyCache` dropped).
fn identify_worker(
    jobs: &Receiver<Job>,
    inner: &Arc<Mutex<CacheInner>>,
    results: &Sender<(PathBuf, AcousticId)>,
) {
    for (_id, path) in jobs {
        let outcome = identify_track(&path);
        {
            let mut g = inner.lock().expect("identify cache mutex");
            g.inflight.remove(&path);
            g.done.insert(path.clone(), outcome.clone());
        }
        if let Some(id) = outcome {
            // A closed results channel means the app is gone; stop posting (the loop
            // ends when the job channel closes too).
            if results.send((path, id)).is_err() {
                break;
            }
        }
    }
}

/// Fingerprint `path` and fold it into an [`AcousticId`], or `None` when there is no
/// usable identity: an unopenable / mid-stream-erroring file, or one too short to
/// yield a Chromaprint sub-fingerprint. Must run on a large-stack thread (see the
/// module docs) — the worker does.
fn identify_track(path: &Path) -> Option<AcousticId> {
    let fp = fingerprint_file(path).ok()?;
    if fp.is_empty() {
        return None;
    }
    Some(AcousticId {
        fingerprint: fp.compressed_base64(),
        duration_secs: fp.duration_secs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cache with no worker thread: the caller holds the job receiver and drives
    /// completion by hand, so the single-flight bookkeeping is tested in isolation
    /// (no decode, no thread, no timing).
    fn headless() -> (IdentifyCache, Receiver<Job>) {
        let (jobs_tx, jobs_rx) = mpsc::channel::<Job>();
        let cache = IdentifyCache {
            inner: Arc::new(Mutex::new(CacheInner::default())),
            jobs: jobs_tx,
        };
        (cache, jobs_rx)
    }

    /// Simulate the worker finishing one job: clear in-flight, record the outcome.
    fn complete(cache: &IdentifyCache, path: &Path, outcome: Option<AcousticId>) {
        let mut g = cache.inner.lock().unwrap();
        g.inflight.remove(path);
        g.done.insert(path.to_path_buf(), outcome);
    }

    fn some_id() -> AcousticId {
        AcousticId {
            fingerprint: "AQABz0mUaEkSRQ".to_string(),
            duration_secs: 212.0,
        }
    }

    #[test]
    fn enqueue_is_single_flight_then_cached() {
        let (cache, rx) = headless();
        let p = PathBuf::from("/music/track.flac");

        // First enqueue schedules exactly one job and marks the path in-flight.
        assert!(cache.enqueue(7, p.clone()), "first enqueue schedules a job");
        assert_eq!(rx.try_recv().ok(), Some((7, p.clone())), "one job queued");
        // A second enqueue while in-flight is suppressed (no duplicate job).
        assert!(
            !cache.enqueue(7, p.clone()),
            "in-flight path is not re-enqueued"
        );
        assert!(rx.try_recv().is_err(), "no second job");
        assert_eq!(cache.lookup(&p), None, "not identified yet");

        // Worker completes with an identity → lookup folds it; never re-enqueued.
        complete(&cache, &p, Some(some_id()));
        assert_eq!(
            cache.lookup(&p),
            Some(some_id()),
            "a completed identity is cached"
        );
        assert!(
            !cache.enqueue(7, p.clone()),
            "an identified path is not re-enqueued"
        );
        assert!(
            rx.try_recv().is_err(),
            "no job for an already-identified path"
        );
    }

    #[test]
    fn failed_identify_is_remembered_and_not_retried() {
        let (cache, rx) = headless();
        let p = PathBuf::from("/music/corrupt.flac");
        assert!(cache.enqueue(1, p.clone()));
        let _ = rx.try_recv();
        // Worker records the None sentinel (decode failed / too short).
        complete(&cache, &p, None);
        assert_eq!(cache.lookup(&p), None, "no usable identity");
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
        let id_b = AcousticId {
            fingerprint: "AQAAAZ".to_string(),
            duration_secs: 99.5,
        };
        cache.seed(&[(a.clone(), some_id()), (b.clone(), id_b.clone())]);
        assert_eq!(cache.lookup(&a), Some(some_id()));
        assert_eq!(cache.lookup(&b), Some(id_b));
        // Seeded = identified → never scheduled; nothing was sent to the worker.
        assert!(
            !cache.enqueue(0, a.clone()),
            "a seeded path is not enqueued"
        );
        assert!(rx.try_recv().is_err(), "seeding queues no jobs");
    }

    /// A 16-bit PCM stereo WAV of a `secs`-long `freq` Hz tone at 44100 Hz. The
    /// committed fixtures are all ~2 s (sized for fast decode tests) — below
    /// Chromaprint's minimum — so the end-to-end test synthesizes its own long tone,
    /// exactly as `fingerprint.rs` does.
    fn tone_wav(freq: f64, secs: f64) -> Vec<u8> {
        let rate = 44_100u32;
        let frames = (secs * f64::from(rate)) as usize;
        let mut samples: Vec<i16> = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let t = i as f64 / f64::from(rate);
            let s =
                (0.6 * (2.0 * std::f64::consts::PI * freq * t).sin() * f64::from(i16::MAX)) as i16;
            samples.push(s); // L
            samples.push(s); // R
        }
        let data_len = (samples.len() * 2) as u32;
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + data_len).to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes()); // PCM
        v.extend_from_slice(&2u16.to_le_bytes()); // channels
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&(rate * 4).to_le_bytes()); // byte rate (2ch × 2B)
        v.extend_from_slice(&4u16.to_le_bytes()); // block align
        v.extend_from_slice(&16u16.to_le_bytes()); // bits
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data_len.to_le_bytes());
        for s in samples {
            v.extend_from_slice(&s.to_le_bytes());
        }
        v
    }

    /// End-to-end through the real worker: enqueue a synthesized 6 s tone, and the
    /// posted identity carries a non-empty wire fingerprint and a sane duration. This
    /// exercises the 64 MiB worker, the decode→fingerprint path, and the result
    /// channel together.
    #[test]
    fn worker_fingerprints_a_real_fixture_end_to_end() {
        let dir = std::env::temp_dir().join(format!("sas_identify_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tone6s.wav");
        std::fs::write(&path, tone_wav(440.0, 6.0)).unwrap();

        let (results_tx, results_rx) = mpsc::channel::<(PathBuf, AcousticId)>();
        let (cache, worker) = IdentifyCache::spawn(results_tx);
        assert!(cache.enqueue(1, path.clone()), "enqueue the fixture");

        // The worker posts the finished identity; wait with a generous deadline.
        let (got_path, id) = results_rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("worker posts an identity for a real tone");
        assert_eq!(got_path, path);
        assert!(!id.fingerprint.is_empty(), "wire fingerprint is non-empty");
        assert!(
            id.duration_secs > 0.0,
            "duration is positive, got {}",
            id.duration_secs
        );
        // The identity is cached and the path is no longer re-enqueueable.
        assert_eq!(cache.lookup(&path), Some(id));
        assert!(
            !cache.enqueue(1, path.clone()),
            "identified path is terminal"
        );

        drop(cache);
        worker.join().expect("worker shuts down cleanly");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
