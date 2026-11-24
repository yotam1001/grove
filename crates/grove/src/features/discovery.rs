//! Discovery and autoplay. Spotify's recommendation endpoints are closed to new
//! apps, so autoplay continues from the artists of what the listener chose to
//! play (the open playlist, album or artist), or from the Discovery mix when
//! there is no such context. It refills before the rotation runs out and skips
//! songs heard this session, in history, queued or hidden.
use super::{
    browse::{parse_tracks, Want, ARTIST},
    Features, Pending,
};
use crate::{library::Action, GroveWindow};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

/// Refill when fewer unheard songs than this remain in the autoplay rotation.
pub(super) const LOW_WATER: usize = 3;
const REFILL_PAUSE: Duration = Duration::from_secs(90);

impl Features {
    /// The artists of the rows being played become autoplay's seeds.
    pub(super) fn remember_seeds(&mut self, from: Option<usize>) {
        let start = from.unwrap_or_default().min(self.rows.len());
        let mut counts: HashMap<String, usize> = HashMap::new();
        let mut order = vec![];
        for t in self.rows[start..].iter().take(200) {
            let artist = self
                .browse
                .credits
                .get(&t.uri)
                .and_then(|c| c.first().map(|(_, u)| u.clone()))
                .unwrap_or_else(|| t.artist_uri.clone());
            if artist.starts_with("spotify:artist:") {
                let n = counts.entry(artist.clone()).or_default();
                if *n == 0 {
                    order.push(artist);
                }
                *n += 1;
            }
        }
        // Most frequent first; ties keep the list's order.
        order.sort_by_key(|a| std::cmp::Reverse(counts[a]));
        if self.view_page == ARTIST && self.browse.header.uri.starts_with("spotify:artist:") {
            order.retain(|a| *a != self.browse.header.uri);
            order.insert(0, self.browse.header.uri.clone());
        }
        order.truncate(3);
        self.browse.seeds = order;
        self.browse.refill_at = None;
    }

    pub(super) fn autoplay_tick(&mut self, ui: &GroveWindow) {
        let current = ui.get_current_uri();
        if !current.is_empty() && self.browse.heard.len() < 5000 {
            self.browse.heard.insert(current.to_string());
        }
        if !self.prefs.autoplay
            || !ui.get_library_ready()
            || ui.get_jam_connected()
            || ui.get_local_active()
            || current.is_empty()
            || !self.queue.is_empty()
            || self
                .browse
                .refill_at
                .is_some_and(|at| at.elapsed() < REFILL_PAUSE)
            || self
                .pending
                .values()
                .any(|p| matches!(p, Pending::Browse(Want::Refill)))
        {
            return;
        }
        if self.unheard() >= LOW_WATER {
            return;
        }
        let mut artists = self.browse.seeds.clone();
        if artists.is_empty() {
            artists = self
                .browse
                .credits
                .get(current.as_str())
                .map(|c| c.iter().map(|(_, u)| u.clone()).take(2).collect())
                .unwrap_or_default();
        }
        let action = if artists.is_empty() {
            Action::Mix
        } else {
            Action::Continue { artists }
        };
        self.browse.refill_at = Some(Instant::now());
        self.request(action, Pending::Browse(Want::Refill));
    }

    /// Unheard, unhidden songs left in the autoplay rotation.
    pub(super) fn unheard(&self) -> usize {
        self.rotation
            .iter()
            .filter(|t| !self.browse.heard.contains(&t.uri) && !self.prefs.hidden.contains(&t.uri))
            .count()
    }

    pub(super) fn refill_loaded(&mut self, data: &Value) {
        if data["kind"] == "mix" && data["cached"] != true {
            self.browse.cache.put("mix", data);
        }
        let (tracks, _) = parse_tracks(&mut self.browse, data);
        let known: HashSet<&str> = self
            .history
            .iter()
            .chain(self.browse.remote_recent.iter())
            .chain(self.queue.iter())
            .map(|t| t.uri.as_str())
            .chain(self.browse.heard.iter().map(String::as_str))
            .collect();
        let fresh: Vec<_> = tracks
            .into_iter()
            .filter(|t| !known.contains(t.uri.as_str()) && !self.prefs.hidden.contains(&t.uri))
            .take(40)
            .collect();
        if fresh.is_empty() {
            return;
        }
        let uris: HashSet<String> = fresh.iter().map(|t| t.uri.clone()).collect();
        self.rotation.retain(|t| !uris.contains(&t.uri));
        let mut rotation = fresh;
        rotation.append(&mut self.rotation);
        rotation.truncate(200);
        self.rotation = rotation;
    }
}
