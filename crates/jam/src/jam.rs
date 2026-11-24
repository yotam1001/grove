//! Host-authoritative jam state. Only metadata is shared; no credentials or audio.
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

pub const MAX_MEMBERS: usize = 16;
pub const MAX_TRACKS: usize = 100;
/// Single additions per guest still waiting in the queue.
pub const MAX_GUEST_SINGLES: usize = 20;
/// Waiting entries per guest, counting album/playlist additions.
pub const MAX_GUEST_ENTRIES: usize = 50;
/// A disconnected member keeps their seat this long (Tide's relay uses 30 s).
pub const AWAY_GRACE: Duration = Duration::from_secs(30);

pub fn id() -> String {
    oauth2::CsrfToken::new_random().secret().clone()
}

fn yes() -> bool {
    true
}

/// What a guest reports about its own sync, visible to the host.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SyncStats {
    pub offset_ms: i64,
    pub rtt_ms: u64,
    pub jitter_ms: u64,
    #[serde(default)]
    pub drift_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Member {
    pub id: String,
    pub name: String,
    pub host: bool,
    #[serde(default = "yes")]
    pub connected: bool,
    #[serde(default)]
    pub following: bool,
    #[serde(default)]
    pub ready_play: u64,
    #[serde(default)]
    pub sync: Option<SyncStats>,
    #[serde(default)]
    pub standby: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub uri: String,
    pub title: String,
    pub member_id: String,
    pub member_name: String,
    pub delivery: Delivery,
    #[serde(default)]
    pub isrc: String,
    #[serde(default)]
    pub duration_ms: u64,
    /// Album or playlist name when the entry came from a collection.
    #[serde(default)]
    pub group: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    Waiting,
    Sending,
    Unconfirmed,
}

/// The guest who takes over if the host disappears (not when it ends the room).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Successor {
    pub member_id: String,
    pub address: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub room_id: String,
    pub revision: u64,
    pub members: Vec<Member>,
    pub queue: Vec<Entry>,
    pub closed: bool,
    pub playback: Option<Playback>,
    #[serde(default)]
    pub auto_advance: bool,
    #[serde(default)]
    pub skip_votes: Vec<String>,
    #[serde(default)]
    pub successor: Option<Successor>,
    /// "nearby", "tide-relay" or "relay"; informational only.
    #[serde(default)]
    pub origin: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Playback {
    pub uri: String,
    pub title: String,
    pub artist: String,
    pub position_ms: u64,
    pub playing: bool,
    /// Host clock. A sample in the future is a scheduled simultaneous start.
    pub sampled_at_ms: u64,
    /// Changes whenever the host starts a song (Tide's `play_id`).
    #[serde(default)]
    pub play_id: u64,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub isrc: String,
    /// Load paused and acknowledge; the host will schedule the start.
    #[serde(default)]
    pub preparing: bool,
}

/// One track offered to the shared queue.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub uri: String,
    pub title: String,
    #[serde(default)]
    pub isrc: String,
    #[serde(default)]
    pub duration_ms: u64,
}

pub fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub struct Room {
    pub snapshot: Snapshot,
    operations: HashSet<(String, String)>,
    away: HashMap<String, Instant>,
}

impl Room {
    pub fn new(name: &str) -> Result<Self, String> {
        Ok(Self {
            snapshot: Snapshot {
                room_id: id(),
                revision: 1,
                members: vec![Member {
                    id: id(),
                    name: clean_name(name)?,
                    host: true,
                    connected: true,
                    following: false,
                    ready_play: 0,
                    sync: None,
                    standby: false,
                }],
                queue: vec![],
                closed: false,
                playback: None,
                auto_advance: true,
                skip_votes: vec![],
                successor: None,
                origin: "nearby".into(),
            },
            operations: HashSet::new(),
            away: HashMap::new(),
        })
    }
    /// Host migration: the successor continues the room from the last snapshot.
    /// Everyone else is marked away until they reconnect with their member ID.
    pub fn restore(mut snapshot: Snapshot, new_host: &str, now: Instant) -> Result<Self, String> {
        snapshot.members.retain(|m| !m.host || m.id == new_host);
        let index = snapshot
            .members
            .iter()
            .position(|m| m.id == new_host)
            .ok_or("The jam could not be handed over.")?;
        let mut host = snapshot.members.remove(index);
        host.host = true;
        host.connected = true;
        host.standby = false;
        host.sync = None;
        let mut away = HashMap::new();
        for member in &mut snapshot.members {
            member.connected = false;
            member.sync = None;
            away.insert(member.id.clone(), now);
        }
        snapshot.members.insert(0, host);
        snapshot.successor = None;
        snapshot.skip_votes.clear();
        snapshot.closed = false;
        for entry in &mut snapshot.queue {
            if entry.delivery == Delivery::Sending {
                entry.delivery = Delivery::Unconfirmed;
            }
        }
        snapshot.revision += 1;
        Ok(Self {
            snapshot,
            operations: HashSet::new(),
            away,
        })
    }
    pub fn host_id(&self) -> &str {
        &self.snapshot.members[0].id
    }
    pub fn join(&mut self, name: &str) -> Result<String, String> {
        self.join_or_resume(name, None, false)
    }
    /// Rejoining with a known member ID keeps the seat, name and contributions.
    pub fn join_or_resume(
        &mut self,
        name: &str,
        resume: Option<&str>,
        standby: bool,
    ) -> Result<String, String> {
        if self.snapshot.closed {
            return Err("This jam has ended.".into());
        }
        let name = clean_name(name)?;
        if let Some(previous) = resume.filter(|id| *id != self.host_id()) {
            if let Some(member) = self.snapshot.members.iter_mut().find(|m| m.id == previous) {
                member.connected = true;
                member.standby = standby;
                self.away.remove(previous);
                self.snapshot.revision += 1;
                return Ok(previous.to_owned());
            }
        }
        if self.snapshot.members.len() >= MAX_MEMBERS {
            return Err("This jam is full (16 people).".into());
        }
        let member = Member {
            id: id(),
            name,
            host: false,
            connected: true,
            following: true,
            ready_play: 0,
            sync: None,
            standby,
        };
        let id = member.id.clone();
        self.snapshot.members.push(member);
        self.snapshot.revision += 1;
        Ok(id)
    }
    pub fn leave(&mut self, member: &str) {
        if member == self.host_id() {
            return;
        }
        let before = self.snapshot.members.len();
        self.snapshot.members.retain(|m| m.id != member);
        self.away.remove(member);
        if self.snapshot.members.len() != before {
            self.forget(member);
            self.snapshot.revision += 1;
        }
    }
    /// A dropped connection: keep the seat for [`AWAY_GRACE`].
    pub fn away(&mut self, member: &str, now: Instant) {
        if let Some(m) = self
            .snapshot
            .members
            .iter_mut()
            .find(|m| m.id == member && !m.host)
        {
            if m.connected {
                m.connected = false;
                m.sync = None;
                self.away.insert(member.into(), now);
                self.forget(member);
                self.snapshot.revision += 1;
            }
        }
    }
    /// Drop members who stayed away too long. Returns whether anything changed.
    pub fn expire(&mut self, now: Instant) -> bool {
        let gone: Vec<String> = self
            .away
            .iter()
            .filter(|(_, since)| now.saturating_duration_since(**since) >= AWAY_GRACE)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &gone {
            self.leave(id);
        }
        !gone.is_empty()
    }
    fn forget(&mut self, member: &str) {
        self.snapshot.skip_votes.retain(|id| id != member);
        if self
            .snapshot
            .successor
            .as_ref()
            .is_some_and(|s| s.member_id == member)
        {
            self.snapshot.successor = None;
        }
    }
    fn member(&self, id: &str) -> Result<&Member, String> {
        if self.snapshot.closed {
            return Err("This jam has ended.".into());
        }
        self.snapshot
            .members
            .iter()
            .find(|m| m.id == id)
            .ok_or("Join the jam first.".into())
    }
    pub fn add(
        &mut self,
        member: &str,
        operation: &str,
        input: &str,
        title: &str,
    ) -> Result<(), String> {
        self.add_items(
            member,
            operation,
            "",
            vec![Item {
                uri: input.into(),
                title: title.into(),
                ..Item::default()
            }],
        )
    }
    /// Add one track, or an album/playlist's tracks under a group name.
    pub fn add_items(
        &mut self,
        member: &str,
        operation: &str,
        group: &str,
        items: Vec<Item>,
    ) -> Result<(), String> {
        let person = self.member(member)?.clone();
        if self
            .operations
            .contains(&(member.to_owned(), operation.to_owned()))
        {
            return Ok(());
        }
        if operation.is_empty() || operation.len() > 128 {
            return Err("Invalid command ID.".into());
        }
        if self.operations.len() >= 4096 {
            return Err("This jam reached its command limit. Start a new room.".into());
        }
        if items.is_empty() || items.len() > MAX_TRACKS {
            return Err("Add between 1 and 100 tracks at a time.".into());
        }
        let group = clean_text(group, 120, "Album and playlist names")?;
        if self.snapshot.queue.len() >= MAX_TRACKS {
            return Err("The shared queue is full (100 tracks).".into());
        }
        let mine = self
            .snapshot
            .queue
            .iter()
            .filter(|e| e.member_id == member)
            .count();
        if !person.host {
            if group.is_empty() && mine >= MAX_GUEST_SINGLES {
                return Err("You can have up to 20 tracks waiting in this jam.".into());
            }
            if mine >= MAX_GUEST_ENTRIES {
                return Err("You can have up to 50 tracks waiting in this jam.".into());
            }
        }
        let room = MAX_TRACKS - self.snapshot.queue.len();
        let allowance = if person.host {
            room
        } else {
            room.min(MAX_GUEST_ENTRIES - mine)
        };
        let mut entries = Vec::new();
        for item in items.into_iter().take(allowance) {
            let uri = crate::track_uri(&item.uri)?;
            if !uri.starts_with("spotify:track:") {
                return Err("Add an individual Spotify track to the jam.".into());
            }
            let title = clean_text(&item.title, 120, "Track names")?;
            let isrc = crate::matching::normalize_isrc(&item.isrc).unwrap_or_default();
            entries.push(Entry {
                id: id(),
                uri,
                title: if title.is_empty() {
                    "Spotify track".into()
                } else {
                    title
                },
                member_id: member.into(),
                member_name: person.name.clone(),
                delivery: Delivery::Waiting,
                isrc,
                duration_ms: item.duration_ms.min(86_400_000),
                group: group.clone(),
            });
        }
        self.snapshot.queue.extend(entries);
        self.operations.insert((member.into(), operation.into()));
        self.snapshot.revision += 1;
        Ok(())
    }
    pub fn remove(&mut self, member: &str, entry: &str) -> Result<(), String> {
        let host = self.member(member)?.host;
        let index = self
            .snapshot
            .queue
            .iter()
            .position(|e| e.id == entry)
            .ok_or("That track is no longer in the shared queue.")?;
        let item = &self.snapshot.queue[index];
        if !host && item.member_id != member {
            return Err("Guests can remove only their own tracks.".into());
        }
        if item.delivery == Delivery::Sending {
            return Err("Wait for Spotify to confirm this track.".into());
        }
        self.snapshot.queue.remove(index);
        self.snapshot.revision += 1;
        Ok(())
    }
    pub fn stage_next(&mut self) -> Result<Entry, String> {
        if self.snapshot.closed {
            return Err("This jam has ended.".into());
        }
        if self
            .snapshot
            .queue
            .iter()
            .any(|e| e.delivery == Delivery::Sending)
        {
            return Err("A track is already being sent to Spotify.".into());
        }
        let first = self
            .snapshot
            .queue
            .first_mut()
            .ok_or("Add a track to the shared queue first.")?;
        if first.delivery == Delivery::Unconfirmed {
            return Err("Check Spotify’s queue, then remove the unconfirmed entry. It will not be retried automatically.".into());
        }
        first.delivery = Delivery::Sending;
        let entry = first.clone();
        self.snapshot.revision += 1;
        Ok(entry)
    }
    pub fn complete(&mut self, entry: &str, success: bool) {
        let Some(index) = self
            .snapshot
            .queue
            .iter()
            .position(|e| e.id == entry && e.delivery == Delivery::Sending)
        else {
            return;
        };
        if success {
            self.snapshot.queue.remove(index);
        } else {
            self.snapshot.queue[index].delivery = Delivery::Unconfirmed;
        }
        self.snapshot.revision += 1;
    }
    /// Guests vote to skip the exact play they hear. A majority of connected
    /// guests skips (one guest alone is a majority of one, like Tide's request).
    pub fn vote_skip(&mut self, member: &str, play_id: u64) -> Result<bool, String> {
        let person = self.member(member)?;
        if person.host {
            return Err("The host skips directly.".into());
        }
        let current = self.snapshot.playback.as_ref().map(|p| p.play_id);
        if play_id == 0 || current != Some(play_id) {
            return Err("That song has already changed.".into());
        }
        if !self.snapshot.skip_votes.iter().any(|id| id == member) {
            self.snapshot.skip_votes.push(member.into());
            self.snapshot.revision += 1;
        }
        Ok(self.skip_majority())
    }
    pub fn skip_majority(&self) -> bool {
        let guests = self
            .snapshot
            .members
            .iter()
            .filter(|m| !m.host && m.connected)
            .count();
        guests > 0 && self.snapshot.skip_votes.len() * 2 > guests
    }
    /// Publish the host's playback frame; a new play clears votes and acks.
    pub fn set_playback(&mut self, playback: Option<Playback>) {
        let previous = self.snapshot.playback.as_ref().map(|p| p.play_id);
        let next = playback.as_ref().map(|p| p.play_id);
        if previous != next {
            self.snapshot.skip_votes.clear();
        }
        self.snapshot.playback = playback;
        self.snapshot.revision += 1;
    }
    pub fn buffered(&mut self, member: &str, play_id: u64) {
        if let Some(m) = self
            .snapshot
            .members
            .iter_mut()
            .find(|m| m.id == member && !m.host)
        {
            if m.ready_play != play_id {
                m.ready_play = play_id;
                self.snapshot.revision += 1;
            }
        }
    }
    /// Connected followers that have not yet buffered `play_id`.
    pub fn waiting_for(&self, play_id: u64) -> Vec<String> {
        crate::sync::unbuffered(&self.snapshot.members, play_id)
    }
    pub fn report(&mut self, member: &str, stats: SyncStats, following: bool) {
        if let Some(m) = self
            .snapshot
            .members
            .iter_mut()
            .find(|m| m.id == member && !m.host)
        {
            m.sync = Some(stats);
            m.following = following;
            self.snapshot.revision += 1;
        }
    }
    pub fn set_successor(&mut self, successor: Option<Successor>) {
        if self.snapshot.successor != successor {
            self.snapshot.successor = successor;
            self.snapshot.revision += 1;
        }
    }
    /// The first connected guest that offered to stand by.
    pub fn successor_candidate(&self) -> Option<String> {
        self.snapshot
            .members
            .iter()
            .find(|m| !m.host && m.connected && m.standby)
            .map(|m| m.id.clone())
    }
}

fn clean_text(text: &str, limit: usize, what: &str) -> Result<String, String> {
    let text = text.trim();
    if text.chars().count() > limit || text.chars().any(char::is_control) {
        return Err(format!(
            "{what} must be at most {limit} characters without control characters."
        ));
    }
    Ok(text.into())
}

fn clean_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 32 || name.chars().any(char::is_control) {
        return Err("Enter a display name of 1–32 characters.".into());
    }
    Ok(name.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    const TRACK: &str = "spotify:track:2JRo0gjbX4GrCqBYdRohoo";
    #[test]
    fn shared_queue_serializes_contributions_and_retry_is_idempotent() {
        let mut room = Room::new("Host").unwrap();
        let guest = room.join("Guest").unwrap();
        room.add(&guest, "one", TRACK, "First").unwrap();
        room.add(&guest, "one", TRACK, "First").unwrap();
        let host = room.host_id().to_owned();
        room.add(&host, "two", TRACK, "Second").unwrap();
        assert_eq!(room.snapshot.queue.len(), 2);
        assert_eq!(room.snapshot.queue[0].title, "First");
        assert_eq!(room.snapshot.revision, 4);
    }
    #[test]
    fn guest_ownership_and_delivery_confirmation_are_enforced() {
        let mut room = Room::new("Host").unwrap();
        let a = room.join("A").unwrap();
        let b = room.join("B").unwrap();
        room.add(&a, "one", TRACK, "Track").unwrap();
        let entry = room.snapshot.queue[0].id.clone();
        assert!(room.remove(&b, &entry).is_err());
        room.stage_next().unwrap();
        assert!(room.remove(&a, &entry).is_err());
        assert!(room.stage_next().is_err());
        room.complete(&entry, false);
        assert_eq!(room.snapshot.queue.len(), 1);
        assert!(room.stage_next().is_err());
        let host = room.host_id().to_owned();
        room.remove(&host, &entry).unwrap();
        room.add(&b, "two", TRACK, "Next").unwrap();
        let staged = room.stage_next().unwrap();
        room.complete("stale-entry", true);
        assert_eq!(room.snapshot.queue.len(), 1);
        room.complete(&staged.id, true);
        assert!(room.snapshot.queue.is_empty());
    }
    #[test]
    fn bounded_members_and_guest_contributions() {
        let mut room = Room::new("Host").unwrap();
        let guest = room.join("Guest").unwrap();
        for n in 0..20 {
            room.add(&guest, &format!("op{n}"), TRACK, "").unwrap();
        }
        assert!(room.add(&guest, "overflow", TRACK, "").is_err());
        while room.snapshot.members.len() < MAX_MEMBERS {
            room.join("Guest").unwrap();
        }
        assert!(room.join("Overflow").is_err());
        assert!(Room::new("\n").is_err());
        assert!(room
            .add(&guest, "bad", "spotify:album:2JRo0gjbX4GrCqBYdRohoo", "")
            .is_err());
    }
    #[test]
    fn collections_are_grouped_and_bounded_per_guest() {
        let mut room = Room::new("Host").unwrap();
        let guest = room.join("Guest").unwrap();
        let items: Vec<Item> = (0..80)
            .map(|n| Item {
                uri: TRACK.into(),
                title: format!("Song {n}"),
                isrc: "usrc17607839".into(),
                duration_ms: 200_000,
            })
            .collect();
        room.add_items(&guest, "album", "An album", items.clone())
            .unwrap();
        assert_eq!(room.snapshot.queue.len(), MAX_GUEST_ENTRIES);
        assert_eq!(room.snapshot.queue[0].group, "An album");
        assert_eq!(room.snapshot.queue[0].isrc, "USRC17607839");
        assert!(room.add_items(&guest, "more", "Again", items).is_err());
        let host = room.host_id().to_owned();
        room.add_items(
            &host,
            "host",
            "Host list",
            vec![
                Item {
                    uri: TRACK.into(),
                    ..Item::default()
                };
                80
            ],
        )
        .unwrap();
        assert_eq!(room.snapshot.queue.len(), MAX_TRACKS);
    }
    #[test]
    fn away_members_resume_their_seat_or_expire() {
        let now = Instant::now();
        let mut room = Room::new("Host").unwrap();
        let guest = room.join("Guest").unwrap();
        room.add(&guest, "one", TRACK, "Mine").unwrap();
        room.away(&guest, now);
        assert!(!room.snapshot.members[1].connected);
        assert!(!room.expire(now + Duration::from_secs(5)));
        let again = room.join_or_resume("Guest", Some(&guest), false).unwrap();
        assert_eq!(again, guest);
        assert!(room.snapshot.members[1].connected);
        room.away(&guest, now);
        assert!(room.expire(now + AWAY_GRACE));
        assert_eq!(room.snapshot.members.len(), 1);
        assert_eq!(
            room.snapshot.queue.len(),
            1,
            "Contributions outlive the member"
        );
        // Unknown or host IDs never grant someone else's seat.
        let host = room.host_id().to_owned();
        let fresh = room.join_or_resume("Other", Some(&host), false).unwrap();
        assert_ne!(fresh, host);
    }
    #[test]
    fn skip_votes_need_a_majority_of_the_current_play() {
        let mut room = Room::new("Host").unwrap();
        let a = room.join("A").unwrap();
        let b = room.join("B").unwrap();
        let c = room.join("C").unwrap();
        room.set_playback(Some(Playback {
            uri: TRACK.into(),
            play_id: 7,
            ..Playback::default()
        }));
        assert!(room.vote_skip(&a, 6).is_err());
        assert!(!room.vote_skip(&a, 7).unwrap());
        assert!(!room.vote_skip(&a, 7).unwrap(), "Repeated votes count once");
        assert!(room.vote_skip(&b, 7).unwrap());
        room.set_playback(Some(Playback {
            uri: TRACK.into(),
            play_id: 8,
            ..Playback::default()
        }));
        assert!(room.snapshot.skip_votes.is_empty());
        room.leave(&b);
        room.leave(&c);
        assert!(room.vote_skip(&a, 8).unwrap(), "A lone guest is a majority");
        let host = room.host_id().to_owned();
        assert!(room.vote_skip(&host, 8).is_err());
    }
    #[test]
    fn buffering_acks_and_migration_keep_the_room() {
        let now = Instant::now();
        let mut room = Room::new("Host").unwrap();
        let a = room.join_or_resume("A", None, true).unwrap();
        let b = room.join("B").unwrap();
        room.add(&b, "one", TRACK, "Queued").unwrap();
        assert_eq!(room.waiting_for(3), vec!["A".to_string(), "B".to_string()]);
        room.buffered(&a, 3);
        assert_eq!(room.waiting_for(3), vec!["B".to_string()]);
        room.report(&b, SyncStats::default(), false);
        assert!(
            room.waiting_for(3).is_empty(),
            "Non-followers never block a start"
        );
        assert_eq!(room.successor_candidate(), Some(a.clone()));
        room.set_successor(Some(Successor {
            member_id: a.clone(),
            address: "192.168.1.20:5000".into(),
        }));
        room.stage_next().unwrap();
        let restored = Room::restore(room.snapshot.clone(), &a, now).unwrap();
        assert_eq!(restored.host_id(), a);
        assert_eq!(restored.snapshot.room_id, room.snapshot.room_id);
        assert_eq!(restored.snapshot.members.len(), 2, "Old host is dropped");
        assert!(!restored.snapshot.members[1].connected);
        assert_eq!(restored.snapshot.queue[0].delivery, Delivery::Unconfirmed);
        assert!(restored.snapshot.successor.is_none());
    }
}
