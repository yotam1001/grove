//! Follower policy: drift correction, buffering acknowledgements and scheduled
//! simultaneous starts.
//!
//! The host's position is projected with the NTP-style clock offset when one is
//! known ([`crate::clock::Estimator`]), otherwise with the sample age plus a
//! bounded transit estimate, so machine clocks never need to match. A sample
//! timestamped in the future is a scheduled start: guests load paused, report
//! `Buffered`, and start at the host's chosen instant.
use crate::jam::{Member, Playback};
use std::time::{Duration, Instant};

/// How long a host waits for buffering acknowledgements before starting anyway.
pub const BUFFER_WAIT: Duration = Duration::from_secs(6);

/// Connected followers that have not acknowledged buffering `play_id`.
pub fn unbuffered(members: &[Member], play_id: u64) -> Vec<String> {
    members
        .iter()
        .filter(|m| !m.host && m.connected && m.following && m.ready_play != play_id)
        .map(|m| m.name.clone())
        .collect()
}

/// Lead time for a scheduled start: enough for the slowest follower's update
/// to arrive (half its round trip, doubled for margin) plus command latency.
pub fn start_lead_ms(members: &[Member]) -> u64 {
    let slowest = members
        .iter()
        .filter(|m| !m.host && m.connected && m.following)
        .filter_map(|m| m.sync.as_ref().map(|s| s.rtt_ms))
        .max()
        .unwrap_or(200);
    (600 + slowest).min(3_000)
}

/// A scheduled start further ahead than this is treated as invalid.
pub const MAX_LEAD_MS: u64 = 10_000;

pub struct Target {
    pub playback: Playback,
    pub host_sent_ms: u64,
    pub received: Instant,
    pub transit_ms: u64,
    /// (host-minus-local offset, local unix ms when the sample arrived).
    pub clock: Option<(i64, u64)>,
}
impl Target {
    fn elapsed(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.received).as_millis() as u64
    }
    /// The host's clock now, as best this guest can tell.
    pub fn host_now(&self, now: Instant) -> u64 {
        match self.clock {
            Some((offset, local)) => local
                .saturating_add(self.elapsed(now))
                .saturating_add_signed(offset),
            None => self
                .host_sent_ms
                .saturating_add(self.transit_ms.min(500))
                .saturating_add(self.elapsed(now)),
        }
    }
    pub fn position(&self, now: Instant) -> u64 {
        let sample = &self.playback;
        if !sample.playing || sample.preparing {
            return sample.position_ms;
        }
        let age = if self.clock.is_some() {
            self.host_now(now).saturating_sub(sample.sampled_at_ms)
        } else if sample.sampled_at_ms > self.host_now(now) {
            0
        } else {
            // Without a clock estimate, never trust more than 5 s of host-side age.
            self.host_sent_ms
                .saturating_sub(sample.sampled_at_ms)
                .min(5000)
                .saturating_add(self.transit_ms.min(500))
                .saturating_add(self.elapsed(now))
        };
        sample.position_ms.saturating_add(age)
    }
    /// Milliseconds until a scheduled start, if this sample is one.
    pub fn starts_in(&self, now: Instant) -> Option<u64> {
        let sample = &self.playback;
        if !sample.playing || sample.preparing {
            return None;
        }
        let host_now = self.host_now(now);
        (sample.sampled_at_ms > host_now).then(|| sample.sampled_at_ms - host_now)
    }
    pub fn stale(&self, now: Instant) -> bool {
        if self.playback.sampled_at_ms > self.host_sent_ms.saturating_add(MAX_LEAD_MS) {
            return true;
        }
        self.host_sent_ms
            .saturating_sub(self.playback.sampled_at_ms)
            .saturating_add(self.elapsed(now))
            > 5000
    }
}
pub struct Local<'a> {
    pub uri: &'a str,
    pub position_ms: u64,
    pub playing: bool,
    pub ready: bool,
}
#[derive(Debug, PartialEq)]
pub enum Correction {
    Load {
        uri: String,
        position_ms: u64,
        playing: bool,
    },
    Seek(u64),
    Play,
    Pause,
    /// Start playing after this delay (scheduled simultaneous start).
    PlayAt {
        delay_ms: u64,
    },
    /// Tell the host this play is loaded and paused, ready to start.
    Buffered {
        play_id: u64,
    },
}
pub struct Follower {
    last_action: Option<Instant>,
    loading_uri: String,
    loads: u8,
    acked: u64,
    scheduled: u64,
    /// Seek only past this drift; tightened once the clock is trusted.
    pub threshold_ms: u64,
}
impl Default for Follower {
    fn default() -> Self {
        Self {
            last_action: None,
            loading_uri: String::new(),
            loads: 0,
            acked: 0,
            scheduled: 0,
            threshold_ms: crate::clock::LOOSE_DRIFT_MS,
        }
    }
}

impl Follower {
    pub fn exhausted(&self, uri: &str, now: Instant) -> bool {
        self.loading_uri == uri
            && self.loads >= 3
            && self
                .last_action
                .is_some_and(|last| now.saturating_duration_since(last) >= Duration::from_secs(3))
    }

    /// Local minus host position, when both play the same song.
    pub fn drift(target: &Target, local: &Local<'_>, now: Instant) -> Option<i64> {
        (local.uri == target.playback.uri
            && local.playing
            && target.playback.playing
            && !target.playback.preparing
            && target.starts_in(now).is_none())
        .then(|| local.position_ms as i64 - target.position(now) as i64)
    }

    fn cooling(&self, now: Instant) -> bool {
        self.last_action
            .is_some_and(|last| now.saturating_duration_since(last) < Duration::from_secs(3))
    }

    fn load(
        &mut self,
        target: &Target,
        position_ms: u64,
        playing: bool,
        now: Instant,
    ) -> Option<Correction> {
        if self.loads >= 3 || self.cooling(now) {
            return None;
        }
        self.loads += 1;
        self.last_action = Some(now);
        Some(Correction::Load {
            uri: target.playback.uri.clone(),
            position_ms,
            playing,
        })
    }

    pub fn plan(&mut self, target: &Target, local: Local<'_>, now: Instant) -> Option<Correction> {
        if !local.ready || target.stale(now) {
            return None;
        }
        let sample = &target.playback;
        if sample.uri != self.loading_uri {
            self.loading_uri = sample.uri.clone();
            self.loads = 0;
            self.last_action = None;
        }
        // Host is buffering a new song: load it paused and acknowledge.
        if sample.preparing {
            if sample.uri != local.uri {
                return self.load(target, sample.position_ms, false, now);
            }
            if local.playing {
                if self.cooling(now) {
                    return None;
                }
                self.last_action = Some(now);
                return Some(Correction::Pause);
            }
            if sample.play_id != 0 && self.acked != sample.play_id {
                self.acked = sample.play_id;
                return Some(Correction::Buffered {
                    play_id: sample.play_id,
                });
            }
            return None;
        }
        // Scheduled start: be loaded and paused, then start on time.
        if let Some(delay_ms) = target.starts_in(now) {
            if sample.uri != local.uri {
                return self.load(target, sample.position_ms, false, now);
            }
            if self.scheduled != sample.play_id || sample.play_id == 0 {
                self.scheduled = sample.play_id;
                return Some(Correction::PlayAt { delay_ms });
            }
            return None;
        }
        if self.cooling(now) {
            return None;
        }
        let position = target.position(now);
        let correction = if sample.uri != local.uri {
            return self.load(target, position, sample.playing, now);
        } else if sample.playing != local.playing {
            if sample.playing {
                Correction::Play
            } else {
                Correction::Pause
            }
        } else if position.abs_diff(local.position_ms) > self.threshold_ms {
            Correction::Seek(position)
        } else {
            return None;
        };
        self.last_action = Some(now);
        Some(correction)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn target(now: Instant) -> Target {
        Target {
            playback: Playback {
                uri: "spotify:track:2JRo0gjbX4GrCqBYdRohoo".into(),
                title: "Track".into(),
                artist: "Artist".into(),
                position_ms: 10000,
                playing: true,
                sampled_at_ms: 900_000,
                ..Playback::default()
            },
            host_sent_ms: 900_100,
            received: now,
            transit_ms: 50,
            clock: None,
        }
    }
    #[test]
    fn confirmed_cross_service_identifier_preserves_host_timing() {
        let now = Instant::now();
        let mut pairs = crate::matching::Pairings::default();
        pairs.set_room("room");
        pairs
            .remember(
                "youtube:abcdefghijk",
                "spotify:track:aaaaaaaaaaaaaaaaaaaaaa",
            )
            .unwrap();
        let mut remote = target(now);
        remote.playback.uri = "youtube:abcdefghijk".into();
        remote.playback.uri = pairs.get(&remote.playback.uri).unwrap().into();
        let mut follower = Follower::default();
        assert_eq!(
            follower.plan(
                &remote,
                Local {
                    uri: "",
                    position_ms: 0,
                    playing: false,
                    ready: true
                },
                now
            ),
            Some(Correction::Load {
                uri: "spotify:track:aaaaaaaaaaaaaaaaaaaaaa".into(),
                position_ms: 10150,
                playing: true
            })
        );
        assert_eq!(remote.position(now + Duration::from_secs(2)), 12150);
        remote.playback.playing = false;
        assert_eq!(remote.position(now + Duration::from_secs(2)), 10000);
    }
    #[test]
    fn follows_track_without_requiring_matching_machine_clocks() {
        let now = Instant::now();
        let mut follower = Follower::default();
        let remote = target(now);
        assert_eq!(
            follower.plan(
                &remote,
                Local {
                    uri: "",
                    position_ms: 0,
                    playing: false,
                    ready: true
                },
                now
            ),
            Some(Correction::Load {
                uri: remote.playback.uri.clone(),
                position_ms: 10150,
                playing: true
            })
        );
        assert_eq!(
            follower.plan(
                &remote,
                Local {
                    uri: "",
                    position_ms: 0,
                    playing: false,
                    ready: true
                },
                now + Duration::from_secs(1)
            ),
            None
        );
    }
    #[test]
    fn corrects_drift_and_pause_but_ignores_stale_or_small_differences() {
        let now = Instant::now();
        let remote = target(now);
        assert_eq!(
            Follower::default().plan(
                &remote,
                Local {
                    uri: &remote.playback.uri,
                    position_ms: 10000,
                    playing: true,
                    ready: true
                },
                now
            ),
            None
        );
        assert_eq!(
            Follower::default().plan(
                &remote,
                Local {
                    uri: &remote.playback.uri,
                    position_ms: 5000,
                    playing: true,
                    ready: true
                },
                now
            ),
            Some(Correction::Seek(10150))
        );
        let mut paused = target(now);
        paused.playback.playing = false;
        assert_eq!(
            Follower::default().plan(
                &paused,
                Local {
                    uri: &paused.playback.uri,
                    position_ms: 10000,
                    playing: true,
                    ready: true
                },
                now
            ),
            Some(Correction::Pause)
        );
        assert_eq!(
            Follower::default().plan(
                &remote,
                Local {
                    uri: "",
                    position_ms: 0,
                    playing: false,
                    ready: true
                },
                now + Duration::from_secs(6)
            ),
            None
        );
    }

    #[test]
    fn queue_updates_cannot_revive_an_old_playback_sample() {
        let now = Instant::now();
        let mut remote = target(now);
        remote.host_sent_ms = remote.playback.sampled_at_ms + 6000;
        assert!(remote.stale(now));
        assert_eq!(
            Follower::default().plan(
                &remote,
                Local {
                    uri: "",
                    position_ms: 0,
                    playing: false,
                    ready: true,
                },
                now
            ),
            None
        );
        remote.host_sent_ms = remote.playback.sampled_at_ms + 4500;
        assert!(!remote.stale(now));
        assert!(remote.stale(now + Duration::from_millis(501)));
    }

    #[test]
    fn bounds_failed_loads_and_allows_a_new_track() {
        let now = Instant::now();
        let mut follower = Follower::default();
        for attempt in 0..3 {
            let tick = now + Duration::from_secs(attempt * 3);
            assert!(matches!(
                follower.plan(
                    &target(tick),
                    Local {
                        uri: "",
                        position_ms: 0,
                        playing: false,
                        ready: true,
                    },
                    tick
                ),
                Some(Correction::Load { .. })
            ));
        }
        let tick = now + Duration::from_secs(9);
        let mut remote = target(tick);
        assert!(follower.exhausted(&remote.playback.uri, tick));
        assert_eq!(
            follower.plan(
                &remote,
                Local {
                    uri: "",
                    position_ms: 0,
                    playing: false,
                    ready: true,
                },
                tick
            ),
            None
        );
        remote.playback.uri = "spotify:track:3JRo0gjbX4GrCqBYdRohoo".into();
        assert!(matches!(
            follower.plan(
                &remote,
                Local {
                    uri: "",
                    position_ms: 0,
                    playing: false,
                    ready: true,
                },
                tick
            ),
            Some(Correction::Load { .. })
        ));
    }

    #[test]
    fn scheduled_starts_wait_for_buffering_then_play_together() {
        let now = Instant::now();
        let uri = "spotify:track:2JRo0gjbX4GrCqBYdRohoo";
        let mut follower = Follower::default();
        let mut remote = target(now);
        remote.playback.preparing = true;
        remote.playback.playing = false;
        remote.playback.play_id = 4;
        remote.playback.position_ms = 0;
        let idle = Local {
            uri: "",
            position_ms: 0,
            playing: false,
            ready: true,
        };
        assert_eq!(
            follower.plan(&remote, idle, now),
            Some(Correction::Load {
                uri: uri.into(),
                position_ms: 0,
                playing: false
            })
        );
        let loaded = || Local {
            uri,
            position_ms: 0,
            playing: false,
            ready: true,
        };
        assert_eq!(
            follower.plan(&remote, loaded(), now),
            Some(Correction::Buffered { play_id: 4 })
        );
        assert_eq!(
            follower.plan(&remote, loaded(), now),
            None,
            "Ack once per play"
        );
        // Host schedules the start 800 ms ahead on its own clock (offset +5 s).
        let local_unix = 1_000_000;
        remote.playback.preparing = false;
        remote.playback.playing = true;
        remote.host_sent_ms = local_unix + 5_000;
        remote.playback.sampled_at_ms = local_unix + 5_800;
        remote.clock = Some((5_000, local_unix));
        assert_eq!(remote.starts_in(now), Some(800));
        assert_eq!(remote.position(now), 0);
        assert!(!remote.stale(now));
        assert_eq!(
            follower.plan(&remote, loaded(), now),
            Some(Correction::PlayAt { delay_ms: 800 })
        );
        assert_eq!(follower.plan(&remote, loaded(), now), None);
        let later = now + Duration::from_millis(1_800);
        assert_eq!(remote.starts_in(later), None);
        assert_eq!(remote.position(later), 1_000);
        let playing = Local {
            uri,
            position_ms: 1_050,
            playing: true,
            ready: true,
        };
        assert_eq!(Follower::drift(&remote, &playing, later), Some(50));
    }
    #[test]
    fn trusted_clock_tightens_the_seek_threshold() {
        let now = Instant::now();
        let remote = target(now);
        let local = |p| Local {
            uri: "spotify:track:2JRo0gjbX4GrCqBYdRohoo",
            position_ms: p,
            playing: true,
            ready: true,
        };
        let mut loose = Follower::default();
        assert_eq!(loose.plan(&remote, local(9_500), now), None);
        let mut tight = Follower {
            threshold_ms: crate::clock::TIGHT_DRIFT_MS,
            ..Follower::default()
        };
        assert_eq!(
            tight.plan(&remote, local(9_500), now),
            Some(Correction::Seek(10_150))
        );
        let mut far = target(now);
        far.playback.sampled_at_ms = far.host_sent_ms + MAX_LEAD_MS + 1;
        assert!(far.stale(now), "Absurd scheduled starts are ignored");
    }
    #[test]
    fn hosts_wait_only_for_connected_followers_and_lead_covers_the_slowest() {
        let member =
            |name: &str, host: bool, connected: bool, following: bool, ready: u64, rtt: u64| {
                Member {
                    id: name.into(),
                    name: name.into(),
                    host,
                    connected,
                    following,
                    ready_play: ready,
                    sync: Some(crate::jam::SyncStats {
                        rtt_ms: rtt,
                        ..Default::default()
                    }),
                    standby: false,
                }
            };
        let members = vec![
            member("Host", true, true, false, 0, 0),
            member("Ready", false, true, true, 9, 40),
            member("Slow", false, true, true, 0, 900),
            member("Away", false, false, true, 0, 5_000),
            member("Solo", false, true, false, 0, 5_000),
        ];
        assert_eq!(unbuffered(&members, 9), vec!["Slow".to_string()]);
        assert_eq!(start_lead_ms(&members), 1_500);
        assert_eq!(start_lead_ms(&members[..1]), 800);
    }
}
