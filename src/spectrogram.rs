//! Bounded source-clock spectral history and a shared intensity palette.
use crate::analyzer::BANDS;
use std::{collections::VecDeque, sync::LazyLock};

pub const SECONDS: f64 = 10.0;
pub const COLUMNS: usize = 300;
const COLUMNS_PER_SECOND: f64 = COLUMNS as f64 / SECONDS;
const RIDGES_PER_SECOND: f64 = 6.0;
static PALETTE: LazyLock<[[u8; 4]; 256]> = LazyLock::new(|| {
    std::array::from_fn(|i| {
        let [r, g, b] = color(-90.0 + i as f32 * 90.0 / 255.0);
        [b, g, r, 255]
    })
});
fn bucket(at: f64, rate: f64) -> i64 {
    (at * rate).floor() as i64
}
fn bgra(db: f32) -> [u8; 4] {
    let index = if db.is_finite() {
        ((db + 90.0) * (255.0 / 90.0)).round().clamp(0.0, 255.0) as usize
    } else {
        0
    };
    PALETTE[index]
}
#[derive(Clone, Debug, PartialEq)]
pub struct Column {
    /// FFT window end, in seconds relative to the current continuous stream.
    pub at: f64,
    pub spectrum: [f32; BANDS],
    pub gap: bool,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct History {
    pub columns: VecDeque<Column>,
}
impl History {
    /// Merge high-rate FFTs into at most 30 columns/s, retaining band peaks.
    /// Returns true when a new display column was added.
    pub fn push(&mut self, at: f64, spectrum: [f32; BANDS]) -> bool {
        let same_bucket = self.columns.back().is_some_and(|last| {
            !last.gap && bucket(last.at, COLUMNS_PER_SECOND) == bucket(at, COLUMNS_PER_SECOND)
        });
        if same_bucket {
            let last = self.columns.back_mut().unwrap();
            for (a, b) in last.spectrum.iter_mut().zip(spectrum) {
                *a = a.max(b);
            }
            return false;
        }
        self.columns.push_back(Column {
            at,
            spectrum,
            gap: false,
        });
        self.trim(at);
        true
    }
    /// Preserve earlier columns, but leave the missing source interval blank.
    pub fn gap(&mut self, at: f64) {
        self.columns.push_back(Column {
            at,
            spectrum: [-90.0; BANDS],
            gap: true,
        });
        self.trim(at);
    }
    fn trim(&mut self, at: f64) {
        while self.columns.len() > COLUMNS + 1
            || self
                .columns
                .front()
                .is_some_and(|first| at - first.at > SECONDS)
        {
            self.columns.pop_front();
        }
    }
    pub fn end(&self) -> f64 {
        self.columns.back().map_or(0.0, |c| c.at)
    }

    /// Time runs left to right, low frequencies bottom to top. Pixels are BGRA
    /// for GPUI; unobserved history remains dark, never invented silence.
    pub fn image(&self) -> image::RgbaImage {
        let mut pixels = [12, 8, 8, 255].repeat(COLUMNS * BANDS);
        let end = bucket(self.end(), COLUMNS_PER_SECOND);
        let mut previous = self
            .columns
            .front()
            .map_or(end, |c| bucket(c.at, COLUMNS_PER_SECOND) - 1);
        for column in &self.columns {
            let at = bucket(column.at, COLUMNS_PER_SECOND);
            let from = (previous - end + COLUMNS as i64).clamp(0, COLUMNS as i64) as usize;
            let to = (at - end + COLUMNS as i64).clamp(0, COLUMNS as i64) as usize;
            if !column.gap {
                // Source-clock buckets map to exact pixels: advancing one
                // bucket shifts the image exactly one pixel, without jitter
                // from overlapping floor/ceil ranges at fractional FFT times.
                let from = from.min(to.saturating_sub(1));
                for (band, db) in column.spectrum.iter().enumerate() {
                    let color = bgra(*db);
                    let row = (BANDS - 1 - band) * COLUMNS * 4;
                    for pixel in pixels[row + from * 4..row + to * 4].as_chunks_mut::<4>().0 {
                        *pixel = color;
                    }
                }
            }
            previous = at;
        }
        image::RgbaImage::from_raw(COLUMNS as u32, BANDS as u32, pixels).unwrap()
    }

    /// Peak-aggregate into fixed source-time buckets, not every Nth deque index.
    /// Completed ridges keep their identity as the history slides. Index-based
    /// subsampling selected a different set of old spectra every frame.
    pub fn waterfall(&self) -> Vec<Column> {
        let mut rows: Vec<Column> = Vec::with_capacity((SECONDS * RIDGES_PER_SECOND) as usize + 1);
        let mut last_bucket = None;
        for column in self.columns.iter().filter(|c| !c.gap) {
            let at = bucket(column.at, RIDGES_PER_SECOND);
            if last_bucket == Some(at) {
                for (peak, db) in rows
                    .last_mut()
                    .unwrap()
                    .spectrum
                    .iter_mut()
                    .zip(column.spectrum)
                {
                    *peak = peak.max(db);
                }
            } else {
                rows.push(Column {
                    at: at as f64 / RIDGES_PER_SECOND,
                    spectrum: column.spectrum,
                    gap: false,
                });
                last_bucket = Some(at);
            }
        }
        rows
    }
}
/// Fixed −90..0 dBFS: near-black → purple → orange → pale yellow.
pub fn color(db: f32) -> [u8; 3] {
    const STOPS: [[f32; 3]; 5] = [
        [8.0, 8.0, 12.0],
        [50.0, 15.0, 100.0],
        [160.0, 35.0, 100.0],
        [245.0, 110.0, 30.0],
        [255.0, 250.0, 180.0],
    ];
    let value = if db.is_finite() {
        ((db + 90.0) / 90.0).clamp(0.0, 1.0) * 4.0
    } else {
        0.0
    };
    let index = (value.floor() as usize).min(3);
    let fraction = value - index as f32;
    std::array::from_fn(|channel| {
        (STOPS[index][channel] * (1.0 - fraction) + STOPS[index + 1][channel] * fraction).round()
            as u8
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_is_bounded_and_merges_peaks() {
        let mut history = History::default();
        assert!(history.push(0.001, [-60.0; BANDS]));
        assert!(!history.push(0.002, [-12.0; BANDS]));
        assert_eq!(history.columns[0].spectrum[0], -12.0);
        for i in 1..20000 {
            history.push(i as f64 / 375.0, [-30.0; BANDS]);
        }
        assert!(history.columns.len() <= COLUMNS + 1);
        assert!(history.end() - history.columns[0].at <= SECONDS);
    }
    #[test]
    fn image_has_correct_time_and_frequency_orientation() {
        let mut history = History::default();
        history.push(1.0, [-90.0; BANDS]);
        let mut spectrum = [-90.0; BANDS];
        spectrum[0] = 0.0;
        history.push(2.0, spectrum);
        let image = history.image();
        let [r, g, b] = color(0.0);
        assert_eq!(image.get_pixel(299, 63).0, [b, g, r, 255]);
        assert_ne!(image.get_pixel(299, 0), image.get_pixel(299, 63));
        assert_ne!(image.get_pixel(0, 63), image.get_pixel(299, 63));
    }
    #[test]
    fn gaps_are_blank_without_erasing_earlier_audio() {
        let mut history = History::default();
        history.push(1.0, [-6.0; BANDS]);
        history.push(2.0, [-6.0; BANDS]);
        history.gap(3.0);
        history.push(4.0, [-6.0; BANDS]);
        let image = history.image();
        let [r, g, b] = color(-6.0);
        assert_eq!(image.get_pixel(225, 0).0, [b, g, r, 255]); // 1.5 s
        assert_eq!(image.get_pixel(255, 0).0, [12, 8, 8, 255]); // 2.5 s gap
        assert_eq!(image.get_pixel(285, 0).0, [b, g, r, 255]); // 3.5 s
        assert_eq!(history.columns.front().unwrap().at, 1.0);
    }
    #[test]
    fn heatmap_scrolls_one_pixel_without_changing_old_columns() {
        let mut history = History::default();
        for i in 0..300 {
            history.push((i as f64 + 0.3) / 30.0, [-(i % 80) as f32; BANDS]);
        }
        let before = history.image();
        history.push(300.3 / 30.0, [-6.0; BANDS]);
        let after = history.image();
        for x in 0..299 {
            for y in 0..BANDS as u32 {
                assert_eq!(after.get_pixel(x, y), before.get_pixel(x + 1, y));
            }
        }
    }
    #[test]
    fn waterfall_keeps_completed_ridges_stable_as_history_rolls() {
        let mut history = History::default();
        for i in 0..300 {
            history.push((i as f64 + 0.3) / 30.0, [-(i % 80) as f32; BANDS]);
        }
        let before = history.waterfall();
        assert!(before.len() <= 61);
        for i in 300..315 {
            history.push((i as f64 + 0.3) / 30.0, [-6.0; BANDS]);
            let rows = history.waterfall();
            assert!(rows.len() <= 61);
            for row in rows
                .iter()
                .filter(|r| r.at > 1.0 && r.at < before.last().unwrap().at)
            {
                assert_eq!(Some(row), before.iter().find(|old| old.at == row.at));
            }
        }
    }
    #[test]
    fn waterfall_peak_merges_transients_instead_of_skipping_them() {
        let mut history = History::default();
        for i in 0..5 {
            let mut spectrum = [-60.0; BANDS];
            if i == 2 {
                spectrum[20] = -3.0;
            }
            history.push((i as f64 + 0.3) / 30.0, spectrum);
        }
        let rows = history.waterfall();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].spectrum[20], -3.0);
        history.gap(2.0);
        assert_eq!(history.waterfall(), rows);
    }
    #[test]
    fn palette_clamps_and_handles_nonfinite_values() {
        assert_eq!(color(-100.0), color(-90.0));
        assert_eq!(color(3.0), color(0.0));
        assert_eq!(color(f32::NAN), color(-90.0));
    }
}
