//! Jam song matching: pairing a Tide host's YouTube song with a confirmed Spotify
//! recording (ISRC first, then duration-aware ranking), opt-in remembered
//! choices, the host's ISRC lookup, and resolving shared songs, albums and
//! playlists for the shared queue.
use super::{Features, Pending};
use crate::{
    library::Action,
    spotify,
    state::{self, Track},
    GroveFeatureTrack, GroveWindow,
};
use serde_json::Value;
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
};
use tide_jam::{
    jam::Item,
    matching::{self, Recording},
};

/// Per-session matching state that is not part of the room.
#[derive(Default)]
pub struct JamState {
    /// Spotify track → (ISRC, duration) for the host's published frames.
    isrc: HashMap<String, (String, u64)>,
    isrc_pending: HashSet<String>,
    remote: Recording,
    candidates: Vec<Recording>,
}
impl JamState {
    pub(super) fn isrc_loaded(&mut self, data: &Value, uri: String) {
        self.isrc_pending.remove(&uri);
        let track = &data["track"];
        if track["uri"].as_str() != Some(uri.as_str()) {
            return;
        }
        let isrc = track["external_ids"]["isrc"]
            .as_str()
            .and_then(matching::normalize_isrc)
            .unwrap_or_default();
        let duration = track["duration_ms"].as_u64().unwrap_or(0);
        if self.isrc.len() >= 200 {
            self.isrc.clear();
        }
        self.isrc.insert(uri, (isrc, duration));
    }
    pub(super) fn isrc_failed(&mut self, pending: &Pending) {
        if let Pending::JamIsrc { uri } = pending {
            self.isrc_pending.remove(uri);
            self.isrc.insert(uri.clone(), (String::new(), 0));
        }
    }
}

/// A Spotify Web API track as a matching candidate.
fn recording(value: &Value) -> Option<Recording> {
    let uri = value["uri"].as_str()?;
    if !uri.starts_with("spotify:track:") {
        return None;
    }
    let artist = value["artists"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a["name"].as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Some(Recording {
        uri: uri.into(),
        title: value["name"].as_str().unwrap_or_default().into(),
        artist,
        duration_ms: value["duration_ms"].as_u64().unwrap_or(0),
        isrc: value["external_ids"]["isrc"]
            .as_str()
            .and_then(matching::normalize_isrc)
            .unwrap_or_default(),
    })
}

pub(super) fn bind(ui: &GroveWindow, this: &Rc<RefCell<Features>>) {
    {
        let mut f = this.borrow_mut();
        let remember = f.prefs.remember_jam_matches;
        let saved = f.prefs.jam_matches.clone();
        f.jam_pairs.set_remember(remember);
        f.jam_pairs.import(&saved);
        ui.set_jam_remember_matches(remember);
    }
    let f = this.clone();
    let weak = ui.as_weak();
    ui.on_jam_pair(move |index| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let mut f = f.borrow_mut();
        if !ui.get_jam_connected()
            || ui.get_jam_host()
            || !ui.get_jam_following()
            || f.jam_remote.is_empty()
        {
            return;
        }
        let uri = if index >= 0 {
            f.jam_candidates.get(index as usize).map(|t| t.uri.clone())
        } else {
            spotify::spotify_uri(&ui.get_jam_match_link()).ok()
        };
        let Some(uri) = uri.filter(|u| u.starts_with("spotify:track:")) else {
            ui.set_jam_message("Choose a song or paste its Spotify track link.".into());
            return;
        };
        let remote = f.jam_remote.clone();
        if f.jam_pairs.remember(&remote, &uri).is_ok() {
            f.persist_matches(&ui);
            ui.set_jam_match_confirmed(true);
            ui.set_jam_message(
                "Song matched for this jam. Choose another if the recording differs.".into(),
            );
        }
    });
}

impl Features {
    pub fn reset_jam(&mut self, ui: &GroveWindow) {
        self.jam_pairs = Default::default();
        self.jam_pairs.set_remember(self.prefs.remember_jam_matches);
        self.jam_pairs.import(&self.prefs.jam_matches);
        self.jam_remote.clear();
        self.jam_candidates.clear();
        self.jam.remote = Recording::default();
        self.jam.candidates.clear();
        self.pending
            .retain(|_, p| !matches!(p, Pending::JamMatch { .. } | Pending::JamCollection { .. }));
        ui.set_jam_matching(false);
        ui.set_jam_tide_host(false);
        ui.set_jam_match_confirmed(false);
        ui.set_jam_matches(ModelRc::default());
        ui.set_jam_match_link("".into());
    }
    fn persist_matches(&mut self, ui: &GroveWindow) {
        if self.jam_pairs.remembering() {
            self.prefs.jam_matches = self.jam_pairs.export();
            self.save(ui);
        }
    }
    /// Opt in or out of remembering recording choices across jams.
    pub fn set_remember_jam(&mut self, ui: &GroveWindow, enabled: bool) {
        self.jam_pairs.set_remember(enabled);
        self.prefs.remember_jam_matches = enabled;
        self.prefs.jam_matches = self.jam_pairs.export();
        ui.set_jam_remember_matches(enabled);
        self.save(ui);
        ui.set_jam_message(
            if enabled {
                "Grove will reuse your song choices in later jams on this computer."
            } else {
                "Saved song choices were forgotten."
            }
            .into(),
        );
    }
    /// ISRC and duration for a host track, looked up once per song.
    pub fn host_isrc(&mut self, ui: &GroveWindow, uri: &str) -> (String, u64) {
        if let Some(found) = self.jam.isrc.get(uri) {
            return found.clone();
        }
        if ui.get_library_ready()
            && uri.starts_with("spotify:track:")
            && self.jam.isrc_pending.insert(uri.to_owned())
            && self
                .request(
                    Action::TrackInfo { uri: uri.into() },
                    Pending::JamIsrc { uri: uri.into() },
                )
                .is_none()
        {
            self.jam.isrc_pending.remove(uri);
        }
        (String::new(), 0)
    }
    /// Resolve only to a confirmed recording; never guess a cover. Exact ISRC
    /// matches count as confirmed.
    pub fn paired_host(
        &mut self,
        ui: &GroveWindow,
        remote: &crate::jam::Playback,
    ) -> Option<String> {
        if remote.uri.starts_with("spotify:track:") {
            ui.set_jam_matching(false);
            return Some(remote.uri.clone());
        }
        if !remote.uri.starts_with("youtube:") {
            return None;
        }
        ui.set_jam_tide_host(true);
        self.jam_pairs.set_room(&ui.get_jam_invite());
        ui.set_jam_matching(true);
        if self.jam_remote != remote.uri {
            self.jam_remote = remote.uri.clone();
            self.jam_candidates.clear();
            self.jam.candidates.clear();
            self.jam.remote = Recording {
                uri: remote.uri.clone(),
                title: remote.title.clone(),
                artist: remote.artist.clone(),
                duration_ms: remote.duration_ms,
                isrc: remote.isrc.clone(),
            };
            ui.set_jam_matches(ModelRc::default());
            ui.set_jam_match_link("".into());
            ui.set_jam_match_title(
                if remote.artist.is_empty() {
                    remote.title.clone()
                } else {
                    format!("{} · {}", remote.title, remote.artist)
                }
                .into(),
            );
            ui.set_jam_match_confirmed(self.jam_pairs.get(&remote.uri).is_some());
            if self.jam_pairs.get(&remote.uri).is_none() && ui.get_library_ready() {
                ui.set_jam_message("Finding this song on Spotify…".into());
                let query = match matching::normalize_isrc(&remote.isrc) {
                    Some(isrc) => format!("isrc:{isrc}"),
                    None => format!(
                        "{} {}",
                        matching::search_title(&remote.title),
                        remote.artist
                    ),
                };
                let requested = self.request(
                    Action::Search {
                        query: query.trim().chars().take(200).collect(),
                        offset: 0,
                    },
                    Pending::JamMatch {
                        invite: ui.get_jam_invite().into(),
                        remote: remote.uri.clone(),
                    },
                );
                if requested.is_none() {
                    ui.set_jam_message(
                        "Spotify is busy. Paste the matching song link to continue.".into(),
                    );
                }
            }
        }
        self.jam_pairs.get(&remote.uri).map(str::to_owned)
    }
    pub fn resolve_jam(
        &mut self,
        ui: &GroveWindow,
        uri: String,
        sender: tokio::sync::mpsc::UnboundedSender<crate::jam_transport::Request>,
    ) {
        self.pending.retain(
            |_, p| !matches!(p,Pending::JamAdd{invite,..} if invite!=ui.get_jam_invite().as_str()),
        );
        if self
            .pending
            .values()
            .any(|p| matches!(p, Pending::JamAdd { .. }))
        {
            return;
        }
        let invite = ui.get_jam_invite().to_string();
        if self
            .request(
                Action::TrackInfo { uri: uri.clone() },
                Pending::JamAdd {
                    uri,
                    invite,
                    sender,
                },
            )
            .is_some()
        {
            ui.set_jam_message("Finding your song…".into());
        } else {
            ui.set_jam_message("Wait a moment, then try again.".into());
        }
    }
    /// Add the first 50 songs of a Spotify album or playlist to the shared queue.
    pub fn resolve_jam_collection(
        &mut self,
        ui: &GroveWindow,
        uri: String,
        sender: tokio::sync::mpsc::UnboundedSender<crate::jam_transport::Request>,
    ) {
        if self
            .pending
            .values()
            .any(|p| matches!(p, Pending::JamCollection { .. }))
        {
            ui.set_jam_message("Wait for the previous album or playlist.".into());
            return;
        }
        let group = if uri.starts_with("spotify:album:") {
            "From an album"
        } else {
            "From a playlist"
        };
        let pending = Pending::JamCollection {
            invite: ui.get_jam_invite().to_string(),
            group: group.into(),
            sender,
        };
        if self
            .request(Action::Collection { uri, offset: 0 }, pending)
            .is_some()
        {
            ui.set_jam_message("Reading the songs…".into());
        } else {
            ui.set_jam_message("Wait a moment, then try again.".into());
        }
    }
    pub(super) fn jam_collection_loaded(
        &mut self,
        ui: &GroveWindow,
        data: &Value,
        group: String,
        sender: tokio::sync::mpsc::UnboundedSender<crate::jam_transport::Request>,
    ) {
        let items: Vec<Item> = data["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| {
                let track = v.get("item").or_else(|| v.get("track")).unwrap_or(v);
                let found = recording(track)?;
                Some(Item {
                    uri: found.uri,
                    title: found.title.chars().take(120).collect(),
                    isrc: found.isrc,
                    duration_ms: found.duration_ms,
                })
            })
            .take(tide_jam::jam::MAX_GUEST_ENTRIES)
            .collect();
        if items.is_empty() {
            ui.set_jam_message("That album or playlist has no playable songs.".into());
            return;
        }
        let count = items.len();
        let _ = sender.send(crate::jam_transport::Request::AddItems { group, items });
        ui.set_jam_track_link("".into());
        ui.set_jam_message(format!("Adding {count} songs to the shared queue.").into());
    }
    pub(super) fn jam_matches_loaded(&mut self, ui: &GroveWindow, data: &Value) {
        let found: Vec<(Recording, Track)> = data["tracks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| {
                let r = recording(v)?;
                Some((r, Track::from_entity(&crate::web_api::entity(v))))
            })
            .take(10)
            .collect();
        let candidates: Vec<Recording> = found.iter().map(|(r, _)| r.clone()).collect();
        let order = matching::rank(&self.jam.remote, &candidates);
        self.jam_candidates = order.iter().map(|&i| found[i].1.clone()).collect();
        self.jam.candidates = order.iter().map(|&i| candidates[i].clone()).collect();
        ui.set_jam_matches(ModelRc::new(VecModel::from(
            self.jam_candidates
                .iter()
                .map(|t| GroveFeatureTrack {
                    title: t.title.clone().into(),
                    artist: t.artist.clone().into(),
                    duration: state::clock(t.duration).into(),
                    uri: t.uri.clone().into(),
                    artist_uri: Default::default(),
                    cover: Default::default(),
                })
                .collect::<Vec<_>>(),
        )));
        if let Some(index) = matching::exact(&self.jam.remote, &self.jam.candidates) {
            let remote = self.jam_remote.clone();
            let uri = self.jam.candidates[index].uri.clone();
            if self.jam_pairs.remember(&remote, &uri).is_ok() {
                self.persist_matches(ui);
                ui.set_jam_match_confirmed(true);
                ui.set_jam_message("Matched the same recording by ISRC.".into());
                return;
            }
        }
        ui.set_jam_message(
            if self.jam_candidates.is_empty() {
                "No matches found. Paste the Spotify link for this song."
            } else {
                "Choose the same recording. Timing may differ between services."
            }
            .into(),
        );
    }
    pub(super) fn jam_track_resolved(
        &mut self,
        ui: &GroveWindow,
        data: &Value,
        uri: String,
        invite: String,
        sender: tokio::sync::mpsc::UnboundedSender<crate::jam_transport::Request>,
    ) {
        if ui.get_jam_connected() && ui.get_jam_invite().as_str() == invite {
            let title = data["track"]["name"]
                .as_str()
                .unwrap_or("Spotify song")
                .to_owned();
            let _ = sender.send(crate::jam_transport::Request::Add { uri, title });
            ui.set_jam_message("".into());
        }
    }
}
