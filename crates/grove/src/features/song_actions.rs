//! The per-song action menu: play next, queue, like, hide, artist, radio and playlists.
use super::{Features, Pending};
use crate::{library::Action, GroveWindow};
use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc};

pub(super) fn bind(ui: &GroveWindow, this: &Rc<RefCell<Features>>) {
    let f = this.clone();
    let weak = ui.as_weak();
    ui.on_song_action(move |action, uri| {
        if let Some(ui) = weak.upgrade() {
            if action == "play" {
                ui.invoke_play_uri(uri);
                return;
            }
            f.borrow_mut().song(&ui, &action, &uri);
        }
    });
}

impl Features {
    pub(super) fn song(&mut self, ui: &GroveWindow, action: &str, uri: &str) {
        let track = self
            .rows
            .iter()
            .chain(self.queue.iter())
            .find(|t| t.uri == uri)
            .cloned()
            .or_else(|| (self.metadata.uri == uri).then(|| self.metadata.clone()));
        match action {
            "Next" | "Queue" => {
                if self.starting.is_some() {
                    ui.set_message(
                        "Wait for the requested track to start before adding more songs.".into(),
                    );
                    return;
                }
                if ui.get_jam_connected() {
                    ui.set_message("Use the shared jam queue while in a jam.".into());
                    return;
                }
                if let Some(t) = track {
                    if self.queue.len() >= 500 {
                        ui.set_message("Grove's queue is full (500 tracks).".into());
                        return;
                    }
                    if action == "Next" {
                        self.queue.insert(0, t);
                    } else {
                        self.queue.push(t);
                    }
                    self.render_queue(ui);
                    ui.set_message("Added to your queue.".into());
                }
            }
            "Radio" => {
                self.open_radio(ui, uri);
            }
            "Save" | "Unsave" => {
                self.request(
                    Action::Save {
                        uri: uri.into(),
                        saved: action == "Save",
                    },
                    Pending::Mutation,
                );
            }
            "Hide" => {
                if self.prefs.hidden.len() < 2000 {
                    self.prefs.hidden.insert(uri.into());
                    self.save(ui);
                    self.render(ui);
                    ui.set_feature_message(
                        "Song hidden. You can bring it back in Settings.".into(),
                    );
                }
            }
            "Artist" => {
                self.open_artist(ui, track);
            }
            "Playlist" | "Remove" => {
                ui.set_removing_from_playlist(action == "Remove");
                ui.set_edit_track(uri.into());
                ui.set_edit_playlist(
                    if ui.get_collection_uri().starts_with("spotify:playlist:") {
                        ui.get_collection_uri()
                    } else {
                        "".into()
                    },
                );
            }
            _ => {}
        }
    }
}
