//! Sleep timer: a timed deadline or the end of the current song, with a
//! ten-second fade before Grove pauses. Pure, so it is tested with fake time.
use std::time::{Duration, Instant};

pub const FADE: Duration = Duration::from_secs(10);
/// Longest custom timer, in minutes.
pub const MAX_MINUTES: u64 = 12 * 60;

#[derive(Clone, Debug, PartialEq)]
pub enum Mode {
    Off,
    At(Instant),
    /// Pause when the song with this URI ends.
    EndOfSong(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    /// Play at this linear level (1 is unchanged).
    Gain(f64),
    /// Pause now; the level returns to 1 once playback has stopped.
    Pause,
}

#[derive(Clone, Debug)]
pub struct SleepTimer {
    mode: Mode,
    /// Silent until the pause is confirmed, so nothing leaks after the fade.
    holding: bool,
}

impl Default for SleepTimer {
    fn default() -> Self {
        Self {
            mode: Mode::Off,
            holding: false,
        }
    }
}

/// What the player is doing, sampled on each tick.
#[derive(Clone, Copy, Debug)]
pub struct Playback<'a> {
    pub playing: bool,
    pub uri: &'a str,
    pub position_ms: f64,
    pub duration_ms: f64,
}

impl SleepTimer {
    #[cfg(test)]
    pub fn active(&self) -> bool {
        self.mode != Mode::Off
    }

    pub fn start_minutes(&mut self, minutes: u64, now: Instant) -> bool {
        if minutes == 0 || minutes > MAX_MINUTES {
            return false;
        }
        self.mode = Mode::At(now + Duration::from_secs(minutes * 60));
        self.holding = false;
        true
    }

    /// Stop after the current song. Needs a song to be loaded.
    pub fn end_of_song(&mut self, uri: &str) -> bool {
        if uri.is_empty() {
            return false;
        }
        self.mode = Mode::EndOfSong(uri.into());
        self.holding = false;
        true
    }

    /// Off, and full level again even if a pause never landed.
    pub fn cancel(&mut self) {
        self.mode = Mode::Off;
        self.holding = false;
    }

    pub fn tick(&mut self, now: Instant, p: Playback) -> Action {
        if self.holding {
            if p.playing {
                return Action::Gain(0.0);
            }
            self.holding = false;
        }
        match self.mode.clone() {
            Mode::Off => Action::Gain(1.0),
            Mode::At(deadline) => {
                let left = deadline.saturating_duration_since(now);
                if !p.playing {
                    // A deadline that passes while nothing plays must not
                    // pause the next listening session.
                    if left.is_zero() {
                        self.mode = Mode::Off;
                    }
                    return Action::Gain(1.0);
                }
                if left.is_zero() {
                    return self.pause();
                }
                Action::Gain(fade_level(left))
            }
            Mode::EndOfSong(uri) => {
                if !p.playing {
                    return Action::Gain(1.0);
                }
                if p.uri != uri {
                    // The next song began: the end passed between ticks.
                    return self.pause();
                }
                if !p.duration_ms.is_finite() || p.duration_ms <= 0.0 {
                    return Action::Gain(1.0);
                }
                let left_ms = (p.duration_ms - p.position_ms).max(0.0);
                if left_ms <= 250.0 {
                    return self.pause();
                }
                Action::Gain(fade_level(Duration::from_millis(left_ms as u64)))
            }
        }
    }

    fn pause(&mut self) -> Action {
        self.mode = Mode::Off;
        self.holding = true;
        Action::Pause
    }

    /// Short text for the player and Settings; empty when off.
    pub fn label(&self, now: Instant) -> String {
        match &self.mode {
            Mode::Off => String::new(),
            Mode::EndOfSong(_) => "Sleep at end of song".into(),
            Mode::At(deadline) => {
                let left = deadline.saturating_duration_since(now).as_secs();
                if left < 60 {
                    format!("Sleep in 0:{left:02}")
                } else if left < 3600 {
                    format!("Sleep in {} min", left.div_ceil(60))
                } else {
                    let minutes = left.div_ceil(60);
                    format!("Sleep in {} h {:02} min", minutes / 60, minutes % 60)
                }
            }
        }
    }
}

fn fade_level(left: Duration) -> f64 {
    if left >= FADE {
        1.0
    } else {
        left.as_secs_f64() / FADE.as_secs_f64()
    }
}

/// Parse a custom length: "25", "25m", "1:30" (h:mm) or "1h 30m".
pub fn parse_minutes(text: &str) -> Option<u64> {
    let text = text.trim().to_ascii_lowercase();
    if text.is_empty() || text.len() > 16 {
        return None;
    }
    let minutes = if let Some((h, m)) = text.split_once(':') {
        let h: u64 = h.trim().parse().ok()?;
        let m: u64 = m.trim().parse().ok()?;
        if m >= 60 {
            return None;
        }
        h * 60 + m
    } else if text.contains('h') {
        let (h, rest) = text.split_once('h')?;
        let h: u64 = h.trim().parse().ok()?;
        let rest = rest
            .trim()
            .trim_end_matches("min")
            .trim_end_matches('m')
            .trim();
        let m: u64 = if rest.is_empty() {
            0
        } else {
            rest.parse().ok()?
        };
        h * 60 + m
    } else {
        text.trim_end_matches("min")
            .trim_end_matches('m')
            .trim()
            .parse()
            .ok()?
    };
    (1..=MAX_MINUTES).contains(&minutes).then_some(minutes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playing(uri: &str, position_ms: f64) -> Playback<'_> {
        Playback {
            playing: true,
            uri,
            position_ms,
            duration_ms: 200_000.0,
        }
    }

    #[test]
    fn timed_sleep_fades_over_ten_seconds_then_pauses_once() {
        let start = Instant::now();
        let mut timer = SleepTimer::default();
        assert!(timer.start_minutes(15, start));
        assert_eq!(timer.label(start), "Sleep in 15 min");
        let p = playing("spotify:track:a", 1000.0);
        assert_eq!(timer.tick(start, p), Action::Gain(1.0));
        let near = start + Duration::from_secs(15 * 60 - 5);
        assert_eq!(timer.label(near), "Sleep in 0:05");
        assert_eq!(timer.tick(near, p), Action::Gain(0.5));
        let end = start + Duration::from_secs(15 * 60);
        assert_eq!(timer.tick(end, p), Action::Pause);
        assert!(!timer.active());
        // Silent while the pause is in flight, unity once it lands.
        assert_eq!(timer.tick(end, p), Action::Gain(0.0));
        let paused = Playback {
            playing: false,
            ..p
        };
        assert_eq!(timer.tick(end, paused), Action::Gain(1.0));
        assert_eq!(timer.tick(end, p), Action::Gain(1.0));
        assert_eq!(timer.label(end), "");
    }

    #[test]
    fn an_idle_expiry_never_pauses_the_next_session() {
        let start = Instant::now();
        let mut timer = SleepTimer::default();
        timer.start_minutes(30, start);
        let idle = Playback {
            playing: false,
            ..playing("spotify:track:a", 0.0)
        };
        let later = start + Duration::from_secs(31 * 60);
        assert_eq!(timer.tick(later, idle), Action::Gain(1.0));
        assert!(!timer.active());
        assert_eq!(
            timer.tick(later, playing("spotify:track:a", 0.0)),
            Action::Gain(1.0)
        );
    }

    #[test]
    fn end_of_song_fades_the_last_ten_seconds_and_catches_a_missed_end() {
        let now = Instant::now();
        let mut timer = SleepTimer::default();
        assert!(!timer.end_of_song(""));
        assert!(timer.end_of_song("spotify:track:a"));
        assert_eq!(timer.label(now), "Sleep at end of song");
        assert_eq!(
            timer.tick(now, playing("spotify:track:a", 100_000.0)),
            Action::Gain(1.0)
        );
        assert_eq!(
            timer.tick(now, playing("spotify:track:a", 195_000.0)),
            Action::Gain(0.5)
        );
        assert_eq!(
            timer.tick(now, playing("spotify:track:a", 199_900.0)),
            Action::Pause
        );
        let paused = Playback {
            playing: false,
            ..playing("spotify:track:a", 0.0)
        };
        assert_eq!(timer.tick(now, paused), Action::Gain(1.0));
        timer.end_of_song("spotify:track:a");
        assert_eq!(
            timer.tick(now, playing("spotify:track:b", 300.0)),
            Action::Pause,
            "Next song started between ticks"
        );
        assert_eq!(
            timer.tick(now, playing("spotify:track:b", 400.0)),
            Action::Gain(0.0)
        );
    }

    #[test]
    fn presets_custom_lengths_and_cancel() {
        let now = Instant::now();
        let mut timer = SleepTimer::default();
        assert!(!timer.start_minutes(0, now));
        assert!(!timer.start_minutes(MAX_MINUTES + 1, now));
        assert!(timer.start_minutes(90, now));
        assert_eq!(timer.label(now), "Sleep in 1 h 30 min");
        timer.cancel();
        assert_eq!(timer.tick(now, playing("x", 0.0)), Action::Gain(1.0));
        for (text, minutes) in [
            ("25", Some(25)),
            ("25m", Some(25)),
            ("25 min", Some(25)),
            ("1:30", Some(90)),
            ("1h 30m", Some(90)),
            ("2h", Some(120)),
            ("0", None),
            ("1:75", None),
            ("soon", None),
            ("", None),
            ("9999", None),
        ] {
            assert_eq!(parse_minutes(text), minutes, "{text}");
        }
    }
}
