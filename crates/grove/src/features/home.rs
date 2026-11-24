//! Home: a featured hero, quick picks and cover shelves built from the data this
//! grant can read (top or liked songs, recent listening, top artists, saved
//! albums and playlists). Pages are cached between launches with a short TTL.
use super::{
    browse::{Entry, Retry, Shelf, HOME},
    Features, Pending,
};
use crate::{browse_cache::Hit, library::Action, GroveWindow};
use serde_json::Value;

impl Features {
    /// Shows Home from the cache when fresh (no request), else loads it. A stale
    /// cache is shown at once while a refresh runs quietly.
    pub(super) fn show_home(&mut self, ui: &GroveWindow, force: bool) {
        if !ui.get_library_ready() {
            return;
        }
        self.view_page = HOME;
        match (!force).then(|| self.browse.cache.get("home")).flatten() {
            Some(Hit::Fresh(data)) => self.apply_cached(ui, Action::Home, data),
            Some(Hit::Stale(data)) => {
                self.apply_cached(ui, Action::Home, data);
                self.refresh_quietly(ui, Action::Home);
            }
            None => self.load(ui, Action::Home, false),
        }
    }

    /// Applies a cached response as if it had just arrived.
    pub(super) fn apply_cached(&mut self, ui: &GroveWindow, action: Action, mut data: Value) {
        self.epoch += 1;
        self.source = Some(action);
        data["cached"] = Value::Bool(true);
        ui.set_feature_busy(false);
        self.page_loaded(ui, &data, false);
    }

    /// Refreshes the current page without clearing what is on screen.
    pub(super) fn refresh_quietly(&mut self, ui: &GroveWindow, action: Action) {
        self.epoch += 1;
        self.source = Some(action.clone());
        let pending = Pending::Page {
            epoch: self.epoch,
            append: false,
        };
        if self.request(action, pending).is_some() {
            ui.set_feature_busy(true);
        }
    }

    pub(super) fn home_loaded(&mut self, ui: &GroveWindow, data: &Value) {
        self.browse.hero_label = match data["note"].as_str().unwrap_or_default() {
            "From your liked songs" => "FROM YOUR LIKED SONGS",
            "From your recent listening" => "FROM YOUR RECENT LISTENING",
            _ => "ON REPEAT",
        }
        .into();
        self.browse.home_shelves = data["shelves"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| {
                Some((
                    s["id"].as_str()?.to_owned(),
                    s["title"].as_str()?.to_owned(),
                    Entry::from_cards(&s["cards"]),
                ))
            })
            .collect();
        if data["cooldown"].is_object() {
            self.show_cooldown(ui, &data["cooldown"], Some(Retry::Page(Action::Home)));
        }
        if data["cached"] != true && data["cooldown"].is_null() {
            self.browse.cache.put("home", data);
        }
        // Home lists the songs; the note belongs to the hero instead.
        ui.set_feature_message("".into());
    }

    /// Shelves in display order. Playlists come from the library already loaded.
    pub(super) fn home_shelf_list(&self) -> Vec<Shelf> {
        let mut shelves: Vec<Shelf> = vec![(
            "made".into(),
            "Made for you".into(),
            vec![Entry::mix(), Entry::liked()],
        )];
        let find = |id: &str| self.browse.home_shelves.iter().find(|s| s.0 == id).cloned();
        shelves.extend(find("recent"));
        shelves.extend(find("artists"));
        if !self.browse.playlists.is_empty() {
            shelves.push((
                "playlists".into(),
                "Your playlists".into(),
                self.browse.playlists.iter().take(12).cloned().collect(),
            ));
        }
        shelves.extend(find("albums"));
        shelves
    }
}
