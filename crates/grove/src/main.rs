#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod app;
mod artwork;
mod audio;
mod auth;
mod beta;
mod browse_cache;
mod crash;
mod credentials;
mod engine;
mod features;
mod jam;
mod jam_sync;
mod jam_transport;
mod library;
mod local;
mod lyrics;
mod media;
mod preferences;
mod shortcuts;
mod spotify;
mod state;
mod theme_watch;
mod tray;
mod ui_review;
mod update;
mod web_api;

use slint::ComponentHandle;
use spotify::Request;
use state::State;
use std::{cell::RefCell, rc::Rc};
slint::include_modules!();

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if beta::command_line()? {
        return Ok(());
    }
    std::env::set_var("SLINT_BACKEND", "winit-software");
    let ui = GroveWindow::new()?;
    ui.set_app_version(format!("Grove {} · Private beta", env!("CARGO_PKG_VERSION")).into());
    if std::env::args().any(|a| a == "--connect-account") {
        ui.set_page(2);
    }
    let modes = app::ReviewModes::parse()?;
    let offline = modes.offline();
    crash::install(offline);
    let state = Rc::new(RefCell::new(State::default()));
    let (tx, events, handle) = spotify::worker();
    let (jam_tx, jam_events, jam_handle) = jam_transport::worker();
    let features = features::Features::install(&ui, tx.clone(), offline);
    beta::install(&ui, state.clone(), offline);
    let live_check = beta::live_check(&ui, state.clone(), offline)?;
    if !offline {
        ui.set_jam_address(jam_transport::suggested_address().into());
    }
    let (local, local_timer) =
        local::install(&ui, tx.clone(), state.clone(), features.clone(), offline);
    app::jam_ui::bind(&ui, &jam_tx, &features);
    app::account::bind(&ui, &tx, &features);
    app::playback_callbacks::bind(&ui, &tx, &state, &features, &local);
    app::window::bind_mini(&ui);
    let shell = app::shell::install(&ui, offline);
    let lyrics = lyrics::install(
        &ui,
        if offline {
            lyrics::fixture()
        } else {
            lyrics::lrclib()
        },
    );
    let (audio, audio_timer) =
        audio::ui::install(&ui, tx.clone(), state.clone(), features.clone(), offline);

    let poll = app::events::start(&ui, events, &state, &jam_tx, &tx, &features, &local);
    let media = media::install(&ui, state.clone(), offline);
    let theme_timer = theme_watch::start(&ui, offline);
    tray::bind(&ui, &features);
    let shortcuts = shortcuts::install(&ui, &features, offline);
    update::install(&ui, offline);
    crash::offer(&ui, offline);
    let jam_timer = app::jam_ui::start_timer(&ui, jam_events, &state, &jam_tx, &tx, &features);

    let smoke_timer = slint::Timer::default();
    if let Some(index) = modes.smoke {
        poll.stop();
        jam_timer.stop();
        app::smoke::start(&ui, &smoke_timer, index)?;
    }
    let review = modes
        .ui
        .map(|path| ui_review::start_with(&ui, Some(state.clone()), path.into()))
        .transpose()?;
    let feature_review = modes
        .features
        .map(|path| features::review::start(&ui, features.clone(), path.into()))
        .transpose()?;
    let audio_review = modes
        .audio
        .map(|p| audio::review::start(&ui, audio.clone(), state.clone(), p.into()))
        .transpose()?;
    let local_review = modes
        .local_audio
        .map(|p| local::review(&ui, local.clone(), state.clone(), p.into()))
        .transpose()?;
    if !offline {
        let _ = tx.send(Request::Restore);
    }
    ui.run()?;
    drop(live_check);
    drop(review);
    drop(feature_review);
    drop(local_review);
    drop(audio_review);
    audio_timer.stop();
    drop(audio_timer);
    drop(audio);
    drop(media);
    drop(lyrics);
    drop(shell);
    drop(shortcuts);
    drop(theme_timer);
    local_timer.stop();
    drop(local_timer);
    // Release all UI/timer sender clones before joining the worker.
    poll.stop();
    jam_timer.stop();
    let _ = jam_tx.send(jam_transport::Request::Shutdown);
    let _ = tx.send(Request::Shutdown);
    drop(poll);
    drop(ui);
    drop(tx);
    drop(smoke_timer);
    drop(jam_timer);
    drop(jam_tx);
    let _ = jam_handle.join();
    let _ = handle.join();
    Ok(())
}
