//! Collection pages (playlists, albums, Liked Songs, the Discovery mix), paging
//! playlists, and editing them. Removal targets the exact entry the user chose,
//! by playlist position and revision, never every copy of the song.
use super::{
    browse::{Header, COLLECTION},
    Features, Pending,
};
use crate::{browse_cache::Hit, library::Action, spotify, GrovePlaylist, GroveWindow};
use serde_json::Value;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::{cell::RefCell, rc::Rc};

pub(super) fn bind(ui: &GroveWindow, this: &Rc<RefCell<Features>>) {
    let f = this.clone();
    let weak = ui.as_weak();
    ui.on_browse(move |uri, title| {
        if let Some(ui) = weak.upgrade() {
            let kind = if uri == "liked" {
                "liked"
            } else if uri == "mix" {
                "mix"
            } else if uri.starts_with("spotify:album:") {
                "album"
            } else {
                "playlist"
            };
            f.borrow_mut().open_collection(&ui, kind, &uri, &title);
        }
    });
    let f = this.clone();
    let weak = ui.as_weak();
    ui.on_playlist_edit(move |remove| {
        if let Some(ui) = weak.upgrade() {
            let mut f = f.borrow_mut();
            let playlist = match spotify::spotify_uri(&ui.get_edit_playlist()) {
                Ok(p) if p.starts_with("spotify:playlist:") => p,
                _ => {
                    ui.set_feature_message(
                        "Paste a Spotify playlist link owned by your account.".into(),
                    );
                    return;
                }
            };
            let uri = ui.get_edit_track().to_string();
            let action = if remove {
                match f.removal_action(&ui, &playlist, &uri) {
                    Ok(action) => action,
                    Err(message) => {
                        ui.set_feature_message(message.into());
                        return;
                    }
                }
            } else {
                Action::PlaylistAdd { playlist, uri }
            };
            if f.request(action, Pending::Mutation).is_some() {
                ui.set_feature_message(
                    "Waiting for Spotify to confirm the playlist change…".into(),
                );
                ui.set_edit_track("".into());
            }
        }
    });
    let f = this.clone();
    let weak = ui.as_weak();
    ui.on_library_more(move || {
        if let Some(ui) = weak.upgrade() {
            let mut f = f.borrow_mut();
            let offset = ui.get_playlists().row_count() as u32;
            f.request(Action::Playlists { offset }, Pending::Playlists);
        }
    });
}

impl Features {
    pub(super) fn open_collection(&mut self, ui: &GroveWindow, kind: &str, uri: &str, title: &str) {
        if !ui.get_library_ready() {
            ui.set_feature_message("Connect Spotify to browse your music.".into());
            return;
        }
        self.remember(ui);
        let entry = self.browse.playlists.iter().find(|e| e.uri == uri).cloned();
        let owned = ui.get_owned_playlists().iter().any(|p| p.uri == uri);
        self.browse.header = match kind {
            "liked" => Header {
                kind: "liked".into(),
                label: "PLAYLIST".into(),
                title: "Liked Songs".into(),
                subtitle: ui.get_account_name().to_string(),
                uri: "liked".into(),
                ..Default::default()
            },
            "mix" => Header {
                kind: "mix".into(),
                label: "MADE FOR YOU".into(),
                title: "Discovery mix".into(),
                subtitle: "Top songs, your artists' newest releases and liked songs".into(),
                uri: "mix".into(),
                ..Default::default()
            },
            "album" => Header {
                kind: "album".into(),
                label: "ALBUM".into(),
                title: title.into(),
                uri: uri.into(),
                ..Default::default()
            },
            _ => Header {
                kind: "playlist".into(),
                label: "PLAYLIST".into(),
                title: entry.as_ref().map_or(title, |e| e.title.as_str()).into(),
                subtitle: entry
                    .as_ref()
                    .map(|e| e.subtitle.clone())
                    .unwrap_or_default(),
                uri: uri.into(),
                image: entry.map(|e| e.image).unwrap_or_default(),
                owned,
                ..Default::default()
            },
        };
        self.browse.artist_shelves.clear();
        self.browse.credited.clear();
        // Playlist editing defaults to the open playlist.
        ui.set_collection_uri(uri.into());
        ui.set_collection_title(self.browse.header.title.clone().into());
        ui.set_collection_owned(owned);
        ui.set_page(COLLECTION);
        ui.set_feature_title(self.browse.header.title.clone().into());
        let action = match kind {
            "album" => Action::Album {
                uri: uri.into(),
                offset: 0,
            },
            "mix" => Action::Mix,
            _ => Action::Collection {
                uri: uri.into(),
                offset: 0,
            },
        };
        if kind == "mix" {
            self.view_page = COLLECTION;
            match self.browse.cache.get("mix") {
                Some(Hit::Fresh(data)) => return self.apply_cached(ui, action, data),
                Some(Hit::Stale(data)) => {
                    self.apply_cached(ui, action.clone(), data);
                    return self.refresh_quietly(ui, action);
                }
                None => {}
            }
        }
        self.load(ui, action, false);
    }

    /// Header details that only the response knows: counts, album facts, restrictions.
    pub(super) fn collection_loaded(&mut self, ui: &GroveWindow, data: &Value) {
        let h = &mut self.browse.header;
        let count = |n: u64| {
            if n == 1 {
                "1 song".to_owned()
            } else {
                format!("{n} songs")
            }
        };
        match data["kind"].as_str() {
            Some("collection_page") if data["uri"].as_str() == Some(h.uri.as_str()) => {
                if let Some(total) = data["total"].as_u64() {
                    let owner = h
                        .subtitle
                        .split(" · ")
                        .next()
                        .unwrap_or_default()
                        .to_owned();
                    h.subtitle = if owner.is_empty() {
                        count(total)
                    } else {
                        format!("{owner} · {}", count(total))
                    };
                }
                h.note = data["note"].as_str().unwrap_or_default().into();
            }
            Some("album")
                if data["uri"].as_str() == Some(h.uri.as_str()) && data["offset"] == 0 =>
            {
                if let Some(title) = data["title"].as_str() {
                    h.title = title.into();
                    ui.set_feature_title(title.into());
                }
                h.label = if data["album_type"] == "single" {
                    "SINGLE"
                } else {
                    "ALBUM"
                }
                .into();
                h.image = data["image"].as_str().unwrap_or_default().into();
                let artists: Vec<&str> = data["artists"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|a| a["name"].as_str())
                    .collect();
                let year = data["release_date"].as_str().and_then(|d| d.get(..4));
                let mut parts = vec![artists.join(", ")];
                parts.extend(year.map(str::to_owned));
                parts.extend(data["total"].as_u64().map(count));
                h.subtitle = parts
                    .into_iter()
                    .filter(|p| !p.is_empty())
                    .collect::<Vec<_>>()
                    .join(" · ");
            }
            Some("mix") => {
                h.note = data["note"].as_str().unwrap_or_default().into();
                h.subtitle = count(self.rows.len() as u64);
                if data["cached"] != true {
                    self.browse.cache.put("mix", data);
                }
            }
            _ => {}
        }
        if data["uri"] == "liked" {
            for t in &self.rows {
                self.browse.saved.insert(t.uri.clone());
            }
        }
        ui.set_feature_message("".into());
        self.clear_cooldown(ui);
    }

    /// The exact entry to remove: the row the user chose, with its playlist
    /// position and the positions of other loaded copies of the same song.
    fn removal_action(
        &mut self,
        ui: &GroveWindow,
        playlist: &str,
        uri: &str,
    ) -> Result<Action, String> {
        let open =
            matches!(&self.source, Some(Action::Collection { uri: open, .. }) if open == playlist);
        let row = self
            .browse
            .removal
            .take()
            .filter(|&i| open && self.rows.get(i).is_some_and(|t| t.uri == uri))
            .filter(|&i| i < self.browse.positions.len());
        let Some(row) = row else {
            return Err(
                "Open the playlist and choose Remove on the entry you want to remove.".into(),
            );
        };
        let copies = self
            .rows
            .iter()
            .zip(&self.browse.positions)
            .enumerate()
            .filter(|(i, (t, _))| *i != row && t.uri == uri)
            .map(|(_, (_, &p))| p)
            .collect();
        let _ = ui;
        Ok(Action::PlaylistRemoveEntry {
            playlist: playlist.into(),
            uri: uri.into(),
            position: self.browse.positions[row],
            copies,
        })
    }

    pub(super) fn playlists_loaded(&mut self, ui: &GroveWindow, data: &Value) {
        let mut rows: Vec<_> = ui.get_playlists().iter().collect();
        let mut owned: Vec<_> = ui.get_owned_playlists().iter().collect();
        for p in data["items"].as_array().into_iter().flatten() {
            if let (Some(title), Some(uri)) = (p["name"].as_str(), p["uri"].as_str()) {
                if p["owner"]["id"].is_string()
                    && p["owner"]["id"] == data["account"]
                    && !owned.iter().any(|p| p.uri.as_str() == uri)
                {
                    owned.push(GrovePlaylist {
                        title: title.into(),
                        uri: uri.into(),
                        subtitle: "Your playlist".into(),
                    });
                }
                rows.push(GrovePlaylist {
                    title: title.into(),
                    uri: uri.into(),
                    subtitle: p["owner"]["display_name"]
                        .as_str()
                        .unwrap_or("Playlist")
                        .into(),
                });
            }
        }
        rows.truncate(2000);
        let has_more = data["has_more"] == true && rows.len() < 2000;
        owned.truncate(2000);
        ui.set_owned_playlists(ModelRc::new(VecModel::from(owned)));
        ui.set_playlists(ModelRc::new(VecModel::from(rows)));
        ui.set_playlists_more(has_more);
        ui.set_library_note("".into());
        self.add_playlists(data);
        self.render(ui);
    }

    pub(super) fn mutation_confirmed(&mut self, ui: &GroveWindow, data: &Value) {
        ui.set_feature_message(
            data["message"]
                .as_str()
                .unwrap_or("Spotify confirmed the change.")
                .into(),
        );
        if data["uri"].as_str() == Some(ui.get_current_uri().as_str()) && data["saved"].is_boolean()
        {
            ui.set_current_saved(data["saved"] == true);
        }
        if let Some(uri) = data["uri"].as_str().filter(|_| data["saved"].is_boolean()) {
            if data["saved"] == true {
                self.browse.saved.insert(uri.into());
            } else {
                self.browse.saved.remove(uri);
            }
        }
        if data["saved"] == false
            && matches!(&self.source,Some(Action::Collection{uri,..}) if uri=="liked")
        {
            let keep: Vec<bool> = self
                .rows
                .iter()
                .map(|t| Some(t.uri.as_str()) != data["uri"].as_str())
                .collect();
            let mut flags = keep.iter();
            self.rows.retain(|_| *flags.next().unwrap_or(&true));
            if self.browse.positions.len() == keep.len() {
                let mut flags = keep.iter();
                self.browse
                    .positions
                    .retain(|_| *flags.next().unwrap_or(&true));
            }
            self.render(ui);
        }
        let removed = &data["removed"];
        if let (Some(uri), Some(position)) = (removed["uri"].as_str(), removed["position"].as_u64())
        {
            let open = matches!(&self.source, Some(Action::Collection { uri: open, .. }) if Some(open.as_str()) == data["uri"].as_str());
            let position = position as u32;
            if let Some(i) = self
                .browse
                .positions
                .iter()
                .zip(&self.rows)
                .position(|(&p, t)| open && p == position && t.uri == uri)
            {
                self.rows.remove(i);
                self.browse.positions.remove(i);
                for p in self.browse.positions.iter_mut().filter(|p| **p > position) {
                    *p -= 1;
                }
                self.render(ui);
            }
        }
    }
}
