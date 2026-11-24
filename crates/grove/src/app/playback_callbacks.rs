//! Playback callbacks: play a URI or pasted link, transport buttons, seek and volume.
use super::send;
use crate::{
    features::Features,
    local::Local,
    spotify::{self, command, RequestSender},
    state::State,
    GroveWindow,
};
use serde_json::json;
use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc};

pub fn bind(
    ui: &GroveWindow,
    tx: &RequestSender,
    state: &Rc<RefCell<State>>,
    features: &Rc<RefCell<Features>>,
    local: &Rc<RefCell<Local>>,
) {
    let weak = ui.as_weak();
    let sender = tx.clone();
    let files = local.clone();
    let playback_state = state.clone();
    ui.on_play_uri(move |uri| {
        if let Some(ui) = weak.upgrade() {
            if ui.get_jam_connected() && !ui.get_jam_host() && ui.get_jam_following() {
                ui.set_message("Stop following the jam before choosing your own music.".into());
                return;
            }
            if !ui.get_authenticated() {
                ui.set_page(2);
                return;
            }
            match spotify::spotify_uri(&uri) {
                Ok(uri) => {
                    if files.borrow().active() {
                        files.borrow_mut().stop(&ui, &playback_state.borrow());
                    }
                    send(
                        &sender,
                        json!({"type":"command","command":"play","uri":uri}),
                    );
                }
                Err(error) => ui.set_message(error.into()),
            }
        }
    });
    let weak = ui.as_weak();
    let sender = tx.clone();
    let files = local.clone();
    let playback_state = state.clone();
    ui.on_play_link(move || {
        if let Some(ui) = weak.upgrade() {
            if ui.get_jam_connected() && !ui.get_jam_host() && ui.get_jam_following() {
                ui.set_message("Stop following the jam before choosing your own music.".into());
                return;
            }
            if !ui.get_authenticated() {
                ui.set_page(2);
                return;
            }
            match spotify::spotify_uri(&ui.get_link()) {
                Ok(uri) => {
                    if files.borrow().active() {
                        files.borrow_mut().stop(&ui, &playback_state.borrow());
                    }
                    send(
                        &sender,
                        json!({"type":"command","command":"play","uri":uri}),
                    );
                    ui.set_message("Playback requested. Waiting for Spotify…".into());
                }
                Err(error) => ui.set_message(error.into()),
            }
        }
    });
    let weak = ui.as_weak();
    let feature_state = features.clone();
    ui.on_enqueue_link(move || {
        if let Some(ui) = weak.upgrade() {
            feature_state.borrow_mut().enqueue_link(&ui, &ui.get_link());
        }
    });
    let sender = tx.clone();
    let current = state.clone();
    let feature_state = features.clone();
    let weak = ui.as_weak();
    let files = local.clone();
    ui.on_transport(move |name| {
        if let Some(ui)=weak.upgrade(){if files.borrow_mut().transport(&ui,&name){return;}}
        if let Some(ui)=weak.upgrade(){if ui.get_jam_connected()&&!ui.get_jam_host()&&ui.get_jam_following(){return;}}
        if matches!(name.as_str(),"shuffle"|"repeat") {
            let s=current.borrow();let is_shuffle=name=="shuffle";
            if s.allows(if is_shuffle{"shuffle"}else{"set_repeat"}){send(&sender,json!({"command":if is_shuffle{"set_shuffle"}else{"set_repeat_context"},"enabled":if is_shuffle{!s.shuffle}else{!s.repeat}}));}return;
        }
        let action = match name.as_str() {
            "skip_next" => "skip_next",
            "skip_prev" => "skip_prev",
            "play" => "play",
            "pause" => "pause",
            _ => return,
        };
        if let Some(ui)=weak.upgrade() {
            if ui.get_jam_connected() && !ui.get_jam_host() && ui.get_jam_following(){return;}
            if action=="skip_next" && feature_state.borrow_mut().next(&ui,&current.borrow()){return;}
        }
        if current.borrow().allows(action) {
            send(&sender, command(action));
        }
    });
    let sender = tx.clone();
    let current = state.clone();
    let weak = ui.as_weak();
    let files = local.clone();
    ui.on_seek(move |position| {
        if let Some(ui)=weak.upgrade(){if files.borrow().seek(&ui,position){return;}}
        if let Some(ui)=weak.upgrade(){if ui.get_jam_connected()&&!ui.get_jam_host()&&ui.get_jam_following(){return;}}
        let state = current.borrow();
        if state.allows("seek") { send(&sender, json!({"type":"command","command":"seek","position_ms": position.clamp(0.0, state.track.duration) as u64})); }
    });
    let sender = tx.clone();
    let current = state.clone();
    let feature_state = features.clone();
    let weak = ui.as_weak();
    let files = local.clone();
    ui.on_set_volume(move |volume| {
        if let Some(ui)=weak.upgrade(){feature_state.borrow_mut().volume(&ui,volume.clamp(0.0,100.0));if files.borrow_mut().volume(&ui,volume){return;}}
        if current.borrow().authenticated { send(&sender, json!({"type":"command","command":"set_volume","volume":volume.clamp(0.0,100.0).round() as u32})); }
    });
}
