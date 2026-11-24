//! Continuous NTP-style clock offset and round-trip estimation.
//!
//! Each probe records the guest send time `t0`, host receive `t1`, host send
//! `t2` and guest receive `t3`. Offset (host minus guest) is
//! `((t1 - t0) + (t2 - t3)) / 2` and RTT is `(t3 - t0) - (t2 - t1)`. Like NTP's
//! clock filter, the estimate uses the lowest-RTT sample in a short window:
//! queueing delay only ever adds latency, so the fastest exchange is the most
//! symmetric one. Relay rooms have no host clock; they feed RTT only.
use std::collections::VecDeque;

pub const WINDOW: usize = 8;
/// Seek threshold before enough good samples exist (the old fixed policy).
pub const LOOSE_DRIFT_MS: u64 = 1_500;
/// Seek threshold once the clock is trusted (Tide issue #133: seek only >300 ms).
pub const TIGHT_DRIFT_MS: u64 = 300;
/// Samples slower than this say little about the offset.
pub const MAX_USEFUL_RTT_MS: u64 = 400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    pub offset_ms: i64,
    pub rtt_ms: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Estimator {
    samples: VecDeque<Sample>,
    total: u64,
}

impl Estimator {
    /// Add a four-timestamp exchange. Returns false for impossible samples.
    pub fn exchange(&mut self, t0: u64, t1: u64, t2: u64, t3: u64) -> bool {
        if t3 < t0 || t2 < t1 {
            return false;
        }
        let rtt = (t3 - t0).saturating_sub(t2 - t1);
        let offset = ((t1 as i128 - t0 as i128) + (t2 as i128 - t3 as i128)) / 2;
        let Ok(offset) = i64::try_from(offset) else {
            return false;
        };
        self.push(Sample {
            offset_ms: offset,
            rtt_ms: rtt,
        });
        true
    }
    /// RTT-only sample (relay polls): the offset is already folded into `age_ms`.
    pub fn round_trip(&mut self, rtt_ms: u64) {
        self.push(Sample {
            offset_ms: 0,
            rtt_ms,
        });
    }
    fn push(&mut self, sample: Sample) {
        if self.samples.len() == WINDOW {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
        self.total = self.total.saturating_add(1);
    }
    pub fn best(&self) -> Option<Sample> {
        self.samples.iter().copied().min_by_key(|s| s.rtt_ms)
    }
    pub fn latest(&self) -> Option<Sample> {
        self.samples.back().copied()
    }
    /// Spread of the recent offsets: a cheap jitter figure for the host view.
    pub fn jitter_ms(&self) -> u64 {
        let (Some(min), Some(max)) = (
            self.samples.iter().map(|s| s.offset_ms).min(),
            self.samples.iter().map(|s| s.offset_ms).max(),
        ) else {
            return 0;
        };
        min.abs_diff(max)
    }
    pub fn confident(&self) -> bool {
        self.samples.len() >= 3 && self.best().is_some_and(|s| s.rtt_ms <= MAX_USEFUL_RTT_MS)
    }
    /// How far a follower may drift before seeking.
    pub fn drift_threshold_ms(&self) -> u64 {
        if self.confident() {
            TIGHT_DRIFT_MS
        } else {
            LOOSE_DRIFT_MS
        }
    }
    /// Host time for a local timestamp, when the offset is known.
    pub fn host_time(&self, local_ms: u64) -> Option<u64> {
        let best = self.best()?;
        Some(local_ms.saturating_add_signed(best.offset_ms))
    }
    pub fn samples(&self) -> u64 {
        self.total
    }
    pub fn clear(&mut self) {
        self.samples.clear();
        self.total = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lowest_rtt_sample_wins_and_threshold_tightens() {
        let mut clock = Estimator::default();
        assert_eq!(clock.drift_threshold_ms(), LOOSE_DRIFT_MS);
        // Host clock is 5 000 ms ahead; symmetric 20 ms paths.
        assert!(clock.exchange(1_000, 6_010, 6_011, 1_021));
        // Congested, asymmetric sample: 300 ms one way, 10 ms back.
        assert!(clock.exchange(2_000, 7_300, 7_301, 2_311));
        assert!(clock.exchange(3_000, 8_012, 8_013, 3_025));
        let best = clock.best().unwrap();
        assert_eq!(best.rtt_ms, 20);
        assert_eq!(best.offset_ms, 5_000);
        assert!(clock.confident());
        assert_eq!(clock.drift_threshold_ms(), TIGHT_DRIFT_MS);
        assert_eq!(clock.host_time(10_000), Some(15_000));
        assert!(clock.jitter_ms() >= 140);
        assert!(!clock.exchange(10, 0, 0, 5));
    }
    #[test]
    fn window_is_bounded_and_slow_links_stay_loose() {
        let mut clock = Estimator::default();
        for n in 0..20 {
            clock.round_trip(900 + n);
        }
        assert_eq!(clock.samples.len(), WINDOW);
        assert_eq!(clock.samples(), 20);
        assert!(!clock.confident());
        assert_eq!(clock.best().unwrap().rtt_ms, 912);
    }
}
