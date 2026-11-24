//! Settings → Audio and the audio lines in Now playing. A 250 ms timer runs
//! the sleep timer, output recovery, device list and resume-position saves.
use super::{
    control::{control, DeviceScan},
    dsp::{CUSTOM_PRESET, EQ_PRESETS},
    recovery::{self, DeviceChoice, OutputRecovery},
    resume::{format_position, parse_position, ResumeStore},
    settings::{AudioSettings, NormalisationKind, BITRATES, FADE_LENGTHS, PREGAINS},
    sleep::{self, Action, Playback, SleepTimer},
};
use crate::{
    features::Features,
    spotify::{Request, RequestSender},
    state::State,
    GroveAudio, GroveBand, GroveChoice, GroveSaved, GroveWindow,
};
use serde_json::json;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

/// Save resume positions at most this often while they change.
const SAVE_EVERY: Duration = Duration::from_secs(15);

pub struct AudioUi {
    offline: bool,
    tx: RequestSender,
    state: Rc<RefCell<State>>,
    features: Rc<RefCell<Features>>,
    pub sleep: SleepTimer,
    recovery: OutputRecovery,
    devices: Vec<DeviceChoice>,
    scan: DeviceScan,
    saved: ResumeStore,
    save_due: Option<Instant>,
    output_lost: bool,
    last_playing: Option<Instant>,
    last_uri: String,
    last_page: i32,
    scanner: Option<Arc<AtomicBool>>,
}

impl Drop for AudioUi {
    fn drop(&mut self) {
        if let Some(stop) = &self.scanner {
            stop.store(true, Ordering::Release);
        }
        self.flush();
    }
}

/// Bind the controls and start the timer. Review modes never scan devices
/// or write preferences.
pub fn install(
    ui: &GroveWindow,
    tx: RequestSender,
    state: Rc<RefCell<State>>,
    features: Rc<RefCell<Features>>,
    offline: bool,
) -> (Rc<RefCell<AudioUi>>, slint::Timer) {
    let mut saved = features.borrow().prefs.audio.clone();
    saved.sanitize();
    control().set_settings(saved.clone());
    let scanner = (!offline).then(start_scanner);
    let this = Rc::new(RefCell::new(AudioUi {
        offline,
        tx,
        state,
        features,
        sleep: SleepTimer::default(),
        recovery: OutputRecovery::default(),
        devices: vec![],
        scan: DeviceScan::default(),
        saved: ResumeStore::default(),
        save_due: None,
        output_lost: false,
        last_playing: None,
        last_uri: String::new(),
        last_page: -1,
        scanner,
    }));
    bind(ui, &this);
    this.borrow_mut().render(ui);
    let timer = slint::Timer::default();
    let weak = ui.as_weak();
    let audio = this.clone();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(250),
        move || {
            if let Some(ui) = weak.upgrade() {
                audio.borrow_mut().tick(&ui, Instant::now());
            }
        },
    );
    (this, timer)
}

fn start_scanner() -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let _ = std::thread::Builder::new()
        .name("grove-output-scan".into())
        .spawn(move || {
            while !flag.load(Ordering::Acquire) {
                let (present, default) = super::sink::scan();
                let control = control();
                let wanted = control.settings().device;
                let opened = control.output().opened;
                if recovery::default_moved(&wanted, opened.as_deref(), default.as_deref()) {
                    // Windows switched its default output: follow it.
                    control.reopen();
                }
                control.set_devices(DeviceScan {
                    present,
                    default,
                    scanned: true,
                });
                for _ in 0..20 {
                    if flag.load(Ordering::Acquire) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        });
    stop
}

fn model<T: Clone + 'static>(rows: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(rows))
}

fn band_label(frequency: f32) -> String {
    if frequency >= 1000.0 {
        format!("{} kHz", (frequency / 100.0).round() / 10.0)
    } else {
        format!("{frequency} Hz")
    }
}

fn gain_label(gain: f32) -> String {
    if gain > 0.0 {
        format!("+{gain:.1} dB")
    } else {
        format!("{gain:.1} dB")
    }
}

fn bind(ui: &GroveWindow, this: &Rc<RefCell<AudioUi>>) {
    let audio = ui.global::<GroveAudio>();
    let (weak, a) = (ui.as_weak(), this.clone());
    audio.on_choose(move |what, value| {
        if let Some(ui) = weak.upgrade() {
            a.borrow_mut().choose(&ui, what.as_str(), value);
        }
    });
    let (weak, a) = (ui.as_weak(), this.clone());
    audio.on_toggle(move |what| {
        if let Some(ui) = weak.upgrade() {
            a.borrow_mut().toggle(&ui, what.as_str());
        }
    });
    let (weak, a) = (ui.as_weak(), this.clone());
    audio.on_set_band(move |index, gain| {
        if let Some(ui) = weak.upgrade() {
            let index = usize::try_from(index).ok();
            a.borrow_mut().change(&ui, |s| {
                if let Some(band) = index.and_then(|i| s.custom_bands.get_mut(i)) {
                    *band = gain;
                }
            });
        }
    });
    let (weak, a) = (ui.as_weak(), this.clone());
    audio.on_edit_position(move |index, text| {
        if let Some(ui) = weak.upgrade() {
            a.borrow_mut().edit_position(&ui, index, text.as_str());
        }
    });
}

impl AudioUi {
    /// Change the settings, persist them and restart the stream if needed.
    pub fn change(&mut self, ui: &GroveWindow, f: impl FnOnce(&mut AudioSettings)) {
        let (before, after) = control().update_settings(f);
        if before == after {
            self.render(ui);
            return;
        }
        let note = if after.needs_engine_restart(&before) {
            if self.state.borrow().connected && !self.offline {
                let _ = self.tx.send(Request::RestartAudio);
                "Restarting playback with the new stream settings…"
            } else {
                "Applies the next time playback connects."
            }
        } else if after.fade_seconds != before.fade_seconds {
            "Fades start with the next song."
        } else {
            ""
        };
        let note = self.persist(&after).unwrap_or_else(|| note.into());
        ui.global::<GroveAudio>().set_note(note.into());
        self.render(ui);
    }

    /// Store the settings in the preferences file. Returns a save error.
    fn persist(&mut self, settings: &AudioSettings) -> Option<String> {
        let mut features = self.features.borrow_mut();
        features.prefs.audio = settings.clone();
        self.save_due = None;
        if self.offline {
            return None;
        }
        features.prefs.save().err()
    }

    /// Write pending resume positions (also on exit).
    fn flush(&mut self) {
        if self.save_due.take().is_some() {
            let settings = control().settings();
            let _ = self.persist(&settings);
        }
    }

    pub fn choose(&mut self, ui: &GroveWindow, what: &str, value: i32) {
        match what {
            "bitrate" => {
                if let Some(kbps) = u16::try_from(value).ok().filter(|v| BITRATES.contains(v)) {
                    self.change(ui, |s| s.bitrate = kbps);
                }
            }
            "normalisation" => {
                if let Some(kind) = usize::try_from(value)
                    .ok()
                    .and_then(|i| NormalisationKind::ALL.get(i))
                    .copied()
                {
                    self.change(ui, |s| s.normalisation = kind);
                }
            }
            "pregain" => {
                if let Some(db) = i8::try_from(value).ok().filter(|v| PREGAINS.contains(v)) {
                    self.change(ui, |s| s.pregain_db = db);
                }
            }
            "device" => {
                if let Some(choice) = usize::try_from(value)
                    .ok()
                    .and_then(|i| self.devices.get(i))
                    .cloned()
                {
                    self.recovery.cancel();
                    self.change(ui, |s| s.device = choice.id);
                }
            }
            "fade" => {
                if let Some(seconds) = u8::try_from(value)
                    .ok()
                    .filter(|v| FADE_LENGTHS.contains(v))
                {
                    self.change(ui, |s| s.fade_seconds = seconds);
                }
            }
            "eq" => {
                if let Some(preset) = u8::try_from(value)
                    .ok()
                    .filter(|v| usize::from(*v) <= CUSTOM_PRESET)
                {
                    self.change(ui, |s| s.eq_preset = preset);
                }
            }
            "bands" => {
                if let Ok(count) = u8::try_from(value) {
                    self.change(ui, |s| s.set_band_count(count));
                }
            }
            "sleep" => {
                let now = Instant::now();
                let uri = self.state.borrow().track.uri.clone();
                match value {
                    0 => self.sleep.cancel(),
                    -1 => {
                        if !self.sleep.end_of_song(&uri) {
                            ui.global::<GroveAudio>()
                                .set_note("Play a song first, then choose End of song.".into());
                        }
                    }
                    minutes => {
                        self.sleep
                            .start_minutes(u64::try_from(minutes).unwrap_or(0), now);
                    }
                }
                self.render_sleep(ui, now);
            }
            "forget" => {
                if let Ok(index) = usize::try_from(value) {
                    self.change(ui, |s| {
                        s.resume.remove(index);
                    });
                }
            }
            "start-over" => {
                let state = self.state.borrow();
                let uri = state.track.uri.clone();
                if state.allows("seek") {
                    let _ = self.tx.send(Request::Command(
                        json!({"type":"command","command":"seek","position_ms":0}),
                    ));
                }
                drop(state);
                self.state.borrow_mut().resumed = None;
                self.change(ui, |s| {
                    s.resume.forget(&uri);
                });
            }
            _ => {}
        }
    }

    pub fn toggle(&mut self, ui: &GroveWindow, what: &str) {
        match what {
            "normalise" => self.change(ui, |s| s.normalise = !s.normalise),
            "remember" => self.change(ui, |s| {
                s.remember_positions = !s.remember_positions;
                if !s.remember_positions {
                    s.resume.clear();
                }
            }),
            "reset-bands" => self.change(ui, |s| s.custom_bands.iter_mut().for_each(|b| *b = 0.0)),
            "forget-all" => self.change(ui, |s| {
                s.resume.clear();
            }),
            "sleep-custom" => {
                let audio = ui.global::<GroveAudio>();
                match sleep::parse_minutes(&audio.get_sleep_custom()) {
                    Some(minutes) => {
                        self.sleep.start_minutes(minutes, Instant::now());
                        audio.set_sleep_custom(SharedString::default());
                        audio.set_note(SharedString::default());
                    }
                    None => audio.set_note(
                        format!(
                            "Enter 1 to {} minutes, for example 25 or 1:30.",
                            sleep::MAX_MINUTES
                        )
                        .into(),
                    ),
                }
                self.render_sleep(ui, Instant::now());
            }
            _ => {}
        }
    }

    fn edit_position(&mut self, ui: &GroveWindow, index: i32, text: &str) {
        let audio = ui.global::<GroveAudio>();
        match (usize::try_from(index).ok(), parse_position(text)) {
            (Some(index), Some(position)) => {
                audio.set_note(SharedString::default());
                self.change(ui, |s| {
                    s.resume.set(index, position);
                });
            }
            _ => {
                audio.set_note("Enter a position like 12:30 or 1:02:30.".into());
                self.render(ui);
            }
        }
    }

    fn render_sleep(&self, ui: &GroveWindow, now: Instant) {
        let label = self.sleep.label(now);
        if ui.get_sleep_remaining().as_str() != label {
            ui.set_sleep_remaining(label.as_str().into());
            ui.global::<GroveAudio>().set_sleep_label(label.into());
        }
    }

    /// Push every setting, device and saved position to the window.
    pub fn render(&mut self, ui: &GroveWindow) {
        let audio = ui.global::<GroveAudio>();
        let settings = control().settings();
        audio.set_bitrate(i32::from(settings.bitrate));
        audio.set_normalise(settings.normalise);
        audio.set_normalisation(settings.normalisation.index());
        audio.set_pregain(i32::from(settings.pregain_db));
        audio.set_fade_seconds(i32::from(settings.fade_seconds));
        audio.set_eq_preset(i32::from(settings.eq_preset));
        let names: Vec<SharedString> = EQ_PRESETS
            .iter()
            .map(|(name, _)| SharedString::from(*name))
            .chain(std::iter::once("Custom".into()))
            .collect();
        audio.set_eq_presets_first(model(names[..4].to_vec()));
        audio.set_eq_presets_more(model(names[4..].to_vec()));
        audio.set_band_count(i32::from(settings.eq_band_count));
        audio.set_bands(model(
            settings
                .band_frequencies()
                .iter()
                .zip(&settings.custom_bands)
                .map(|(f, g)| GroveBand {
                    label: band_label(*f).into(),
                    gain: *g,
                    value: gain_label(*g).into(),
                })
                .collect(),
        ));
        audio.set_remember_positions(settings.remember_positions);
        self.saved = settings.resume.clone();
        audio.set_saved_positions(model(
            settings
                .resume
                .entries()
                .iter()
                .take(20)
                .map(|e| GroveSaved {
                    title: if e.title.is_empty() {
                        "Saved episode".into()
                    } else {
                        e.title.clone().into()
                    },
                    position: format_position(e.position_ms).into(),
                    total: format_position(e.duration_ms).into(),
                })
                .collect(),
        ));
        self.render_devices(ui, &settings);
    }

    fn render_devices(&mut self, ui: &GroveWindow, settings: &AudioSettings) {
        let audio = ui.global::<GroveAudio>();
        self.scan = control().devices();
        self.devices = recovery::choices(
            &self.scan.present,
            self.scan.default.as_deref(),
            &settings.device,
        );
        audio.set_devices(model(
            self.devices
                .iter()
                .map(|d| GroveChoice {
                    label: d.label.clone().into(),
                    selected: d.id == settings.device,
                    available: d.available,
                })
                .collect(),
        ));
        let output = control().output();
        let present = recovery::wanted_present(
            &self.scan.present,
            self.scan.default.as_deref(),
            &settings.device,
        );
        audio.set_output_attention(output.lost || (self.scan.scanned && !present));
        audio.set_output_note(
            if output.lost && self.recovery.pending() {
                "The output went away. Grove resumes here if it returns within 10 minutes."
            } else if output.lost {
                "The output is unavailable. Reconnect it or choose another output."
            } else if self.scan.scanned && !present {
                "Saved output unavailable. Reconnect it or choose System default."
            } else if !self.scan.scanned && !self.offline {
                "Looking for outputs…"
            } else {
                ""
            }
            .into(),
        );
    }

    pub fn tick(&mut self, ui: &GroveWindow, now: Instant) {
        let state = self.state.clone();
        let state = state.borrow();
        if state.playing {
            self.last_playing = Some(now);
        }
        // Sleep timer: fade, then pause once.
        let action = self.sleep.tick(
            now,
            Playback {
                playing: state.playing,
                uri: &state.track.uri,
                position_ms: f64::from(state.position()),
                duration_ms: f64::from(state.track.duration),
            },
        );
        match action {
            Action::Gain(gain) => control().set_sleep_gain(gain),
            Action::Pause => {
                control().set_sleep_gain(0.0);
                if state.allows("pause") && !self.offline {
                    let _ = self.tx.send(Request::Command(
                        json!({"type":"command","command":"pause"}),
                    ));
                }
                ui.set_message("Sleep timer paused playback.".into());
            }
        }
        self.render_sleep(ui, now);
        // Reconnect state and source quality.
        let audio = ui.global::<GroveAudio>();
        if ui.get_reconnecting().as_str() != state.reconnecting {
            ui.set_reconnecting(state.reconnecting.as_str().into());
            audio.set_reconnecting(state.reconnecting.as_str().into());
        }
        if audio.get_source_quality().as_str() != state.source() {
            audio.set_source_quality(state.source().into());
        }
        let note = match &state.resumed {
            Some((uri, position)) if *uri == state.track.uri => {
                format!("Resumed at {}", format_position(*position))
            }
            _ => String::new(),
        };
        if audio.get_resume_note().as_str() != note {
            audio.set_resume_note(note.into());
        }
        // Output loss and the bounded resume.
        if state.track.uri != self.last_uri {
            self.last_uri = state.track.uri.clone();
            self.recovery.cancel();
        }
        let output = control().output();
        let settings_device = control().settings().device;
        if output.lost && !self.output_lost {
            let was_playing = self
                .last_playing
                .is_some_and(|t| now.saturating_duration_since(t) < Duration::from_secs(5));
            self.recovery.lost(now, was_playing, &state.track.uri);
        }
        let scan = control().devices();
        if output.lost
            && self.recovery.pending()
            && scan.scanned
            && recovery::wanted_present(&scan.present, scan.default.as_deref(), &settings_device)
            && self.recovery.returned(now, state.playing, &state.track.uri)
            && state.allows("play")
            && !self.offline
        {
            let _ = self
                .tx
                .send(Request::Command(json!({"type":"command","command":"play"})));
            ui.set_message("Audio output is back. Resuming.".into());
        }
        let refresh_devices = scan != self.scan || output.lost != self.output_lost;
        self.output_lost = output.lost;
        drop(state);
        if refresh_devices {
            let settings = control().settings();
            self.render_devices(ui, &settings);
        }
        // Resume positions from the engine: show them, save them now and then.
        if control().take_resume_dirty() && self.save_due.is_none() {
            self.save_due = Some(now + SAVE_EVERY);
        }
        // Refresh the list when entries come or go, or when Settings opens,
        // not on every playhead sample, so an edit in progress survives.
        let settings = control().settings();
        let page = ui.get_page();
        let listed = |store: &ResumeStore| {
            store
                .entries()
                .iter()
                .map(|e| e.uri.clone())
                .collect::<Vec<_>>()
        };
        if listed(&settings.resume) != listed(&self.saved) || (page == 8 && self.last_page != 8) {
            self.render(ui);
        }
        self.last_page = page;
        if self.save_due.is_some_and(|due| now >= due) {
            self.save_due = None;
            if let Some(error) = self.persist(&settings) {
                ui.set_message(error.into());
            }
        }
    }
}
