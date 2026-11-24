//! One system media handler, dispatched through the same guarded UI callbacks.
//! `MediaControls` is the platform seam: Windows uses SMTC (plus the taskbar
//! and opt-in tray from tide-windows); macOS uses Now Playing and Linux MPRIS,
//! both through souvlaki.
use crate::{state::State, GroveWindow};
use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc, time::Duration};
pub fn install(ui: &GroveWindow, state: Rc<RefCell<State>>, offline: bool) -> slint::Timer {
    let weak = ui.as_weak();
    ui.on_media_command(move |code, value| {
        if let Some(ui) = weak.upgrade() {
            // Sources gate themselves: the media session exists only while media
            // keys or the tray are on, and global shortcuts are explicit opt-ins.
            if ui.get_jam_connected() && !ui.get_jam_host() && ui.get_jam_following() {
                return;
            }
            match code {
                0 => ui.invoke_transport("play".into()),
                1 | 5 => ui.invoke_transport("pause".into()),
                2 => ui.invoke_transport(if ui.get_playing() { "pause" } else { "play" }.into()),
                3 => ui.invoke_transport("skip_next".into()),
                4 => ui.invoke_transport("skip_prev".into()),
                6 => ui.invoke_seek(value * 1000.0),
                7 if !ui.get_mini() => ui.invoke_switch_mini(),
                // MPRIS Raise: bring the window forward.
                8 => {
                    let _ = ui.show();
                }
                _ => {}
            }
        }
    });
    let timer = slint::Timer::default();
    if offline {
        return timer;
    }
    let weak = ui.as_weak();
    let mut media = None::<Box<dyn MediaControls>>;
    let mut attempts = 0;
    let mut remembered = (String::new(), String::new(), String::new());
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(250),
        move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            // The tray lives on the same native session, so keep it while either is on.
            if !ui.get_media_keys() && !ui.global::<crate::GroveSystem>().get_close_to_tray() {
                media = None;
                attempts = 0;
                return;
            }
            if media.is_none() && attempts < 20 {
                attempts += 1;
                match start(&ui) {
                    Ok(session) => media = Some(session),
                    Err(e) if attempts == 20 => {
                        ui.set_message(format!("System media controls unavailable: {e}").into())
                    }
                    _ => {}
                }
            }
            let s = state.borrow();
            // Keep crash reports free of what Grove showed (see crash.rs).
            let shown = (
                ui.get_account_name().to_string(),
                ui.get_track_title().to_string(),
                ui.get_artist().to_string(),
            );
            if shown != remembered {
                crate::crash::remember(&shown.0);
                crate::crash::remember(&shown.1);
                crate::crash::remember(&shown.2);
                remembered = shown;
            }
            if let Some(media) = media.as_mut() {
                let (title, artist, playing, position, duration) = if ui.get_local_active() {
                    (
                        ui.get_track_title(),
                        ui.get_artist(),
                        ui.get_playing(),
                        ui.get_position() as f64 / 1000.0,
                        ui.get_duration() as f64 / 1000.0,
                    )
                } else {
                    (
                        s.track.title.clone().into(),
                        s.track.artist.clone().into(),
                        s.playing,
                        s.position() as f64 / 1000.0,
                        s.track.duration as f64 / 1000.0,
                    )
                };
                media.set_close_to_tray(ui.global::<crate::GroveSystem>().get_close_to_tray());
                media.update(&title, &artist, playing, position, duration);
            }
        },
    );
    timer
}
/// What Grove needs from a platform's system media integration.
pub trait MediaControls {
    fn update(&mut self, title: &str, artist: &str, playing: bool, position: f64, duration: f64);
    /// Closing the window hides it to the tray instead (Windows only).
    fn set_close_to_tray(&mut self, enabled: bool);
}

#[cfg(windows)]
impl MediaControls for tide_windows::MediaSession {
    fn update(&mut self, title: &str, artist: &str, playing: bool, position: f64, duration: f64) {
        let _ = tide_windows::MediaSession::update(
            self, title, artist, "Grove", playing, position, duration,
        );
    }
    fn set_close_to_tray(&mut self, enabled: bool) {
        tide_windows::MediaSession::set_close_to_tray(self, enabled);
    }
}

/// Media-command codes shared with the Slint `media-command` callback.
#[cfg(windows)]
fn code(action: tide_windows::MediaAction) -> (i32, f32) {
    use tide_windows::MediaAction::*;
    match action {
        Play => (0, 0.0),
        Pause => (1, 0.0),
        Toggle => (2, 0.0),
        Next => (3, 0.0),
        Previous => (4, 0.0),
        Stop => (5, 0.0),
        Seek(seconds) => (6, seconds as f32),
        ShowMini => (7, 0.0),
    }
}

#[cfg(windows)]
fn start(ui: &GroveWindow) -> Result<Box<dyn MediaControls>, String> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let window = ui.window().window_handle();
    let handle = window
        .window_handle()
        .map_err(|_| "Native window is not ready")?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Err("Windows media controls need a native window".into());
    };
    let weak = ui.as_weak();
    let session = tide_windows::start(handle.hwnd.get() as *mut std::ffi::c_void, move |action| {
        let (code, value) = code(action);
        let _ = weak.upgrade_in_event_loop(move |ui| ui.invoke_media_command(code, value));
    })?;
    Ok(Box::new(session))
}

/// macOS Now Playing and Linux MPRIS. Grove has no tray there, so the
/// close-to-tray setting does nothing.
#[cfg(any(target_os = "macos", target_os = "linux"))]
struct SystemMedia {
    controls: souvlaki::MediaControls,
    shown: (String, String, u64),
    playing: Option<bool>,
    /// Where the last reported position puts playback now, to notice seeks.
    reported: Option<(f64, std::time::Instant)>,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl MediaControls for SystemMedia {
    fn update(&mut self, title: &str, artist: &str, playing: bool, position: f64, duration: f64) {
        use souvlaki::{MediaMetadata, MediaPlayback, MediaPosition};
        let shown = (
            title.to_owned(),
            artist.to_owned(),
            duration.max(0.0) as u64,
        );
        if shown != self.shown {
            let _ = self.controls.set_metadata(MediaMetadata {
                title: Some(title),
                artist: Some(artist),
                album: Some("Grove"),
                duration: (duration > 0.0).then(|| Duration::from_secs_f64(duration)),
                ..Default::default()
            });
            self.shown = shown;
            self.playing = None;
        }
        // The timer runs four times a second. Report a change of state, or a
        // position that drifted from what the system extrapolates (a seek).
        let expected = self.reported.map(|(at, since)| {
            at + if self.playing == Some(true) {
                since.elapsed().as_secs_f64()
            } else {
                0.0
            }
        });
        if self.playing != Some(playing) || expected.is_none_or(|e| (e - position).abs() > 2.0) {
            let progress = Some(MediaPosition(Duration::from_secs_f64(position.max(0.0))));
            let _ = self.controls.set_playback(if title.is_empty() {
                MediaPlayback::Stopped
            } else if playing {
                MediaPlayback::Playing { progress }
            } else {
                MediaPlayback::Paused { progress }
            });
            self.playing = Some(playing);
            self.reported = Some((position, std::time::Instant::now()));
        }
    }
    fn set_close_to_tray(&mut self, _enabled: bool) {}
}

/// Media-command codes shared with the Slint `media-command` callback.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn system_code(event: souvlaki::MediaControlEvent) -> Option<(i32, f32)> {
    use souvlaki::MediaControlEvent::*;
    Some(match event {
        Play => (0, 0.0),
        Pause => (1, 0.0),
        Toggle => (2, 0.0),
        Next => (3, 0.0),
        Previous => (4, 0.0),
        Stop => (5, 0.0),
        SetPosition(position) => (6, position.0.as_secs_f32()),
        Raise => (8, 0.0),
        _ => return None,
    })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn start(ui: &GroveWindow) -> Result<Box<dyn MediaControls>, String> {
    let mut controls = souvlaki::MediaControls::new(souvlaki::PlatformConfig {
        dbus_name: "grove_music",
        display_name: "Grove",
        hwnd: None,
    })
    .map_err(|e| format!("{e:?}"))?;
    let weak = ui.as_weak();
    controls
        .attach(move |event| {
            if let Some((code, value)) = system_code(event) {
                let _ = weak.upgrade_in_event_loop(move |ui| ui.invoke_media_command(code, value));
            }
        })
        .map_err(|e| format!("{e:?}"))?;
    Ok(Box::new(SystemMedia {
        controls,
        shown: Default::default(),
        playing: None,
        reported: None,
    }))
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn start(_ui: &GroveWindow) -> Result<Box<dyn MediaControls>, String> {
    Err("System media controls are not available on this platform yet.".into())
}
