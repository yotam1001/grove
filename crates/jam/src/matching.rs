//! Cross-service recording matching (Tide ↔ Grove).
//!
//! A pair is only used after the listener confirms it, except for an exact ISRC
//! match whose duration also agrees: an ISRC identifies one recording, so that
//! is treated as confirmed. Choices are room-scoped by default; remembering them
//! across rooms is opt-in and bounded.
use std::collections::{BTreeMap, HashMap};

pub const MAX_SESSION_PAIRS: usize = 100;
pub const MAX_REMEMBERED: usize = 200;
/// Same recording on two services can differ by encoder padding or a fade.
pub const DURATION_TOLERANCE_MS: u64 = 3_000;

/// ISO 3901: CC-XXX-YY-NNNNN, written without hyphens (12 characters).
pub fn normalize_isrc(input: &str) -> Option<String> {
    let value: String = input
        .trim()
        .chars()
        .filter(|c| *c != '-')
        .collect::<String>()
        .to_ascii_uppercase();
    let bytes = value.as_bytes();
    (bytes.len() == 12
        && bytes[..2].iter().all(u8::is_ascii_uppercase)
        && bytes[2..5].iter().all(u8::is_ascii_alphanumeric)
        && bytes[5..].iter().all(u8::is_ascii_digit))
    .then_some(value)
}

/// Durations agree within 3 s or 2 % of the longer one. Unknown is not a match.
pub fn duration_matches(a_ms: u64, b_ms: u64) -> bool {
    if a_ms == 0 || b_ms == 0 {
        return false;
    }
    let tolerance = DURATION_TOLERANCE_MS.max(a_ms.max(b_ms) / 50);
    a_ms.abs_diff(b_ms) <= tolerance
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recording {
    pub uri: String,
    pub title: String,
    pub artist: String,
    pub duration_ms: u64,
    pub isrc: String,
}

/// Remove video decorations so a search finds the audio recording.
pub fn search_title(title: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for c in title.chars() {
        match c {
            '(' | '[' | '【' => depth += 1,
            ')' | ']' | '】' => depth = depth.saturating_sub(1),
            '|' if depth == 0 => break,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let mut text = out.split_whitespace().collect::<Vec<_>>().join(" ");
    for noise in [
        " official music video",
        " official video",
        " official audio",
        " lyric video",
        " lyrics",
        " hd",
    ] {
        let cut = text.len().saturating_sub(noise.len());
        if text.is_char_boundary(cut) && text[cut..].eq_ignore_ascii_case(noise) {
            text.truncate(cut);
        }
    }
    text.trim().chars().take(200).collect()
}

fn words(text: &str) -> Vec<String> {
    search_title(text)
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Higher is better. Live/remix/cover versions are penalised unless asked for.
pub fn score(remote: &Recording, candidate: &Recording) -> i64 {
    let mut score = 0i64;
    if let (Some(a), Some(b)) = (
        normalize_isrc(&remote.isrc),
        normalize_isrc(&candidate.isrc),
    ) {
        if a == b {
            score += 1_000;
        }
    }
    if duration_matches(remote.duration_ms, candidate.duration_ms) {
        score += 200;
    } else if remote.duration_ms != 0 && candidate.duration_ms != 0 {
        score -= (remote.duration_ms.abs_diff(candidate.duration_ms) / 1_000).min(300) as i64;
    }
    let wanted = words(&format!("{} {}", remote.title, remote.artist));
    let offered = words(&format!("{} {}", candidate.title, candidate.artist));
    score += 10 * wanted.iter().filter(|w| offered.contains(w)).count() as i64;
    for marker in [
        "live",
        "remix",
        "cover",
        "karaoke",
        "instrumental",
        "sped",
        "slowed",
    ] {
        let asked = wanted.iter().any(|w| w == marker);
        let given = offered.iter().any(|w| w == marker);
        if given && !asked {
            score -= 60;
        }
    }
    score
}

/// Candidates in best-first order (stable for equal scores).
pub fn rank(remote: &Recording, candidates: &[Recording]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..candidates.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(score(remote, &candidates[i])));
    order
}

/// An automatic choice: identical ISRC and agreeing duration (when both known).
pub fn exact(remote: &Recording, candidates: &[Recording]) -> Option<usize> {
    let isrc = normalize_isrc(&remote.isrc)?;
    candidates.iter().position(|c| {
        normalize_isrc(&c.isrc).as_deref() == Some(isrc.as_str())
            && (remote.duration_ms == 0
                || c.duration_ms == 0
                || duration_matches(remote.duration_ms, c.duration_ms))
    })
}

/// Explicit recording pairs. A new room never inherits session choices; opt-in
/// remembered choices (saved by the app) survive rooms.
#[derive(Default)]
pub struct Pairings {
    room: String,
    pairs: HashMap<String, String>,
    remember: bool,
    remembered: BTreeMap<String, String>,
}
impl Pairings {
    pub fn set_room(&mut self, room: &str) {
        if self.room != room {
            self.room = room.into();
            self.pairs.clear();
        }
    }
    pub fn get(&self, remote: &str) -> Option<&str> {
        self.pairs
            .get(remote)
            .or_else(|| self.remember.then(|| self.remembered.get(remote)).flatten())
            .map(String::as_str)
    }
    pub fn remember(&mut self, remote: &str, local: &str) -> Result<(), String> {
        let remote = crate::track_uri(remote)?;
        let local = crate::track_uri(local)?;
        if self.room.is_empty() {
            return Err("Join a jam first.".into());
        }
        if self.pairs.len() >= MAX_SESSION_PAIRS && !self.pairs.contains_key(&remote) {
            self.pairs.clear();
        }
        self.pairs.insert(remote.clone(), local.clone());
        if self.remember {
            if self.remembered.len() >= MAX_REMEMBERED && !self.remembered.contains_key(&remote) {
                if let Some(first) = self.remembered.keys().next().cloned() {
                    self.remembered.remove(&first);
                }
            }
            self.remembered.insert(remote, local);
        }
        Ok(())
    }
    /// Opt in or out of remembering choices. Opting out forgets saved pairs.
    pub fn set_remember(&mut self, enabled: bool) {
        self.remember = enabled;
        if !enabled {
            self.remembered.clear();
        }
    }
    pub fn remembering(&self) -> bool {
        self.remember
    }
    /// Load saved pairs, keeping only valid provider identifiers.
    pub fn import(&mut self, saved: &BTreeMap<String, String>) {
        self.remembered = saved
            .iter()
            .filter(|(remote, local)| {
                crate::track_uri(remote).ok().as_deref() == Some(remote.as_str())
                    && crate::track_uri(local).ok().as_deref() == Some(local.as_str())
            })
            .take(MAX_REMEMBERED)
            .map(|(a, b)| (a.clone(), b.clone()))
            .collect();
    }
    pub fn export(&self) -> BTreeMap<String, String> {
        if self.remember {
            self.remembered.clone()
        } else {
            BTreeMap::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn choices_are_bounded_validated_and_room_scoped() {
        let mut pairs = Pairings::default();
        let youtube = "youtube:abcdefghijk";
        let spotify = "spotify:track:aaaaaaaaaaaaaaaaaaaaaa";
        assert!(pairs.remember(youtube, spotify).is_err());
        pairs.set_room("first");
        pairs.remember(youtube, spotify).unwrap();
        assert_eq!(pairs.get(youtube), Some(spotify));
        assert!(pairs.remember(youtube, "file:///song.mp3").is_err());
        pairs.set_room("second");
        assert!(pairs.get(youtube).is_none());
        for n in 0..150 {
            pairs
                .remember(&format!("youtube:{n:011}"), spotify)
                .unwrap();
        }
        assert!(pairs.pairs.len() <= MAX_SESSION_PAIRS);
        assert!(pairs.export().is_empty(), "Nothing is saved without opt-in");
    }
    #[test]
    fn opt_in_choices_survive_rooms_and_opt_out_forgets() {
        let youtube = "youtube:abcdefghijk";
        let spotify = "spotify:track:aaaaaaaaaaaaaaaaaaaaaa";
        let mut pairs = Pairings::default();
        pairs.set_remember(true);
        pairs.set_room("first");
        pairs.remember(youtube, spotify).unwrap();
        pairs.set_room("second");
        assert_eq!(pairs.get(youtube), Some(spotify));
        let saved = pairs.export();
        let mut restored = Pairings::default();
        let mut tampered = saved.clone();
        tampered.insert("file:///x".into(), spotify.into());
        restored.import(&tampered);
        assert!(
            restored.get(youtube).is_none(),
            "Import alone does not opt in"
        );
        restored.set_remember(true);
        restored.import(&tampered);
        assert_eq!(restored.get(youtube), Some(spotify));
        assert_eq!(restored.export().len(), 1);
        restored.set_remember(false);
        assert!(restored.get(youtube).is_none());
        for n in 0..250 {
            pairs
                .remember(&format!("youtube:{n:011}"), spotify)
                .unwrap();
        }
        assert!(pairs.export().len() <= MAX_REMEMBERED);
    }
    #[test]
    fn isrc_and_duration_pick_the_same_recording() {
        assert_eq!(
            normalize_isrc("us-rc1-76-07839").as_deref(),
            Some("USRC17607839")
        );
        assert!(normalize_isrc("not an isrc").is_none());
        assert!(duration_matches(200_000, 202_500));
        assert!(!duration_matches(200_000, 210_000));
        assert!(duration_matches(600_000, 611_000), "2 % of long tracks");
        assert!(!duration_matches(0, 200_000));
        let remote = Recording {
            uri: "spotify:track:aaaaaaaaaaaaaaaaaaaaaa".into(),
            title: "Forest Floor".into(),
            artist: "Artist".into(),
            duration_ms: 200_000,
            isrc: "USRC17607839".into(),
        };
        let candidates = vec![
            Recording {
                title: "Forest Floor - Live".into(),
                artist: "Artist".into(),
                duration_ms: 260_000,
                ..Recording::default()
            },
            Recording {
                title: "Forest Floor".into(),
                artist: "Artist".into(),
                duration_ms: 201_000,
                isrc: "usrc17607839".into(),
                ..Recording::default()
            },
            Recording {
                title: "Forest Floor".into(),
                artist: "Artist".into(),
                duration_ms: 230_000,
                isrc: "USRC17607839".into(),
                ..Recording::default()
            },
        ];
        assert_eq!(exact(&remote, &candidates), Some(1));
        assert_eq!(rank(&remote, &candidates)[0], 1);
        assert_eq!(*rank(&remote, &candidates).last().unwrap(), 0);
        let youtube = Recording {
            title: "Artist - Forest Floor (Official Video)".into(),
            duration_ms: 201_500,
            ..Recording::default()
        };
        assert!(
            exact(&youtube, &candidates).is_none(),
            "No ISRC: listener confirms"
        );
        assert_eq!(rank(&youtube, &candidates)[0], 1);
    }
    #[test]
    fn video_titles_become_recording_searches() {
        assert_eq!(
            search_title("Artist - Song (Official Music Video) [4K] | Label"),
            "Artist - Song"
        );
        assert_eq!(search_title("Song Official Video"), "Song");
        assert_eq!(search_title("Plain"), "Plain");
    }
}
