//! Artist and song-radio pages, opened from a song's action menu.
use super::Features;
use crate::{library::Action, state::Track, GroveWindow};

impl Features {
    pub(super) fn open_radio(&mut self, ui: &GroveWindow, uri: &str) {
        self.remember(ui);
        ui.set_page(7);
        ui.set_feature_title("Song radio".into());
        self.load(ui, Action::Radio { uri: uri.into() }, false);
    }
    pub(super) fn open_artist(&mut self, ui: &GroveWindow, track: Option<Track>) {
        if let Some(t) = track.filter(|t| !t.artist_uri.is_empty()) {
            self.remember(ui);
            ui.set_page(7);
            ui.set_feature_title(t.artist.into());
            self.load(ui, Action::Artist { uri: t.artist_uri }, false);
        } else {
            ui.set_feature_message("Artist information is not available for this track.".into());
        }
    }
}
