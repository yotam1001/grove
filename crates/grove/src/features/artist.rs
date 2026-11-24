//! Artist pages: a hero header with Play/Shuffle and Follow, popular songs (top
//! tracks where Spotify still allows them, otherwise songs from the newest
//! releases), album and single shelves, and a choice among credited artists.
//! Song radio stays a plain list on page 7.
use super::{
    browse::{Entry, Header, Want, ARTIST},
    Features, Pending,
};
use crate::{library::Action, state::Track, GroveWindow};
use serde_json::Value;

impl Features {
    pub(super) fn open_radio(&mut self, ui: &GroveWindow, uri: &str) {
        self.remember(ui);
        ui.set_page(7);
        ui.set_feature_title("Song radio".into());
        self.load(ui, Action::Radio { uri: uri.into() }, false);
    }

    /// From a song menu or the player: the first credited artist, with the
    /// others offered on the artist page.
    pub(super) fn open_artist(&mut self, ui: &GroveWindow, track: Option<Track>) {
        let Some(t) = track.filter(|t| !t.artist_uri.is_empty()) else {
            ui.set_feature_message("Artist information is not available for this track.".into());
            return;
        };
        let credits = self.browse.credits.get(&t.uri).cloned().unwrap_or_default();
        let (name, uri, others) = match credits.split_first() {
            Some(((name, uri), rest)) => (name.clone(), uri.clone(), rest.to_vec()),
            None => (
                t.artist.split(", ").next().unwrap_or_default().to_owned(),
                t.artist_uri.clone(),
                vec![],
            ),
        };
        self.open_artist_page(ui, &uri, &name, others);
    }

    pub(super) fn open_artist_page(
        &mut self,
        ui: &GroveWindow,
        uri: &str,
        name: &str,
        others: Vec<(String, String)>,
    ) {
        if !uri.starts_with("spotify:artist:") {
            ui.set_feature_message("Artist information is not available for this track.".into());
            return;
        }
        self.remember(ui);
        self.browse.header = Header {
            kind: "artist".into(),
            label: "ARTIST".into(),
            title: name.into(),
            uri: uri.into(),
            can_follow: true,
            note: "Popular".into(),
            ..Default::default()
        };
        self.browse.artist_shelves.clear();
        self.browse.credited = others
            .into_iter()
            .filter(|(_, u)| u != uri)
            .map(|(title, uri)| Entry {
                kind: "artist".into(),
                uri,
                title,
                ..Default::default()
            })
            .collect();
        ui.set_page(ARTIST);
        ui.set_feature_title(name.into());
        self.load(ui, Action::ArtistPage { uri: uri.into() }, false);
    }

    pub(super) fn artist_loaded(&mut self, ui: &GroveWindow, data: &Value) {
        let h = &mut self.browse.header;
        if data["uri"].as_str() != Some(h.uri.as_str()) {
            return;
        }
        if let Some(name) = data["title"].as_str() {
            h.title = name.into();
            ui.set_feature_title(name.into());
        }
        h.image = data["image"].as_str().unwrap_or_default().into();
        // Followers and popularity were removed in February 2026; genres may be empty.
        let genres: Vec<&str> = data["genres"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .take(3)
            .collect();
        h.subtitle = genres.join(" · ");
        h.note = data["note"].as_str().unwrap_or("Popular").into();
        h.following = data["following"] == true;
        h.can_follow = data["following"].is_boolean();
        let mut shelves = vec![];
        for (key, title) in [("albums", "Albums"), ("singles", "Singles and EPs")] {
            let entries = Entry::from_cards(&data[key]);
            if !entries.is_empty() {
                shelves.push((key.to_owned(), title.to_owned(), entries));
            }
        }
        self.browse.artist_shelves = shelves;
        self.clear_cooldown(ui);
    }

    pub(super) fn toggle_follow(&mut self, ui: &GroveWindow) {
        let h = &self.browse.header;
        if h.kind != "artist" || !h.can_follow {
            return;
        }
        let follow = !h.following;
        let uri = h.uri.clone();
        let want = Want::Follow {
            uri: uri.clone(),
            follow,
        };
        if self
            .request(Action::Follow { uri, follow }, Pending::Browse(want))
            .is_some()
        {
            ui.set_feature_message(
                if follow {
                    "Following on Spotify…"
                } else {
                    "Unfollowing on Spotify…"
                }
                .into(),
            );
        }
    }

    pub(super) fn follow_confirmed(&mut self, ui: &GroveWindow, uri: &str, follow: bool) {
        if self.browse.header.uri == uri {
            self.browse.header.following = follow;
        }
        // The followed-artists tab changed; refresh it on the next visit.
        self.browse.cache.expire("library:artists");
        self.browse.artists.clear();
        ui.set_feature_message(
            if follow {
                "You follow this artist on Spotify."
            } else {
                "You no longer follow this artist."
            }
            .into(),
        );
        self.render(ui);
    }
}
