//! Keyboard control: opt-in global chords and in-window Space / ← / →.
//!
//! Global shortcuts use `RegisterHotKey` through tide-windows: no keyboard
//! hook, no typing capture and no hardware media-key grab. A chord another app
//! already owns is reported and the previous shortcuts stay active. Recording
//! happens only while the Settings field has focus (ported from Tide's
//! shortcut_capture.rs). In-window keys are ignored while a text field has
//! focus; the window's FocusScope checks that before calling `window-key`.
use crate::{features::Features, GroveSystem, GroveWindow};
use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc};
use tide_windows::shortcuts::{parse_bindings, Chord};

/// Seconds moved by ← and →.
const SEEK_STEP: f32 = 10.0;

/// Owns the hotkey registrations; dropping it unregisters every chord.
pub struct Shortcuts {
    _registrar: Rc<RefCell<Registrar>>,
}

enum Registrar {
    #[cfg(windows)]
    Native(tide_windows::shortcuts::GlobalShortcuts),
    /// Offline reviews and non-Windows builds validate without registering.
    ValidateOnly,
    Unavailable(String),
}

impl Registrar {
    fn apply(&mut self, chords: [Option<Chord>; 3]) -> Result<(), String> {
        match self {
            #[cfg(windows)]
            Registrar::Native(native) => native.apply(chords),
            Registrar::ValidateOnly => {
                for (index, chord) in chords.iter().enumerate() {
                    if chord.is_some() && chords[..index].contains(chord) {
                        return Err("Use a different shortcut for each action.".into());
                    }
                }
                Ok(())
            }
            Registrar::Unavailable(error) => Err(error.clone()),
        }
    }
}

/// Encode a captured key press as a saved chord, Ok(None) for a lone modifier.
fn encode(
    text: &str,
    control: bool,
    alt: bool,
    shift: bool,
    meta: bool,
) -> Result<Option<String>, &'static str> {
    use slint::platform::Key;
    let Some(key) = text.chars().next().filter(|_| text.chars().count() == 1) else {
        return Ok(None);
    };
    if [
        Key::Control,
        Key::ControlR,
        Key::Alt,
        Key::AltGr,
        Key::Shift,
        Key::ShiftR,
        Key::Meta,
        Key::MetaR,
    ]
    .into_iter()
    .any(|k| char::from(k) == key)
    {
        return Ok(None);
    }
    if meta || !(control || alt) {
        return Err("Hold Ctrl or Alt, then press a letter, number, Space or function key.");
    }
    let name = if key.is_ascii_alphanumeric() {
        key.to_ascii_uppercase().to_string()
    } else if key == ' ' {
        "Space".into()
    } else if ('\u{f704}'..='\u{f71b}').contains(&key) {
        format!("F{}", key as u32 - 0xf704 + 1)
    } else if let Some(name) = tide_windows::shortcuts::layout_key_name(key) {
        name
    } else {
        return Err("Choose a letter, number, Space or function key with Ctrl or Alt.");
    };
    let mut value = String::new();
    if control {
        value.push_str("Ctrl+");
    }
    if alt {
        value.push_str("Alt+");
    }
    if shift {
        value.push_str("Shift+");
    }
    value.push_str(&name);
    parse_bindings(&[value.clone(), String::new(), String::new()])
        .map_err(|_| "That combination is reserved. Choose another shortcut.")?;
    Ok(Some(value))
}

/// A saved binding in the canonical form Settings draws as keycaps. Text that
/// is not a modifier chord is returned unchanged.
pub fn display_shortcut(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    let mut modifiers = [None; 3];
    let mut key = None;
    for part in text.split('+').map(str::trim) {
        let slot = match part.to_ascii_uppercase().as_str() {
            "CTRL" | "CONTROL" => Some((0, "Ctrl")),
            "ALT" => Some((1, "Alt")),
            "SHIFT" => Some((2, "Shift")),
            _ => None,
        };
        match slot {
            Some((index, name)) if key.is_none() && modifiers[index].is_none() => {
                modifiers[index] = Some(name)
            }
            None if key.is_none() && !part.is_empty() => key = Some(part),
            _ => return text.into(),
        }
    }
    let Some(key) = key else {
        return text.into();
    };
    let upper = key.to_ascii_uppercase();
    let key = if upper == "SPACE" {
        "Space".to_string()
    } else if key.chars().count() == 1
        || (upper.starts_with('F') && upper[1..].parse::<u32>().is_ok())
    {
        upper
    } else {
        key.to_string()
    };
    let mut value = String::new();
    for name in modifiers.into_iter().flatten() {
        value.push_str(name);
        value.push('+');
    }
    value.push_str(&key);
    value
}

/// What an in-window key does, given the player state. Returns the
/// `media-command` code and value, or None to let the key through.
pub fn window_action(key: &str, ui: &GroveWindow) -> Option<(i32, f32)> {
    let following = ui.get_jam_connected() && !ui.get_jam_host() && ui.get_jam_following();
    if following || (ui.get_mini() && key != "space") {
        return None;
    }
    match key {
        "space" if ui.get_playing() && ui.get_can_pause() => Some((2, 0.0)),
        "space" if !ui.get_playing() && ui.get_can_play() => Some((2, 0.0)),
        "left" | "right" if ui.get_can_seek() && ui.get_duration() > 0.0 => {
            let position = ui.get_position() / 1000.0;
            let duration = ui.get_duration() / 1000.0;
            let step = if key == "left" { -SEEK_STEP } else { SEEK_STEP };
            Some((6, (position + step).clamp(0.0, duration)))
        }
        _ => None,
    }
}

fn show(ui: &GroveWindow, values: &[String; 3]) {
    let system = ui.global::<GroveSystem>();
    system.set_shortcut_toggle(display_shortcut(&values[0]).into());
    system.set_shortcut_next(display_shortcut(&values[1]).into());
    system.set_shortcut_previous(display_shortcut(&values[2]).into());
}

fn apply(registrar: &RefCell<Registrar>, values: &[String; 3]) -> Result<(), String> {
    let bindings = parse_bindings(values)?;
    registrar.borrow_mut().apply(bindings)
}

fn cancel(ui: &GroveWindow, features: &RefCell<Features>, registrar: &RefCell<Registrar>) {
    let system = ui.global::<GroveSystem>();
    if system.get_shortcut_recording_index() < 0 {
        return;
    }
    system.set_shortcut_recording_index(-1);
    let saved = features.borrow().prefs.shortcuts.clone();
    system.set_shortcut_status(apply(registrar, &saved).err().unwrap_or_default().into());
}

fn commit(
    ui: &GroveWindow,
    features: &RefCell<Features>,
    registrar: &RefCell<Registrar>,
    index: usize,
    value: String,
) {
    let system = ui.global::<GroveSystem>();
    let mut values = features.borrow().prefs.shortcuts.clone();
    values[index] = value;
    // Validation leaves the saved values and registrations untouched.
    if let Err(error) = parse_bindings(&values) {
        system.set_shortcut_status(error.into());
        return;
    }
    match apply(registrar, &values) {
        Ok(()) => {
            system.set_shortcut_recording_index(-1);
            show(ui, &values);
            let mut f = features.borrow_mut();
            f.prefs.shortcuts = values;
            f.save(ui);
            system.set_shortcut_status("Saved".into());
        }
        Err(error) => {
            cancel(ui, features, registrar);
            system.set_shortcut_status(error.into());
        }
    }
}

pub fn install(ui: &GroveWindow, features: &Rc<RefCell<Features>>, offline: bool) -> Shortcuts {
    let system = ui.global::<GroveSystem>();
    let registrar = Rc::new(RefCell::new(native(ui, offline)));
    system.set_shortcuts_supported(!matches!(*registrar.borrow(), Registrar::Unavailable(_)));
    let saved = features.borrow().prefs.shortcuts.clone();
    show(ui, &saved);
    if let Err(error) = apply(&registrar, &saved) {
        system.set_shortcut_status(error.into());
    }

    let weak = ui.as_weak();
    system.on_window_key(move |key| {
        let Some(ui) = weak.upgrade() else {
            return false;
        };
        match window_action(key.as_str(), &ui) {
            Some((code, value)) => {
                ui.invoke_media_command(code, value);
                true
            }
            None => false,
        }
    });
    let (f, r, w) = (features.clone(), registrar.clone(), ui.as_weak());
    system.on_begin_shortcut(move |index| {
        let Some(ui) = w.upgrade() else { return };
        if !(0..3).contains(&index) {
            return;
        }
        cancel(&ui, &f, &r);
        let system = ui.global::<GroveSystem>();
        // Release the current chords so pressing one of them records it.
        match r.borrow_mut().apply([None; 3]) {
            Ok(()) => {
                system.set_shortcut_recording_index(index);
                system.set_shortcut_status("Press your shortcut. Esc cancels.".into());
            }
            Err(error) => system.set_shortcut_status(error.into()),
        }
    });
    let (f, r, w) = (features.clone(), registrar.clone(), ui.as_weak());
    system.on_cancel_shortcut(move || {
        if let Some(ui) = w.upgrade() {
            cancel(&ui, &f, &r);
        }
    });
    let (f, r, w) = (features.clone(), registrar.clone(), ui.as_weak());
    system.on_clear_shortcut(move |index| {
        let Some(ui) = w.upgrade() else { return };
        if !(0..3).contains(&index) {
            return;
        }
        cancel(&ui, &f, &r);
        commit(&ui, &f, &r, index as usize, String::new());
    });
    let (f, r, w) = (features.clone(), registrar.clone(), ui.as_weak());
    system.on_record_shortcut(move |text, control, alt, shift, meta| {
        let Some(ui) = w.upgrade() else { return };
        let index = ui.global::<GroveSystem>().get_shortcut_recording_index();
        if !(0..3).contains(&index) {
            return;
        }
        match encode(&text, control, alt, shift, meta) {
            Ok(Some(value)) => commit(&ui, &f, &r, index as usize, value),
            Ok(None) => {}
            Err(error) => ui.global::<GroveSystem>().set_shortcut_status(error.into()),
        }
    });
    Shortcuts {
        _registrar: registrar,
    }
}

#[cfg(windows)]
fn native(ui: &GroveWindow, offline: bool) -> Registrar {
    if offline {
        return Registrar::ValidateOnly;
    }
    let weak = ui.as_weak();
    match tide_windows::shortcuts::GlobalShortcuts::new(move |action| {
        let code = match action {
            tide_windows::MediaAction::Toggle => 2,
            tide_windows::MediaAction::Next => 3,
            tide_windows::MediaAction::Previous => 4,
            _ => return,
        };
        let _ = weak.upgrade_in_event_loop(move |ui| ui.invoke_media_command(code, 0.0));
    }) {
        Ok(native) => Registrar::Native(native),
        Err(error) => Registrar::Unavailable(error),
    }
}

#[cfg(not(windows))]
fn native(_ui: &GroveWindow, offline: bool) -> Registrar {
    if offline {
        Registrar::ValidateOnly
    } else {
        Registrar::Unavailable("Global shortcuts are available in the Windows app.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_shortcuts_are_shown_in_canonical_form() {
        assert_eq!(display_shortcut(" control + alt + p "), "Ctrl+Alt+P");
        assert_eq!(
            display_shortcut("shift+ctrl+alt+space"),
            "Ctrl+Alt+Shift+Space"
        );
        assert_eq!(display_shortcut("alt+f5"), "Alt+F5");
        assert_eq!(display_shortcut(""), "");
        assert_eq!(display_shortcut("Ctrl+Ctrl+P"), "Ctrl+Ctrl+P");
        assert_eq!(display_shortcut("Ctrl+P+Q"), "Ctrl+P+Q");
    }

    #[test]
    fn captured_chords_match_supported_global_bindings() {
        assert_eq!(
            encode("p", true, true, false, false).unwrap().as_deref(),
            Some("Ctrl+Alt+P")
        );
        assert_eq!(
            encode(" ", false, true, true, false).unwrap().as_deref(),
            Some("Alt+Shift+Space")
        );
        assert_eq!(
            encode("\u{f704}", true, false, false, false)
                .unwrap()
                .as_deref(),
            Some("Ctrl+F1")
        );
        assert!(
            encode("\u{f70f}", true, false, false, false).is_err(),
            "F12 is reserved"
        );
        assert!(
            encode("p", false, false, false, false).is_err(),
            "needs Ctrl or Alt"
        );
        assert!(
            encode("p", true, false, false, true).is_err(),
            "Windows key is not allowed"
        );
        assert!(encode("\u{0011}", true, false, false, false)
            .unwrap()
            .is_none());
        assert!(encode("typed text", true, false, false, false)
            .unwrap()
            .is_none());
    }

    #[test]
    fn validate_only_registrar_rejects_duplicate_actions() {
        let mut registrar = Registrar::ValidateOnly;
        let chord =
            parse_bindings(&["Ctrl+Alt+P".into(), String::new(), String::new()]).unwrap()[0];
        assert!(registrar.apply([chord, None, None]).is_ok());
        assert!(registrar.apply([chord, chord, None]).is_err());
    }

    /// Registers and unregisters real, unlikely chords on this desktop session.
    #[cfg(windows)]
    #[test]
    fn native_registration_round_trips_and_reports_conflicts() {
        use tide_windows::shortcuts::GlobalShortcuts;
        let chords = parse_bindings(&[
            "Ctrl+Alt+Shift+F20".into(),
            "Ctrl+Alt+Shift+F19".into(),
            String::new(),
        ])
        .unwrap();
        let mut owner = GlobalShortcuts::new(|_| {}).unwrap();
        owner.apply(chords).unwrap();
        let mut other = GlobalShortcuts::new(|_| {}).unwrap();
        let error = other.apply(chords).unwrap_err();
        assert!(error.contains("already in use"), "{error}");
        drop(owner);
        other
            .apply(chords)
            .expect("dropping the owner unregisters its chords");
        other.apply([None; 3]).unwrap();
    }
}
