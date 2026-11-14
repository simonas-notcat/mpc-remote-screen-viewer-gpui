//! Source-clock stereo density, shared display auto-gain, and phase correlation.
//! Receives each source frame once, independently of overlapping FFT windows.
use std::{collections::VecDeque, sync::Arc, time::Duration};

pub const SECONDS: f32 = 0.2;
pub const IMAGE_SIZE: usize = 129; // An exact center pixel for mono.
const GAIN_RELEASE_SECONDS: f64 = 0.5;
const TARGET_PEAK: f64 = 0.85;
const MAX_GAIN: f64 = 8.0; // About +18 dB, display only.
const NOISE_FLOOR: f64 = 0.0001; // -80 dBFS; don't magnify digital silence/noise.

#[derive(Clone, Debug, PartialEq)]
struct Chunk {
    end: u64,
    samples: Arc<[[f32; 2]]>, // Unwindowed L/R, before display gain.
    energy: [f64; 3],         // L², R², L·R.
    peak: f64,
}
fn levels(samples: &[[f32; 2]]) -> ([f64; 3], f64) {
    let mut energy = [0.0; 3];
    let mut peak = 0.0f64;
    for &[left, right] in samples {
        let (left, right) = (f64::from(left), f64::from(right));
        energy[0] += left * left;
        energy[1] += right * right;
        energy[2] += left * right;
        peak = peak.max(left.abs()).max(right.abs());
    }
    (energy, peak)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    chunks: VecDeque<Chunk>,
    rate: u32,
    first: u64,
    end: u64,
    pub gain: f32,
    /// Normalized L/R cross-power over the last 200 ms, before display gain.
    /// Undefined for silence or when either channel has negligible energy.
    pub correlation: Option<f32>,
}
impl Default for Snapshot {
    fn default() -> Self {
        Self {
            chunks: VecDeque::new(),
            rate: 0,
            first: 0,
            end: 0,
            gain: 1.0,
            correlation: None,
        }
    }
}
impl Snapshot {
    pub fn len(&self) -> usize {
        (self.end - self.first) as usize
    }
    pub fn is_empty(&self) -> bool {
        self.end == self.first
    }
    /// Transparent BGRA for GPUI. Accumulate sample density, not connecting
    /// lines. The same gain/scale is used for every point and both axes.
    /// Wall-clock age also fades the cloud when source delivery pauses.
    pub fn image(&self, age: Duration) -> image::RgbaImage {
        let mut density = vec![0.0f32; IMAGE_SIZE * IMAGE_SIZE];
        let age = age.as_secs_f32();
        if age < SECONDS && !self.is_empty() {
            let scale = (IMAGE_SIZE - 1) as f32 * 0.5;
            // Keep exposure consistent across sample rates, not dots/second.
            let exposure = 48000.0 / self.rate as f32;
            for chunk in &self.chunks {
                let first = chunk.end - chunk.samples.len() as u64;
                let skip = self.first.saturating_sub(first) as usize;
                for (i, &[left, right]) in chunk.samples.iter().enumerate().skip(skip) {
                    let elapsed = (self.end - 1 - first - i as u64) as f32 / self.rate as f32 + age;
                    if elapsed >= SECONDS || left.abs().max(right.abs()) < NOISE_FLOOR as f32 {
                        continue;
                    }
                    let side = (right - left) * 0.5 * self.gain;
                    let mid = (left + right) * 0.5 * self.gain;
                    let x = (scale + side * scale).clamp(0.0, (IMAGE_SIZE - 1) as f32);
                    let y = (scale - mid * scale).clamp(0.0, (IMAGE_SIZE - 1) as f32);
                    let (ix, iy) = (x.floor() as usize, y.floor() as usize);
                    let (fx, fy) = (x.fract(), y.fract());
                    let weight = (1.0 - elapsed / SECONDS).powi(2) * exposure;
                    // Bilinear splats avoid jumping between single pixels.
                    for (dx, wx) in [(0, 1.0 - fx), (1, fx)] {
                        for (dy, wy) in [(0, 1.0 - fy), (1, fy)] {
                            let (x, y) = (ix + dx, iy + dy);
                            if x < IMAGE_SIZE && y < IMAGE_SIZE {
                                density[y * IMAGE_SIZE + x] += weight * wx * wy;
                            }
                        }
                    }
                }
            }
        }
        // A small glow kernel and logarithmic-like exposure reveal sparse dots
        // without letting repeated positions overwhelm the rest of the cloud.
        image::RgbaImage::from_fn(IMAGE_SIZE as u32, IMAGE_SIZE as u32, |x, y| {
            let (x, y) = (x as usize, y as usize);
            let mut glow = 0.0;
            for dy in -1isize..=1 {
                for dx in -1isize..=1 {
                    let (nx, ny) = (x as isize + dx, y as isize + dy);
                    if (0..IMAGE_SIZE as isize).contains(&nx)
                        && (0..IMAGE_SIZE as isize).contains(&ny)
                    {
                        let weight =
                            if dx == 0 { 2.0 } else { 1.0 } * if dy == 0 { 2.0 } else { 1.0 };
                        glow += density[ny as usize * IMAGE_SIZE + nx as usize] * weight / 16.0;
                    }
                }
            }
            let alpha = ((1.0 - (-glow * 0.3).exp()).sqrt() * 255.0).round() as u8;
            let hot = 1.0 - (-glow * 0.03).exp();
            // BGRA with straight alpha, matching GPUI's RenderImage contract.
            image::Rgba([
                (100.0 + hot * 100.0) as u8,
                (190.0 + hot * 65.0) as u8,
                (65.0 + hot * 120.0) as u8,
                alpha,
            ])
        })
    }
}

pub(crate) struct History {
    rate: u32,
    chunks: VecDeque<Chunk>,
    pending: Vec<[f32; 2]>,
    end: u64,
    envelope: f64,
}
impl History {
    pub fn new(rate: u32) -> Self {
        Self {
            rate,
            chunks: VecDeque::new(),
            pending: Vec::with_capacity(2048),
            end: 0,
            envelope: 1.0,
        }
    }
    pub fn push(&mut self, left: f32, right: f32) {
        // Decoder input is normalized i16. Keep direct callers from introducing
        // NaNs or unbounded coordinates into the density/correlation state.
        let sample = if left.is_finite() && right.is_finite() {
            [left.clamp(-1.0, 1.0), right.clamp(-1.0, 1.0)]
        } else {
            [0.0; 2]
        };
        self.pending.push(sample);
        self.end += 1;
    }
    /// Publish with the FFT, but retain only fresh source frames. Chunk Arcs
    /// keep UI polls cheap without copying the full 200 ms of sample history.
    pub fn snapshot(&mut self) -> Arc<Snapshot> {
        if !self.pending.is_empty() {
            let samples = std::mem::replace(&mut self.pending, Vec::with_capacity(2048));
            let (energy, peak) = levels(&samples);
            let dt = samples.len() as f64 / f64::from(self.rate);
            self.envelope = peak.max(self.envelope * (-dt / GAIN_RELEASE_SECONDS).exp());
            self.chunks.push_back(Chunk {
                end: self.end,
                samples: samples.into(),
                energy,
                peak,
            });
        }
        let frames = (f64::from(self.rate) * f64::from(SECONDS)).round() as u64;
        let first = self.end.saturating_sub(frames);
        while self.chunks.front().is_some_and(|chunk| chunk.end <= first) {
            self.chunks.pop_front();
        }
        let mut energy = [0.0; 3];
        let mut peak = self.envelope;
        for chunk in &self.chunks {
            let skip = first.saturating_sub(chunk.end - chunk.samples.len() as u64) as usize;
            let (sums, maximum) = if skip == 0 {
                (chunk.energy, chunk.peak)
            } else {
                levels(&chunk.samples[skip..])
            };
            for (total, value) in energy.iter_mut().zip(sums) {
                *total += value;
            }
            peak = peak.max(maximum);
        }
        let floor = NOISE_FLOOR * NOISE_FLOOR * (self.end - first) as f64;
        let correlation = (energy[0] > floor && energy[1] > floor)
            .then(|| (energy[2] / (energy[0] * energy[1]).sqrt()).clamp(-1.0, 1.0) as f32);
        Arc::new(Snapshot {
            chunks: self.chunks.clone(),
            rate: self.rate,
            first,
            end: self.end,
            gain: (TARGET_PEAK / peak).clamp(1.0, MAX_GAIN) as f32,
            correlation,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal(rate: u32, seconds: f32, sample: impl Fn(f32) -> [f32; 2]) -> Arc<Snapshot> {
        let mut history = History::new(rate);
        for i in 0..(rate as f32 * seconds).round() as usize {
            let [left, right] = sample(std::f32::consts::TAU * 250.0 * i as f32 / rate as f32);
            history.push(left, right);
            if (i + 1).is_multiple_of(1024) {
                history.snapshot();
            }
        }
        history.snapshot()
    }
    fn brightness(image: &image::RgbaImage) -> u64 {
        image.pixels().map(|p| u64::from(p[3])).sum()
    }

    #[test]
    fn correlation_distinguishes_mono_antiphase_uncorrelated_and_missing_channels() {
        for rate in [8000, 44100, 48000, 192000, 384000] {
            let mono = signal(rate, 0.4, |phase| [phase.sin() * 0.4; 2]);
            assert_eq!(mono.correlation, Some(1.0));
            let inverted = signal(rate, 0.4, |phase| [phase.sin() * 0.4, -phase.sin() * 0.4]);
            assert_eq!(inverted.correlation, Some(-1.0));
            let wide = signal(rate, 0.4, |phase| [phase.sin() * 0.4, phase.cos() * 0.4]);
            assert!(wide.correlation.unwrap().abs() < 0.001);
            assert_eq!(signal(rate, 0.4, |_| [0.0; 2]).correlation, None);
            assert_eq!(
                signal(rate, 0.4, |p| [p.sin() * 0.4, 0.0]).correlation,
                None
            );
        }
    }
    #[test]
    fn correlation_excludes_expired_samples_even_inside_a_shared_chunk() {
        let mut history = History::new(48000);
        for i in 0..12000 {
            history.push(0.5, 0.5);
            if (i + 1usize).is_multiple_of(1024) {
                history.snapshot();
            }
        }
        history.snapshot();
        for _ in 0..4600 {
            history.push(0.5, -0.5);
        }
        let snapshot = history.snapshot();
        assert_eq!(snapshot.len(), 9600);
        assert!((snapshot.correlation.unwrap() - (5000.0 - 4600.0) / 9600.0).abs() < 1e-6);
        for _ in 0..5000 {
            history.push(0.5, -0.5);
        }
        assert_eq!(history.snapshot().correlation, Some(-1.0));
    }
    #[test]
    fn density_has_mono_and_antiphase_orientation_without_connecting_lines() {
        let mono = signal(48000, 0.2, |p| [p.sin() * 0.5; 2]).image(Duration::ZERO);
        let antiphase =
            signal(48000, 0.2, |p| [p.sin() * 0.5, -p.sin() * 0.5]).image(Duration::ZERO);
        let center = IMAGE_SIZE as u32 / 2;
        assert!(mono.get_pixel(center, center - 20)[3] > 0);
        assert_eq!(mono.get_pixel(center + 20, center)[3], 0);
        assert!(antiphase.get_pixel(center + 20, center)[3] > 0);
        assert_eq!(antiphase.get_pixel(center, center - 20)[3], 0);
        let wide = signal(48000, 0.2, |p| [p.sin() * 0.5, p.cos() * 0.5]).image(Duration::ZERO);
        for (x, y) in [(center - 20, center - 20), (center + 20, center + 20)] {
            assert!(wide.get_pixel(x, y)[3] > 0);
        }
        for (left, right, x) in [(0.5, 0.0, center - 16), (0.0, 0.5, center + 16)] {
            let image = signal(48000, 0.02, |_| [left, right]).image(Duration::ZERO);
            assert!(image.get_pixel(x, center - 16)[3] > 0);
            assert_eq!(image.get_pixel(center, center)[3], 0); // No line to/from center.
        }
    }
    #[test]
    fn density_exposure_is_sample_rate_independent() {
        let a = signal(48000, 0.2, |p| [p.sin() * 0.4; 2]).image(Duration::ZERO);
        let b = signal(192000, 0.2, |p| [p.sin() * 0.4; 2]).image(Duration::ZERO);
        let ratio = brightness(&a) as f64 / brightness(&b) as f64;
        assert!((0.98..1.02).contains(&ratio), "exposure ratio {ratio}");
    }
    #[test]
    fn persistence_uses_source_time_and_also_fades_while_delivery_stalls() {
        let snapshot = signal(48000, 0.02, |p| [p.sin() * 0.4; 2]);
        let fresh = brightness(&snapshot.image(Duration::ZERO));
        let fading = brightness(&snapshot.image(Duration::from_millis(150)));
        assert!(fading > 0 && fading < fresh);
        assert_eq!(brightness(&snapshot.image(Duration::from_millis(200))), 0);
        // A complete window of actual silence also removes old dots/correlation.
        let mut history = History::new(48000);
        for _ in 0..9600 {
            history.push(0.5, 0.5);
        }
        let old = history.snapshot();
        for _ in 0..9600 {
            history.push(0.0, 0.0);
        }
        let silent = history.snapshot();
        assert_eq!(silent.correlation, None);
        assert_eq!(brightness(&silent.image(Duration::ZERO)), 0);
        assert!(brightness(&old.image(Duration::ZERO)) > 0); // Immutable snapshot.
    }
    #[test]
    fn history_is_bounded_to_two_hundred_ms_with_shared_immutable_chunks() {
        for rate in [8000, 44100, 48000, 192000, 384000] {
            let snapshot = signal(rate, 1.0, |p| [p.sin() * 0.25; 2]);
            let frames = (rate as f64 * f64::from(SECONDS)).round() as usize;
            assert_eq!(snapshot.len(), frames);
            let retained: usize = snapshot.chunks.iter().map(|c| c.samples.len()).sum();
            assert!(retained < frames + 1024);
            let clone = snapshot.as_ref().clone();
            assert!(Arc::ptr_eq(
                &snapshot.chunks[0].samples,
                &clone.chunks[0].samples
            ));
        }
    }
    #[test]
    fn auto_gain_is_shared_slow_to_rise_bounded_and_preserves_balance() {
        let mut history = History::new(48000);
        for _ in 0..1024 {
            history.push(0.01, 0.005);
        }
        let initial = history.snapshot().gain;
        assert_eq!(initial, 1.0);
        let mut snapshot = history.snapshot();
        for _ in 0..100 {
            for _ in 0..1024 {
                history.push(0.01, 0.005);
            }
            let next = history.snapshot();
            assert!(next.gain >= snapshot.gain && next.gain <= snapshot.gain * 1.05);
            snapshot = next;
        }
        assert_eq!(snapshot.gain, MAX_GAIN as f32);
        assert_eq!(snapshot.correlation, Some(1.0));
        // Original channel levels are never changed or normalized independently.
        assert!(
            snapshot
                .chunks
                .iter()
                .all(|c| c.samples.iter().all(|&p| p == [0.01, 0.005]))
        );
        history.push(1.0, -1.0);
        let loud = history.snapshot();
        assert_eq!(loud.gain, 1.0); // Immediate protection against transient clipping.
        assert_eq!(snapshot.gain, MAX_GAIN as f32);
    }
    #[test]
    fn silence_and_invalid_samples_never_generate_noise_or_invalid_metrics() {
        let mut history = History::new(48000);
        for sample in [0.0, 1e-6, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            for _ in 0..1024 {
                history.push(sample, sample);
            }
        }
        let snapshot = history.snapshot();
        assert!(snapshot.gain.is_finite() && (1.0..=8.0).contains(&snapshot.gain));
        assert_eq!(snapshot.correlation, None);
        assert_eq!(brightness(&snapshot.image(Duration::ZERO)), 0);
        assert_eq!(brightness(&Snapshot::default().image(Duration::ZERO)), 0);
    }
}
