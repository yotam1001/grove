//! Page navigation: page switches, a Back stack for collection, artist and radio
//! pages, refresh and pagination of loaded track lists.
use super::{
    browse::{parse_tracks, HOME, LIBRARY, SEARCH},
    Features,
};
use crate::{library::Action, GroveBrowse, GroveWindow};
use serde_json::Value;
use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc};

pub(super) fn bind(ui: &GroveWindow, this: &Rc<RefCell<Features>>) {
    let f = this.clone();
    let weak = ui.as_weak();
    ui.on_page_changed(move |page| {
        if let Some(ui) = weak.upgrade() {
            let Ok(mut f) = f.try_borrow_mut() else {
                return;
            };
            if f.view_page == page {
                return;
            }
            let left = f.view_page;
            f.keep_visited(&ui, left);
            // Pages Grove opens itself set `view_page` first; this is the user
            // choosing a page, which starts a new Back history.
            f.view_page = page;
            f.epoch += 1;
            f.browse.stack.clear();
            ui.set_feature_busy(false);
            ui.set_feature_message("".into());
            ui.global::<GroveBrowse>().set_choice_row(-1);
            match page {
                LIBRARY => {
                    let tab = ui.global::<GroveBrowse>().get_library_tab();
                    f.library_tab(&ui, tab, false);
                }
                HOME => f.show_home(&ui, false),
                SEARCH => f.show_search(&ui),
                9 => {
                    f.rows = f.history.clone();
                    f.source = None;
                    f.render(&ui);
                }
                _ => {}
            }
        }
    });
    let f = this.clone();
    let weak = ui.as_weak();
    ui.on_feature_back(move || {
        if let Some(ui) = weak.upgrade() {
            let mut f = f.borrow_mut();
            if !f.go_back(&ui) {
                f.view_page = -1;
                drop(f);
                ui.set_page(HOME);
            }
        }
    });
    let f = this.clone();
    let weak = ui.as_weak();
    ui.on_feature_refresh(move || {
        if let Some(ui) = weak.upgrade() {
            let mut f = f.borrow_mut();
            match ui.get_page() {
                HOME => f.show_home(&ui, true),
                LIBRARY => {
                    let tab = ui.global::<GroveBrowse>().get_library_tab();
                    f.library_tab(&ui, tab, true);
                }
                _ => {
                    if let Some(action) = f.source.clone() {
                        f.load(&ui, first_page(action), false);
                    }
                }
            }
        }
    });
    let f = this.clone();
    let weak = ui.as_weak();
    ui.on_feature_next(move || {
        if let Some(ui) = weak.upgrade() {
            let mut f = f.borrow_mut();
            if let Some(action) = f.source.clone() {
                if let Some(next) = next_page(action) {
                    f.load(&ui, next, true);
                }
            }
        }
    });
}

impl Features {
    /// Remembers the current page so Back can return to it.
    pub(super) fn remember(&mut self, ui: &GroveWindow) {
        self.push_snapshot(ui);
    }

    fn go_back(&mut self, ui: &GroveWindow) -> bool {
        let Some(previous) = self.browse.stack.pop() else {
            return false;
        };
        self.epoch += 1;
        self.view_page = previous.page;
        self.rows = previous.rows;
        self.browse.positions = previous.positions;
        self.source = previous.source;
        self.browse.header = previous.header;
        self.browse.artist_shelves = previous.shelves;
        self.browse.credited = previous.credited;
        let (uri, title, owned) = previous.collection;
        ui.set_collection_uri(uri);
        ui.set_collection_title(title);
        ui.set_collection_owned(owned);
        ui.set_page(previous.page);
        ui.set_feature_title(previous.title);
        ui.set_feature_more(previous.more);
        ui.set_feature_busy(false);
        ui.set_feature_message("".into());
        ui.global::<GroveBrowse>().set_choice_row(-1);
        self.render(ui);
        true
    }

    pub(super) fn page_loaded(&mut self, ui: &GroveWindow, data: &Value, append: bool) {
        let (rows, positions) = parse_tracks(&mut self.browse, data);
        if !append {
            self.rows.clear();
            self.browse.positions.clear();
        }
        self.rows.extend(rows);
        self.browse.positions.extend(positions);
        self.rows.truncate(2000);
        self.browse.positions.truncate(self.rows.len());
        let kind = data["kind"].as_str().unwrap_or_default();
        match kind {
            "home" => {
                self.rotation = self.rows.clone();
                self.home_loaded(ui, data);
            }
            "search_all" => self.search_loaded(ui, data),
            "artist_page" => self.artist_loaded(ui, data),
            "collection_page" | "album" | "mix" => self.collection_loaded(ui, data),
            _ => {
                if let Some(title) = data["title"].as_str() {
                    ui.set_feature_title(title.into());
                }
            }
        }
        ui.set_feature_more(
            data["has_more"] == true
                && self.rows.len() < 2000
                && self.source.clone().and_then(next_page).is_some(),
        );
        // Card pages show their own empty states and notes.
        let quiet = matches!(kind, "home" | "search_all" | "artist_page")
            || data["restricted"] == true
            || (matches!(kind, "collection_page" | "album" | "mix") && !self.rows.is_empty());
        if quiet {
            ui.set_feature_message("".into());
        } else {
            ui.set_feature_message(
                if self.rows.is_empty() {
                    "No playable tracks were returned."
                } else {
                    data["note"].as_str().unwrap_or("")
                }
                .into(),
            );
        }
        if std::mem::take(&mut self.browse.play_on_load) && !self.rows.is_empty() {
            let weak = ui.as_weak();
            slint::Timer::single_shot(std::time::Duration::ZERO, move || {
                if let Some(ui) = weak.upgrade() {
                    ui.invoke_play_selection(false);
                }
            });
        }
        self.render(ui);
    }
}

pub(super) fn first_page(action: Action) -> Action {
    match action {
        Action::Search { query, .. } => Action::SearchAll { query },
        Action::Collection { uri, .. } => Action::Collection { uri, offset: 0 },
        Action::Album { uri, .. } => Action::Album { uri, offset: 0 },
        other => other,
    }
}
fn next_page(action: Action) -> Option<Action> {
    match action {
        Action::SearchAll { query } => Some(Action::Search { query, offset: 10 }),
        Action::Search { query, offset } if offset < 1000 => Some(Action::Search {
            query,
            offset: offset + 10,
        }),
        Action::Collection { uri, offset } if offset < 100_000 => Some(Action::Collection {
            uri,
            offset: offset + 50,
        }),
        Action::Album { uri, offset } if offset < 10_000 => Some(Action::Album {
            uri,
            offset: offset + 50,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pagination_preserves_collection_and_search() {
        assert!(
            matches!(next_page(Action::Search{query:"a & b".into(),offset:20}),Some(Action::Search{query,offset:30}) if query=="a & b")
        );
        assert!(matches!(
            next_page(Action::SearchAll { query: "a".into() }),
            Some(Action::Search { offset: 10, .. })
        ));
        assert!(matches!(
            next_page(Action::Album {
                uri: "x".into(),
                offset: 0
            }),
            Some(Action::Album { offset: 50, .. })
        ));
        assert!(next_page(Action::Home).is_none());
        assert!(next_page(Action::Mix).is_none());
        assert!(next_page(Action::Search {
            query: "a".into(),
            offset: 1000
        })
        .is_none());
        assert!(matches!(
            first_page(Action::Search {
                query: "a".into(),
                offset: 40
            }),
            Action::SearchAll { .. }
        ));
    }
}
