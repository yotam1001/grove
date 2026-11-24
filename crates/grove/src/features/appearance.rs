//! Appearance and listening preferences: theme, layout, autoplay, media keys, history.
use super::Features;
use crate::{preferences::Preferences, GroveWindow};
use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc};

/// Loads saved preferences (defaults when offline) and applies them to the window.
pub(super) fn load_preferences(ui: &GroveWindow, offline: bool) -> Preferences {
    let (prefs, error) = if offline {
        (Preferences::default(), None)
    } else {
        match Preferences::load() {
            Ok(p) => (p, None),
            Err(e) => (Preferences::default(), Some(e)),
        }
    };
    crate::theme_watch::apply(ui, prefs.follow_system, prefs.light);
    ui.set_compact_mode(prefs.compact);
    ui.set_autoplay(prefs.autoplay);
    ui.set_media_keys(prefs.media_keys);
    ui.set_local_history(prefs.local_history);
    ui.set_accent_follows_art(prefs.accent_art);
    if prefs.compact {
        ui.window().set_size(slint::LogicalSize::new(760.0, 640.0));
    }
    if let Some(error) = error {
        ui.set_message(error.into());
    }
    prefs
}

pub(super) fn bind(ui: &GroveWindow, this: &Rc<RefCell<Features>>) {
    let f = this.clone();
    let weak = ui.as_weak();
    ui.on_appearance(move |action| {
        if let Some(ui) = weak.upgrade() {
            let mut f = f.borrow_mut();
            match action.as_str() {
                "theme" => {
                    f.prefs.light = !f.prefs.light;
                    f.prefs.follow_system = false;
                }
                "theme-dark" | "theme-light" => {
                    f.prefs.light = action == "theme-light";
                    f.prefs.follow_system = false;
                }
                "theme-system" => f.prefs.follow_system = true,
                "compact" => {
                    f.prefs.compact = !f.prefs.compact;
                    ui.window().set_size(slint::LogicalSize::new(
                        if f.prefs.compact { 760.0 } else { 1200.0 },
                        if f.prefs.compact { 640.0 } else { 800.0 },
                    ));
                }
                "autoplay" => f.prefs.autoplay = !f.prefs.autoplay,
                "media" => f.prefs.media_keys = !f.prefs.media_keys,
                "history" => f.prefs.local_history = !f.prefs.local_history,
                "clear-history" => {
                    f.history.clear();
                    ui.set_feature_message("Session listening history cleared.".into());
                }
                "reset-hidden" => f.prefs.hidden.clear(),
                "accent-art" => f.prefs.accent_art = !f.prefs.accent_art,
                _ => return,
            }
            crate::theme_watch::apply(&ui, f.prefs.follow_system, f.prefs.light);
            ui.set_compact_mode(f.prefs.compact);
            ui.set_autoplay(f.prefs.autoplay);
            ui.set_media_keys(f.prefs.media_keys);
            ui.set_local_history(f.prefs.local_history);
            ui.set_accent_follows_art(f.prefs.accent_art);
            if action == "autoplay" || action == "reset-hidden" {
                f.render_queue(&ui);
            }
            f.save(&ui);
        }
    });
}

impl Features {
    pub fn save(&self, ui: &GroveWindow) {
        if !self.offline {
            if let Err(e) = self.prefs.save() {
                ui.set_message(e.into());
            }
        }
    }
    pub fn volume(&mut self, ui: &GroveWindow, value: f32) {
        self.prefs.volume = value;
        self.save(ui);
    }
}
