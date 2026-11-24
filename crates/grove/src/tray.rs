//! Opt-in "keep playing in the tray". The icon, its menu (Open, Mini player,
//! Play/pause, Previous, Next, Minimize to tray, Quit) and the close
//! interception live in tide-windows' taskbar module on the same native
//! session as the Windows media controls (see media.rs). Closing the window
//! with the title-bar button or Alt+F4 then hides it while playback goes on;
//! the tray's Quit still closes Grove, and so does an installer shutdown.
use crate::{features::Features, GroveSystem, GroveWindow};
use slint::ComponentHandle;
use std::{cell::RefCell, rc::Rc};

pub fn bind(ui: &GroveWindow, features: &Rc<RefCell<Features>>) {
    let system = ui.global::<GroveSystem>();
    system.set_tray_supported(cfg!(windows));
    system.set_close_to_tray(cfg!(windows) && features.borrow().prefs.close_to_tray);
    let weak = ui.as_weak();
    let features = features.clone();
    system.on_toggle(move |what| {
        let Some(ui) = weak.upgrade() else { return };
        if what.as_str() != "tray" || !cfg!(windows) {
            return;
        }
        let mut f = features.borrow_mut();
        f.prefs.close_to_tray = !f.prefs.close_to_tray;
        ui.global::<GroveSystem>()
            .set_close_to_tray(f.prefs.close_to_tray);
        f.save(&ui);
    });
}
