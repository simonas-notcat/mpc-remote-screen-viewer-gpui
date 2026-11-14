//! Cheap per-attempt network statistics, independent of UI paints/polling.
//! One sample per >=1 second, retaining the latest 120 actual intervals.
use std::{collections::VecDeque, time::Duration};

pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
pub const HISTORY_LIMIT: usize = 120;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rates {
    pub frames_per_second: f64,
    /// Decimal kilobits, including the five-byte envelope of every received message.
    pub kilobits_per_second: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    /// Monotonic time since this connection attempt started.
    pub at: Duration,
    pub interval: Duration,
    pub rates: Rates,
    pub rtt: Option<Duration>,
    /// None until a FrameUpdate has been processed; zero is best, not unknown.
    pub quality: Option<u8>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Statistics {
    pub received_bytes: u64,
    pub frames: u64,
    pub skipped_rectangles: u64,
    pub quality: u8,
    pub rtt: Option<Duration>,
    /// None during initial sampling or after disconnection. Idle streaming is Some(0).
    pub rates: Option<Rates>,
    history: VecDeque<Sample>,
    last_sample: Duration,
    last_frames: u64,
    last_bytes: u64,
}
impl Statistics {
    pub fn history(&self) -> &VecDeque<Sample> {
        &self.history
    }
    pub fn received_message(&mut self, payload_bytes: usize) {
        self.received_bytes = self
            .received_bytes
            .saturating_add(payload_bytes as u64)
            .saturating_add(5);
    }
    pub fn processed_frame(&mut self, quality: u8, skipped_rectangles: usize) {
        self.frames = self.frames.saturating_add(1);
        self.quality = quality;
        self.skipped_rectangles = self
            .skipped_rectangles
            .saturating_add(skipped_rectangles as u64);
    }
    /// Called by the worker even on read timeouts. Delayed work produces one sample
    /// over its real interval, never an artificial burst of backfilled samples.
    pub fn sample(&mut self, now: Duration) {
        if now < self.last_sample
            || self.frames < self.last_frames
            || self.received_bytes < self.last_bytes
        {
            // Defensive reset for a replaced clock/counter set, not wrapping subtraction.
            self.history.clear();
            self.rates = None;
            self.baseline(now);
            return;
        }
        let interval = now - self.last_sample;
        if interval < SAMPLE_INTERVAL {
            return;
        }
        let seconds = interval.as_secs_f64();
        let rates = Rates {
            frames_per_second: (self.frames - self.last_frames) as f64 / seconds,
            kilobits_per_second: (self.received_bytes - self.last_bytes) as f64 * 0.008 / seconds,
        };
        if self.history.len() == HISTORY_LIMIT {
            self.history.pop_front();
        }
        self.history.push_back(Sample {
            at: now,
            interval,
            rates,
            rtt: self.rtt,
            quality: (self.frames > 0).then_some(self.quality),
        });
        self.rates = Some(rates);
        self.baseline(now);
    }
    /// Preserve counters/history for diagnosis, but never display stale rates as live.
    pub fn disconnected(&mut self) {
        self.rates = None;
    }
    fn baseline(&mut self, now: Duration) {
        self.last_sample = now;
        self.last_frames = self.frames;
        self.last_bytes = self.received_bytes;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_use_processed_frames_all_envelopes_and_actual_elapsed_time() {
        let mut stats = Statistics::default();
        stats.received_message(0);
        stats.received_message(995);
        stats.processed_frame(2, 3);
        stats.processed_frame(0, 0);
        stats.rtt = Some(Duration::from_millis(12));
        stats.sample(Duration::ZERO);
        stats.sample(Duration::from_millis(999));
        assert_eq!(stats.rates, None);
        assert!(stats.history().is_empty());
        stats.sample(Duration::from_millis(1250));
        let rates = stats.rates.unwrap();
        assert_eq!(rates.frames_per_second, 1.6);
        assert!((rates.kilobits_per_second - 6.432).abs() < 1e-9);
        assert_eq!(stats.received_bytes, 1005);
        assert_eq!(stats.skipped_rectangles, 3);
        let sample = stats.history()[0];
        assert_eq!(sample.at, Duration::from_millis(1250));
        assert_eq!(sample.interval, sample.at);
        assert_eq!(sample.quality, Some(0));
        assert_eq!(sample.rtt, stats.rtt);
        // Polling repeatedly does not count frames or generate samples.
        for _ in 0..1000 {
            stats.sample(Duration::from_millis(1300));
        }
        assert_eq!(stats.history().len(), 1);
        assert_eq!(stats.frames, 2);
        stats.sample(Duration::from_millis(2250));
        assert_eq!(stats.rates, Some(Rates::default()));
    }

    #[test]
    fn delayed_sampling_is_not_backfilled_and_history_is_bounded_and_ordered() {
        let mut stats = Statistics::default();
        stats.processed_frame(1, 0);
        stats.sample(Duration::from_secs(10));
        assert_eq!(stats.history().len(), 1);
        assert_eq!(stats.rates.unwrap().frames_per_second, 0.1);
        for second in 11..=10000 {
            stats.processed_frame(0, 0);
            stats.sample(Duration::from_secs(second));
            assert!(stats.history().len() <= HISTORY_LIMIT);
        }
        assert_eq!(stats.history().len(), HISTORY_LIMIT);
        assert_eq!(stats.history().front().unwrap().at.as_secs(), 9881);
        assert_eq!(stats.history().back().unwrap().at.as_secs(), 10000);
        let copy = stats.clone();
        stats.sample(Duration::from_secs(10001));
        assert_eq!(copy.history().back().unwrap().at.as_secs(), 10000);
    }

    #[test]
    fn disconnect_and_new_attempt_do_not_show_stale_rates_or_quality() {
        let mut stats = Statistics::default();
        stats.sample(SAMPLE_INTERVAL);
        assert_eq!(stats.history()[0].quality, None);
        stats.processed_frame(4, 0);
        stats.sample(SAMPLE_INTERVAL * 2);
        let history = stats.history().clone();
        stats.disconnected();
        assert_eq!(stats.rates, None);
        assert_eq!(stats.history(), &history);
        assert_eq!(stats.frames, 1);
        stats = Statistics::default();
        assert!(stats.history().is_empty());
        assert_eq!(stats.rtt, None);
        assert_eq!(stats.rates, None);
        stats.sample(SAMPLE_INTERVAL);
        assert_eq!(stats.rates, Some(Rates::default()));
        assert_eq!(stats.history()[0].quality, None);
    }

    #[test]
    fn regressions_and_saturated_counters_never_wrap_or_produce_invalid_rates() {
        let mut stats = Statistics::default();
        stats.received_message(10);
        stats.processed_frame(0, 0);
        stats.sample(SAMPLE_INTERVAL * 2);
        stats.sample(SAMPLE_INTERVAL); // replaced clock
        assert!(stats.history().is_empty());
        assert_eq!(stats.rates, None);
        stats.sample(SAMPLE_INTERVAL * 2);
        assert_eq!(stats.rates, Some(Rates::default()));
        stats.frames = 0; // replaced counters
        stats.received_bytes = 0;
        stats.sample(SAMPLE_INTERVAL * 3);
        assert!(stats.history().is_empty());
        stats.frames = u64::MAX;
        stats.received_bytes = u64::MAX;
        stats.skipped_rectangles = u64::MAX;
        stats.received_message(100);
        stats.processed_frame(255, 1);
        stats.sample(SAMPLE_INTERVAL * 4);
        assert_eq!(stats.frames, u64::MAX);
        assert_eq!(stats.received_bytes, u64::MAX);
        assert_eq!(stats.skipped_rectangles, u64::MAX);
        let rates = stats.rates.unwrap();
        assert!(rates.frames_per_second.is_finite());
        assert!(rates.kilobits_per_second.is_finite());
        stats.sample(SAMPLE_INTERVAL * 5);
        assert_eq!(stats.rates, Some(Rates::default()));
    }
}
