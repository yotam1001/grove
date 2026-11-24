//! Library tabs: Playlists (with Liked Songs and the Discovery mix pinned),
//! saved Albums, followed Artists and History, plus filtering inside the list.
use super::{
    browse::{parse_tracks, Entry, Retry, Want, LIBRARY},
    Features, Pending,
};
use crate::{
    browse_cache::{BrowseCache, Hit},
    library::Action,
    state::Track,
    GroveBrowse, GroveWindow,
};
use serde_json::Value;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::{cell::RefCell, rc::Rc};

pub(super) fn bind(ui: &GroveWindow, this: &Rc<RefCell<Features>>) {
    let g = ui.global::<GroveBrowse>();
    let f = this.clone();
    let weak = ui.as_weak();
    g.on_library_tab_changed(move |tab| {
        if let Some(ui) = weak.upgrade() {
            // Tabs 10-13 are "refresh tab 0-3".
            f.borrow_mut().library_tab(&ui, tab % 10, tab >= 10);
        }
    });
    let f = this.clone();
    let weak = ui.as_weak();
    g.on_library_filter_changed(move |_| {
        if let Some(ui) = weak.upgrade() {
            f.borrow_mut().render(&ui);
        }
    });
    let f = this.clone();
    let weak = ui.as_weak();
    g.on_library_next(move || {
        if let Some(ui) = weak.upgrade() {
            f.borrow_mut().library_next(&ui);
        }
    });
}

/// A Library or Home card for a playlist from `/me/playlists`.
pub(super) fn playlist_entry(p: &Value, account: &str) -> Option<Entry> {
    let title = p["name"].as_str().filter(|t| !t.is_empty())?;
    let uri = p["uri"]
        .as_str()
        .filter(|u| u.starts_with("spotify:playlist:"))?;
    let owner = if p["owner"]["id"]
        .as_str()
        .is_some_and(|o| o == account && !o.is_empty())
    {
        "By you".to_owned()
    } else {
        p["owner"]["display_name"]
            .as_str()
            .map(|o| format!("By {o}"))
            .unwrap_or_else(|| "Playlist".into())
    };
    // February 2026 renamed a playlist's `tracks` object to `items`.
    let count = p["items"]["total"]
        .as_u64()
        .or_else(|| p["tracks"]["total"].as_u64());
    let subtitle = match count {
        Some(1) => format!("{owner} · 1 song"),
        Some(n) => format!("{owner} · {n} songs"),
        None => owner,
    };
    Some(Entry {
        kind: "playlist".into(),
        uri: uri.into(),
        title: title.into(),
        subtitle,
        image: crate::web_api::pick_image(&p["images"])
            .as_str()
            .unwrap_or_default()
            .into(),
        index: -1,
    })
}

impl Features {
    /// The library grant is ready: bind the page cache to this account and
    /// remember playlist artwork for Library, Home and collection headers.
    pub fn library_ready(&mut self, ui: &GroveWindow, playlists: &Value) {
        let account = playlists["account"].as_str().unwrap_or_default();
        if !account.is_empty() && self.browse.cache.account() != account {
            self.browse.cache = if self.offline {
                let mut cache = BrowseCache::memory();
                cache.bind(account);
                cache
            } else {
                BrowseCache::open(account)
            };
            self.browse.albums.clear();
            self.browse.artists.clear();
            self.browse.remote_recent.clear();
        }
        self.browse.playlists = playlists["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| playlist_entry(p, account))
            .collect();
        self.show_recent_searches(ui);
        self.render(ui);
    }

    /// The note under the Playlists tab for a page from the Spotify worker. A
    /// cooldown shows the banner with Retry while the Playlists tab is open, and
    /// a page from the last launch says so instead of passing as live.
    pub fn playlists_note(&mut self, ui: &GroveWindow, playlists: &Value) -> &'static str {
        let shown = !playlists["items"].as_array().is_none_or(|i| i.is_empty());
        let on_tab = ui.get_page() == LIBRARY && ui.global::<GroveBrowse>().get_library_tab() == 0;
        if playlists["cooldown"].is_object() {
            if on_tab {
                self.show_cooldown(ui, &playlists["cooldown"], Some(Retry::Tab(0)));
            }
            return if shown {
                "Spotify is busy, so these are your playlists from last time. Refresh when the cooldown ends."
            } else {
                "Spotify is busy. Your playlists load when the cooldown ends."
            };
        }
        if on_tab && playlists["cached"] != true {
            self.clear_cooldown(ui);
        }
        if playlists["load_failed"] == true {
            "Playlist loading failed. Use Refresh to retry."
        } else if playlists["error"].is_string() && playlists["cached"] == true {
            "Spotify could not be reached, so these are your playlists from last time."
        } else if playlists["stale"] == true {
            "Showing your playlists from last time while Grove refreshes them."
        } else if !playlists["next"].is_null() {
            "Showing the first 50 playlists."
        } else {
            "Your saved playlists"
        }
    }

    pub(super) fn add_playlists(&mut self, data: &Value) {
        let account = data["account"].as_str().unwrap_or_default();
        for entry in data["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| playlist_entry(p, account))
        {
            if !self.browse.playlists.iter().any(|e| e.uri == entry.uri) {
                self.browse.playlists.push(entry);
            }
        }
        self.browse.playlists.truncate(2000);
    }

    pub(super) fn show_recent_searches(&self, ui: &GroveWindow) {
        let recent: Vec<SharedString> = self
            .browse
            .cache
            .recent_searches
            .iter()
            .map(|q| q.as_str().into())
            .collect();
        ui.global::<GroveBrowse>()
            .set_recent_searches(ModelRc::new(VecModel::from(recent)));
    }

    pub(super) fn library_tab(&mut self, ui: &GroveWindow, tab: i32, force: bool) {
        let g = ui.global::<GroveBrowse>();
        g.set_library_tab(tab);
        g.set_choice_row(-1);
        g.set_library_note("".into());
        self.view_page = LIBRARY;
        self.epoch += 1;
        ui.set_feature_busy(false);
        ui.set_feature_more(false);
        self.rows.clear();
        self.browse.positions.clear();
        self.source = None;
        if !ui.get_library_ready() {
            self.render(ui);
            return;
        }
        let (key, action) = match tab {
            1 => ("library:albums", Action::SavedAlbums { offset: 0 }),
            2 => ("library:artists", Action::FollowedArtists { after: None }),
            3 => ("library:recent", Action::RecentlyPlayed),
            _ => {
                if force {
                    ui.invoke_refresh_library();
                }
                self.render(ui);
                return;
            }
        };
        let loaded = match tab {
            1 => !self.browse.albums.is_empty(),
            2 => !self.browse.artists.is_empty(),
            _ => !self.browse.remote_recent.is_empty(),
        };
        if tab == 3 {
            self.rows = self.history_rows();
        }
        // What this session already loaded comes first; the disk cache next.
        if loaded && !force {
            self.render(ui);
            return;
        }
        let hit = (!force).then(|| self.browse.cache.get(key)).flatten();
        let stale = match hit {
            Some(Hit::Fresh(data)) => {
                self.library_loaded(ui, tab, false, &data, true);
                false
            }
            Some(Hit::Stale(data)) => {
                self.library_loaded(ui, tab, false, &data, true);
                true
            }
            None => force || !loaded,
        };
        if stale {
            self.request_tab(ui, tab, action, false);
        }
        self.render(ui);
    }

    fn request_tab(&mut self, ui: &GroveWindow, tab: i32, action: Action, append: bool) {
        let want = Want::LibraryTab {
            tab,
            append,
            epoch: self.epoch,
        };
        if self.request(action, Pending::Browse(want)).is_some() {
            ui.set_feature_busy(true);
        } else {
            ui.set_feature_message("Wait for the current requests to finish.".into());
        }
    }

    pub(super) fn library_next(&mut self, ui: &GroveWindow) {
        if ui.get_feature_busy() {
            return;
        }
        let tab = ui.global::<GroveBrowse>().get_library_tab();
        let action = match tab {
            1 if self.browse.albums_more => Action::SavedAlbums {
                offset: self.browse.albums.len() as u32,
            },
            2 if self.browse.artists_after.is_some() => Action::FollowedArtists {
                after: self.browse.artists_after.clone(),
            },
            _ => return,
        };
        self.request_tab(ui, tab, action, true);
    }

    /// Session history first (newest), then Spotify's recently played without repeats.
    pub(super) fn history_rows(&self) -> Vec<Track> {
        let mut seen = std::collections::HashSet::new();
        self.history
            .iter()
            .chain(self.browse.remote_recent.iter())
            .filter(|t| seen.insert(t.uri.clone()))
            .take(150)
            .cloned()
            .collect()
    }

    pub(super) fn library_loaded(
        &mut self,
        ui: &GroveWindow,
        tab: i32,
        append: bool,
        data: &Value,
        cached: bool,
    ) {
        let note = match data["kind"].as_str() {
            Some("library_albums") => {
                let entries = Entry::from_cards(&data["cards"]);
                if append {
                    self.browse.albums.extend(entries);
                } else {
                    self.browse.albums = entries;
                }
                self.browse.albums.truncate(2000);
                self.browse.albums_more =
                    data["has_more"] == true && self.browse.albums.len() < 2000;
                if !append && !cached {
                    self.browse.cache.put("library:albums", data);
                }
                (self.browse.albums.is_empty()).then_some("Albums you save in Spotify appear here.")
            }
            Some("library_artists") => {
                let entries = Entry::from_cards(&data["cards"]);
                if append {
                    self.browse.artists.extend(entries);
                } else {
                    self.browse.artists = entries;
                }
                self.browse.artists.truncate(2000);
                self.browse.artists_after = data["after"]
                    .as_str()
                    .filter(|a| !a.is_empty() && self.browse.artists.len() < 2000)
                    .map(str::to_owned);
                if !append && !cached {
                    self.browse.cache.put("library:artists", data);
                }
                (self.browse.artists.is_empty()).then_some("Artists you follow appear here.")
            }
            Some("recently_played") => {
                let (tracks, _) = parse_tracks(&mut self.browse, data);
                self.browse.remote_recent = tracks;
                if !cached {
                    self.browse.cache.put("library:recent", data);
                }
                if ui.global::<GroveBrowse>().get_library_tab() == 3 {
                    self.rows = self.history_rows();
                }
                self.rows.is_empty().then_some(if self.prefs.local_history {
                    "Songs you listen to will appear here."
                } else {
                    "Turn on session history in Settings to see this session's songs here."
                })
            }
            _ => None,
        };
        let _ = tab;
        if !cached {
            self.clear_cooldown(ui);
        }
        ui.global::<GroveBrowse>()
            .set_library_note(note.unwrap_or_default().into());
        self.render(ui);
    }

    /// A failed tab load. Missing scopes degrade to what Grove already has.
    pub(super) fn library_failed(&mut self, ui: &GroveWindow, tab: i32, error: String) {
        let g = ui.global::<GroveBrowse>();
        match tab {
            3 => {
                g.set_library_note(
                    "Showing this session's history. Sign in again to include Spotify's recently played songs."
                        .into(),
                );
                self.rows = self.history_rows();
                self.render(ui);
            }
            2 if error.contains("does not make this available") => {
                g.set_library_note("Sign in again to let Grove read the artists you follow.".into())
            }
            _ => ui.set_feature_message(error.into()),
        }
    }

    pub(super) fn library_throttled(&mut self, ui: &GroveWindow, tab: i32, data: &Value) {
        self.show_cooldown(ui, data, Some(Retry::Tab(tab)));
        if tab == 3 {
            self.rows = self.history_rows();
            self.render(ui);
        }
    }
}
