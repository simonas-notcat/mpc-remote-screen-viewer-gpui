//! Bounded source-clock audio queue with cubic resampling and drift correction.
//! Synthetic tests do not establish hardware latency or audible playback quality.
use crate::protocol::{AudioFormat, Error, Result};

pub const DEFAULT_TARGET_BUFFER_MS: u32 = 80;
pub const MIN_TARGET_BUFFER_MS: u32 = 10; // Two 5 ms fades.
pub const MAX_TARGET_BUFFER_MS: u32 = 300;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    pub buffered_ms: f64,
    pub target_buffer_ms: u32,
    pub underflows: u64,
    pub discontinuities: u64,
    pub latency_trims: u64,
    pub dropped_packets: u64,
    pub ratio: f64,
    pub playing: bool,
}

pub struct Playout {
    format: AudioFormat,
    ring: Vec<f32>,
    capacity: u64,
    write: u64,
    read: f64,
    expected: Option<u64>,
    pending_gap: bool,
    markers: [u64; 64],
    marker_head: usize,
    marker_len: usize,
    target: f64,
    maximum: f64,
    fade: f64,
    elapsed: f64,
    trim_remaining: Option<f64>,
    stats: Stats,
}
impl Playout {
    pub fn new(format: AudioFormat) -> Result<Self> {
        format.validate()?;
        let capacity = format.sample_rate.next_power_of_two() as u64;
        let rate = f64::from(format.sample_rate);
        let fade = (0.005 * rate).max(1.0);
        let target = (f64::from(DEFAULT_TARGET_BUFFER_MS) * rate / 1000.0).max(2.0 * fade);
        let maximum = (0.75 * capacity as f64).min((1.5 * target).max(0.300 * rate));
        Ok(Self {
            format,
            ring: vec![0.0; capacity as usize * usize::from(format.channels)],
            capacity,
            write: 0,
            read: 0.0,
            expected: None,
            pending_gap: false,
            markers: [0; 64],
            marker_head: 0,
            marker_len: 0,
            target,
            maximum,
            fade,
            elapsed: 0.0,
            trim_remaining: None,
            stats: Stats {
                target_buffer_ms: DEFAULT_TARGET_BUFFER_MS,
                ratio: 1.0,
                ..Stats::default()
            },
        })
    }
    /// Update latency without reallocating the ring or changing source indices.
    /// An active stream fades out, trims or prefills to the new target, then ramps in.
    pub fn set_target_buffer_ms(&mut self, milliseconds: u32) {
        let milliseconds = milliseconds.clamp(MIN_TARGET_BUFFER_MS, MAX_TARGET_BUFFER_MS);
        if milliseconds == self.stats.target_buffer_ms {
            return;
        }
        let old_target = self.target;
        self.stats.target_buffer_ms = milliseconds;
        let rate = f64::from(self.format.sample_rate);
        self.target = (f64::from(milliseconds) * rate / 1000.0).max(2.0 * self.fade);
        self.maximum = (0.75 * self.capacity as f64).min((1.5 * self.target).max(0.300 * rate));
        if self.stats.playing {
            if self.trim_remaining.is_none() {
                self.trim_remaining = Some(self.fade);
                if self.write as f64 - self.read > self.target {
                    self.stats.latency_trims += 1;
                }
            }
        } else if self.target < old_target {
            // A setting changed before output startup: discard excess without a click.
            self.read = self.read.max(self.write as f64 - self.target);
            self.pop_markers();
        }
    }
    pub fn stats(&self) -> Stats {
        Stats {
            buffered_ms: (self.write as f64 - self.read).max(0.0) * 1000.0
                / f64::from(self.format.sample_rate),
            ..self.stats
        }
    }
    pub fn mark_gap(&mut self) {
        self.pending_gap = true;
    }
    /// Returns accepted frames. Duplicate/overlapping source positions never replay.
    pub fn push(&mut self, first: u64, samples: &[f32]) -> Result<usize> {
        let channels = usize::from(self.format.channels);
        if samples.is_empty() || !samples.len().is_multiple_of(channels) {
            return Err(Error::Invalid("interleaved audio length"));
        }
        let frames = samples.len() / channels;
        if frames > 8192 || samples.iter().any(|s| !s.is_finite()) {
            return Err(Error::Invalid("decoded audio samples"));
        }
        let end = first
            .checked_add(frames as u64)
            .ok_or(Error::Invalid("audio source index overflow"))?;
        let skip = if let Some(expected) = self.expected {
            if end <= expected {
                return Ok(0);
            }
            if first > expected {
                self.pending_gap = true;
            }
            expected.saturating_sub(first) as usize
        } else {
            0
        };
        self.expected = Some(end);
        let frames = frames - skip;
        let retained = (self.read as u64).saturating_sub(1);
        // Four spare frames protect the cubic interpolator at the ring boundary.
        if self.write.saturating_sub(retained) + frames as u64 + 4 > self.capacity
            || (self.pending_gap && self.marker_len == self.markers.len())
        {
            self.pending_gap = true;
            self.stats.dropped_packets += 1;
            return Ok(0);
        }
        if self.pending_gap {
            self.markers[(self.marker_head + self.marker_len) % 64] = self.write;
            self.marker_len += 1;
            self.pending_gap = false;
        }
        for (i, frame) in samples[skip * channels..]
            .chunks_exact(channels)
            .enumerate()
        {
            let offset = ((self.write + i as u64) & (self.capacity - 1)) as usize * channels;
            self.ring[offset..offset + channels].copy_from_slice(frame);
        }
        self.write += frames as u64;
        Ok(frames)
    }
    fn marker(&self) -> Option<u64> {
        (self.marker_len != 0).then(|| self.markers[self.marker_head])
    }
    fn pop_markers(&mut self) -> bool {
        let mut popped = false;
        while self.marker().is_some_and(|m| m as f64 <= self.read) {
            self.marker_head = (self.marker_head + 1) % 64;
            self.marker_len -= 1;
            popped = true;
        }
        popped
    }
    fn sample(&self, position: u64, channel: usize) -> f32 {
        let position = position.min(self.write - 1);
        self.ring[(position & (self.capacity - 1)) as usize * usize::from(self.format.channels)
            + channel]
    }
    /// Writes interleaved samples through a caller-supplied, allocation-free sink.
    /// The caller pre-clears the destination (prefill/underflow leave silence).
    pub fn render(
        &mut self,
        frames: usize,
        channels: usize,
        rate: u32,
        mut put: impl FnMut(usize, f32),
    ) {
        if channels == 0 || rate == 0 || frames == 0 {
            return;
        }
        let buffered = self.write as f64 - self.read;
        if !self.stats.playing {
            if buffered < self.target {
                return;
            }
            if buffered > self.maximum {
                self.read = self.write as f64 - self.target;
            }
            self.pop_markers();
            self.stats.playing = true;
            self.elapsed = 0.0;
            self.trim_remaining = None;
        }
        let buffered = self.write as f64 - self.read;
        if self.trim_remaining.is_none() && buffered > self.maximum {
            self.trim_remaining = Some(self.fade);
            self.stats.latency_trims += 1;
        }
        let error = ((buffered - self.target) / self.target).clamp(-1.0, 1.0);
        self.stats.ratio += 0.05 * (1.0 + 0.004 * error - self.stats.ratio);
        let step = f64::from(self.format.sample_rate) / f64::from(rate) * self.stats.ratio;
        let source_channels = usize::from(self.format.channels);
        for frame in 0..frames {
            if self.pop_markers() {
                self.elapsed = 0.0;
                self.stats.discontinuities += 1;
            }
            let boundary = self.marker().unwrap_or(self.write) as f64;
            let remaining = boundary - self.read;
            if remaining <= 0.0 {
                self.stats.playing = false;
                self.trim_remaining = None;
                self.stats.underflows += 1;
                break;
            }
            let gain = (self.elapsed / self.fade).min(1.0)
                * (remaining / self.fade).min(1.0)
                * self
                    .trim_remaining
                    .map_or(1.0, |r| (r / self.fade).max(0.0));
            let position = self.read as u64;
            let t = (self.read - position as f64) as f32;
            let mut values = [0.0; 8];
            for (ch, value) in values[..source_channels].iter_mut().enumerate() {
                *value = cubic(
                    self.sample(position.saturating_sub(1), ch),
                    self.sample(position, ch),
                    self.sample(position + 1, ch),
                    self.sample(position + 2, ch),
                    t,
                );
            }
            for ch in 0..channels {
                // Route corresponding channels; leave extra outputs silent.
                // Mono sources duplicate, and mono output averages source channels.
                let value = if source_channels == 1 {
                    values[0]
                } else if channels == 1 {
                    values[..source_channels].iter().sum::<f32>() / source_channels as f32
                } else {
                    values
                        .get(ch)
                        .copied()
                        .filter(|_| ch < source_channels)
                        .unwrap_or(0.0)
                };
                put(frame * channels + ch, value * gain as f32);
            }
            self.read += step;
            self.elapsed += step;
            if let Some(remaining) = &mut self.trim_remaining {
                *remaining -= step;
                if *remaining <= 0.0 {
                    self.trim_remaining = None;
                    self.read = self.read.max(self.write as f64 - self.target);
                    self.pop_markers();
                    self.elapsed = 0.0;
                    if self.write as f64 - self.read < self.target {
                        // Raising the target needs a fresh prefill, not a minute of
                        // slow drift correction. The rest of this callback is silent.
                        self.stats.playing = false;
                        break;
                    }
                }
            }
        }
    }
}

pub fn cubic(p0: f32, p1: f32, p2: f32, p3: f32, t: f32) -> f32 {
    let a = 0.5 * (p2 - p0);
    let b = p0 - 2.5 * p1 + 2.0 * p2 - 0.5 * p3;
    let c = 0.5 * (p3 - p0) + 1.5 * (p1 - p2);
    p1 + t * (a + t * (b + t * c))
}

pub fn peak_db(peak: f32) -> f32 {
    if peak.is_finite() {
        20.0 * peak.abs().max(0.000001).log10()
    } else {
        -120.0
    }
}
