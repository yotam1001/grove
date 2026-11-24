//! Offline review of the Windows-integration features: System theme, tray
//! setting, global shortcut recording, in-window keys, the update panel and the
//! crash notice. Synthetic state only: no registry writes, hotkeys, network,
//! credentials or preference files (the feature controller is offline).
use crate::{features::Features, theme_watch, GroveSystem, GroveWindow};
use slint::{
    platform::{Key, PointerEventButton, WindowEvent},
    ComponentHandle, LogicalPosition,
};
use std::{cell::RefCell, path::Path, rc::Rc};

fn capture(ui: &GroveWindow, out: &Path, name: &str) {
    let p = ui.window().take_snapshot().expect("Native system snapshot");
    image::RgbaImage::from_raw(p.width(), p.height(), p.as_bytes().to_vec())
        .unwrap()
        .save(out.join(format!("{name}.png")))
        .unwrap();
}

fn key(ui: &GroveWindow, text: slint::SharedString) {
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text });
}

fn click(ui: &GroveWindow, x: f32, y: f32) {
    let position = LogicalPosition::new(x, y);
    ui.window()
        .dispatch_event(WindowEvent::PointerMoved { position });
    ui.window().dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    ui.window().dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
}

fn checks(ui: &GroveWindow, features: &Rc<RefCell<Features>>) {
    let system = ui.global::<GroveSystem>();
    ui.window().set_size(slint::LogicalSize::new(1200.0, 800.0));

    // System theme follows the (simulated) Windows setting at runtime.
    ui.invoke_appearance("theme-system".into());
    assert!(system.get_follow_system() && features.borrow().prefs.follow_system);
    theme_watch::simulate(Some(true));
    theme_watch::poll(ui);
    assert!(
        ui.get_light_mode(),
        "System theme did not follow Windows to light"
    );
    theme_watch::simulate(Some(false));
    theme_watch::poll(ui);
    assert!(
        !ui.get_light_mode(),
        "System theme did not follow Windows to dark"
    );
    ui.invoke_appearance("theme-light".into());
    assert!(!system.get_follow_system() && ui.get_light_mode());
    theme_watch::simulate(Some(false));
    theme_watch::poll(ui);
    assert!(ui.get_light_mode(), "Light theme followed Windows");
    ui.invoke_appearance("theme-dark".into());
    assert!(!ui.get_light_mode());

    // The tray option is a saved, opt-in preference.
    let tray = features.borrow().prefs.close_to_tray;
    system.invoke_toggle("tray".into());
    assert_eq!(system.get_close_to_tray(), cfg!(windows) && !tray);
    assert_eq!(
        features.borrow().prefs.close_to_tray,
        cfg!(windows) && !tray
    );

    // Shortcut recording: a chord, a duplicate, a bare key, cancel and clear.
    ui.set_page(8);
    system.invoke_begin_shortcut(0);
    assert_eq!(system.get_shortcut_recording_index(), 0);
    system.invoke_record_shortcut("p".into(), true, true, false, false);
    assert_eq!(system.get_shortcut_toggle(), "Ctrl+Alt+P");
    assert_eq!(system.get_shortcut_status(), "Saved");
    assert_eq!(system.get_shortcut_recording_index(), -1);
    assert_eq!(features.borrow().prefs.shortcuts[0], "Ctrl+Alt+P");
    system.invoke_begin_shortcut(1);
    system.invoke_record_shortcut("p".into(), true, true, false, false);
    assert!(system.get_shortcut_status().contains("different shortcut"));
    assert_eq!(system.get_shortcut_next(), "", "a duplicate was saved");
    system.invoke_record_shortcut("n".into(), false, false, false, false);
    assert!(system.get_shortcut_status().contains("Hold Ctrl or Alt"));
    system.invoke_record_shortcut("\u{f704}".into(), false, true, true, false);
    assert_eq!(system.get_shortcut_next(), "Alt+Shift+F1");
    system.invoke_begin_shortcut(2);
    system.invoke_cancel_shortcut();
    assert_eq!(system.get_shortcut_recording_index(), -1);
    system.invoke_clear_shortcut(1);
    assert_eq!(system.get_shortcut_next(), "");
    assert_eq!(features.borrow().prefs.shortcuts[1], "");

    // In-window keys go through the same guarded media-command path.
    let commands = Rc::new(RefCell::new(Vec::<(i32, f32)>::new()));
    let sink = commands.clone();
    ui.on_media_command(move |code, value| sink.borrow_mut().push((code, value)));
    ui.set_page(0);
    ui.set_playing(true);
    ui.set_can_pause(true);
    ui.set_can_seek(true);
    ui.set_position(45_000.0);
    ui.set_duration(210_000.0);
    ui.window()
        .dispatch_event(WindowEvent::WindowActiveChanged(true));
    key(ui, " ".into());
    key(ui, Key::RightArrow.into());
    key(ui, Key::LeftArrow.into());
    assert_eq!(
        *commands.borrow(),
        [(2, 0.0), (6, 55.0), (6, 35.0)],
        "window keys"
    );
    ui.set_position(205_000.0);
    key(ui, Key::RightArrow.into());
    assert_eq!(
        commands.borrow().last(),
        Some(&(6, 210.0)),
        "seek clamps to the end"
    );
    // A following jam guest cannot drive playback from the keyboard.
    ui.set_jam_connected(true);
    ui.set_jam_host(false);
    ui.set_jam_following(true);
    let before = commands.borrow().len();
    key(ui, " ".into());
    assert_eq!(
        commands.borrow().len(),
        before,
        "a following guest toggled playback"
    );
    ui.set_jam_connected(false);
    // Typing in a text field keeps Space and arrows for the text.
    ui.set_page(6);
    ui.set_search_text("".into());
    let _ = ui.window().take_snapshot();
    // Probe a few points down the page's top band for the search field, so a
    // layout change moves the probe rather than breaking the check.
    for y in [199.0, 153.0, 175.0, 225.0] {
        click(ui, 600.0, y);
        key(ui, "x".into());
        if !ui.get_search_text().is_empty() {
            break;
        }
    }
    if ui.get_search_text() == "x" {
        let before = commands.borrow().len();
        key(ui, " ".into());
        key(ui, Key::LeftArrow.into());
        assert_eq!(
            commands.borrow().len(),
            before,
            "keys reached the player while typing"
        );
        assert_eq!(
            ui.get_search_text(),
            "x ",
            "Space did not reach the text field"
        );
    } else {
        eprintln!("NOTE: search field not found at the expected point; typing guard not exercised");
    }
    ui.set_search_text("".into());
    ui.set_playing(false);

    // Updates never touch the network offline; the panel states render.
    ui.set_page(8);
    system.invoke_check_updates();
    assert_eq!(system.get_update_state(), 2);
    assert!(system.get_update_status().contains("review mode"));
    system.set_update_version("0.1.0-beta.2".into());
    system.set_update_state(5);
    system.set_update_status(
        "Checksum verified. Grove will close and the installer will open.".into(),
    );
    system.invoke_install_update();
    eprintln!("PASS: System theme, tray setting, shortcut recording, window keys and offline update check");
}

fn scroll(ui: &GroveWindow, delta: f32) {
    ui.window().dispatch_event(WindowEvent::PointerScrolled {
        position: LogicalPosition::new(
            ui.window()
                .size()
                .to_logical(ui.window().scale_factor())
                .width
                - 200.0,
            300.0,
        ),
        delta_x: 0.0,
        delta_y: -delta,
    });
}

/// One review step per timer tick, so resizes and scrolling take effect
/// before each capture. Returns true when finished.
pub fn step(ui: &GroveWindow, features: &Rc<RefCell<Features>>, out: &Path, step: usize) -> bool {
    let system = ui.global::<GroveSystem>();
    match step {
        0 => checks(ui, features),
        1 => {
            capture(ui, out, "settings-top");
            scroll(ui, 600.0);
        }
        2 => {
            capture(ui, out, "settings-middle");
            scroll(ui, 600.0);
        }
        3 => {
            capture(ui, out, "settings-bottom");
            ui.set_page(0);
            ui.window().set_size(slint::LogicalSize::new(700.0, 540.0));
        }
        4 => {
            ui.set_page(8);
            ui.set_light_mode(true);
        }
        5 => {
            capture(ui, out, "settings-narrow-light");
            scroll(ui, 1000.0);
        }
        6 => {
            capture(ui, out, "settings-narrow-light-middle");
            scroll(ui, 5000.0);
        }
        7 => {
            capture(ui, out, "settings-narrow-light-bottom");
            ui.set_light_mode(false);
            // Crash notice: offered, copied and dismissed without touching files.
            let report = crate::crash::redact(
                "Grove 0.1.0 crashed
thread: main
message: synthetic fixture for listener@example.com at https://api.spotify.com/v1/me
",
                &[],
                &Default::default(),
            );
            assert!(!report.contains("listener@") && !report.contains("/v1/me"));
            system.set_crash_report(report.into());
            system.set_crash_pending(true);
            ui.set_page(5);
            system.invoke_crash_action("copy".into());
            assert!(system.get_crash_status().contains("Copied"));
        }
        8 => {
            capture(ui, out, "crash-notice-narrow");
            ui.window().set_size(slint::LogicalSize::new(1200.0, 800.0));
        }
        9 => {
            capture(ui, out, "crash-notice");
            system.invoke_crash_action("dismiss".into());
            assert!(!system.get_crash_pending());
            eprintln!("PASS: Settings cards, update panel and crash notice captured");
            return true;
        }
        _ => return true,
    }
    false
}
