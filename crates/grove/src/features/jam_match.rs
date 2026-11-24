//! Jam song matching: pairing a Tide host's YouTube song with a confirmed Spotify recording,
//! and resolving titles for songs added to the shared queue.
use super::{Features, Pending};
use crate::{
    library::Action,
    spotify,
    state::{self, Track},
    GroveFeatureTrack, GroveWindow,
};
use serde_json::Value;
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{cell::RefCell, rc::Rc};

pub(super) fn bind(ui: &GroveWindow, this: &Rc<RefCell<Features>>) {
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
        self.jam_remote.clear();
        self.jam_candidates.clear();
        self.pending
            .retain(|_, p| !matches!(p, Pending::JamMatch { .. }));
        ui.set_jam_matching(false);
        ui.set_jam_tide_host(false);
        ui.set_jam_match_confirmed(false);
        ui.set_jam_matches(ModelRc::default());
        ui.set_jam_match_link("".into());
    }
    /// Resolve only to an explicitly confirmed Spotify recording; never guess a cover.
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
            ui.set_jam_matches(ModelRc::default());
            ui.set_jam_match_link("".into());
            ui.set_jam_match_title(format!("{} · {}", remote.title, remote.artist).into());
            ui.set_jam_match_confirmed(self.jam_pairs.get(&remote.uri).is_some());
            if ui.get_library_ready() {
                ui.set_jam_message("Finding this song on Spotify…".into());
                let requested = self.request(
                    Action::Search {
                        query: format!("{} {}", remote.title, remote.artist)
                            .chars()
                            .take(200)
                            .collect(),
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
    pub(super) fn jam_matches_loaded(&mut self, ui: &GroveWindow, data: &Value) {
        self.jam_candidates = data["tracks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|v| {
                v["uri"]
                    .as_str()
                    .is_some_and(|u| u.starts_with("spotify:track:"))
            })
            .take(10)
            .map(|v| Track::from_entity(&crate::web_api::entity(v)))
            .collect();
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
