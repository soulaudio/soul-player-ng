//! Headless test doubles: a no-op sink and a constant-sample decoder, so the
//! whole player stack runs in unit tests without an audio device or real files.

use soul_audio_core::{
    DecodeError, Decoder, SampleFormat, SeekableDecoder, Sink, SinkError, StreamInfo,
};

/// A [`Sink`] that discards output, no device, no backpressure. The deck reads
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

/// A [`Sink`] that reports **exclusive** output at a configurable rate, for testing
/// native-rate switching, where the engine reopens an exclusive device **in place** at
/// each track's rate. Discards output like [`NullSink`]. [`reopen`](Sink::reopen)
/// models a real exclusive device: by default it adopts the requested rate (the device
/// supports it); with `refuse_reopen` set it KEEPS its current rate (the device refused
/// the new rate and the real sink would fall back). `reopens` counts reopen calls so a
/// test can assert the engine doesn't thrash a refused rate.
pub(crate) struct ExclusiveSink {
    pub rate: u32,
    pub channels: u16,
    pub refuse_reopen: bool,
    pub reopens: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl ExclusiveSink {
    pub(crate) fn new(rate: u32, channels: u16) -> Self {
        Self {
            rate,
            channels,
            refuse_reopen: false,
            reopens: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// An exclusive sink whose device REFUSES a reopen (keeps its current rate). The
    /// returned `reopens` counter lets a test assert the refused rate isn't retried.
    pub(crate) fn refusing(rate: u32, channels: u16) -> Self {
        Self {
            refuse_reopen: true,
            ..Self::new(rate, channels)
        }
    }
}

impl Sink for ExclusiveSink {
    fn stream_info(&self) -> StreamInfo {
        StreamInfo {
            sample_rate: self.rate,
            channels: self.channels,
            format: SampleFormat::Int { bits: 24 },
        }
    }
    fn exclusive(&self) -> bool {
        true
    }
    fn write(&mut self, _frames: &[i32]) -> Result<(), SinkError> {
        Ok(())
    }
    fn flush(&mut self) -> Result<(), SinkError> {
        Ok(())
    }
    fn reopen(&mut self, rate: u32) -> Result<(), SinkError> {
        self.reopens
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if !self.refuse_reopen {
            self.rate = rate; // device supports the new rate → adopt it
        }
        // else: keep the current rate (the real exclusive sink falls back on refusal)
        Ok(())
    }
}

/// A [`Sink`] that RECORDS everything the deck writes, applying the same ramped
/// output-stage volume gain as the real `soul-audio-desktop::sink::CpalSink`
/// (`cur_gain += (target − cur_gain)·0.01` per interleaved sample, truncating
/// cast) and counting `discard` (seek) requests. Lets unit tests observe the
/// actual effect of volume / pause / stop / gapless on the output stream without
/// a device, the headless, deterministic mirror of what a listener hears.
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
    /// If set, `stream_info().sample_rate` flips to this once the first frame has been
    /// decoded, mimicking implicit HE-AAC (SBR), which reports the AAC core rate at
    /// load and the 2× output rate only after decoding starts.
    sbr_rate: Option<u32>,
    confirmed: bool,
    channels: u16,
    total: u64,
    pos: u64,
    value: i32,
}

impl ConstDecoder {
    pub(crate) fn new(rate: u32, channels: u16, frames: u64, value: i32) -> Self {
        Self {
            rate,
            sbr_rate: None,
            confirmed: false,
            channels,
            total: frames,
            pos: 0,
            value,
        }
    }

    /// A decoder that mimics implicit HE-AAC (SBR): reports `core_rate` at load, then
    /// flips `stream_info().sample_rate` to `sbr_rate` after the first `decode_into`.
    pub(crate) fn new_sbr(
        core_rate: u32,
        sbr_rate: u32,
        channels: u16,
        frames: u64,
        value: i32,
    ) -> Self {
        Self {
            rate: core_rate,
            sbr_rate: Some(sbr_rate),
            confirmed: false,
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
            sample_rate: if self.confirmed {
                self.sbr_rate.unwrap_or(self.rate)
            } else {
                self.rate
            },
            channels: self.channels,
            format: SampleFormat::Int { bits: 24 },
        }
    }
    fn decode_into(&mut self, out: &mut [i32]) -> Result<usize, DecodeError> {
        self.confirmed = true; // decoding the first frame confirms the (SBR) output rate
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

/// A [`Sink`] that carries an output IDENTITY, the selected device + exclusive mode,
/// and counts the frames written to it, for testing LIVE output-config changes (where
/// changing the device or exclusive mode rebuilds the sink mid-playback via
/// `swap_sink`). It discards the audio but records that writes keep flowing after a
/// swap, and reports `exclusive()` from its config so a test can see the mode changed.
pub(crate) struct ConfigSink {
    pub device: Option<String>,
    pub exclusive: bool,
    pub rate: u32,
    pub channels: u16,
    pub frames: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// When set, counts live "device holders": incremented on construction and
    /// decremented on `Drop`. Lets a test prove release-then-acquire, the OLD sink
    /// must be dropped (its device freed) BEFORE the new one opens. `None` = a
    /// device-free placeholder (the parked sink), which never counts.
    device_live: Option<std::sync::Arc<std::sync::atomic::AtomicUsize>>,
    /// When set and `true`, the device is "unplugged": `write`/`flush` fail with
    /// `SinkError::Closed`, exactly as a real exclusive sink does when its DAC vanishes.
    unplugged: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl ConfigSink {
    /// Wire an "unplug" switch: flipping `flag` to `true` makes the sink fail with
    /// `SinkError::Closed` from then on.
    pub(crate) fn with_unplug(
        mut self,
        flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        self.unplugged = Some(flag);
        self
    }

    fn is_unplugged(&self) -> bool {
        self.unplugged
            .as_ref()
            .is_some_and(|f| f.load(std::sync::atomic::Ordering::Relaxed))
    }

    pub(crate) fn new(
        device: Option<String>,
        exclusive: bool,
        rate: u32,
        channels: u16,
        frames: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> Self {
        Self {
            device,
            exclusive,
            rate,
            channels,
            frames,
            device_live: None,
            unplugged: None,
        }
    }

    /// Like [`ConfigSink::new`] but counts as a live device holder in `device_live`
    /// (incremented now, decremented on `Drop`), for release-then-acquire tests.
    pub(crate) fn new_counting(
        device: Option<String>,
        exclusive: bool,
        rate: u32,
        channels: u16,
        frames: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        device_live: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> Self {
        device_live.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self {
            device,
            exclusive,
            rate,
            channels,
            frames,
            device_live: Some(device_live),
            unplugged: None,
        }
    }
}

impl Drop for ConfigSink {
    fn drop(&mut self) {
        if let Some(live) = &self.device_live {
            live.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

impl Sink for ConfigSink {
    fn stream_info(&self) -> StreamInfo {
        StreamInfo {
            sample_rate: self.rate,
            channels: self.channels,
            format: SampleFormat::Int { bits: 24 },
        }
    }
    fn exclusive(&self) -> bool {
        self.exclusive
    }
    fn write(&mut self, frames: &[i32]) -> Result<(), SinkError> {
        if self.is_unplugged() {
            return Err(SinkError::Closed);
        }
        self.frames.fetch_add(
            frames.len() / usize::from(self.channels.max(1)),
            std::sync::atomic::Ordering::Relaxed,
        );
        Ok(())
    }
    fn flush(&mut self) -> Result<(), SinkError> {
        if self.is_unplugged() {
            return Err(SinkError::Closed);
        }
        Ok(())
    }
}
