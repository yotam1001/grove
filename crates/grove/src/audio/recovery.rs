//! Output devices: the Settings list, and one bounded resume after the
//! chosen output disappears (Bluetooth, USB or HDMI) and comes back.
use super::settings::SYSTEM_DEFAULT;
use std::time::{Duration, Instant};

/// Resume only if the output returns within this window.
pub const RESUME_WINDOW: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Debug, PartialEq)]
pub struct DeviceChoice {
    /// What is saved: [`SYSTEM_DEFAULT`] or a device name.
    pub id: String,
    pub label: String,
    pub available: bool,
}

/// System default first, then every present device, then a saved device
/// that is missing, labelled so the listener knows why it is silent.
pub fn choices(present: &[String], default: Option<&str>, saved: &str) -> Vec<DeviceChoice> {
    let mut list = vec![DeviceChoice {
        id: SYSTEM_DEFAULT.into(),
        label: match default {
            Some(name) => format!("System default · {name}"),
            None => "System default".into(),
        },
        available: true,
    }];
    let mut seen = std::collections::HashSet::new();
    for name in present.iter().take(32) {
        if name.is_empty() || !seen.insert(name.as_str()) {
            continue;
        }
        list.push(DeviceChoice {
            id: name.clone(),
            label: name.clone(),
            available: true,
        });
    }
    if saved != SYSTEM_DEFAULT && !seen.contains(saved) {
        list.push(DeviceChoice {
            id: saved.into(),
            label: format!("{saved} · unavailable"),
            available: false,
        });
    }
    list
}

/// Whether the wanted output can be opened from what Windows reports.
pub fn wanted_present(present: &[String], default: Option<&str>, wanted: &str) -> bool {
    if wanted == SYSTEM_DEFAULT {
        default.is_some()
    } else {
        present.iter().any(|name| name == wanted)
    }
}

/// When following the system default, a new default means reopening.
pub fn default_moved(wanted: &str, opened: Option<&str>, default: Option<&str>) -> bool {
    wanted == SYSTEM_DEFAULT && default.is_some() && opened.is_some() && opened != default
}

#[derive(Clone, Debug, PartialEq)]
struct Lost {
    at: Instant,
    was_playing: bool,
    uri: String,
}

/// Resume at most once per loss, only for the same song, only inside the window.
#[derive(Clone, Debug, Default)]
pub struct OutputRecovery {
    lost: Option<Lost>,
}

impl OutputRecovery {
    pub fn pending(&self) -> bool {
        self.lost.is_some()
    }

    /// The sink reported that the output failed while this song played.
    pub fn lost(&mut self, now: Instant, was_playing: bool, uri: &str) {
        if self.lost.is_none() {
            self.lost = Some(Lost {
                at: now,
                was_playing,
                uri: uri.into(),
            });
        }
    }

    /// The listener acted (play, pause, another song): stop waiting.
    pub fn cancel(&mut self) {
        self.lost = None;
    }

    /// Call while the output is known to be back. True means "press play".
    pub fn returned(&mut self, now: Instant, playing: bool, uri: &str) -> bool {
        let Some(lost) = self.lost.take() else {
            return false;
        };
        lost.was_playing
            && !playing
            && lost.uri == uri
            && now.saturating_duration_since(lost.at) <= RESUME_WINDOW
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn device_list_labels_the_default_and_a_missing_saved_output() {
        let present = names(&["Speakers", "Headphones", "Speakers"]);
        let list = choices(&present, Some("Speakers"), "Desk DAC");
        let labels: Vec<_> = list.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "System default · Speakers",
                "Speakers",
                "Headphones",
                "Desk DAC · unavailable"
            ]
        );
        assert!(!list[3].available);
        assert_eq!(choices(&present, None, "Headphones").len(), 3);
        assert_eq!(
            choices(&[], None, SYSTEM_DEFAULT)[0].label,
            "System default"
        );
    }

    #[test]
    fn default_changes_and_presence_follow_the_saved_choice() {
        let present = names(&["Speakers", "Headphones"]);
        assert!(wanted_present(&present, Some("Speakers"), SYSTEM_DEFAULT));
        assert!(!wanted_present(&[], None, SYSTEM_DEFAULT));
        assert!(wanted_present(&present, None, "Headphones"));
        assert!(!wanted_present(&present, None, "Desk DAC"));
        assert!(default_moved(
            SYSTEM_DEFAULT,
            Some("Speakers"),
            Some("Headphones")
        ));
        assert!(!default_moved(
            SYSTEM_DEFAULT,
            Some("Speakers"),
            Some("Speakers")
        ));
        assert!(
            !default_moved("Speakers", Some("Speakers"), Some("Headphones")),
            "A fixed choice stays"
        );
        assert!(
            !default_moved(SYSTEM_DEFAULT, None, Some("Headphones")),
            "Nothing open yet"
        );
    }

    #[test]
    fn output_return_resumes_once_for_the_same_song_inside_the_window() {
        let now = Instant::now();
        let mut recovery = OutputRecovery::default();
        recovery.lost(now, true, "spotify:track:a");
        recovery.lost(now + Duration::from_secs(5), false, "spotify:track:a");
        assert!(recovery.pending());
        assert!(recovery.returned(now + Duration::from_secs(30), false, "spotify:track:a"));
        assert!(
            !recovery.returned(now + Duration::from_secs(31), false, "spotify:track:a"),
            "Once only"
        );
        for (was_playing, playing, uri, after) in [
            (false, false, "spotify:track:a", 30),
            (true, true, "spotify:track:a", 30),
            (true, false, "spotify:track:b", 30),
            (true, false, "spotify:track:a", RESUME_WINDOW.as_secs() + 1),
        ] {
            let mut recovery = OutputRecovery::default();
            recovery.lost(now, was_playing, "spotify:track:a");
            assert!(!recovery.returned(now + Duration::from_secs(after), playing, uri));
        }
        recovery.lost(now, true, "spotify:track:a");
        recovery.cancel();
        assert!(!recovery.returned(now, false, "spotify:track:a"));
    }
}
