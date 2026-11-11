//! Headless test doubles: a no-op sink and a constant-sample decoder, so the
//! whole player stack runs in unit tests without an audio device or real files.

use soul_audio_core::{
    DecodeError, Decoder, SampleFormat, SeekableDecoder, Sink, SinkError, StreamInfo,
};

/// A [`Sink`] that discards output — no device, no backpressure. The deck reads
/// its `stream_info` once to configure rate/channels.
pub(crate) struct NullSink {
    pub rate: u32,
    pub channels: u16,
}

impl NullSink {
    pub(crate) fn new(rate: u32, channels: u16) -> Self {
        Self { rate, channels }
    }
}

impl Sink for NullSink {
    fn stream_info(&self) -> StreamInfo {
        StreamInfo {
            sample_rate: self.rate,
            channels: self.channels,
            format: SampleFormat::Int { bits: 24 },
        }
    }
    fn write(&mut self, _frames: &[i32]) -> Result<(), SinkError> {
        Ok(())
    }
    fn flush(&mut self) -> Result<(), SinkError> {
        Ok(())
    }
}

/// A [`Sink`] that RECORDS everything the deck writes, applying the same ramped
/// output-stage volume gain as the real `soul-audio-desktop::sink::CpalSink`
/// (`cur_gain += (target − cur_gain)·0.01` per interleaved sample, truncating
/// cast) and counting `discard` (seek) requests. Lets unit tests observe the
/// actual effect of volume / pause / stop / gapless on the output stream without
/// a device — the headless, deterministic mirror of what a listener hears.
pub(crate) struct CapturingSink {
    pub rate: u32,
    pub channels: u16,
    pub cur_gain: f32,
    pub target_gain: f32,
    pub written: Vec<i32>,
    pub discards: usize,
}

impl CapturingSink {
    pub(crate) fn new(rate: u32, channels: u16) -> Self {
        Self {
            rate,
            channels,
            cur_gain: 1.0,
            target_gain: 1.0,
            written: Vec::new(),
            discards: 0,
        }
    }

    /// Channel `c` of the recorded interleaved stream.
    pub(crate) fn channel(&self, c: usize) -> Vec<i32> {
        self.written
            .chunks_exact(usize::from(self.channels))
            .map(|f| f[c])
            .collect()
    }
}

impl Sink for CapturingSink {
    fn stream_info(&self) -> StreamInfo {
        StreamInfo {
            sample_rate: self.rate,
            channels: self.channels,
            format: SampleFormat::Int { bits: 24 },
        }
    }
    fn write(&mut self, frames: &[i32]) -> Result<(), SinkError> {
        for &s in frames {
            self.cur_gain += (self.target_gain - self.cur_gain) * 0.01;
            self.written.push((s as f32 * self.cur_gain) as i32);
        }
        Ok(())
    }
    fn flush(&mut self) -> Result<(), SinkError> {
        Ok(())
    }
    fn discard(&mut self) -> Result<(), SinkError> {
        self.discards += 1;
        Ok(())
    }
    fn set_volume(&mut self, volume: f32) {
        self.target_gain = volume.clamp(0.0, 1.0);
    }
}

/// A decoder emitting `frames` per-channel frames of a constant sample, then EOF.
/// Seekable (clamps to the total) so it satisfies `SeekableDecoder`.
pub(crate) struct ConstDecoder {
    rate: u32,
    channels: u16,
    total: u64,
    pos: u64,
    value: i32,
}

impl ConstDecoder {
    pub(crate) fn new(rate: u32, channels: u16, frames: u64, value: i32) -> Self {
        Self {
            rate,
            channels,
            total: frames,
            pos: 0,
            value,
        }
    }
}

impl Decoder for ConstDecoder {
    fn stream_info(&self) -> StreamInfo {
        StreamInfo {
            sample_rate: self.rate,
            channels: self.channels,
            format: SampleFormat::Int { bits: 24 },
        }
    }
    fn decode_into(&mut self, out: &mut [i32]) -> Result<usize, DecodeError> {
        let ch = self.channels as u64;
        let remaining_frames = self.total.saturating_sub(self.pos);
        let want_frames = (out.len() as u64 / ch).min(remaining_frames);
        let n = (want_frames * ch) as usize;
        for s in &mut out[..n] {
            *s = self.value;
        }
        self.pos += want_frames;
        Ok(n)
    }
}

impl SeekableDecoder for ConstDecoder {
    fn seek_to(&mut self, sample: u64) -> Result<u64, DecodeError> {
        self.pos = sample.min(self.total);
        Ok(self.pos)
    }
    fn total_frames(&self) -> Option<u64> {
        Some(self.total)
    }
}
