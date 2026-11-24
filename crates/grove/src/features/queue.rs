//! Grove's own queue: editing, drag reordering, search, playing a selection,
//! advancing, autoplay (which never picks hidden songs) and related songs.
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
    bind_drawer(ui, this);
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
        let row = |t: &Track| crate::GroveTrack {
            title: t.title.clone().into(),
            artist: t.artist.clone().into(),
            uri: t.uri.clone().into(),
            duration: state::clock(t.duration).into(),
        };
        ui.set_local_queue(ModelRc::new(VecModel::from(
            self.queue.iter().map(row).collect::<Vec<_>>(),
        )));
        let autoplay = self.autoplay_preview(ui);
        ui.set_autoplay_queue(ModelRc::new(VecModel::from(
            autoplay.iter().map(row).collect::<Vec<_>>(),
        )));
        // Search within the queue: manual entries keep their index; autoplay
        // entries are encoded as -(index + 1).
        let needle = ui.get_queue_query().trim().to_lowercase();
        let (rows, indexes): (Vec<_>, Vec<_>) = if needle.is_empty() {
            (Vec::new(), Vec::new())
        } else {
            self.queue
                .iter()
                .enumerate()
                .filter(|(_, t)| matches(t, &needle))
                .map(|(i, t)| (row(t), i as i32))
                .chain(
                    autoplay
                        .iter()
                        .enumerate()
                        .filter(|(_, t)| matches(t, &needle))
                        .map(|(i, t)| (row(t), -(i as i32) - 1)),
                )
                .take(200)
                .unzip()
        };
        ui.set_queue_search_results(ModelRc::new(VecModel::from(rows)));
        ui.set_queue_search_index(ModelRc::new(VecModel::from(indexes)));
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

/// Queue drawer callbacks (#27) and Hide beside Like (#30).
pub(super) fn bind_drawer(ui: &GroveWindow, this: &Rc<RefCell<Features>>) {
    let f = this.clone();
    let weak = ui.as_weak();
    ui.on_queue_move(move |from, to| {
        if let Some(ui) = weak.upgrade() {
            let mut f = f.borrow_mut();
            if f.starting.is_some() {
                ui.set_message(
                    "Wait for the requested track to start before editing the queue.".into(),
                );
                return;
            }
            if move_entry(&mut f.queue, from, to) {
                f.render_queue(&ui);
            }
        }
    });
    let f = this.clone();
    let weak = ui.as_weak();
    ui.on_queue_filter(move |query| {
        if let Some(ui) = weak.upgrade() {
            ui.set_queue_query(query);
            f.borrow().render_queue(&ui);
        }
    });
    let f = this.clone();
    let weak = ui.as_weak();
    ui.on_autoplay_action(move |action, index| {
        if let Some(ui) = weak.upgrade() {
            let mut f = f.borrow_mut();
            let preview = f.autoplay_preview(&ui);
            if let Some(track) = usize::try_from(index).ok().and_then(|i| preview.get(i)) {
                if action == "queue" {
                    let uri = track.uri.clone();
                    f.add_to_queue(&ui, track.clone(), false, &uri);
                }
            }
        }
    });
    let f = this.clone();
    let weak = ui.as_weak();
    ui.on_load_related(move || {
        if let Some(ui) = weak.upgrade() {
            f.borrow_mut().load_related(&ui);
        }
    });
    let f = this.clone();
    let weak = ui.as_weak();
    ui.on_related_action(move |action, index| {
        if let Some(ui) = weak.upgrade() {
            let mut f = f.borrow_mut();
            let Some(track) = usize::try_from(index)
                .ok()
                .and_then(|i| f.related.get(i))
                .cloned()
            else {
                return;
            };
            match action.as_str() {
                "play" => {
                    drop(f);
                    ui.invoke_play_uri(track.uri.into());
                }
                "queue" | "next" => {
                    let uri = track.uri.clone();
                    f.add_to_queue(&ui, track, action == "next", &uri);
                }
                _ => {}
            }
        }
    });
    let f = this.clone();
    let weak = ui.as_weak();
    ui.on_hide_current(move |hide| {
        if let Some(ui) = weak.upgrade() {
            f.borrow_mut().hide_current(&ui, hide);
        }
    });
}

/// Moves one queue entry to `to`, shifting the songs between. False if invalid.
pub(super) fn move_entry(queue: &mut Vec<Track>, from: i32, to: i32) -> bool {
    let (Ok(from), Ok(to)) = (usize::try_from(from), usize::try_from(to)) else {
        return false;
    };
    if from >= queue.len() || to >= queue.len() || from == to {
        return false;
    }
    let track = queue.remove(from);
    queue.insert(to, track);
    true
}

fn matches(track: &Track, needle: &str) -> bool {
    track.title.to_lowercase().contains(needle) || track.artist.to_lowercase().contains(needle)
}

impl Features {
    /// The songs autoplay would add next: Home rotation in order, skipping
    /// hidden songs, the current song and songs already queued.
    pub(super) fn autoplay_preview(&self, ui: &GroveWindow) -> Vec<Track> {
        if !self.prefs.autoplay {
            return Vec::new();
        }
        let current = ui.get_current_uri();
        self.rotation
            .iter()
            .filter(|t| {
                t.uri != current.as_str()
                    && !self.prefs.hidden.contains(&t.uri)
                    && !self.queue.iter().any(|q| q.uri == t.uri)
            })
            .take(8)
            .cloned()
            .collect()
    }

    fn add_to_queue(&mut self, ui: &GroveWindow, track: Track, next: bool, uri: &str) {
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
        if spotify::spotify_uri(uri).is_err() {
            return;
        }
        if self.queue.len() >= 500 {
            ui.set_message("Grove's queue is full (500 tracks).".into());
            return;
        }
        if next {
            self.queue.insert(0, track);
        } else {
            self.queue.push(track);
        }
        self.render_queue(ui);
        ui.set_message("Added to your queue.".into());
    }

    pub(super) fn load_related(&mut self, ui: &GroveWindow) {
        let uri = ui.get_current_uri().to_string();
        if !uri.starts_with("spotify:track:") || !ui.get_library_ready() {
            self.related.clear();
            self.render_related(ui);
            ui.set_related_state(0);
            ui.set_related_note("Play a Spotify song to see related music.".into());
            return;
        }
        if self
            .request(
                Action::Radio { uri: uri.clone() },
                Pending::Related(uri.clone()),
            )
            .is_some()
        {
            ui.set_related_state(1);
            ui.set_related_note("".into());
        }
    }

    pub(super) fn related_loaded(&mut self, ui: &GroveWindow, data: &Value) {
        let current = ui.get_current_uri();
        self.related = data["tracks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| {
                t["uri"]
                    .as_str()
                    .is_some_and(|u| u.starts_with("spotify:track:") && u != current.as_str())
            })
            .map(|t| Track::from_entity(&crate::web_api::entity(t)))
            .filter(|t| !self.prefs.hidden.contains(&t.uri))
            .take(30)
            .collect();
        ui.set_related_state(if self.related.is_empty() { 3 } else { 2 });
        ui.set_related_note(
            data["note"]
                .as_str()
                .unwrap_or("Browsing here never changes what's playing.")
                .into(),
        );
        self.render_related(ui);
    }

    pub(super) fn related_failed(&mut self, ui: &GroveWindow, error: &str) {
        self.related.clear();
        self.render_related(ui);
        ui.set_related_state(3);
        ui.set_related_note(error.into());
    }

    pub(super) fn render_related(&mut self, ui: &GroveWindow) {
        let art = &mut self.art;
        let rows = self
            .related
            .iter()
            .map(|t| crate::GroveFeatureTrack {
                title: t.title.clone().into(),
                artist: t.artist.clone().into(),
                duration: state::clock(t.duration).into(),
                uri: t.uri.clone().into(),
                artist_uri: t.artist_uri.clone().into(),
                cover: art.get(&t.artwork_url),
            })
            .collect::<Vec<_>>();
        ui.set_related_tracks(ModelRc::new(VecModel::from(rows)));
    }

    /// Hide ("not interested") from the player: the song leaves lists and
    /// autoplay; it keeps playing until you skip it.
    pub(super) fn hide_current(&mut self, ui: &GroveWindow, hide: bool) {
        let uri = ui.get_current_uri().to_string();
        if spotify::spotify_uri(&uri).ok().as_deref() != Some(uri.as_str()) {
            return;
        }
        if hide {
            if self.prefs.hidden.len() >= 2000 {
                ui.set_message("Too many hidden songs. Show some again in Settings.".into());
                return;
            }
            self.prefs.hidden.insert(uri);
            ui.set_message("Hidden. Grove won't autoplay this song or show it in lists.".into());
        } else {
            self.prefs.hidden.remove(&uri);
            ui.set_message("This song can appear again.".into());
        }
        ui.set_current_hidden(hide);
        self.save(ui);
        self.render(ui);
        self.render_queue(ui);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn t(id: &str) -> Track {
        Track {
            uri: format!("spotify:track:{id}"),
            title: id.into(),
            ..Default::default()
        }
    }
    #[test]
    fn drag_moves_one_entry_and_rejects_bad_indexes() {
        let mut q = vec![t("a"), t("b"), t("c"), t("d")];
        assert!(move_entry(&mut q, 0, 2));
        assert_eq!(
            q.iter().map(|t| t.title.as_str()).collect::<String>(),
            "bcad"
        );
        assert!(move_entry(&mut q, 3, 0));
        assert_eq!(
            q.iter().map(|t| t.title.as_str()).collect::<String>(),
            "dbca"
        );
        assert!(!move_entry(&mut q, 1, 1));
        assert!(!move_entry(&mut q, -1, 0));
        assert!(!move_entry(&mut q, 0, 4));
        assert_eq!(q.len(), 4);
    }
}
