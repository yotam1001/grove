//! Page navigation: page switches, Back, refresh and pagination of loaded track lists.
use super::Features;
use crate::{library::Action, state::Track, GroveWindow};
use serde_json::Value;
use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc};

pub(super) struct PreviousPage {
    page: i32,
    rows: Vec<Track>,
    source: Option<Action>,
    title: slint::SharedString,
    more: bool,
}

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
            f.view_page = page;
            f.epoch += 1;
            ui.set_feature_busy(false);
            ui.set_feature_message("".into());
            match page {
                3 if !ui.get_collection_uri().is_empty() => f.load(
                    &ui,
                    Action::Collection {
                        uri: ui.get_collection_uri().into(),
                        offset: 0,
                    },
                    false,
                ),
                5 => f.load(&ui, Action::Home, false),
                6 => {
                    f.rows.clear();
                    f.source = None;
                    f.render(&ui);
                }
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
            if let Some(previous) = f.previous.take() {
                f.epoch += 1;
                f.view_page = previous.page;
                f.rows = previous.rows;
                f.source = previous.source;
                ui.set_page(previous.page);
                ui.set_feature_title(previous.title);
                ui.set_feature_more(previous.more);
                ui.set_feature_busy(false);
                ui.set_feature_message("".into());
                f.render(&ui);
            }
        }
    });
    let f = this.clone();
    let weak = ui.as_weak();
    ui.on_feature_refresh(move || {
        if let Some(ui) = weak.upgrade() {
            let mut f = f.borrow_mut();
            if let Some(action) = f.source.clone() {
                f.load(&ui, first_page(action), false);
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
    /// Remembers the current list so Back can return to it from an artist or radio page.
    pub(super) fn remember(&mut self, ui: &GroveWindow) {
        if ui.get_page() != 7 {
            self.previous = Some(PreviousPage {
                page: ui.get_page(),
                rows: self.rows.clone(),
                source: self.source.clone(),
                title: ui.get_feature_title(),
                more: ui.get_feature_more(),
            });
        }
    }
    pub(super) fn page_loaded(&mut self, ui: &GroveWindow, data: &Value, append: bool) {
        let collection = data["kind"] == "collection_page";
        let items = if collection {
            &data["items"]
        } else {
            &data["tracks"]
        };
        let rows = items
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| {
                let item = if collection {
                    v.get("item").or_else(|| v.get("track"))?
                } else {
                    v
                };
                if item["uri"]
                    .as_str()
                    .is_some_and(|u| u.starts_with("spotify:track:"))
                {
                    Some(Track::from_entity(&crate::web_api::entity(item)))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        if !append {
            self.rows.clear();
        }
        self.rows.extend(rows);
        self.rows.truncate(2000);
        if data["kind"] == "home" {
            self.rotation = self.rows.clone();
        }
        if let Some(title) = data["title"].as_str() {
            ui.set_feature_title(title.into());
        }
        ui.set_feature_more(
            data["has_more"] == true
                && self.rows.len() < 2000
                && self.source.clone().and_then(next_page).is_some(),
        );
        ui.set_feature_message(
            if self.rows.is_empty() {
                "No playable tracks were returned."
            } else {
                data["note"].as_str().unwrap_or("")
            }
            .into(),
        );
        self.render(ui);
    }
}

pub(super) fn first_page(action: Action) -> Action {
    match action {
        Action::Search { query, .. } => Action::Search { query, offset: 0 },
        Action::Collection { uri, .. } => Action::Collection { uri, offset: 0 },
        other => other,
    }
}
fn next_page(action: Action) -> Option<Action> {
    match action {
        Action::Search { query, offset } if offset < 1000 => Some(Action::Search {
            query,
            offset: offset + 10,
        }),
        Action::Collection { uri, offset } if offset < 100_000 => Some(Action::Collection {
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
        assert!(next_page(Action::Home).is_none());
        assert!(next_page(Action::Search {
            query: "a".into(),
            offset: 1000
        })
        .is_none());
    }
}
