//! `--review-audio <dir>`: drives Settings → Audio and the Now playing audio
//! lines with synthetic state and saves screenshots. No Spotify, no audio
//! device, no credential or preference access.
use super::{
    control::{control, DeviceScan},
    dsp::CUSTOM_PRESET,
    ui::AudioUi,
};
use crate::{state::State, GroveAudio, GroveWindow};
use serde_json::json;
use slint::{ComponentHandle, Model};
use std::{cell::RefCell, path::PathBuf, rc::Rc, time::Duration, time::Instant};

const EPISODE: &str = "spotify:episode:2JRo0gjbX4GrCqBYdRohoo";
const LONG_MIX: &str = "spotify:track:3JRo0gjbX4GrCqBYdRohoo";

fn capture(ui: &GroveWindow, out: &std::path::Path, name: &str) {
    let pixels = ui.window().take_snapshot().expect("Native audio snapshot");
    image::RgbaImage::from_raw(pixels.width(), pixels.height(), pixels.as_bytes().to_vec())
        .expect("Snapshot pixels")
        .save(out.join(format!("{name}.png")))
        .expect("Save audio review screenshot");
}

fn playing(state: &mut State, uri: &str, title: &str, duration_ms: u64, position_ms: u64) {
    state.apply(&json!({"type":"auth_state","logged_in":true,"is_active":true}));
    state.apply(&json!({"type":"playback_state","status":"playing","is_active":true,
        "item":{"uri":uri,"decorations":{"identity":{"name":title},"creators":[{"entity":{"decorations":{"identity":{"name":"Review show"}}}}],"playback":{"duration_ms":duration_ms}}},
        "position":{"position_ms":position_ms,"speed":1},"available_actions":{"play":{},"pause":{},"seek":{}}}));
}

pub fn start(
    ui: &GroveWindow,
    audio: Rc<RefCell<AudioUi>>,
    state: Rc<RefCell<State>>,
    out: PathBuf,
) -> Result<slint::Timer, std::io::Error> {
    std::fs::create_dir_all(&out)?;
    let timer = slint::Timer::default();
    let weak = ui.as_weak();
    let mut step = 0;
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(350), move || {
        let Some(ui) = weak.upgrade() else { return; };
        let g = ui.global::<GroveAudio>();
        match step {
            0 => {
                ui.window().set_size(slint::LogicalSize::new(1200.0, 800.0));
                ui.set_page(8);
                control().set_devices(DeviceScan {
                    present: vec!["Speakers (Synthetic audio)".into(), "Headphones (Synthetic Bluetooth)".into()],
                    default: Some("Speakers (Synthetic audio)".into()),
                    scanned: true,
                });
                audio.borrow_mut().render(&ui);
            }
            1 => {
                ui.set_review_scroll(1150.0);
                assert_eq!(g.get_bitrate(), 160);
                assert_eq!(g.get_devices().row_count(), 3);
                assert!(g.get_devices().row_data(0).unwrap().selected, "System default is the default");
                let mut a = audio.borrow_mut();
                a.choose(&ui, "bitrate", 320);
                a.choose(&ui, "bitrate", 999);
                assert_eq!(g.get_bitrate(), 320, "Unsupported bitrates are ignored");
                assert!(g.get_note().contains("next time playback connects"), "Offline review restarts nothing");
                a.toggle(&ui, "normalise");
                a.choose(&ui, "normalisation", 2);
                a.choose(&ui, "pregain", -3);
                assert!(g.get_normalise());
                assert_eq!((g.get_normalisation(), g.get_pregain()), (2, -3));
                a.choose(&ui, "device", 2);
                assert!(g.get_devices().row_data(2).unwrap().selected);
                assert_eq!(control().settings().device, "Headphones (Synthetic Bluetooth)");
            }
            2 => {
                capture(&ui, &out, "audio-quality-output");
                // The saved headphones disappear: labelled, and the bounded resume waits.
                control().set_devices(DeviceScan {
                    present: vec!["Speakers (Synthetic audio)".into()],
                    default: Some("Speakers (Synthetic audio)".into()),
                    scanned: true,
                });
                playing(&mut state.borrow_mut(), LONG_MIX, "Long synthetic mix", 40 * 60_000, 600_000);
                audio.borrow_mut().tick(&ui, Instant::now());
                control().set_output(|o| { o.lost = true; o.opened = None; });
                state.borrow_mut().apply(&json!({"type":"playback_changed","status":"paused"}));
                audio.borrow_mut().tick(&ui, Instant::now());
                let last = g.get_devices().row_data(2).unwrap();
                assert!(last.label.ends_with("· unavailable") && !last.available && last.selected);
                assert!(g.get_output_note().contains("within 10 minutes"), "{}", g.get_output_note());
            }
            3 => {
                capture(&ui, &out, "audio-output-lost");
                control().set_output(|o| { o.lost = false; o.opened = Some("Speakers (Synthetic audio)".into()); });
                audio.borrow_mut().choose(&ui, "device", 0);
                assert_eq!(control().settings().device, "auto");
                let mut a = audio.borrow_mut();
                a.choose(&ui, "fade", 6);
                a.choose(&ui, "fade", 5);
                assert_eq!(g.get_fade_seconds(), 6);
                a.choose(&ui, "eq", CUSTOM_PRESET as i32);
                assert_eq!(g.get_bands().row_count(), 10);
                drop(a);
                for (band, gain) in [(0, 6.0), (1, 4.5), (2, 2.0), (6, -2.5), (8, 3.0), (9, 4.0)] {
                    g.invoke_set_band(band, gain);
                }
                g.invoke_set_band(3, 40.0);
                assert_eq!(control().settings().custom_bands[3], 12.0, "Bands are clamped to ±12 dB");
                g.invoke_set_band(3, 0.0);
                assert_eq!(g.get_bands().row_data(0).unwrap().value, "+6.0 dB");
                assert!(!control().settings().eq_bands().is_empty());
                ui.set_review_scroll(1700.0);
            }
            4 => {
                capture(&ui, &out, "audio-fade-equalizer");
                g.invoke_choose("bands".into(), 5);
                assert_eq!(g.get_bands().row_count(), 5);
                assert_eq!(control().settings().custom_bands[0], 4.5, "The curve carries over");
                // Sleep timer and saved positions.
                g.invoke_choose("sleep".into(), 30);
                assert_eq!(ui.get_sleep_remaining(), "Sleep in 30 min");
                g.set_sleep_custom("1:30".into());
                g.invoke_toggle("sleep-custom".into());
                assert_eq!(g.get_sleep_label(), "Sleep in 1 h 30 min");
                g.set_sleep_custom("later".into());
                g.invoke_toggle("sleep-custom".into());
                assert!(g.get_note().contains("for example"));
                assert_eq!(g.get_sleep_label(), "Sleep in 1 h 30 min", "A bad entry keeps the timer");
                control().with_settings(|s| {
                    s.resume.record(EPISODE, "Synthetic episode 12 · The long interview", 1_234_000, 3_600_000);
                    s.resume.record(LONG_MIX, "Long synthetic mix", 600_000, 40 * 60_000);
                });
                audio.borrow_mut().tick(&ui, Instant::now());
                assert_eq!(g.get_saved_positions().row_count(), 2);
                g.invoke_edit_position(1, "45:30".into());
                assert_eq!(control().settings().resume.get(EPISODE), Some(2_730_000), "Edited before playback");
                g.invoke_edit_position(1, "soon".into());
                assert!(g.get_note().contains("12:30"));
                assert_eq!(g.get_saved_positions().row_data(1).unwrap().position, "45:30");
                ui.set_review_scroll(2250.0);
            }
            5 => {
                capture(&ui, &out, "audio-bands-sleep-resume");
                g.invoke_choose("eq".into(), 1);
                // Now playing: resumed episode, source quality, sleep and reconnect.
                {
                    let mut s = state.borrow_mut();
                    playing(&mut s, EPISODE, "Synthetic episode 12 · The long interview", 3_600_000, 2_730_000);
                    s.apply(&json!({"type":"source","uri":EPISODE,"label":"Ogg Vorbis · 96 kbps · 44.1 kHz"}));
                    s.apply(&json!({"type":"resumed","uri":EPISODE,"position_ms":2_730_000}));
                }
                crate::app::window::refresh(&ui, &state.borrow());
                g.invoke_choose("sleep".into(), -1);
                assert_eq!(g.get_sleep_label(), "Sleep at end of song");
                audio.borrow_mut().tick(&ui, Instant::now());
                assert_eq!(g.get_resume_note(), "Resumed at 45:30");
                assert_eq!(g.get_source_quality(), "Ogg Vorbis · 96 kbps · 44.1 kHz");
                ui.set_review_scroll(0.0);
                ui.set_page(0);
            }
            6 => {
                capture(&ui, &out, "now-playing-audio");
                state.borrow_mut().apply(&json!({"type":"reconnecting","label":"Reconnecting… retry 3 in 4 s"}));
                crate::app::window::refresh(&ui, &state.borrow());
                audio.borrow_mut().tick(&ui, Instant::now());
                assert_eq!(ui.get_reconnecting(), "Reconnecting… retry 3 in 4 s");
                assert!(!ui.get_can_pause() && !ui.get_can_play(), "Controls wait while reconnecting");
                assert_eq!(ui.get_track_title(), "Synthetic episode 12 · The long interview", "The song stays on screen");
            }
            7 => {
                capture(&ui, &out, "now-playing-reconnecting");
                ui.window().set_size(slint::LogicalSize::new(700.0, 540.0));
            }
            8 => {
                capture(&ui, &out, "now-playing-narrow");
                ui.set_page(8);
            }
            9 => ui.set_review_scroll(1450.0),
            10 => {
                capture(&ui, &out, "audio-narrow-quality");
                ui.set_review_scroll(1900.0);
            }
            11 => {
                capture(&ui, &out, "audio-narrow-output-fade");
                g.invoke_choose("eq".into(), CUSTOM_PRESET as i32);
                ui.set_review_scroll(2300.0);
            }
            12 => {
                capture(&ui, &out, "audio-narrow-equalizer");
                ui.set_review_scroll(2700.0);
            }
            13 => {
                capture(&ui, &out, "audio-narrow-bands-sleep");
                ui.set_review_scroll(3150.0);
            }
            14 => {
                capture(&ui, &out, "audio-narrow-resume");
                g.invoke_choose("sleep".into(), 0);
                assert_eq!(ui.get_sleep_remaining(), "");
                g.invoke_toggle("forget-all".into());
                assert_eq!(g.get_saved_positions().row_count(), 0);
                eprintln!("PASS: Settings → Audio controls, output labels, sleep timer, resume positions and reconnect display");
                slint::quit_event_loop().unwrap();
            }
            _ => {}
        }
        eprintln!("Audio review stage {step} passed");
        step += 1;
    });
    Ok(timer)
}
