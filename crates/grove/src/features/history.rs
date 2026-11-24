//! Session listening history: a song counts after 30 seconds of actual playback.
use super::Features;
use crate::state;
use std::time::{Duration, Instant};

impl Features {
    pub(super) fn record_listening(
        &mut self,
        state: &state::State,
        elapsed: Duration,
        now: Instant,
    ) {
        if state.playing
            && state.active
            && !state.loading
            && self.prefs.local_history
            && !state.track.uri.is_empty()
        {
            self.listened += elapsed;
            if self.listened >= Duration::from_secs(30) && self.played.is_none() {
                self.history.insert(0, state.track.clone());
                self.history.truncate(100);
                self.played = Some((state.track.uri.clone(), now));
            }
        }
    }
}
