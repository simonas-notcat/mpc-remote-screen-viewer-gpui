//! Bounded, source-clock amplitude history, independent of FFT windowing.
use std::{collections::VecDeque, sync::Arc};

pub const SECONDS: u64 = 1;
const BUCKETS_PER_SECOND: u128 = 2000;
const BUCKETS: usize = SECONDS as usize * BUCKETS_PER_SECOND as usize;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Column {
    pub min: [f32; 2],
    pub max: [f32; 2],
}
impl Column {
    fn sample(left: f32, right: f32) -> Self {
        let sample = [left, right].map(|value| {
            if value.is_finite() {
                value.clamp(-1.0, 1.0)
            } else {
                0.0
            }
        });
        Self {
            min: sample,
            max: sample,
        }
    }
    fn merge(&mut self, other: Self) {
        for channel in 0..2 {
            self.min[channel] = self.min[channel].min(other.min[channel]);
            self.max[channel] = self.max[channel].max(other.max[channel]);
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Bucket {
    first: u64,
    end: u64, // Exclusive source-frame position; gaps are never included.
    levels: Column,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    rate: u32,
    end: u64,
    buckets: VecDeque<Bucket>,
}
impl Snapshot {
    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty()
    }
    /// Latest observed source position, in seconds relative to stream start.
    pub fn end(&self) -> f64 {
        if self.rate == 0 {
            0.0
        } else {
            self.end as f64 / f64::from(self.rate)
        }
    }
    /// Peak-merge every bucket intersecting each display pixel. Never decimate
    /// samples or interpolate across gaps. None means unobserved, not silence.
    /// Work is bounded by history size plus the requested display width.
    pub fn columns(&self, width: usize) -> Vec<Option<Column>> {
        let mut columns: Vec<Option<Column>> = vec![None; width];
        if self.rate == 0 || width == 0 {
            return columns;
        }
        let frames = u128::from(self.rate) * u128::from(SECONDS);
        let width128 = width as u128;
        for bucket in &self.buckets {
            // Use integer ages, not floating-point absolute timestamps. Early
            // audio stays at the right of a full one-second window, and very
            // large source positions don't lose sample/pixel precision.
            let from = (u128::from(self.end - bucket.first) * width128).div_ceil(frames);
            let to = u128::from(self.end - bucket.end) * width128 / frames;
            let from = width - from.min(width128) as usize;
            let to = width - to.min(width128) as usize;
            for column in &mut columns[from..to] {
                if let Some(column) = column {
                    column.merge(bucket.levels);
                } else {
                    *column = Some(bucket.levels);
                }
            }
        }
        columns
    }
}

pub(crate) struct History {
    data: Snapshot,
    bucket_end: u128,
}
impl History {
    pub fn new(rate: u32) -> Self {
        assert!(rate > 0);
        Self {
            data: Snapshot {
                rate,
                ..Snapshot::default()
            },
            bucket_end: 0,
        }
    }
    /// Accept each fresh, unwindowed source frame exactly once. Mono callers
    /// duplicate the sample; multichannel callers supply only the first L/R pair.
    pub fn push(&mut self, at: u64, left: f32, right: f32) {
        let Some(end) = at.checked_add(1).filter(|_| at >= self.data.end) else {
            return;
        };
        let levels = Column::sample(left, right);
        if let Some(last) = self.data.buckets.back_mut()
            && last.end == at
            && u128::from(at) < self.bucket_end
        {
            last.end = end;
            last.levels.merge(levels);
        } else {
            let rate = u128::from(self.data.rate);
            let bucket = u128::from(at) * BUCKETS_PER_SECOND / rate;
            self.bucket_end = ((bucket + 1) * rate).div_ceil(BUCKETS_PER_SECOND);
            self.data.buckets.push_back(Bucket {
                first: at,
                end,
                levels,
            });
        }
        self.data.end = end;
        self.trim();
    }
    /// Advance through a missing interval without inventing samples or erasing
    /// earlier audio. The next push starts a new bucket even within the same
    /// 0.5 ms time bucket, so a small gap never becomes connecting audio.
    pub fn gap(&mut self, at: u64) {
        self.data.end = self.data.end.max(at);
        self.trim();
    }
    fn trim(&mut self) {
        let first = self
            .data
            .end
            .saturating_sub(u64::from(self.data.rate) * SECONDS);
        while self.data.buckets.len() > BUCKETS + 1
            || self.data.buckets.front().is_some_and(|b| b.end <= first)
        {
            self.data.buckets.pop_front();
        }
        // A boundary bucket can include at most 0.5 ms before the visible
        // window. The count cap also bounds pathological sub-bucket gaps;
        // those can shorten retained history, but never fill missing audio.
    }
    pub fn snapshot(&self) -> Arc<Snapshot> {
        Arc::new(self.data.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_and_display_columns_preserve_stereo_transients_without_auto_gain() {
        let mut history = History::new(48000);
        for at in 0..48 {
            let (left, right) = match at {
                3 => (-0.9, 0.1),
                37 => (0.75, -0.8),
                _ => (0.01, -0.01),
            };
            history.push(at, left, right);
        }
        let snapshot = history.snapshot();
        assert_eq!(snapshot.buckets.len(), 2);
        assert_eq!(snapshot.end(), 0.001);
        assert_eq!(
            snapshot.columns(1),
            vec![Some(Column {
                min: [-0.9, -0.8],
                max: [0.75, 0.1],
            })]
        );
        let columns = snapshot.columns(2000);
        assert!(columns[..1998].iter().all(Option::is_none));
        assert_eq!(columns[1998].unwrap().min, [-0.9, -0.01]);
        assert_eq!(columns[1999].unwrap().max, [0.75, -0.01]);
        assert!(snapshot.columns(0).is_empty());
    }

    #[test]
    fn gaps_stay_blank_and_keep_preceding_audio() {
        let mut history = History::new(8000);
        for at in 0..2000 {
            history.push(at, 0.5, -0.5);
        }
        history.gap(6000);
        let gap = history.snapshot();
        assert_eq!(gap.end(), 0.75);
        assert!(gap.columns(8)[4..].iter().all(Option::is_none));
        for at in 6000..8000 {
            history.push(at, -0.5, 0.5);
        }
        let columns = history.snapshot().columns(8);
        assert!(columns[..2].iter().all(|c| c.unwrap().min == [0.5, -0.5]));
        assert!(columns[2..6].iter().all(Option::is_none));
        assert!(columns[6..].iter().all(|c| c.unwrap().max == [-0.5, 0.5]));
        history.gap(16000);
        assert!(history.snapshot().is_empty());
    }

    #[test]
    fn gaps_inside_one_bucket_are_not_connected_or_filled() {
        let mut history = History::new(8000);
        history.push(0, 0.5, 0.5);
        history.gap(3);
        history.push(3, -0.5, -0.5);
        let snapshot = history.snapshot();
        assert_eq!(snapshot.buckets.len(), 2);
        let columns = snapshot.columns(8000);
        assert!(columns[..7996].iter().all(Option::is_none));
        assert_eq!(columns[7996].unwrap().min, [0.5; 2]);
        assert_eq!(columns[7997..7999], [None, None]);
        assert_eq!(columns[7999].unwrap().max, [-0.5; 2]);
    }

    #[test]
    fn history_is_bounded_at_all_rates_and_with_extreme_gap_fragmentation() {
        for rate in [8000, 44100, 48000, 192000, 384000] {
            let mut history = History::new(rate);
            for at in 0..u64::from(rate) * 2 {
                history.push(at, 0.25, -0.25);
            }
            let snapshot = history.snapshot();
            assert_eq!(snapshot.end(), 2.0);
            assert_eq!(snapshot.buckets.len(), BUCKETS);
            assert_eq!(snapshot.buckets.front().unwrap().first, u64::from(rate));
            assert!(snapshot.columns(731).iter().all(Option::is_some));
        }
        let mut history = History::new(384000);
        for at in (0..384000).step_by(2) {
            history.push(at, 0.5, 0.5);
        }
        assert!(history.data.buckets.len() <= BUCKETS + 1);
        assert!(history.data.buckets.iter().all(|b| b.end - b.first == 1));
    }

    #[test]
    fn snapshots_are_immutable_and_duplicates_do_not_rewrite_peaks() {
        let mut history = History::new(48000);
        history.push(0, 0.01, -0.01);
        let original = history.snapshot();
        history.push(0, 1.0, -1.0);
        assert_eq!(&*history.snapshot(), &*original);
        history.push(1, 0.02, -0.02);
        assert_eq!(original.columns(1)[0].unwrap().max, [0.01, -0.01]);
        assert_eq!(history.snapshot().columns(1)[0].unwrap().max, [0.02, -0.01]);
    }

    #[test]
    fn empty_silence_nonfinite_and_overrange_samples_are_safe() {
        assert!(Snapshot::default().is_empty());
        assert_eq!(Snapshot::default().end(), 0.0);
        assert_eq!(Snapshot::default().columns(2), vec![None, None]);
        let mut history = History::new(48000);
        history.push(0, f32::NAN, f32::INFINITY);
        assert_eq!(history.snapshot().columns(1)[0].unwrap().min, [0.0; 2]);
        history.push(1, -2.0, 3.0);
        let column = history.snapshot().columns(1)[0].unwrap();
        assert_eq!(column.min, [-1.0, 0.0]);
        assert_eq!(column.max, [0.0, 1.0]);
    }

    #[test]
    fn large_source_positions_keep_pixel_precision_without_overflow() {
        let mut history = History::new(8000);
        history.push(u64::MAX - 3, 0.5, -0.5);
        history.push(u64::MAX - 1, -0.5, 0.5);
        history.push(u64::MAX, 1.0, 1.0); // No representable exclusive end.
        let columns = history.snapshot().columns(8000);
        assert!(columns[..7997].iter().all(Option::is_none));
        assert_eq!(columns[7997].unwrap().max, [0.5, -0.5]);
        assert_eq!(columns[7998], None);
        assert_eq!(columns[7999].unwrap().min, [-0.5, 0.5]);
    }
}
