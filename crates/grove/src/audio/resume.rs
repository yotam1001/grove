//! Resume positions for podcast episodes and tracks over 20 minutes.
use serde::{Deserialize, Serialize};

/// Tracks at least this long get a saved position.
pub const LONG_TRACK_MS: u64 = 20 * 60 * 1000;
/// Newest entries kept.
pub const CAPACITY: usize = 100;
/// Positions this close to either end are not worth keeping.
const EDGE_MS: u64 = 15_000;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub uri: String,
    pub title: String,
    pub position_ms: u64,
    pub duration_ms: u64,
}

/// Most recent first.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ResumeStore {
    entries: Vec<Entry>,
}

pub fn qualifies(uri: &str, duration_ms: u64) -> bool {
    uri.starts_with("spotify:episode:")
        || (uri.starts_with("spotify:track:") && duration_ms >= LONG_TRACK_MS)
}

impl ResumeStore {
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn get(&self, uri: &str) -> Option<u64> {
        self.entries
            .iter()
            .find(|e| e.uri == uri)
            .map(|e| e.position_ms)
    }

    /// Record the playhead. Returns true when the stored data changed.
    pub fn record(&mut self, uri: &str, title: &str, position_ms: u64, duration_ms: u64) -> bool {
        if !qualifies(uri, duration_ms) || duration_ms == 0 {
            return false;
        }
        if position_ms < EDGE_MS || position_ms + EDGE_MS >= duration_ms {
            return self.forget(uri);
        }
        let index = self.entries.iter().position(|e| e.uri == uri);
        if let Some(i) = index {
            let entry = &self.entries[i];
            if i == 0 && entry.position_ms.abs_diff(position_ms) < 1000 && entry.title == title {
                return false;
            }
            self.entries.remove(i);
        }
        self.entries.insert(
            0,
            Entry {
                uri: uri.into(),
                title: title.chars().take(200).collect(),
                position_ms,
                duration_ms,
            },
        );
        self.entries.truncate(CAPACITY);
        true
    }

    /// The song finished or the listener started over.
    pub fn forget(&mut self, uri: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.uri != uri);
        self.entries.len() != before
    }

    /// Edit a saved position before playback starts.
    pub fn set(&mut self, index: usize, position_ms: u64) -> bool {
        let Some(entry) = self.entries.get_mut(index) else {
            return false;
        };
        let position = position_ms.min(entry.duration_ms.saturating_sub(EDGE_MS));
        if position == entry.position_ms {
            return false;
        }
        entry.position_ms = position;
        true
    }

    pub fn remove(&mut self, index: usize) -> bool {
        if index < self.entries.len() {
            self.entries.remove(index);
            true
        } else {
            false
        }
    }

    pub fn clear(&mut self) -> bool {
        let changed = !self.entries.is_empty();
        self.entries.clear();
        changed
    }

    pub fn sanitize(&mut self) {
        let mut seen = std::collections::HashSet::new();
        self.entries.retain(|e| {
            crate::spotify::spotify_uri(&e.uri).ok().as_deref() == Some(e.uri.as_str())
                && qualifies(&e.uri, e.duration_ms)
                && e.position_ms < e.duration_ms
                && e.duration_ms <= 24 * 3600 * 1000
                && seen.insert(e.uri.clone())
        });
        for entry in &mut self.entries {
            if entry.title.chars().count() > 200 || entry.title.chars().any(char::is_control) {
                entry.title = entry
                    .title
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(200)
                    .collect();
            }
        }
        self.entries.truncate(CAPACITY);
    }
}

/// "h:mm:ss" or "m:ss".
pub fn format_position(ms: u64) -> String {
    let seconds = ms / 1000;
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

/// Parse "m:ss", "h:mm:ss" or plain minutes into milliseconds.
pub fn parse_position(text: &str) -> Option<u64> {
    let text = text.trim();
    if text.is_empty() || text.len() > 12 {
        return None;
    }
    let parts: Vec<&str> = text.split(':').collect();
    let numbers: Option<Vec<u64>> = parts.iter().map(|p| p.trim().parse().ok()).collect();
    let numbers = numbers?;
    let seconds = match numbers.as_slice() {
        [m] => m * 60,
        [m, s] if *s < 60 => m * 60 + s,
        [h, m, s] if *m < 60 && *s < 60 => h * 3600 + m * 60 + s,
        _ => return None,
    };
    (seconds <= 24 * 3600).then_some(seconds * 1000)
}

#[cfg(test)]
mod tests {
    use super::*;
    const EPISODE: &str = "spotify:episode:2JRo0gjbX4GrCqBYdRohoo";
    const LONG: &str = "spotify:track:2JRo0gjbX4GrCqBYdRohoo";

    #[test]
    fn only_episodes_and_long_tracks_are_remembered() {
        assert!(qualifies(EPISODE, 60_000));
        assert!(qualifies(LONG, LONG_TRACK_MS));
        assert!(!qualifies(LONG, LONG_TRACK_MS - 1));
        assert!(!qualifies(
            "spotify:album:2JRo0gjbX4GrCqBYdRohoo",
            LONG_TRACK_MS
        ));
        let mut store = ResumeStore::default();
        assert!(!store.record(LONG, "Short", 60_000, 200_000));
        assert!(store.record(EPISODE, "Episode", 600_000, 3_600_000));
        assert_eq!(store.get(EPISODE), Some(600_000));
        assert!(
            !store.record(EPISODE, "Episode", 600_400, 3_600_000),
            "Sub-second moves are not writes"
        );
    }

    #[test]
    fn the_ends_clear_positions_and_the_newest_comes_first() {
        let mut store = ResumeStore::default();
        store.record(EPISODE, "Episode", 600_000, 3_600_000);
        store.record(LONG, "Long mix", 1_300_000, 2_400_000);
        assert_eq!(store.entries()[0].uri, LONG);
        store.record(EPISODE, "Episode", 700_000, 3_600_000);
        assert_eq!(store.entries()[0].uri, EPISODE);
        assert!(
            store.record(EPISODE, "Episode", 3_590_000, 3_600_000),
            "Finishing clears it"
        );
        assert_eq!(store.get(EPISODE), None);
        assert!(
            store.record(LONG, "Long mix", 5_000, 2_400_000),
            "Starting over clears it"
        );
        assert!(store.entries().is_empty());
        for i in 0..(CAPACITY + 5) {
            store.record(
                &format!("spotify:episode:{:0>22}", i),
                "E",
                60_000,
                3_600_000,
            );
        }
        assert_eq!(store.entries().len(), CAPACITY);
    }

    #[test]
    fn positions_are_editable_before_playback() {
        let mut store = ResumeStore::default();
        store.record(EPISODE, "Episode", 600_000, 3_600_000);
        assert!(store.set(0, parse_position("45:30").unwrap()));
        assert_eq!(store.get(EPISODE), Some(2_730_000));
        assert!(store.set(0, 99 * 3_600_000));
        assert_eq!(
            store.get(EPISODE),
            Some(3_585_000),
            "Clamped before the end"
        );
        assert!(!store.set(4, 0));
        assert!(store.remove(0));
        assert!(!store.remove(0));
        for (text, ms) in [
            ("1:05", Some(65_000)),
            ("1:02:03", Some(3_723_000)),
            ("12", Some(720_000)),
            ("1:75", None),
            ("x", None),
            ("", None),
            ("1:2:3:4", None),
        ] {
            assert_eq!(parse_position(text), ms, "{text}");
        }
        assert_eq!(format_position(65_000), "1:05");
        assert_eq!(format_position(3_723_000), "1:02:03");
    }

    #[test]
    fn loaded_entries_are_validated() {
        let mut store: ResumeStore = serde_json::from_str(&format!(
            r#"[{{"uri":"{EPISODE}","title":"ok\u0007","position_ms":5,"duration_ms":10}},
               {{"uri":"{EPISODE}","title":"dup","position_ms":5,"duration_ms":10}},
               {{"uri":"https://evil.test","title":"x","position_ms":5,"duration_ms":10}},
               {{"uri":"{LONG}","title":"short","position_ms":5,"duration_ms":10}},
               {{"uri":"{LONG}","title":"past","position_ms":5000000,"duration_ms":2400000}}]"#
        ))
        .unwrap();
        store.sanitize();
        assert_eq!(store.entries().len(), 1);
        assert_eq!(store.entries()[0].title, "ok");
    }
}
