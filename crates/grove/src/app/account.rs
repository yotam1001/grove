//! Account callbacks: library sign-in, playback approval, sign-out and reconnects.
use crate::{
    features::Features,
    spotify::{Request, RequestSender},
    GroveWindow,
};
use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc};

pub fn bind(ui: &GroveWindow, tx: &RequestSender, features: &Rc<RefCell<Features>>) {
    let sender = tx.clone();
    let weak = ui.as_weak();
    let feature_state = features.clone();
    ui.on_connect_engine(move || {
        if let Some(ui) = weak.upgrade() {
            feature_state.borrow_mut().invalidate(&ui);
        }
        let _ = sender.send(Request::StartPlayback(false));
    });
    let weak = ui.as_weak();
    let sender = tx.clone();
    let feature_state = features.clone();
    ui.on_sign_in_library(move || {
        if let Some(ui) = weak.upgrade() {
            feature_state.borrow_mut().invalidate(&ui);
            let _ = sender.send(Request::SignInLibrary(ui.get_client_id().to_string()));
        }
    });
    let sender = tx.clone();
    ui.on_sign_out(move || {
        let _ = sender.send(Request::SignOut);
    });
    let sender = tx.clone();
    let weak = ui.as_weak();
    let feature_state = features.clone();
    ui.on_cancel_sign_in(move || {
        if let Some(ui) = weak.upgrade() {
            feature_state.borrow_mut().invalidate(&ui);
        }
        let _ = sender.send(Request::Cancel);
    });
    let sender = tx.clone();
    let weak = ui.as_weak();
    let feature_state = features.clone();
    ui.on_approve_playback(move || {
        if let Some(ui) = weak.upgrade() {
            feature_state.borrow_mut().invalidate(&ui);
        }
        let _ = sender.send(Request::StartPlayback(true));
    });
    let weak = ui.as_weak();
    let sender = tx.clone();
    ui.on_refresh_library(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_message("Refreshing your playlists…".into());
            let _ = sender.send(Request::RefreshLibrary);
        }
    });
    let sender = tx.clone();
    ui.on_disconnect_engine(move || {
        let _ = sender.send(Request::Disconnect);
    });
}
