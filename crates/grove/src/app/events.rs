//! The 100 ms poll loop that applies Spotify worker events to the window.
use super::{
    jam_ui::JamSender,
    send,
    window::{refresh, refresh_queue},
};
use crate::{
    features::Features,
    jam_transport,
    local::Local,
    spotify::{Event, RequestSender},
    state::{clock, State},
    GrovePlaylist, GroveWindow,
};
use serde_json::json;
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{cell::Cell, cell::RefCell, rc::Rc, sync::mpsc, time::Duration};

pub fn start(
    ui: &GroveWindow,
    events: mpsc::Receiver<Event>,
    state: &Rc<RefCell<State>>,
    jam_tx: &JamSender,
    tx: &RequestSender,
    features: &Rc<RefCell<Features>>,
    local: &Rc<RefCell<Local>>,
) -> slint::Timer {
    let poll = slint::Timer::default();
    let weak = ui.as_weak();
    let current = state.clone();
    let playback_generation = Rc::new(Cell::new(0u64));
    let room_sender = jam_tx.clone();
    let feature_state = features.clone();
    let sender = tx.clone();
    let files = local.clone();
    poll.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(100),
        move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let mut state = current.borrow_mut();
            let mut changed = false;
            while let Ok(event) = events.try_recv() {
                changed = true;
                match event {
                    Event::Feature { id, data } => feature_state.borrow_mut().response(&ui,id,data),
                    Event::Connected(generation) => {
                        playback_generation.set(generation);
                        *state = State::default();
                        state.connected = true;
                        refresh_queue(&ui, &state);
                        ui.set_connecting(false);
                        ui.set_message(String::new().into());
                    }
                    Event::Disconnected(reason) => {
                        playback_generation.set(0);
                        *state = State::default();
                        refresh_queue(&ui, &state);
                        ui.set_connecting(false);
                        ui.set_message(reason.into());
                    }
                    Event::PlaybackStopped(generation) => {
                        if generation != playback_generation.get() {continue;}
                        playback_generation.set(0); *state=State::default(); refresh_queue(&ui,&state);
                        ui.set_message("Playback stopped. Reconnect Spotify from Account.".into());
                    }
                    Event::Payload(generation, payload) => {
                        if generation != playback_generation.get() {
                            continue;
                        }
                        let event_type = payload["type"].as_str().unwrap_or_default();
                        if event_type=="loading"{state.apply(&payload);ui.set_message("Loading Spotify audio…".into());continue;}
                        if event_type=="auth_state"&&payload["logged_in"]==true{send(&sender,json!({"command":"set_volume","volume":feature_state.borrow().prefs.volume.round() as u32}));}
                        if event_type=="end_of_track" {feature_state.borrow_mut().ended(&ui,&state,payload["play_request_id"].as_u64());continue;}
                        if event_type == "error" {
                            ui.set_message(
                                payload["message"]
                                    .as_str()
                                    .unwrap_or("Spotify rejected the command.")
                                    .into(),
                            );
                            continue;
                        }
                        state.apply(&payload);
                        if state.playing&&files.borrow().playing_file(){files.borrow_mut().stop(&ui,&state);ui.set_message("Spotify playback resumed.".into());}
                        if event_type == "queue_changed" || !state.active {
                            refresh_queue(&ui, &state);
                        }
                        if state.playing && (ui.get_message().starts_with("Playback requested")||ui.get_message().starts_with("Loading Spotify")) {
                            ui.set_message(String::new().into());
                        }
                    }
                    Event::Busy(busy, message) => {
                        ui.set_connecting(busy);
                        if !message.is_empty() {
                            ui.set_message(message.into());
                        } else if [
                            "Checking Spotify",
                            "Continue in your browser",
                            "Setting up local",
                            "Connecting Spotify",
                            "Spotify is busy. Keeping your approval",
                        ]
                        .iter()
                        .any(|prefix| ui.get_message().starts_with(prefix))
                        {
                            ui.set_message(String::new().into());
                        }
                    }
                    Event::Error(error) => {
                        ui.set_message(error.into());
                    }
                    Event::SignedOut => {
                        if files.borrow().active(){files.borrow_mut().stop(&ui,&state);}
                        feature_state.borrow_mut().clear(&ui);
                        let _=room_sender.send(jam_transport::Request::Leave);
                        ui.set_library_ready(false);
                        ui.set_account_name("Not signed in".into());
                        ui.set_playlists(ModelRc::default());
                        ui.set_owned_playlists(ModelRc::default());
                        ui.set_collection_uri(String::new().into());
                        ui.set_library_note(String::new().into());
                    }
                    Event::Library { name, playlists } => {
                        ui.set_library_ready(true);
                        ui.set_feature_message("".into());
                        feature_state.borrow_mut().grant_ready(&ui);
                        if ui.get_jam_name()=="Listener"{ui.set_jam_name(name.clone().into());}
                        ui.set_account_name(name.into());
                        ui.set_owned_playlists(ModelRc::new(VecModel::from(playlists["items"].as_array().into_iter().flatten().filter(|p|p["owner"]["id"].is_string()&&p["owner"]["id"]==playlists["account"]).filter_map(|p|Some(GrovePlaylist{title:p["name"].as_str()?.into(),uri:p["uri"].as_str()?.into(),subtitle:"Your playlist".into()})).collect::<Vec<_>>())));
                        let rows = playlists["items"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|p| {
                                Some(GrovePlaylist {
                                    title: p["name"].as_str()?.into(),
                                    subtitle: p["owner"]["display_name"]
                                        .as_str()
                                        .unwrap_or("Playlist")
                                        .into(),
                                    uri: p["uri"].as_str()?.into(),
                                })
                            })
                            .collect::<Vec<_>>();
                        ui.set_playlists(ModelRc::new(VecModel::from(rows)));
                        ui.set_playlists_more(!playlists["next"].is_null());
                        ui.set_library_note(
                            if playlists["load_failed"] == true {
                                "Playlist loading failed. Use Refresh to retry."
                            } else if !playlists["next"].is_null() {
                                "Showing the first 50 playlists."
                            } else {
                                "Your saved playlists"
                            }
                            .into(),
                        );
                        if ui.get_message().starts_with("Refreshing") {
                            ui.set_message(String::new().into());
                        }
                    }

                }
            }
            if files.borrow().active() {
                // The local timer publishes its independent backend state.
            } else if changed {
                refresh(&ui, &state);
            } else if state.playing {
                ui.set_position(state.position());
                ui.set_elapsed(clock(state.position()).into());
            }
            feature_state.borrow_mut().tick(&ui,&state);
        },
    );
    poll
}
