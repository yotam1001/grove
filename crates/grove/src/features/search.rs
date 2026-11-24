//! Search: a pasted Spotify link plays directly, anything else searches the catalogue.
use super::Features;
use crate::{library::Action, spotify, GroveWindow};
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
            f.borrow_mut().load(
                &ui,
                Action::Search {
                    query: query.into(),
                    offset: 0,
                },
                false,
            );
        }
    });
}
