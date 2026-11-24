//! Search: a pasted Spotify link plays directly, anything else opens the All
//! view (Top result, Songs, Artists, Albums, Playlists). Recent searches are
//! remembered per account. Each new search bumps the page epoch, so the latest
//! search always wins over a late reply to an older one.
use super::{
    browse::{Entry, Want, SEARCH},
    Features, Pending,
};
use crate::{library::Action, spotify, GroveBrowse, GroveWindow};
use serde_json::Value;
use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc};

pub(super) fn bind(ui: &GroveWindow, this: &Rc<RefCell<Features>>) {
    let f = this.clone();
    let weak = ui.as_weak();
    ui.on_search(move |query| {
        if let Some(ui) = weak.upgrade() {
            if let Ok(uri) = spotify::spotify_uri(&query) {
                ui.invoke_play_uri(uri.into());
                return;
            }
            f.borrow_mut().search_all(&ui, &query);
        }
    });
}

pub(super) fn bind_browse(ui: &GroveWindow, this: &Rc<RefCell<Features>>) {
    let g = ui.global::<GroveBrowse>();
    let f = this.clone();
    let weak = ui.as_weak();
    g.on_search_tab_changed(move |tab| {
        if let Some(ui) = weak.upgrade() {
            f.borrow_mut().search_tab(&ui, tab);
        }
    });
    let f = this.clone();
    let weak = ui.as_weak();
    g.on_search_more(move || {
        if let Some(ui) = weak.upgrade() {
            f.borrow_mut().search_more(&ui);
        }
    });
    let f = this.clone();
    let weak = ui.as_weak();
    g.on_recent_search(move |query| {
        if let Some(ui) = weak.upgrade() {
            ui.set_search_text(query.clone());
            f.borrow_mut().search_all(&ui, &query);
        }
    });
    let f = this.clone();
    let weak = ui.as_weak();
    g.on_clear_recent(move || {
        if let Some(ui) = weak.upgrade() {
            let mut f = f.borrow_mut();
            f.browse.cache.forget_searches();
            f.show_recent_searches(&ui);
        }
    });
}

const KINDS: [&str; 3] = ["album", "artist", "playlist"];

impl Features {
    pub(super) fn search_all(&mut self, ui: &GroveWindow, query: &str) {
        let query = query.trim();
        let g = ui.global::<GroveBrowse>();
        if query.is_empty() {
            g.set_search_query("".into());
            self.browse.search = None;
            self.rows.clear();
            self.render(ui);
            return;
        }
        if !ui.get_library_ready() {
            ui.set_feature_message("Connect Spotify to browse your music.".into());
            return;
        }
        g.set_search_query(query.into());
        g.set_search_tab(0);
        g.set_choice_row(-1);
        self.browse.search = None;
        self.browse.search_cards.clear();
        self.browse.cache.remember_search(query);
        self.show_recent_searches(ui);
        self.load(
            ui,
            Action::SearchAll {
                query: query.into(),
            },
            false,
        );
    }

    pub(super) fn search_loaded(&mut self, ui: &GroveWindow, data: &Value) {
        self.browse.search = Some(data.clone());
        self.clear_cooldown(ui);
        let tab = ui.global::<GroveBrowse>().get_search_tab();
        if tab >= 2 {
            self.search_tab(ui, tab);
        }
    }

    /// Switching tabs never contacts Spotify: Albums, Artists and Playlists start
    /// with the All view's cards and page with "Load more".
    pub(super) fn search_tab(&mut self, ui: &GroveWindow, tab: i32) {
        let g = ui.global::<GroveBrowse>();
        g.set_search_tab(tab.clamp(0, 4));
        g.set_choice_row(-1);
        if let (Some(kind), Some(data)) = (
            KINDS.get((tab - 2).max(-1) as usize).filter(|_| tab >= 2),
            self.browse.search.as_ref(),
        ) {
            let key = format!("{kind}s");
            self.browse.search_cards = Entry::from_cards(&data[&key]);
            self.browse.search_cards_more = self.browse.search_cards.len() >= 10;
        }
        self.render(ui);
    }

    pub(super) fn search_more(&mut self, ui: &GroveWindow) {
        let g = ui.global::<GroveBrowse>();
        let tab = g.get_search_tab();
        let Some(kind) = KINDS.get((tab - 2).max(0) as usize).filter(|_| tab >= 2) else {
            return;
        };
        if ui.get_feature_busy() || !self.browse.search_cards_more {
            return;
        }
        let action = Action::SearchKind {
            query: g.get_search_query().to_string(),
            kind: (*kind).into(),
            offset: self.browse.search_cards.len() as u32,
        };
        let want = Want::SearchKind { epoch: self.epoch };
        if self.request(action, Pending::Browse(want)).is_some() {
            ui.set_feature_busy(true);
        }
    }

    pub(super) fn search_kind_loaded(&mut self, ui: &GroveWindow, data: &Value) {
        let g = ui.global::<GroveBrowse>();
        let expected = KINDS.get((g.get_search_tab() - 2).max(0) as usize);
        if data["query"].as_str() != Some(g.get_search_query().as_str())
            || expected.copied() != data["type"].as_str()
        {
            return;
        }
        let more = Entry::from_cards(&data["cards"]);
        for entry in more {
            if !self.browse.search_cards.iter().any(|e| e.uri == entry.uri) {
                self.browse.search_cards.push(entry);
            }
        }
        self.browse.search_cards.truncate(500);
        self.browse.search_cards_more =
            data["has_more"] == true && self.browse.search_cards.len() < 500;
        self.render(ui);
    }

    /// Returning to Search shows the last results without another request.
    pub(super) fn show_search(&mut self, ui: &GroveWindow) {
        self.view_page = SEARCH;
        if let Some(data) = self.browse.search.clone() {
            let query = data["query"].as_str().unwrap_or_default().to_owned();
            self.apply_cached(ui, Action::SearchAll { query }, data);
        } else {
            self.rows.clear();
            self.source = None;
            self.render(ui);
        }
    }
}
