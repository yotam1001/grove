//! Window state shared by every callback: playback refresh and the mini player switch.
use crate::{
    state::{clock, State},
    GroveTrack, GroveWindow,
};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{cell::Cell, rc::Rc};

pub fn refresh(ui: &GroveWindow, state: &State) {
    ui.set_connected(state.connected);
    ui.set_authenticated(state.authenticated);
    ui.set_playing(state.playing);
    ui.set_can_play(state.allows("play"));
    ui.set_can_pause(state.allows("pause"));
    ui.set_can_seek(state.allows("seek"));
    ui.set_can_next(state.allows("skip_next"));
    ui.set_can_previous(state.allows("skip_prev"));
    ui.set_can_queue(state.allows("add_to_queue"));
    ui.set_can_shuffle(state.allows("shuffle"));
    ui.set_can_repeat(state.allows("set_repeat"));
    ui.set_shuffle(state.shuffle);
    ui.set_repeat(state.repeat);
    ui.set_track_title(
        if state.track.title.is_empty() {
            "Nothing playing yet"
        } else {
            &state.track.title
        }
        .into(),
    );
    ui.set_artist(
        if !state.authenticated {
            if ui.get_library_ready() {
                "Spotify library connected"
            } else {
                "Connect Spotify to start listening"
            }
        } else if state.track.artist.is_empty() {
            "Spotify"
        } else {
            &state.track.artist
        }
        .into(),
    );
    ui.set_context_name(
        if state.context.is_empty() {
            ""
        } else {
            &state.context
        }
        .into(),
    );
    ui.set_position(state.position());
    ui.set_duration(state.track.duration);
    ui.set_volume(state.volume);
    ui.set_elapsed(clock(state.position()).into());
    ui.set_total(clock(state.track.duration).into());
    ui.set_status(
        if !state.connected {
            if ui.get_library_ready() {
                "Library connected · playback off"
            } else {
                "Connect Spotify"
            }
        } else if !state.authenticated {
            "Finish connecting Spotify"
        } else if !state.active {
            "Ready to play"
        } else if state.playing {
            "Playing on Grove"
        } else {
            "Connected · paused"
        }
        .into(),
    );
}

pub fn refresh_queue(ui: &GroveWindow, state: &State) {
    ui.set_tracks(ModelRc::new(VecModel::from(
        state
            .queue
            .iter()
            .map(|track| GroveTrack {
                title: track.title.clone().into(),
                artist: track.artist.clone().into(),
                uri: track.uri.clone().into(),
                duration: clock(track.duration).into(),
            })
            .collect::<Vec<_>>(),
    )));
}

pub fn bind_mini(ui: &GroveWindow) {
    let weak = ui.as_weak();
    let expanded_size = Rc::new(Cell::new(slint::PhysicalSize::new(1200, 800)));
    ui.on_switch_mini(move || {
        if let Some(ui) = weak.upgrade() {
            let mini = !ui.get_mini();
            if mini {
                expanded_size.set(ui.window().size());
            }
            ui.set_mini(mini);
            if mini {
                ui.window().set_size(slint::LogicalSize::new(600.0, 104.0));
            } else {
                ui.window().set_size(expanded_size.get());
            }
        }
    });
}
