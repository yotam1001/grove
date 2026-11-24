//! When to retry a lost librespot session, and what to restore afterwards.
use std::time::{Duration, Instant, SystemTime};

/// Waits between attempts. The last value repeats until the budget runs out.
const DELAYS: [u64; 7] = [1, 2, 4, 8, 15, 30, 60];
/// Give up after this long without a working session.
pub const BUDGET: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Debug, PartialEq)]
pub struct Backoff {
    attempt: u32,
    started: Instant,
}

impl Backoff {
    pub fn new(now: Instant) -> Self {
        Self {
            attempt: 0,
            started: now,
        }
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// The wait before the next attempt, or None once the budget is spent.
    pub fn next(&mut self, now: Instant) -> Option<Duration> {
        if now.saturating_duration_since(self.started) >= BUDGET {
            return None;
        }
        let index = (self.attempt as usize).min(DELAYS.len() - 1);
        self.attempt += 1;
        Some(Duration::from_secs(DELAYS[index]))
    }

    /// The network came back (or the PC woke): try again soon.
    pub fn hurry(&mut self) {
        self.attempt = self.attempt.min(1);
    }
}

/// Spots a wall-clock jump between ticks, which means Windows slept.
#[derive(Clone, Debug)]
pub struct WakeDetector {
    last: SystemTime,
    interval: Duration,
}

impl WakeDetector {
    pub fn new(now: SystemTime, interval: Duration) -> Self {
        Self {
            last: now,
            interval,
        }
    }

    pub fn woke(&mut self, now: SystemTime) -> bool {
        let gap = now.duration_since(self.last).unwrap_or_default();
        self.last = now;
        gap > self.interval * 4 + Duration::from_secs(10)
    }
}

/// The track and position to load once a new session is ready.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RestorePoint {
    pub track_uri: String,
    /// The album or playlist the track came from, when Grove started it.
    pub context_uri: String,
    pub position_ms: u32,
    pub playing: bool,
}

impl RestorePoint {
    pub fn is_empty(&self) -> bool {
        self.track_uri.is_empty()
    }

    /// Back off a couple of seconds so the listener hears where they left off.
    pub fn resume_position(&self) -> u32 {
        self.position_ms.saturating_sub(2_000)
    }
}

/// Short player text for a pending retry.
pub fn label(attempt: u32, retry_in: Duration) -> String {
    let seconds = retry_in.as_secs();
    if attempt <= 1 || seconds == 0 {
        "Reconnecting…".into()
    } else {
        format!("Reconnecting… retry {attempt} in {seconds} s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_grow_then_cap_and_the_budget_ends_retries() {
        let start = Instant::now();
        let mut backoff = Backoff::new(start);
        let waits: Vec<u64> = (0..9)
            .map(|_| backoff.next(start).unwrap().as_secs())
            .collect();
        assert_eq!(waits, vec![1, 2, 4, 8, 15, 30, 60, 60, 60]);
        assert_eq!(backoff.attempt(), 9);
        backoff.hurry();
        assert_eq!(backoff.next(start), Some(Duration::from_secs(2)));
        assert_eq!(backoff.next(start + BUDGET), None);
    }

    #[test]
    fn wall_clock_jumps_mean_the_pc_slept() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let mut wake = WakeDetector::new(t, Duration::from_secs(5));
        assert!(!wake.woke(t + Duration::from_secs(5)));
        assert!(
            !wake.woke(t + Duration::from_secs(12)),
            "A slow tick is not sleep"
        );
        assert!(wake.woke(t + Duration::from_secs(600)));
        assert!(!wake.woke(t), "A clock set backwards is not sleep");
    }

    #[test]
    fn restore_points_resume_slightly_earlier_and_labels_stay_short() {
        let point = RestorePoint {
            track_uri: "spotify:track:a".into(),
            position_ms: 61_000,
            ..Default::default()
        };
        assert_eq!(point.resume_position(), 59_000);
        assert_eq!(
            RestorePoint {
                position_ms: 500,
                ..point.clone()
            }
            .resume_position(),
            0
        );
        assert!(RestorePoint::default().is_empty());
        assert_eq!(label(1, Duration::from_secs(1)), "Reconnecting…");
        assert_eq!(
            label(4, Duration::from_secs(8)),
            "Reconnecting… retry 4 in 8 s"
        );
    }
}
