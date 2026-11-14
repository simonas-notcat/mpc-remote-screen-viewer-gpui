//! Packet decoding and a shared playback sink. Codec 2 still needs device validation.
use crate::{
    playout::{
        DEFAULT_TARGET_BUFFER_MS, MAX_TARGET_BUFFER_MS, MIN_TARGET_BUFFER_MS, Playout, Stats,
    },
    protocol::{AudioFormat, AudioPacket, Error, Reader, Result},
};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub format: Option<AudioFormat>,
    pub error: Option<String>,
    pub stats: Stats,
    pub input_peak: f32,
    pub output_peak: f32,
    /// Enabled output callbacks that returned silence because the source was busy.
    pub contention_callbacks: u64,
}
#[derive(Default)]
struct Source {
    epoch: u64,
    format: Option<AudioFormat>,
    queue: Option<Playout>,
    error: Option<String>,
}
/// One sink per session. The callback never waits for its lock or allocates.
pub struct Audio {
    source: Mutex<Source>,
    enabled: AtomicBool,
    volume: AtomicU32,
    target_buffer_ms: AtomicU32,
    input_peak: AtomicU32,
    output_peak: AtomicU32,
    recording: Mutex<Option<crate::recording::AudioInput>>,
    analyzer: Mutex<Option<crate::analyzer::Input>>,
    contention_callbacks: AtomicU64,
}
impl Default for Audio {
    fn default() -> Self {
        Self {
            source: Mutex::default(),
            enabled: AtomicBool::new(false),
            volume: AtomicU32::new(1.0f32.to_bits()),
            target_buffer_ms: AtomicU32::new(DEFAULT_TARGET_BUFFER_MS),
            input_peak: AtomicU32::new(0),
            output_peak: AtomicU32::new(0),
            recording: Mutex::new(None),
            analyzer: Mutex::new(None),
            contention_callbacks: AtomicU64::new(0),
        }
    }
}
impl Audio {
    pub fn set_analyzer(&self, input: Option<crate::analyzer::Input>) {
        *self.analyzer.lock().unwrap() = input;
    }
    pub fn set_recording(&self, input: Option<crate::recording::AudioInput>) {
        *self.recording.lock().unwrap() = input;
    }
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Release);
    }
    pub fn set_volume(&self, volume: f32) {
        let volume = if volume.is_finite() {
            volume.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.volume.store(volume.to_bits(), Ordering::Relaxed);
    }
    pub fn set_target_buffer_ms(&self, milliseconds: u32) {
        self.target_buffer_ms.store(
            milliseconds.clamp(MIN_TARGET_BUFFER_MS, MAX_TARGET_BUFFER_MS),
            Ordering::Relaxed,
        );
    }
    pub fn reset(&self) {
        let old = {
            let mut source = self.source.lock().unwrap();
            let epoch = source.epoch.wrapping_add(1);
            std::mem::replace(
                &mut *source,
                Source {
                    epoch,
                    ..Source::default()
                },
            )
        };
        drop(old);
        self.contention_callbacks.store(0, Ordering::Relaxed);
        self.input_peak.store(0, Ordering::Relaxed);
        self.output_peak.store(0, Ordering::Relaxed);
    }
    pub fn configure(&self, format: AudioFormat) -> Result<()> {
        // Allocate outside the lock; output renders silence while state changes.
        let mut queue = Playout::new(format)?;
        queue.set_target_buffer_ms(self.target_buffer_ms.load(Ordering::Relaxed));
        let old = {
            let mut source = self.source.lock().unwrap();
            let epoch = source.epoch.wrapping_add(1);
            std::mem::replace(
                &mut *source,
                Source {
                    epoch,
                    format: Some(format),
                    queue: Some(queue),
                    error: None,
                },
            )
        };
        drop(old);
        Ok(())
    }
    pub fn invalidate(&self, error: String) {
        self.reset();
        self.source.lock().unwrap().error = Some(error);
    }
    pub fn packet(&self, payload: &[u8]) -> Result<()> {
        if !self.enabled.load(Ordering::Acquire) {
            return Ok(());
        }
        let (format, epoch) = {
            let source = self.source.lock().unwrap();
            let Some(format) = source.format else {
                return Ok(());
            };
            (format, source.epoch)
        };
        let decoded = (|| {
            let packet = AudioPacket::parse(payload, format)?;
            Ok((packet.first_sample_index, decode(format, &packet)?))
        })();
        match decoded {
            Ok((first, samples)) => {
                let peak = samples.iter().fold(0.0f32, |a, s| a.max(s.abs()));
                {
                    let mut source = self.source.lock().unwrap();
                    // Epoch also rejects a reset/reconfigure to the very same format.
                    if source.epoch != epoch || source.format != Some(format) {
                        return Ok(());
                    }
                    source.queue.as_mut().unwrap().push(first, &samples)?;
                    source.error = None;
                }
                // Cloning the tap, copying samples, and metering never hold the
                // playback lock: a recording must not make the RT callback silent.
                let recording = self.recording.lock().unwrap().clone();
                if let Some(input) = recording {
                    input.push(format, epoch, first, &samples);
                }
                let analyzer = self.analyzer.lock().unwrap().clone();
                if let Some(input) = analyzer {
                    input.push(format, epoch, first, &samples);
                }
                self.input_peak.fetch_max(peak.to_bits(), Ordering::Relaxed);
                Ok(())
            }
            Err(error) => {
                let mut source = self.source.lock().unwrap();
                if source.epoch == epoch && source.format == Some(format) {
                    source.queue.as_mut().unwrap().mark_gap();
                    source.error = Some(format!("Skipped corrupt device audio: {error}"));
                }
                Err(error)
            }
        }
    }
    /// Interval peaks, consumed by the meter polling task (not the callback).
    pub fn take_peaks(&self) -> (f32, f32) {
        (
            f32::from_bits(self.input_peak.swap(0, Ordering::Relaxed)),
            f32::from_bits(self.output_peak.swap(0, Ordering::Relaxed)),
        )
    }
    pub fn status(&self) -> Status {
        let source = self.source.lock().unwrap();
        Status {
            format: source.format,
            error: source.error.clone(),
            stats: source
                .queue
                .as_ref()
                .map_or_else(Stats::default, Playout::stats),
            input_peak: f32::from_bits(self.input_peak.load(Ordering::Relaxed)),
            output_peak: f32::from_bits(self.output_peak.load(Ordering::Relaxed)),
            contention_callbacks: self.contention_callbacks.load(Ordering::Relaxed),
        }
    }
    /// The destination is always completely initialized, including on contention.
    pub fn render(
        &self,
        frames: usize,
        channels: usize,
        rate: u32,
        mut put: impl FnMut(usize, f32),
    ) {
        for i in 0..frames * channels {
            put(i, 0.0);
        }
        let mut peak = 0.0f32;
        if self.enabled.load(Ordering::Acquire) {
            match self.source.try_lock() {
                Ok(mut source) => {
                    if let Some(queue) = &mut source.queue {
                        queue.set_target_buffer_ms(self.target_buffer_ms.load(Ordering::Relaxed));
                        let volume = f32::from_bits(self.volume.load(Ordering::Relaxed));
                        let gain = volume * volume;
                        queue.render(frames, channels, rate, |i, sample| {
                            let sample = sample * gain;
                            peak = peak.max(sample.abs());
                            put(i, sample);
                        });
                    }
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    self.contention_callbacks.fetch_add(1, Ordering::Relaxed);
                }
                Err(std::sync::TryLockError::Poisoned(_)) => {}
            }
        }
        self.output_peak
            .fetch_max(peak.to_bits(), Ordering::Relaxed);
    }
}

const STEP: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];
const INDEX: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

pub fn decode(format: AudioFormat, packet: &AudioPacket<'_>) -> Result<Vec<f32>> {
    // Validate even when the caller manually constructed a packet/format.
    packet.validate(format)?;
    let c = usize::from(format.channels);
    let f = usize::from(packet.frames);
    let samples: Vec<i16> = match format.codec {
        0 => packet
            .encoded
            .as_chunks::<2>()
            .0
            .iter()
            .map(|s| i16::from_le_bytes(*s))
            .collect(),
        1 => decode_ima(packet.encoded, f, c)?,
        2 => decode_rice(packet.encoded, f, c)?,
        _ => unreachable!(),
    };
    Ok(samples
        .into_iter()
        .map(|s| f32::from(s) / 32768.0)
        .collect())
}
fn decode_ima(encoded: &[u8], frames: usize, channels: usize) -> Result<Vec<i16>> {
    let mut r = Reader::new(encoded);
    let mut predictor = [0i32; 8];
    let mut index = [0i32; 8];
    let mut out = vec![0; frames * channels];
    for ch in 0..channels {
        predictor[ch] = i32::from(r.u16()? as i16);
        index[ch] = i32::from(r.u8()?);
        r.u8()?; // reserved
        if index[ch] > 88 {
            return Err(Error::Invalid("IMA step index"));
        }
        out[ch] = predictor[ch] as i16;
    }
    let nibbles = r.rest();
    for n in 0..(frames - 1) * channels {
        let ch = n % channels;
        let nibble = (nibbles[n / 2] >> ((n % 2) * 4)) & 15;
        let step = STEP[index[ch] as usize];
        let mut diff = step >> 3;
        if nibble & 4 != 0 {
            diff += step;
        }
        if nibble & 2 != 0 {
            diff += step >> 1;
        }
        if nibble & 1 != 0 {
            diff += step >> 2;
        }
        if nibble & 8 != 0 {
            diff = -diff;
        }
        predictor[ch] = (predictor[ch] + diff).clamp(-32768, 32767);
        index[ch] = (index[ch] + INDEX[(nibble & 7) as usize]).clamp(0, 88);
        out[channels + n] = predictor[ch] as i16;
    }
    Ok(out)
}
struct Bits<'a> {
    bytes: &'a [u8],
    position: usize,
}
impl Bits<'_> {
    fn read(&mut self, n: usize) -> Result<u32> {
        if self.position + n > self.bytes.len() * 8 {
            return Err(Error::Invalid("truncated Rice bits"));
        }
        let mut value = 0;
        for bit in 0..n {
            value |= u32::from((self.bytes[self.position / 8] >> (self.position % 8)) & 1) << bit;
            self.position += 1;
        }
        Ok(value)
    }
    fn residual(&mut self, k: usize) -> Result<i32> {
        let mut q = 0;
        while q < 24 && self.read(1)? == 1 {
            q += 1;
        }
        let u = if q == 24 {
            self.read(21)?
        } else {
            (q << k) | self.read(k)?
        };
        Ok((u >> 1) as i32 ^ -((u & 1) as i32))
    }
}
fn decode_rice(encoded: &[u8], frames: usize, channels: usize) -> Result<Vec<i16>> {
    let mut r = Reader::new(encoded);
    let mut out = vec![0i16; frames * channels];
    for ch in 0..channels {
        let control = r.u8()?;
        let length = usize::from(r.u16()?);
        let block = r.take(length)?;
        if control == 0x80 {
            if length != frames * 2 {
                return Err(Error::Invalid("raw lossless channel length"));
            }
            for (i, s) in block.as_chunks::<2>().0.iter().enumerate() {
                out[i * channels + ch] = i16::from_le_bytes(*s);
            }
            continue;
        }
        if control & 0xc0 != 0 {
            return Err(Error::Invalid("lossless channel control"));
        }
        let order = usize::from((control >> 4) & 3);
        let k = usize::from(control & 15);
        if order >= frames {
            return Err(Error::Invalid("lossless predictor order"));
        }
        let mut initial = Reader::new(block);
        for i in 0..order {
            out[i * channels + ch] = initial.u16()? as i16;
        }
        let mut bits = Bits {
            bytes: initial.rest(),
            position: 0,
        };
        for i in order..frames {
            let s = |back| i32::from(out[(i - back) * channels + ch]);
            let predicted = match order {
                0 => 0,
                1 => s(1),
                2 => 2 * s(1) - s(2),
                3 => 3 * s(1) - 3 * s(2) + s(3),
                _ => unreachable!(),
            };
            let sample = predicted
                .checked_add(bits.residual(k)?)
                .ok_or(Error::Invalid("lossless arithmetic overflow"))?;
            out[i * channels + ch] =
                i16::try_from(sample).map_err(|_| Error::Invalid("lossless sample overflow"))?;
        }
        if bits.position.div_ceil(8) != bits.bytes.len() {
            return Err(Error::Invalid("lossless channel has unused bytes"));
        }
    }
    if !r.rest().is_empty() {
        return Err(Error::Invalid("lossless packet has unused bytes"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callback_contention_outputs_silence_without_waiting() {
        let audio = Audio::default();
        audio.set_enabled(true);
        let _held = audio.source.lock().unwrap();
        let mut output = [f32::NAN; 128];
        // Same-thread lock ownership makes a blocking lock here deadlock the test.
        audio.render(64, 2, 48000, |i, v| output[i] = v);
        assert!(output.iter().all(|v| *v == 0.0));
        assert_eq!(audio.contention_callbacks.load(Ordering::Relaxed), 1);
        drop(_held);
        assert_eq!(audio.status().contention_callbacks, 1);
        audio.reset();
        assert_eq!(audio.status().contention_callbacks, 0);
    }

    #[test]
    fn recording_tap_contention_does_not_hold_the_playback_lock() {
        use std::{
            sync::Arc,
            thread,
            time::{Duration, Instant},
        };
        let audio = Arc::new(Audio::default());
        audio.set_enabled(true);
        audio
            .configure(AudioFormat {
                sample_rate: 8000,
                channels: 1,
                codec: 0,
            })
            .unwrap();
        // Block tap delivery while a packet is decoded and enqueued.
        let held = audio.recording.lock().unwrap();
        let sink = audio.clone();
        let packet = thread::spawn(move || {
            let mut bytes = 0u64.to_le_bytes().to_vec();
            bytes.extend_from_slice(&640u16.to_le_bytes());
            for _ in 0..640 {
                bytes.extend_from_slice(&16384i16.to_le_bytes());
            }
            sink.packet(&bytes).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let enqueued = loop {
            if audio.source.try_lock().is_ok_and(|source| {
                source
                    .queue
                    .as_ref()
                    .is_some_and(|queue| queue.stats().buffered_ms > 0.0)
            }) {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            thread::sleep(Duration::from_millis(1));
        };
        let mut output = [0.0; 128];
        audio.render(128, 1, 8000, |i, value| output[i] = value);
        drop(held);
        packet.join().unwrap();
        assert!(enqueued, "Recording tap held the playback lock");
        assert!(output.iter().any(|value| *value > 0.0));
        assert_eq!(audio.status().contention_callbacks, 0);
        let epoch = audio.source.lock().unwrap().epoch;
        audio
            .configure(AudioFormat {
                sample_rate: 8000,
                channels: 1,
                codec: 0,
            })
            .unwrap();
        assert_ne!(audio.source.lock().unwrap().epoch, epoch);
    }

    #[test]
    fn ima_tables_match_standard_fixture() {
        let tables: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/ima-adpcm-tables.json")).unwrap();
        assert_eq!(serde_json::json!(STEP.to_vec()), tables["step"]);
        assert_eq!(serde_json::json!(INDEX), tables["index"]);
    }
}
