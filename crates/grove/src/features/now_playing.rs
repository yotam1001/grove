//! The current track: metadata, album, cover art, saved and hidden state, and
//! the per-poll tick.
use super::{Features, Pending};
use crate::{
    library::Action,
    state::{self, Track},
    GroveWindow,
};
use serde_json::Value;
use std::time::{Duration, Instant};

impl Features {
    pub fn tick(&mut self, ui: &GroveWindow, state: &state::State) {
        let now = Instant::now();
        let elapsed = now
            .duration_since(self.last_tick)
            .min(Duration::from_millis(500));
        self.last_tick = now;
        if ui.get_current_uri().as_str() != state.track.uri {
            ui.set_current_uri(state.track.uri.clone().into());
            ui.set_current_cover(self.art.get(&state.track.artwork_url));
            ui.set_current_saved(false);
            ui.set_current_album("".into());
            // Related songs follow the song that is playing.
            let related_used = ui.get_related_state() != 0;
            self.related.clear();
            self.render_related(ui);
            ui.set_related_state(0);
            if related_used {
                self.load_related(ui);
            }
            self.metadata = state.track.clone();
            self.listened = Duration::ZERO;
            self.played = None;
            if state.track.uri.starts_with("spotify:track:") && ui.get_library_ready() {
                self.request(
                    Action::TrackInfo {
                        uri: state.track.uri.clone(),
                    },
                    Pending::Metadata(state.track.uri.clone()),
                );
                self.request(
                    Action::Contains {
                        uri: state.track.uri.clone(),
                    },
                    Pending::Saved(state.track.uri.clone()),
                );
            }
        }
        if let Some((uri, old, since)) = &self.starting {
            if state.track.uri == *uri
                && state.play_request_id.is_some()
                && state.play_request_id != *old
                && state.active
            {
                if self.queue.first().is_some_and(|t| t.uri == *uri) {
                    self.queue.remove(0);
                }
                self.starting = None;
                self.render_queue(ui);
            } else if since.elapsed() > Duration::from_secs(15) {
                self.starting = None;
                ui.set_message(
                    "Playback was not confirmed. The Grove queue was preserved; retry Next.".into(),
                );
            }
        }
        let hidden = self.prefs.hidden.contains(ui.get_current_uri().as_str());
        if ui.get_current_hidden() != hidden {
            ui.set_current_hidden(hidden);
        }
        self.record_listening(state, elapsed, now);
        self.browse_tick(ui);
        if self.art.poll() {
            self.render(ui);
            if !self.related.is_empty() {
                self.render_related(ui);
            }
            ui.set_current_cover(self.art.get(&self.metadata.artwork_url));
        }
    }
    pub(super) fn metadata_loaded(&mut self, ui: &GroveWindow, data: &Value, uri: String) {
        if ui.get_current_uri().as_str() == uri {
            self.metadata = Track::from_entity(&crate::web_api::entity(&data["track"]));
            ui.set_current_album(
                data["track"]["album"]["name"]
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .take(200)
                    .collect::<String>()
                    .into(),
            );
            ui.set_current_cover(self.art.get(&self.metadata.artwork_url));
        }
    }
    pub(super) fn saved_loaded(&mut self, ui: &GroveWindow, data: &Value, uri: String) {
        if ui.get_current_uri().as_str() == uri {
            ui.set_current_saved(data["saved"] == true);
        }
    }
}
