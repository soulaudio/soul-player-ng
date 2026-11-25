//! Acoustic fingerprinting (Chromaprint, default **Algorithm 2**), the client's
//! half of the cloud-enrichment story.
//!
//! # The split (client here, enrichment in the cloud)
//!
//! This desktop client (and, later, the DAP) computes a compact **acoustic
//! fingerprint** for each track, a one-way representation of the audio's chroma
//! over time. Paired with any MusicBrainz **recording MBID** read from the file's
//! tags (baked into the `.soulidx`), that fingerprint is the track's *identity*.
//! Only the identity ever leaves the machine; never the audio, and only when the
//! user opts in.
//!
//! The **enrichment**, resolving a fingerprint to a canonical recording and
//! filling genre/metadata, is done by a **separate, closed-source cloud service**
//! (planned in Go or .NET), NOT by this client calling AcoustID / MusicBrainz
//! directly. That service owns the fingerprint→recording matching index and the
//! aggregated metadata graph (MusicBrainz CC0 + Discogs + Bandcamp). This crate
//! only produces the fingerprint; it never contacts that service itself.
//!
//! # Why Chromaprint (and this crate)
//!
//! We fingerprint the **clean original files** we already decode, for
//! *near-identical* matching, exactly Chromaprint's design point (compact ~2.5 KB,
//! fast). Neural fingerprinting's robustness to noise/reverb/mic is irrelevant to
//! us. Chromaprint is also the lingua franca across languages: this Rust client
//! (`rusty-chromaprint`), and a Go (`gochromaprint`) or .NET (`AcoustID.NET`) cloud
//! can all speak it, and its matching is *bit-error-tolerant* so a resampler
//! difference between client and cloud doesn't break a match.
//!
//! **Licensing:** `rusty-chromaprint` is MIT (uses the MIT `rubato` resampler), so
//! this *distributed* client stays permissive. `chromaprint-next` is bit-identical
//! and faster but its resampler is a `LGPL-2.1` port of FFmpeg's `av_resample`,
//! which fails our permissive-only policy. (The cloud, being a service, may use any
//! variant server-side.)

use std::path::Path;

use base64::Engine;
use rusty_chromaprint::{Configuration, FingerprintCompressor, Fingerprinter};
use soul_audio_core::Decoder;

/// Cap the audio fed to the fingerprinter, matching fpcalc/AcoustID's default
/// (~first two minutes). Bounds memory + keeps fingerprints comparable with the
/// cloud's index, which is built the same way.
const MAX_FINGERPRINT_SECS: u32 = 120;

/// A track's acoustic fingerprint plus the decoded duration, the identity payload
/// the client submits to the enrichment cloud (with the tags-MBID, when present).
/// The audio itself never leaves the machine.
#[derive(Clone, Debug, PartialEq)]
pub struct Fingerprint {
    /// Raw Chromaprint sub-fingerprints (default Algorithm 2), one `u32` per frame.
    pub raw: Vec<u32>,
    /// Decoded audio duration in seconds. Sent alongside the fingerprint, AcoustID
    /// and our cloud key on `(fingerprint, duration)` and use it to align matches.
    pub duration_secs: f64,
}

impl Fingerprint {
    /// The AcoustID-style wire string: the Chromaprint-**compressed** fingerprint,
    /// URL-safe base64 without padding, the exact form `fpcalc`/AcoustID transmit,
    /// so our cloud (or AcoustID itself) decodes it directly.
    #[must_use]
    pub fn compressed_base64(&self) -> String {
        let config = Configuration::preset_test2();
        let bytes = FingerprintCompressor::from(&config).compress(&self.raw);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }

    /// Whether no fingerprint was produced (too little audio, Chromaprint needs a
    /// few seconds before it emits its first sub-fingerprint).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }
}

/// Fingerprint already-decoded interleaved **i16** PCM (`L,R,L,R…` for stereo).
/// `sample_rate` is the PCM rate, Chromaprint resamples to 11025 Hz internally, so
/// any rate is fine. Pure + deterministic: the unit-testable core.
#[must_use]
pub fn fingerprint_pcm(samples: &[i16], sample_rate: u32, channels: u16) -> Fingerprint {
    let config = Configuration::preset_test2();
    let mut fp = Fingerprinter::new(&config);
    // `start` only errors on an implausible rate/channel count, treat as "nothing
    // to fingerprint" rather than surfacing an error to the caller.
    if fp
        .start(sample_rate.max(1), u32::from(channels.max(1)))
        .is_err()
    {
        return Fingerprint {
            raw: Vec::new(),
            duration_secs: 0.0,
        };
    }
    fp.consume(samples);
    fp.finish();
    let frames = samples.len() / usize::from(channels.max(1));
    Fingerprint {
        raw: fp.fingerprint().to_vec(),
        duration_secs: frames as f64 / f64::from(sample_rate.max(1)),
    }
}

/// Decode `path` through the shared decoder (the same one playback uses) and
/// fingerprint up to [`MAX_FINGERPRINT_SECS`] of it. The decoder's native-bit `i32`
/// samples are scaled to `i16` for Chromaprint. Errors only when the file can't be
/// opened/decoded; a too-short (or silent/degenerate) file returns an **empty**
/// fingerprint, not an error.
///
/// **Call from a thread with a large stack.** `OwnedDecoder` (the big `AnyDecoder`)
/// is large by value and overflows a normal stack, the future background
/// "identify" job runs on a 64 MiB worker, exactly as [`crate::loudness`] does for
/// the same reason.
pub fn fingerprint_file(path: &Path) -> anyhow::Result<Fingerprint> {
    let mut dec = soul_audio_desktop::open_track(path)?;
    let info = dec.stream_info();
    let (sample_rate, channels, bits) = (info.sample_rate, info.channels, info.bits_per_sample());

    let cap_samples = (MAX_FINGERPRINT_SECS as usize)
        .saturating_mul(sample_rate.max(1) as usize)
        .saturating_mul(usize::from(channels.max(1)));
    let mut pcm: Vec<i16> = Vec::new();
    let mut buf = [0i32; 8192];
    loop {
        let n = dec
            .decode_into(&mut buf)
            .map_err(|e| anyhow::anyhow!("decoding {}: {e:?}", path.display()))?;
        if n == 0 {
            break; // end of stream
        }
        for &s in &buf[..n] {
            pcm.push(scale_to_i16(s, bits));
        }
        if pcm.len() >= cap_samples {
            break; // enough audio for a fingerprint
        }
    }
    Ok(fingerprint_pcm(&pcm, sample_rate, channels))
}

/// Scale a native-`bits` `i32` PCM sample to `i16` (clamped): 16-bit → identity,
/// 24-bit → `>> 8`, 8-bit → `<< 8`. (The playback deck normalizes to canonical
/// 24-bit; the fingerprinter wants 16-bit, so we convert straight here.)
fn scale_to_i16(sample: i32, bits: u8) -> i16 {
    let shifted = if bits >= 16 {
        sample >> (bits - 16)
    } else {
        sample << (16 - bits)
    };
    shifted.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `secs` of an interleaved-stereo sine tone at `freq` Hz, 44100 Hz, as i16.
    fn sine_pcm(freq: f64, secs: f64, rate: u32) -> Vec<i16> {
        let frames = (secs * f64::from(rate)) as usize;
        let mut v = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let t = i as f64 / f64::from(rate);
            let s =
                (0.6 * (2.0 * std::f64::consts::PI * freq * t).sin() * f64::from(i16::MAX)) as i16;
            v.push(s); // L
            v.push(s); // R
        }
        v
    }

    #[test]
    fn fingerprint_pcm_is_deterministic_and_nonempty() {
        let pcm = sine_pcm(440.0, 8.0, 44_100);
        let a = fingerprint_pcm(&pcm, 44_100, 2);
        let b = fingerprint_pcm(&pcm, 44_100, 2);
        assert!(!a.is_empty(), "8 s of audio produces a fingerprint");
        assert_eq!(a.raw, b.raw, "the same PCM always fingerprints identically");
        assert!(
            (a.duration_secs - 8.0).abs() < 0.05,
            "duration ~8 s, got {}",
            a.duration_secs
        );
    }

    #[test]
    fn different_audio_yields_different_fingerprints() {
        // Different *pitch classes* (A4 vs E4) → different chroma → different print.
        let a = fingerprint_pcm(&sine_pcm(440.0, 8.0, 44_100), 44_100, 2);
        let e = fingerprint_pcm(&sine_pcm(329.63, 8.0, 44_100), 44_100, 2);
        assert_ne!(
            a.raw, e.raw,
            "an A4 and an E4 tone must fingerprint differently"
        );
    }

    #[test]
    fn octave_equivalent_tones_share_a_fingerprint() {
        // Chromaprint is a *chroma* fingerprint, octave-invariant, so A4 (440 Hz)
        // and A5 (880 Hz), the same pitch class, fingerprint identically. Documents
        // the property (and that `different_audio` must use distinct pitch classes).
        let a4 = fingerprint_pcm(&sine_pcm(440.0, 8.0, 44_100), 44_100, 2);
        let a5 = fingerprint_pcm(&sine_pcm(880.0, 8.0, 44_100), 44_100, 2);
        assert_eq!(
            a4.raw, a5.raw,
            "octave-equivalent pure tones share a chroma"
        );
    }

    #[test]
    fn too_little_audio_is_empty() {
        // A fraction of a second: below Chromaprint's minimum → no sub-fingerprint.
        let fp = fingerprint_pcm(&sine_pcm(440.0, 0.1, 44_100), 44_100, 2);
        assert!(fp.is_empty(), "a 0.1 s clip is too short to fingerprint");
    }

    #[test]
    fn compressed_base64_is_url_safe_and_decodes_back() {
        let fp = fingerprint_pcm(&sine_pcm(440.0, 8.0, 44_100), 44_100, 2);
        let s = fp.compressed_base64();
        assert!(
            !s.is_empty(),
            "a non-empty fingerprint compresses to a string"
        );
        // URL-safe base64, no padding: only [A-Za-z0-9_-].
        assert!(
            s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "wire string is URL-safe base64 without padding: {s:.32}…"
        );
        assert!(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(&s)
                .is_ok(),
            "the wire string round-trips through URL-safe base64"
        );
    }

    #[test]
    fn scale_to_i16_handles_bit_depths() {
        assert_eq!(scale_to_i16(12_345, 16), 12_345, "16-bit is identity");
        assert_eq!(
            scale_to_i16(12_345 << 8, 24),
            12_345,
            "24-bit shifts down 8"
        );
        assert_eq!(scale_to_i16(100, 8), 100 << 8, "8-bit shifts up 8");
        assert_eq!(scale_to_i16(i32::MAX, 24), i16::MAX, "overflow clamps");
    }

    #[test]
    fn fingerprint_file_decodes_and_fingerprints_a_wav() {
        // A 6 s tone WAV, decoded through the shared decoder → a real fingerprint.
        let dir = std::env::temp_dir().join(format!("sas_fp_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tone.wav");
        std::fs::write(&path, tone_wav(440.0, 6.0, 44_100)).unwrap();

        // `OwnedDecoder` is large by value → run on a big stack, as the real job does.
        let p = path.clone();
        let fp = std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(move || fingerprint_file(&p))
            .unwrap()
            .join()
            .unwrap()
            .expect("fingerprint the wav");
        assert!(!fp.is_empty(), "a 6 s file fingerprints to something");
        assert!(
            (fp.duration_secs - 6.0).abs() < 0.1,
            "duration ~6 s, got {}",
            fp.duration_secs
        );
        // Same identity as fingerprinting the PCM directly (the decode path matches).
        let direct = fingerprint_pcm(&sine_pcm(440.0, 6.0, 44_100), 44_100, 2);
        assert_eq!(
            fp.raw, direct.raw,
            "decoding the WAV then fingerprinting == fingerprinting the PCM"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A 16-bit PCM stereo WAV of a `freq` Hz tone (matches [`sine_pcm`]).
    fn tone_wav(freq: f64, secs: f64, rate: u32) -> Vec<u8> {
        let samples = sine_pcm(freq, secs, rate);
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
}
