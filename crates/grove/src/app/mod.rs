//! Wiring between the Slint window and the Spotify, jam and local-audio workers.
//! Each file owns one area so features can change independently.
pub mod account;
pub mod events;
pub mod jam_ui;
pub mod playback_callbacks;
pub mod shell;
pub mod smoke;
pub mod system_review;
pub mod window;

use crate::spotify::{Request, RequestSender};

/// Synthetic Settings state for screenshots: shortcuts, tray and an update.
pub fn system_fixtures(ui: &crate::GroveWindow) {
    use slint::ComponentHandle;
    let system = ui.global::<crate::GroveSystem>();
    system.set_shortcut_toggle("Ctrl+Alt+P".into());
    system.set_shortcut_next("Ctrl+Alt+Shift+Right".into());
    system.set_shortcut_status("A shortcut is already in use by Windows or another app. Previous shortcuts are still active; choose another combination.".into());
    system.set_close_to_tray(true);
    system.set_update_version("0.1.0-beta.2".into());
    system.set_update_state(3);
    system.set_update_status("31 MB from GitHub Releases · synthetic fixture".into());
}

pub fn send(tx: &RequestSender, payload: serde_json::Value) {
    let _ = tx.send(Request::Command(payload));
}

/// Offline review modes chosen on the command line. At most one may be active.
pub struct ReviewModes {
    /// Position of `--smoke-ui` in the arguments; its output directory follows it.
    pub smoke: Option<usize>,
    pub ui: Option<String>,
    pub features: Option<String>,
    pub local_audio: Option<String>,
}

impl ReviewModes {
    pub fn parse() -> Result<Self, Box<dyn std::error::Error>> {
        let smoke = std::env::args().position(|s| s == "--smoke-ui");
        let review_path = std::env::args()
            .position(|s| s == "--review-ui")
            .map(|index| {
                std::env::args()
                    .nth(index + 1)
                    .ok_or("--review-ui needs an output directory")
            })
            .transpose()?;
        let feature_review_path = std::env::args()
            .position(|s| s == "--review-features")
            .map(|i| {
                std::env::args()
                    .nth(i + 1)
                    .ok_or("--review-features needs an output directory")
            })
            .transpose()?;
        let local_review_path = std::env::args()
            .position(|s| s == "--review-local-audio")
            .map(|i| {
                std::env::args()
                    .nth(i + 1)
                    .ok_or("--review-local-audio needs an output directory")
            })
            .transpose()?;
        if [
            smoke.is_some(),
            review_path.is_some(),
            feature_review_path.is_some(),
            local_review_path.is_some(),
        ]
        .into_iter()
        .filter(|v| *v)
        .count()
            > 1
        {
            return Err("Choose one UI review mode.".into());
        }
        Ok(Self {
            smoke,
            ui: review_path,
            features: feature_review_path,
            local_audio: local_review_path,
        })
    }

    pub fn offline(&self) -> bool {
        self.smoke.is_some()
            || self.ui.is_some()
            || self.features.is_some()
            || self.local_audio.is_some()
    }
}
