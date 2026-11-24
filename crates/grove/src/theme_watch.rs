//! "System" theme: follow the Windows app light/dark setting at runtime.
//!
//! Windows has no change event that reaches a winit window without a hook, so
//! a one-second timer re-reads the per-user `AppsUseLightTheme` value (one
//! registry read, no allocation) while the System option is chosen.
use crate::{GroveSystem, GroveWindow};
use slint::ComponentHandle;
use std::{cell::Cell, time::Duration};

thread_local! {
    /// Offline reviews and tests set this instead of reading the registry.
    static SIMULATED: Cell<Option<Option<bool>>> = const { Cell::new(None) };
}

/// Whether Windows currently asks apps to use a light theme; None if unknown.
pub fn system_light() -> Option<bool> {
    if let Some(simulated) = SIMULATED.with(Cell::get) {
        return simulated;
    }
    #[cfg(windows)]
    {
        tide_windows::app_light_theme()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Replace the Windows setting with a fixed value (review modes and tests).
pub fn simulate(light: Option<bool>) {
    SIMULATED.with(|cell| cell.set(Some(light)));
}

/// The theme the window should show for these preferences.
pub fn resolve(follow_system: bool, light: bool, system: Option<bool>) -> bool {
    if follow_system {
        system.unwrap_or(light)
    } else {
        light
    }
}

/// Apply saved preferences to the window.
pub fn apply(ui: &GroveWindow, follow_system: bool, light: bool) {
    ui.global::<GroveSystem>().set_follow_system(follow_system);
    let system = if follow_system { system_light() } else { None };
    let wanted = resolve(follow_system, light, system);
    if ui.get_light_mode() != wanted {
        ui.set_light_mode(wanted);
    }
}

/// One timer tick: switch the window when Windows changed its setting.
pub fn poll(ui: &GroveWindow) {
    if !ui.global::<GroveSystem>().get_follow_system() {
        return;
    }
    if let Some(light) = system_light() {
        if ui.get_light_mode() != light {
            ui.set_light_mode(light);
        }
    }
}

pub fn start(ui: &GroveWindow, offline: bool) -> slint::Timer {
    if offline {
        // Reviews never depend on this PC's theme.
        simulate(Some(false));
    }
    ui.global::<GroveSystem>()
        .set_system_theme_supported(cfg!(windows) || system_light().is_some());
    let timer = slint::Timer::default();
    let weak = ui.as_weak();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_secs(1),
        move || {
            if let Some(ui) = weak.upgrade() {
                poll(&ui);
            }
        },
    );
    timer
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn system_choice_follows_windows_and_falls_back_to_the_saved_theme() {
        assert!(resolve(true, false, Some(true)));
        assert!(!resolve(true, true, Some(false)));
        assert!(
            resolve(true, true, None),
            "unknown Windows setting keeps the saved theme"
        );
        assert!(
            !resolve(false, false, Some(true)),
            "Dark/Light ignore Windows"
        );
        simulate(Some(true));
        assert_eq!(system_light(), Some(true));
        simulate(None);
        assert_eq!(system_light(), None);
    }
}
