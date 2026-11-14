//! Best-effort source-audio analysis. Never runs on the output callback.
use crate::protocol::AudioFormat;
use rustfft::{FftPlanner, num_complex::Complex};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, AtomicUsize, Ordering},
    mpsc::{self, Receiver, SyncSender, TrySendError},
};

pub const BANDS: usize = 64;
const FFT: usize = 2048;
const HOP: usize = FFT / 2;
// Bound by both source duration and packet count. Four packets were only a few
// milliseconds for small packets and could overflow on a single TCP burst.
const QUEUE_BLOCKS: usize = 256;
const QUEUE_MS: usize = 500;
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Statistics {
    pub dropped_blocks: u64,
    pub dropped_frames: u64,
    /// Missing source positions seen by the worker (includes local queue loss).
    pub gap_frames: u64,
    pub gaps: u64,
    pub fft_windows: u64,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub spectrum: [f32; BANDS], // dBFS, combined channel power
    pub peak: [f32; 2],
    pub rms: [f32; 2],
    /// Bounded source-clock density history, shared without copying samples.
    pub vectorscope: Arc<crate::vectorscope::Snapshot>,
    pub vectorscope_age: std::time::Duration,
    /// One second of unwindowed, peak-preserving source amplitude history.
    pub waveform: Arc<crate::waveform::Snapshot>,
    pub sample_rate: u32,
    pub channels: u8,
    pub history: Arc<crate::spectrogram::History>,
    pub statistics: Statistics,
}
impl Default for Snapshot {
    fn default() -> Self {
        Self {
            spectrum: [-90.0; BANDS],
            peak: [-90.0; 2],
            rms: [-90.0; 2],
            vectorscope: Arc::default(),
            vectorscope_age: std::time::Duration::ZERO,
            waveform: Arc::default(),
            sample_rate: 0,
            channels: 0,
            history: Arc::default(),
            statistics: Statistics::default(),
        }
    }
}
struct Block {
    format: AudioFormat,
    epoch: u64,
    first: u64,
    samples: Vec<f32>,
}
#[derive(Default)]
struct Traffic {
    queued_frames: AtomicUsize,
    dropped_blocks: AtomicU64,
    dropped_frames: AtomicU64,
}
impl Traffic {
    fn reserve(&self, frames: usize, limit: usize) -> bool {
        let mut queued = self.queued_frames.load(Ordering::Relaxed);
        loop {
            let Some(total) = queued.checked_add(frames).filter(|&total| total <= limit) else {
                return false;
            };
            match self.queued_frames.compare_exchange_weak(
                queued,
                total,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(observed) => queued = observed,
            }
        }
    }
}
#[derive(Clone)]
pub struct Input {
    sender: SyncSender<Block>,
    traffic: Arc<Traffic>,
}
impl Input {
    fn channel(capacity: usize) -> (Self, Receiver<Block>) {
        let (sender, receiver) = mpsc::sync_channel(capacity);
        (
            Self {
                sender,
                traffic: Arc::default(),
            },
            receiver,
        )
    }
    fn dropped(&self, frames: usize) {
        self.traffic.dropped_blocks.fetch_add(1, Ordering::Relaxed);
        self.traffic
            .dropped_frames
            .fetch_add(frames as u64, Ordering::Relaxed);
    }
    pub fn push(&self, format: AudioFormat, epoch: u64, first: u64, samples: &[f32]) {
        // Match protocol bounds even for callers other than the decoder.
        if format.validate().is_err()
            || samples.is_empty()
            || !samples.len().is_multiple_of(usize::from(format.channels))
            || samples.len() / usize::from(format.channels) > usize::from(u16::MAX)
            || first
                .checked_add((samples.len() / usize::from(format.channels)) as u64)
                .is_none()
        {
            return;
        }
        let frames = samples.len() / usize::from(format.channels);
        // Reserve before copying. Allow one protocol-sized packet even when it
        // exceeds the duration budget, but never let bursts grow without bound.
        let limit = (format.sample_rate as usize * QUEUE_MS / 1000).max(frames);
        if !self.traffic.reserve(frames, limit) {
            self.dropped(frames);
            return;
        }
        // Never wait for analysis; the audio/network producer remains best effort.
        if let Err(error) = self.sender.try_send(Block {
            format,
            epoch,
            first,
            samples: samples.to_vec(),
        }) {
            self.traffic
                .queued_frames
                .fetch_sub(frames, Ordering::Relaxed);
            if matches!(error, TrySendError::Full(_)) {
                self.dropped(frames);
            }
        }
    }
}
pub struct Analyzer {
    pub input: Input,
    latest: Arc<Mutex<(Snapshot, std::time::Instant)>>,
}
impl Analyzer {
    pub fn new() -> std::io::Result<Self> {
        let (input, rx) = Input::channel(QUEUE_BLOCKS);
        let traffic = input.traffic.clone();
        let latest = Arc::new(Mutex::new((Snapshot::default(), std::time::Instant::now())));
        let output = latest.clone();
        std::thread::Builder::new()
            .name("audio-analyzer".into())
            .spawn(move || {
                let mut processor = Processor::new();
                for block in rx {
                    traffic.queued_frames.fetch_sub(
                        block.samples.len() / usize::from(block.format.channels),
                        Ordering::Relaxed,
                    );
                    processor.process(block, |snapshot| {
                        *output.lock().unwrap() = (snapshot, std::time::Instant::now());
                    });
                }
            })?;
        Ok(Self { input, latest })
    }
    pub fn snapshot(&self) -> Snapshot {
        let latest = self.latest.lock().unwrap();
        let mut snapshot = latest.0.clone();
        snapshot.statistics.dropped_blocks =
            self.input.traffic.dropped_blocks.load(Ordering::Relaxed);
        snapshot.statistics.dropped_frames =
            self.input.traffic.dropped_frames.load(Ordering::Relaxed);
        let age = latest.1.elapsed();
        if !snapshot.vectorscope.is_empty() {
            if age.as_secs_f32() >= crate::vectorscope::SECONDS {
                snapshot.vectorscope = Arc::default();
            } else {
                snapshot.vectorscope_age = age;
            }
        }
        if age > std::time::Duration::from_millis(250) {
            // Expire live levels, not recorded history. Bursty delivery or a
            // temporary pause must not erase the spectrogram.
            snapshot.spectrum = [-90.0; BANDS];
            snapshot.peak = [-90.0; 2];
            snapshot.rms = [-90.0; 2];
        }
        snapshot
    }
}
/// Stream identity and FFT continuity are separate: a missing analysis packet
/// invalidates the partial FFT, not the preceding ten seconds of history.
struct Processor {
    analysis: Analysis,
    vectorscope: crate::vectorscope::History,
    waveform: crate::waveform::History,
    clock: Option<(u64, AudioFormat, u64)>,
    origin: u64,
    history: crate::spectrogram::History,
    published: Arc<crate::spectrogram::History>,
    statistics: Statistics,
}
impl Processor {
    fn new() -> Self {
        Self {
            analysis: Analysis::new(),
            vectorscope: crate::vectorscope::History::new(48000),
            waveform: crate::waveform::History::new(48000),
            clock: None,
            origin: 0,
            history: Default::default(),
            published: Arc::default(),
            statistics: Statistics::default(),
        }
    }
    fn process(&mut self, block: Block, mut publish: impl FnMut(Snapshot)) {
        let channels = usize::from(block.format.channels);
        let end = block.first + (block.samples.len() / channels) as u64;
        let new_stream = !self
            .clock
            .is_some_and(|(epoch, format, _)| epoch == block.epoch && format == block.format);
        if new_stream {
            self.analysis.clear();
            self.vectorscope = crate::vectorscope::History::new(block.format.sample_rate);
            self.waveform = crate::waveform::History::new(block.format.sample_rate);
            self.origin = block.first;
            self.history = Default::default();
            self.published = Arc::default();
            self.clock = Some((block.epoch, block.format, block.first));
            publish(Snapshot {
                statistics: self.statistics.clone(),
                ..Snapshot::default()
            });
        }
        let expected = self.clock.unwrap().2;
        // Reordered/duplicate data must not rewind the history. For overlaps,
        // consume only the new suffix, preserving the current FFT window.
        if end <= expected {
            return;
        }
        let skip = expected.saturating_sub(block.first) as usize;
        if block.first > expected {
            self.statistics.gap_frames += block.first - expected;
            self.statistics.gaps += 1;
            self.analysis.clear();
            self.vectorscope = crate::vectorscope::History::new(block.format.sample_rate);
            self.waveform.gap(block.first - self.origin);
            self.history
                .gap((block.first - self.origin) as f64 / block.format.sample_rate as f64);
            self.published = Arc::new(self.history.clone());
            publish(Snapshot {
                sample_rate: block.format.sample_rate,
                channels: block.format.channels,
                history: self.published.clone(),
                waveform: self.waveform.snapshot(),
                statistics: self.statistics.clone(),
                ..Snapshot::default()
            });
        }
        self.clock = Some((block.epoch, block.format, end));
        for (index, frame) in block.samples.chunks_exact(channels).enumerate().skip(skip) {
            let right = if channels > 1 { frame[1] } else { frame[0] };
            self.vectorscope.push(frame[0], right);
            self.waveform
                .push(block.first + index as u64 - self.origin, frame[0], right);
            self.analysis.samples[0].push(frame[0]);
            if channels > 1 {
                self.analysis.samples[1].push(frame[1]);
            }
            if self.analysis.samples[0].len() == FFT {
                let mut snapshot = self
                    .analysis
                    .analyze(block.format.sample_rate, block.format.channels);
                let at = (block.first + index as u64 + 1 - self.origin) as f64
                    / block.format.sample_rate as f64;
                if self.history.push(at, snapshot.spectrum) {
                    self.published = Arc::new(self.history.clone());
                }
                snapshot.history = self.published.clone();
                snapshot.vectorscope = self.vectorscope.snapshot();
                snapshot.waveform = self.waveform.snapshot();
                self.statistics.fft_windows += 1;
                snapshot.statistics = self.statistics.clone();
                publish(snapshot);
                for channel in self.analysis.samples.iter_mut().take(channels.min(2)) {
                    channel.drain(..HOP);
                }
            }
        }
    }
}
fn db(amplitude: f32) -> f32 {
    (20.0 * amplitude.max(1e-9).log10()).max(-90.0)
}
struct Analysis {
    samples: [Vec<f32>; 2],
    fft: Arc<dyn rustfft::Fft<f32>>,
    buffer: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    window: Vec<f32>,
    power: [f32; FFT / 2 + 1],
    bands: [(usize, usize); BANDS],
    rate: u32,
    empty_history: Arc<crate::spectrogram::History>,
}
impl Analysis {
    fn new() -> Self {
        let fft = FftPlanner::new().plan_fft_forward(FFT);
        let scratch = vec![Complex::default(); fft.get_inplace_scratch_len()];
        Self {
            samples: [Vec::with_capacity(FFT), Vec::with_capacity(FFT)],
            fft,
            scratch,
            buffer: vec![Complex::default(); FFT],
            power: [0.0; FFT / 2 + 1],
            bands: [(1, 2); BANDS],
            rate: 0,
            empty_history: Arc::default(),
            window: (0..FFT)
                .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / FFT as f32).cos())
                .collect(),
        }
    }
    fn clear(&mut self) {
        for channel in &mut self.samples {
            channel.clear();
        }
    }
    fn analyze(&mut self, rate: u32, channels: u8) -> Snapshot {
        if self.rate != rate {
            let upper = (rate as f32 / 2.0).min(20000.0);
            for (band, bins) in self.bands.iter_mut().enumerate() {
                let low = 20.0 * (upper / 20.0).powf(band as f32 / BANDS as f32);
                let high = 20.0 * (upper / 20.0).powf((band + 1) as f32 / BANDS as f32);
                let first = ((low * FFT as f32 / rate as f32).round() as usize).clamp(1, FFT / 2);
                let end = ((high * FFT as f32 / rate as f32).ceil() as usize)
                    .clamp(first + 1, FFT / 2 + 1);
                *bins = (first, end);
            }
            self.rate = rate;
        }
        let mut result = Snapshot {
            spectrum: [-90.0; BANDS],
            peak: [-90.0; 2],
            rms: [-90.0; 2],
            vectorscope: Arc::default(),
            vectorscope_age: std::time::Duration::ZERO,
            waveform: Arc::default(),
            sample_rate: rate,
            channels,
            history: self.empty_history.clone(),
            statistics: Statistics::default(),
        };
        self.power.fill(0.0);
        let analyzed_channels = usize::from(channels.min(2));
        for c in 0..analyzed_channels {
            let mut peak = 0.0f32;
            let mut squares = 0.0;
            for ((&sample, &window), buffer) in self.samples[c]
                .iter()
                .zip(&self.window)
                .zip(&mut self.buffer)
            {
                peak = peak.max(sample.abs());
                squares += sample * sample;
                *buffer = Complex::new(sample * window, 0.0);
            }
            result.peak[c] = db(peak);
            result.rms[c] = db((squares / FFT as f32).sqrt());
            self.fft
                .process_with_scratch(&mut self.buffer, &mut self.scratch);
            for (power, bin) in self.power.iter_mut().zip(&self.buffer) {
                *power += bin.norm_sqr();
            }
        }
        if channels == 1 {
            result.peak[1] = result.peak[0];
            result.rms[1] = result.rms[0];
        }
        let gain = 1.0 / ((FFT as f32 / 4.0) * (analyzed_channels as f32).sqrt());
        for (dbfs, &(first, end)) in result.spectrum.iter_mut().zip(&self.bands) {
            *dbfs = db(self.power[first..end]
                .iter()
                .copied()
                .fold(0.0f32, f32::max)
                .sqrt()
                * gain);
        }
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tone_levels_and_antiphase_stereo() {
        let mut analysis = Analysis::new();
        for i in 0..FFT {
            let value = 0.5 * (std::f32::consts::TAU * 32.0 * i as f32 / FFT as f32).sin();
            analysis.samples[0].push(value);
            analysis.samples[1].push(-value);
        }
        let result = analysis.analyze(48000, 2);
        assert!((result.peak[0] + 6.02).abs() < 0.05);
        assert!((result.rms[1] + 9.03).abs() < 0.05);
        assert!((result.spectrum.iter().copied().fold(-90.0f32, f32::max) + 6.02).abs() < 0.1);
        analysis.clear();
        assert!(analysis.samples[0].is_empty());
    }
    #[test]
    fn vectorscope_counts_fresh_source_frames_not_overlapping_fft_windows() {
        let mut processor = Processor::new();
        let mut latest = Snapshot::default();
        processor.process(block(0, FFT), |s| latest = s);
        assert_eq!(latest.vectorscope.len(), FFT);
        assert_eq!(latest.vectorscope.correlation, Some(1.0));
        let original = latest.vectorscope.clone();
        processor.process(block(0, FFT), |_| panic!("duplicate must not publish"));
        processor.process(block((FFT - 16) as u64, HOP + 16), |s| latest = s);
        assert_eq!(latest.vectorscope.len(), FFT + HOP);
        assert_eq!(original.len(), FFT); // Immutable after further worker updates.
        assert!(Arc::ptr_eq(
            &latest.vectorscope,
            &latest.clone().vectorscope
        ));
        processor.process(block((FFT + HOP) as u64, FFT * 6), |s| latest = s);
        assert_eq!(latest.vectorscope.len(), 9600); // 200 ms, not one FFT.
    }
    #[test]
    fn waveform_counts_fresh_frames_and_does_not_mutate_published_history() {
        let mut processor = Processor::new();
        let mut latest = Snapshot::default();
        processor.process(block(500000, FFT), |s| latest = s);
        assert_eq!(latest.waveform.end(), FFT as f64 / 48000.0);
        let original = latest.waveform.clone();
        processor.process(block(500000, FFT), |_| panic!("duplicate must not publish"));
        processor.process(block(500000 + (FFT - 16) as u64, HOP + 16), |s| latest = s);
        assert_eq!(latest.waveform.end(), (FFT + HOP) as f64 / 48000.0);
        assert_eq!(original.end(), FFT as f64 / 48000.0);
        let column = latest.waveform.columns(1)[0].unwrap();
        assert_eq!(column.min, [0.5; 2]);
        assert_eq!(column.max, [0.5; 2]);
        assert!(Arc::ptr_eq(&latest.waveform, &latest.clone().waveform));
    }
    #[test]
    fn waveform_does_not_depend_on_packet_boundaries() {
        let frames = FFT + HOP * 4;
        let samples: Vec<f32> = (0..frames)
            .flat_map(|i| {
                let value = if i == 113 {
                    0.9
                } else {
                    (i % 17) as f32 * 0.01
                };
                [value, -value]
            })
            .collect();
        let format = AudioFormat {
            sample_rate: 44100,
            channels: 2,
            codec: 0,
        };
        let mut whole = Snapshot::default();
        Processor::new().process(
            Block {
                format,
                epoch: 1,
                first: 900000,
                samples: samples.clone(),
            },
            |s| whole = s,
        );
        let mut processor = Processor::new();
        let mut split = Snapshot::default();
        let mut first = 0;
        for size in [1, 17, 997, 30, 2049].into_iter().cycle() {
            let end = (first + size).min(frames);
            processor.process(
                Block {
                    format,
                    epoch: 1,
                    first: 900000 + first as u64,
                    samples: samples[first * 2..end * 2].to_vec(),
                },
                |s| split = s,
            );
            first = end;
            if first == frames {
                break;
            }
        }
        assert_eq!(whole.waveform, split.waveform);
        let column = split.waveform.columns(1)[0].unwrap();
        assert_eq!(column.min, [0.0, -0.9]);
        assert_eq!(column.max, [0.9, 0.0]);
    }
    #[test]
    fn source_views_use_the_same_first_lr_pair_for_multichannel_sources() {
        let mut stereo = block(0, FFT);
        stereo.format.channels = 2;
        stereo.samples = [0.25, -0.5].repeat(FFT);
        let mut multi = block(0, FFT);
        multi.format.channels = 8;
        multi.samples = [0.25, -0.5, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0].repeat(FFT);
        let mut a = Snapshot::default();
        let mut b = Snapshot::default();
        Processor::new().process(stereo, |s| a = s);
        Processor::new().process(multi, |s| b = s);
        assert_eq!(a.vectorscope, b.vectorscope);
        assert_eq!(a.vectorscope.correlation, Some(-1.0));
        assert_eq!(a.waveform, b.waveform);
        assert_eq!(a.waveform.columns(1)[0].unwrap().min, [0.25, -0.5]);
    }
    #[test]
    fn vectorscope_auto_gain_does_not_change_spectrum_or_raw_meters() {
        let mut fft_only = Analysis::new();
        fft_only.samples[0].resize(FFT, 0.01);
        let expected = fft_only.analyze(48000, 1);
        let mut quiet = block(0, 48000);
        quiet.samples.fill(0.01);
        let mut latest = Snapshot::default();
        Processor::new().process(quiet, |s| latest = s);
        assert!(latest.vectorscope.gain > 4.0);
        assert_eq!(latest.vectorscope.correlation, Some(1.0));
        assert_eq!(latest.spectrum, expected.spectrum);
        assert_eq!(latest.peak, expected.peak);
        assert_eq!(latest.rms, expected.rms);
    }
    #[test]
    fn source_tap_ignores_playback_volume() {
        let analyzer = Analyzer::new().unwrap();
        let audio = crate::audio::Audio::default();
        audio.set_enabled(true);
        audio.set_volume(0.0);
        audio
            .configure(AudioFormat {
                sample_rate: 48000,
                channels: 1,
                codec: 0,
            })
            .unwrap();
        audio.set_analyzer(Some(analyzer.input.clone()));
        let mut packet = 0u64.to_le_bytes().to_vec();
        packet.extend((FFT as u16).to_le_bytes());
        for _ in 0..FFT {
            packet.extend(16384i16.to_le_bytes());
        }
        audio.packet(&packet).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let snapshot = analyzer.snapshot();
            if snapshot.sample_rate != 0 {
                assert!((snapshot.peak[0] + 6.02).abs() < 0.05);
                assert_eq!(snapshot.channels, 1);
                assert_eq!(snapshot.vectorscope.len(), FFT);
                assert_eq!(snapshot.vectorscope.correlation, Some(1.0));
                assert_eq!(snapshot.vectorscope.gain, 1.0);
                assert_eq!(snapshot.waveform.columns(1)[0].unwrap().max, [0.5; 2]);
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        // A new epoch must not splice old samples into the next FFT.
        analyzer.input.push(
            AudioFormat {
                sample_rate: 48000,
                channels: 1,
                codec: 0,
            },
            0,
            100000,
            &[0.0; 16],
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while analyzer.snapshot().sample_rate != 0 {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    #[test]
    fn worker_retains_fft_history_between_ui_polls() {
        let analyzer = Analyzer::new().unwrap();
        let format = AudioFormat {
            sample_rate: 48000,
            channels: 1,
            codec: 0,
        };
        let frames = FFT + HOP * 10;
        analyzer.input.push(format, 7, 500000, &vec![0.5; frames]);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let snapshot = analyzer.snapshot();
            if snapshot.history.end() >= (frames - HOP) as f64 / 48000.0 {
                assert!(snapshot.history.columns.len() > 4);
                assert!((snapshot.history.columns[0].at - FFT as f64 / 48000.0).abs() < 1e-9);
                assert!(
                    snapshot
                        .history
                        .columns
                        .iter()
                        .all(|c| c.spectrum.iter().all(|db| db.is_finite()))
                );
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        // Same format, new epoch: history must not survive a reconnect/reset.
        analyzer.input.push(format, 8, 0, &[0.0; 16]);
        while analyzer.snapshot().sample_rate != 0 {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(analyzer.snapshot().history.columns.is_empty());
    }
    #[test]
    fn stalled_source_does_not_leave_frozen_levels() {
        let analyzer = Analyzer::new().unwrap();
        let mut history = crate::spectrogram::History::default();
        history.push(1.0, [-12.0; BANDS]);
        let history = Arc::new(history);
        let mut waveform = crate::waveform::History::new(48000);
        for at in 0..FFT as u64 {
            waveform.push(at, 0.5, 0.5);
        }
        let waveform = waveform.snapshot();
        *analyzer.latest.lock().unwrap() = (
            Snapshot {
                sample_rate: 48000,
                peak: [0.0; 2],
                vectorscope: {
                    let mut scope = crate::vectorscope::History::new(48000);
                    for _ in 0..FFT {
                        scope.push(0.5, 0.5);
                    }
                    scope.snapshot()
                },
                history: history.clone(),
                waveform: waveform.clone(),
                ..Snapshot::default()
            },
            std::time::Instant::now() - std::time::Duration::from_secs(1),
        );
        let snapshot = analyzer.snapshot();
        assert_eq!(snapshot.peak, [-90.0; 2]);
        assert_eq!(snapshot.spectrum, [-90.0; BANDS]);
        assert!(snapshot.vectorscope.is_empty());
        assert_eq!(snapshot.vectorscope.correlation, None);
        assert!(Arc::ptr_eq(&snapshot.history, &history));
        assert!(Arc::ptr_eq(&snapshot.waveform, &waveform));
    }
    #[test]
    fn scope_snapshot_ages_during_a_pause_and_expires_after_persistence() {
        let analyzer = Analyzer::new().unwrap();
        let mut latest = Snapshot::default();
        Processor::new().process(block(0, FFT), |s| latest = s);
        let scope = latest.vectorscope.clone();
        *analyzer.latest.lock().unwrap() = (
            latest,
            std::time::Instant::now() - std::time::Duration::from_millis(50),
        );
        let fading = analyzer.snapshot();
        assert!(fading.vectorscope_age >= std::time::Duration::from_millis(50));
        assert!(Arc::ptr_eq(&fading.vectorscope, &scope));
        analyzer.latest.lock().unwrap().1 =
            std::time::Instant::now() - std::time::Duration::from_millis(210);
        let expired = analyzer.snapshot();
        assert!(expired.vectorscope.is_empty());
        assert_eq!(expired.vectorscope.correlation, None);
        assert!(!expired.history.columns.is_empty());
    }
    fn block(first: u64, frames: usize) -> Block {
        Block {
            format: AudioFormat {
                sample_rate: 48000,
                channels: 1,
                codec: 0,
            },
            epoch: 1,
            first,
            samples: vec![0.5; frames],
        }
    }
    #[test]
    fn dropped_packets_preserve_history_but_reset_partial_fft() {
        let mut processor = Processor::new();
        let mut latest = Snapshot::default();
        processor.process(block(0, 16384), |s| latest = s);
        let first = latest.history.columns.front().unwrap().clone();
        let before = latest.history.columns.len();
        assert_eq!(latest.vectorscope.len(), 9600);
        processor.process(block(20480, 16), |s| latest = s);
        assert_eq!(processor.analysis.samples[0].len(), 16);
        assert!(latest.vectorscope.is_empty());
        assert_eq!(latest.history.columns.front(), Some(&first));
        assert!(latest.history.columns.len() > before);
        assert!(latest.history.columns.back().unwrap().gap);
        assert!(!latest.waveform.is_empty());
        assert_eq!(latest.waveform.end(), 20480.0 / 48000.0);
        assert!(
            latest.waveform.columns(480)[440..]
                .iter()
                .all(Option::is_none)
        );
        processor.process(block(20496, FFT), |s| latest = s);
        assert_eq!(latest.vectorscope.len(), FFT);
        assert_eq!(latest.history.columns.front(), Some(&first));
        assert!(!latest.history.columns.back().unwrap().gap);
        assert!(latest.history.end() > 0.4);
        assert_eq!(processor.origin, 0);
        assert_eq!(latest.statistics.gaps, 1);
        assert_eq!(latest.statistics.gap_frames, 4096);
        let waveform = latest.waveform.columns(480);
        assert!(waveform[420..458].iter().all(Option::is_none));
        assert!(waveform[460..].iter().all(Option::is_some));
    }
    #[test]
    fn duplicates_and_overlaps_do_not_rewind_or_clear_history() {
        let mut processor = Processor::new();
        processor.process(block(0, FFT), |_| {});
        let original = processor.published.clone();
        processor.process(block(0, FFT), |_| panic!("duplicate must not publish"));
        assert!(Arc::ptr_eq(&processor.published, &original));
        assert_eq!(processor.analysis.samples[0].len(), HOP);
        processor.process(block((FFT - 16) as u64, HOP + 16), |_| {});
        assert_eq!(processor.analysis.samples[0].len(), HOP);
        assert_eq!(processor.clock.unwrap().2, (FFT + HOP) as u64);
        assert_eq!(processor.history.columns.front(), original.columns.front());
        assert!(!processor.history.columns.iter().any(|c| c.gap));
    }
    #[test]
    fn stream_or_format_changes_still_clear_history() {
        let mut processor = Processor::new();
        let mut latest = Snapshot::default();
        processor.process(block(0, FFT * 2), |s| latest = s);
        assert_eq!(latest.vectorscope.len(), FFT * 2);
        let mut changed = block(0, 16);
        changed.epoch = 2;
        processor.process(changed, |s| latest = s);
        assert!(processor.history.columns.is_empty());
        assert!(latest.vectorscope.is_empty());
        assert!(latest.waveform.is_empty());
        let mut changed = block(0, FFT * 2);
        changed.epoch = 2;
        processor.process(changed, |s| latest = s);
        assert!(!processor.history.columns.is_empty());
        assert_eq!(latest.vectorscope.len(), FFT * 2);
        let mut changed = block(0, 16);
        changed.epoch = 2;
        changed.format.sample_rate = 44100;
        processor.process(changed, |s| latest = s);
        assert!(processor.history.columns.is_empty());
        assert!(latest.vectorscope.is_empty());
        assert!(latest.waveform.is_empty());
    }
    #[test]
    fn full_analysis_queue_drops_without_waiting() {
        let (input, rx) = Input::channel(1);
        let format = AudioFormat {
            sample_rate: 48000,
            channels: 1,
            codec: 0,
        };
        input.push(format, 0, 0, &[0.0]);
        input.push(format, 0, 1, &[0.5]);
        assert_eq!(rx.try_recv().unwrap().first, 0);
        assert!(rx.try_recv().is_err());
        assert_eq!(input.traffic.dropped_blocks.load(Ordering::Relaxed), 1);
        assert_eq!(input.traffic.dropped_frames.load(Ordering::Relaxed), 1);
    }
    #[test]
    fn small_packet_bursts_fit_and_source_duration_still_bounds_memory() {
        let (input, rx) = Input::channel(QUEUE_BLOCKS);
        let format = AudioFormat {
            sample_rate: 48000,
            channels: 1,
            codec: 0,
        };
        for index in 0..200 {
            input.push(format, 1, index * 120, &[0.25; 120]);
        }
        assert_eq!(input.traffic.queued_frames.load(Ordering::Relaxed), 24000);
        assert_eq!(input.traffic.dropped_blocks.load(Ordering::Relaxed), 0);
        input.push(format, 1, 24000, &[0.25; 120]);
        assert_eq!(input.traffic.dropped_blocks.load(Ordering::Relaxed), 1);
        assert_eq!(input.traffic.dropped_frames.load(Ordering::Relaxed), 120);
        let blocks: Vec<_> = rx.try_iter().collect();
        assert_eq!(blocks.len(), 200);
        for (index, block) in blocks.iter().enumerate() {
            assert_eq!(block.first, index as u64 * 120);
        }
    }
    #[test]
    fn worker_drains_bursts_and_publishes_drop_statistics() {
        let analyzer = Analyzer::new().unwrap();
        let format = AudioFormat {
            sample_rate: 48000,
            channels: 1,
            codec: 0,
        };
        for index in 0..200 {
            analyzer.input.push(format, 1, index * 120, &[0.25; 120]);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while analyzer.snapshot().statistics.fft_windows < 22
            || analyzer.input.traffic.queued_frames.load(Ordering::Relaxed) != 0
        {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let snapshot = analyzer.snapshot();
        assert_eq!(snapshot.statistics.dropped_blocks, 0);
        assert_eq!(snapshot.statistics.gaps, 0);
        assert!(!snapshot.history.columns.is_empty());
        assert_eq!(
            analyzer.input.traffic.queued_frames.load(Ordering::Relaxed),
            0
        );
        // Queue accounting is also released when the consumer is gone.
        let (input, receiver) = Input::channel(QUEUE_BLOCKS);
        drop(receiver);
        input.push(format, 1, 0, &[0.25; 120]);
        assert_eq!(input.traffic.queued_frames.load(Ordering::Relaxed), 0);
    }
    #[test]
    fn mono_fast_path_matches_stereo_and_reuses_power_without_accumulation() {
        let mut analysis = Analysis::new();
        for c in &mut analysis.samples {
            c.extend(
                (0..FFT)
                    .map(|i| 0.5 * (std::f32::consts::TAU * 32.0 * i as f32 / FFT as f32).sin()),
            );
        }
        let mono = analysis.analyze(48000, 1);
        let stereo = analysis.analyze(48000, 2);
        for (a, b) in mono.spectrum.iter().zip(stereo.spectrum) {
            assert!((a - b).abs() < 1e-4);
        }
        assert_eq!(mono.peak, stereo.peak);
        assert_eq!(mono.rms, stereo.rms);
        for c in &mut analysis.samples {
            c.fill(0.0);
        }
        assert_eq!(analysis.analyze(48000, 1).spectrum, [-90.0; BANDS]);
        let old_bins = analysis.bands;
        analysis.analyze(44100, 2);
        assert_ne!(analysis.bands, old_bins);
    }
    #[test]
    #[ignore = "manual CPU benchmark; run with --ignored --nocapture"]
    fn benchmark_analysis() {
        for rate in [48000, 192000, 384000] {
            for channels in [1, 2] {
                let mut processor = Processor::new();
                let start = std::time::Instant::now();
                let frames = 10 * rate as usize;
                let mut windows = 0;
                for first in (0..frames).step_by(1024) {
                    let count = (frames - first).min(1024);
                    processor.process(
                        Block {
                            format: AudioFormat {
                                sample_rate: rate,
                                channels,
                                codec: 0,
                            },
                            epoch: 1,
                            first: first as u64,
                            samples: vec![0.25; count * usize::from(channels)],
                        },
                        |snapshot| {
                            std::hint::black_box(snapshot);
                            windows += 1;
                        },
                    );
                }
                eprintln!(
                    "{rate} Hz / {channels} ch: {:.1} ms for 10 s audio ({windows} snapshots)",
                    start.elapsed().as_secs_f64() * 1000.0
                );
                let start = std::time::Instant::now();
                for _ in 0..300 {
                    std::hint::black_box(processor.history.image());
                }
                eprintln!(
                    "300 history images: {:.1} ms",
                    start.elapsed().as_secs_f64() * 1000.0
                );
                let scope = processor.vectorscope.snapshot();
                let start = std::time::Instant::now();
                for _ in 0..300 {
                    std::hint::black_box(scope.image(std::time::Duration::ZERO));
                }
                eprintln!(
                    "300 density images: {:.1} ms",
                    start.elapsed().as_secs_f64() * 1000.0
                );
                let waveform = processor.waveform.snapshot();
                let start = std::time::Instant::now();
                for _ in 0..300 {
                    std::hint::black_box(waveform.columns(1200));
                }
                eprintln!(
                    "300 waveform envelopes (1200 px): {:.1} ms",
                    start.elapsed().as_secs_f64() * 1000.0
                );
            }
        }
    }
    #[test]
    fn silence_is_finite() {
        let mut analysis = Analysis::new();
        for c in &mut analysis.samples {
            c.resize(FFT, 0.0);
        }
        let result = analysis.analyze(44100, 1);
        assert_eq!(result.spectrum, [-90.0; BANDS]);
        assert_eq!(result.rms, [-90.0; 2]);
        assert!(result.vectorscope.is_empty());
    }
}
