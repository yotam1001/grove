//! Library collections: opening playlists and Liked Songs, paging playlists, editing them.
use super::{Features, Pending};
use crate::{library::Action, spotify, GrovePlaylist, GroveWindow};
use serde_json::Value;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::{cell::RefCell, rc::Rc};

pub(super) fn bind(ui: &GroveWindow, this: &Rc<RefCell<Features>>) {
    let f = this.clone();
    let weak = ui.as_weak();
    ui.on_browse(move |uri, title| {
        if let Some(ui) = weak.upgrade() {
            ui.set_collection_uri(uri.clone());
            ui.set_collection_owned(ui.get_owned_playlists().iter().any(|p| p.uri == uri));
            ui.set_collection_title(title);
            f.borrow_mut().load(
                &ui,
                Action::Collection {
                    uri: uri.into(),
                    offset: 0,
                },
                false,
            );
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
                Action::PlaylistRemove { playlist, uri }
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
        if data["saved"] == false
            && matches!(&self.source,Some(Action::Collection{uri,..}) if uri=="liked")
        {
            self.rows
                .retain(|t| Some(t.uri.as_str()) != data["uri"].as_str());
            self.render(ui);
        }
    }
}
