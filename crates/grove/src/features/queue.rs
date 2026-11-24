//! Grove's own queue: editing, playing a selection, advancing and autoplay.
use super::{Features, Pending};
use crate::{
    library::Action,
    spotify::{self, Request},
    state::{self, Track},
    GroveWindow,
};
use serde_json::{json, Value};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{cell::RefCell, rc::Rc, time::Instant};

pub(super) fn bind(ui: &GroveWindow, this: &Rc<RefCell<Features>>) {
    let f = this.clone();
    let weak = ui.as_weak();
    ui.on_queue_edit(move |action, index| {
        if let Some(ui) = weak.upgrade() {
            let mut f = f.borrow_mut();
            let i = index.max(0) as usize;
            if f.starting.is_some() {
                ui.set_message(
                    "Wait for the requested track to start before editing the queue.".into(),
                );
                return;
            }
            match action.as_str() {
                "remove" if i < f.queue.len() => {
                    f.queue.remove(i);
                }
                "up" if i > 0 && i < f.queue.len() => f.queue.swap(i, i - 1),
                "down" if i + 1 < f.queue.len() => f.queue.swap(i, i + 1),
                "clear" => f.queue.clear(),
                _ => {}
            }
            f.render_queue(&ui);
        }
    });
    let f = this.clone();
    let weak = ui.as_weak();
    ui.on_play_selection(move |shuffle| {
        if let Some(ui) = weak.upgrade() {
            let mut f = f.borrow_mut();
            if !ui.get_authenticated() || ui.get_jam_connected() {
                ui.set_message(
                    "Enable playback and leave the jam before playing this selection.".into(),
                );
                return;
            }
            let mut rows: Vec<_> = f
                .rows
                .iter()
                .filter(|t| !f.prefs.hidden.contains(&t.uri))
                .take(500)
                .cloned()
                .collect();
            if shuffle {
                shuffle_tracks(&mut rows);
            }
            if let Some(first) = rows.first().cloned() {
                f.queue = rows.into_iter().skip(1).collect();
                f.render_queue(&ui);
                drop(f);
                ui.invoke_play_uri(first.uri.into());
            }
        }
    });
}

impl Features {
    pub(super) fn render_queue(&self, ui: &GroveWindow) {
        ui.set_local_queue(ModelRc::new(VecModel::from(
            self.queue
                .iter()
                .map(|t| crate::GroveTrack {
                    title: t.title.clone().into(),
                    artist: t.artist.clone().into(),
                    uri: t.uri.clone().into(),
                    duration: state::clock(t.duration).into(),
                })
                .collect::<Vec<_>>(),
        )));
    }
    pub fn next(&mut self, ui: &GroveWindow, state: &state::State) -> bool {
        if self.queue.is_empty() || !state.authenticated || ui.get_jam_connected() {
            return false;
        }
        if self.starting.is_some() {
            return true;
        }
        let uri = self.queue[0].uri.clone();
        self.starting = Some((uri.clone(), state.play_request_id, Instant::now()));
        if !self.offline {
            let _ = self
                .tx
                .send(Request::Command(json!({"command":"play","uri":uri})));
        }
        true
    }
    pub fn ended(&mut self, ui: &GroveWindow, state: &state::State, id: Option<u64>) {
        if id != state.play_request_id
            || id.is_none()
            || ui.get_jam_connected()
            || ui.get_local_active()
            || state.loading
        {
            return;
        }
        if self.queue.is_empty() && self.prefs.autoplay {
            if let Some(index) = self
                .rotation
                .iter()
                .position(|t| t.uri != state.track.uri && !self.prefs.hidden.contains(&t.uri))
            {
                let track = self.rotation.remove(index);
                self.rotation.push(track.clone());
                self.queue.push(track);
                self.render_queue(ui);
            }
        }
        self.next(ui, state);
    }
    pub fn enqueue_link(&mut self, ui: &GroveWindow, input: &str) {
        if ui.get_jam_connected() {
            ui.set_message("Add this song to the shared jam instead.".into());
            return;
        }
        match spotify::spotify_uri(input) {
            Ok(uri) if uri.starts_with("spotify:track:") => {
                if self
                    .request(Action::TrackInfo { uri }, Pending::Enqueue)
                    .is_none()
                {
                    ui.set_message("Wait for the current request, then try again.".into());
                } else {
                    ui.set_message("Finding your song…".into());
                }
            }
            _ => ui.set_message("Paste a Spotify song link.".into()),
        }
    }
    pub(super) fn enqueued(&mut self, ui: &GroveWindow, data: &Value) {
        if self.queue.len() >= 500 {
            ui.set_message("Your queue is full. Remove a song before adding another.".into());
            return;
        }
        let track = Track::from_entity(&crate::web_api::entity(&data["track"]));
        if spotify::spotify_uri(&track.uri).is_err() {
            ui.set_message("Spotify couldn't find this song.".into());
            return;
        }
        self.queue.push(track);
        self.render_queue(ui);
        ui.set_message("Added to your queue.".into());
    }
}

fn shuffle_tracks(tracks: &mut [Track]) {
    use std::hash::{Hash, Hasher};
    let salt = crate::jam::id();
    tracks.sort_by_cached_key(|t| {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        salt.hash(&mut h);
        t.uri.hash(&mut h);
        h.finish()
    });
}
