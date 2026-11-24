//! Grove-owned preferences. Spotify credentials remain in Credential Manager.
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub light: bool,
    pub compact: bool,
    pub autoplay: bool,
    pub media_keys: bool,
    pub local_history: bool,
    pub volume: f32,
    pub hidden: HashSet<String>,
    /// The accent follows the current artwork colour.
    pub accent_art: bool,
    /// Follow the Windows app light/dark setting instead of `light`.
    pub follow_system: bool,
    /// Closing the window hides Grove to the notification area.
    pub close_to_tray: bool,
    /// Global Play/pause, Next and Previous chords; "" when unset.
    pub shortcuts: [String; 3],
    #[serde(deserialize_with = "crate::audio::settings::lenient")]
    pub audio: crate::audio::settings::AudioSettings,
    /// Opt-in: reuse confirmed Tide ↔ Spotify recording choices across jams.
    pub remember_jam_matches: bool,
    pub jam_matches: BTreeMap<String, String>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            light: false,
            compact: false,
            autoplay: false,
            media_keys: true,
            local_history: false,
            volume: 35.0,
            hidden: HashSet::new(),
            accent_art: false,
            follow_system: false,
            close_to_tray: false,
            shortcuts: Default::default(),
            audio: Default::default(),
            remember_jam_matches: false,
            jam_matches: BTreeMap::new(),
        }
    }
}
/// Grove's per-user data folder: `%APPDATA%\GroveMusic` on Windows,
/// `~/Library/Application Support/GroveMusic` on macOS and
/// `$XDG_CONFIG_HOME/GroveMusic` (default `~/.config`) elsewhere.
pub fn data_dir() -> Option<PathBuf> {
    data_dir_from(|name| std::env::var_os(name))
}

fn data_dir_from(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    let absolute = |name: &str| var(name).map(PathBuf::from).filter(|p| p.is_absolute());
    let root = if cfg!(windows) {
        absolute("APPDATA")
    } else if cfg!(target_os = "macos") {
        absolute("HOME").map(|home| home.join("Library").join("Application Support"))
    } else {
        absolute("XDG_CONFIG_HOME").or_else(|| absolute("HOME").map(|home| home.join(".config")))
    };
    root.map(|root| root.join("GroveMusic"))
}

fn path() -> Result<PathBuf, String> {
    Ok(data_dir()
        .ok_or("The application-data directory is unavailable.")?
        .join("preferences.json"))
}
impl Preferences {
    pub fn load() -> Result<Self, String> {
        let path = path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let size = std::fs::metadata(&path)
            .map_err(|_| "Could not inspect Grove preferences.")?
            .len();
        if size > 256 * 1024 {
            return Err("Grove preferences are too large. Defaults were used.".into());
        }
        let bytes = std::fs::read(path).map_err(|_| "Could not read Grove preferences.")?;
        let mut prefs: Self = serde_json::from_slice(&bytes)
            .map_err(|_| "Grove preferences are unreadable. Defaults were used.")?;
        prefs.volume = if prefs.volume.is_finite() {
            prefs.volume.clamp(0.0, 100.0)
        } else {
            35.0
        };
        prefs
            .hidden
            .retain(|uri| crate::spotify::spotify_uri(uri).ok().as_deref() == Some(uri));
        if prefs.hidden.len() > 2000 {
            prefs.hidden.clear();
        }
        for shortcut in &mut prefs.shortcuts {
            if shortcut.len() > 64 {
                shortcut.clear();
            }
        }
        prefs.audio.sanitize();
        if !prefs.remember_jam_matches || prefs.jam_matches.len() > 200 {
            prefs.jam_matches.clear();
        }
        Ok(prefs)
    }
    pub fn save(&self) -> Result<(), String> {
        let path = path()?;
        let parent = path.parent().ok_or("Invalid Grove preference path.")?;
        std::fs::create_dir_all(parent)
            .map_err(|_| "Could not create Grove preference directory.")?;
        let temporary = parent.join(format!("preferences-{}.tmp", crate::jam::id()));
        let bytes = serde_json::to_vec(self).map_err(|_| "Could not prepare Grove preferences.")?;
        std::fs::write(&temporary, bytes).map_err(|_| "Could not write Grove preferences.")?;
        if std::fs::rename(&temporary, &path).is_err() {
            let _ = std::fs::remove_file(&temporary);
            return Err(
                "Could not save Grove preferences. Your changes apply only for this session."
                    .into(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_data_folder_follows_each_platform_and_ignores_relative_paths() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| std::ffi::OsString::from(v))
            }
        };
        #[cfg(windows)]
        {
            assert_eq!(
                data_dir_from(env(&[("APPDATA", r"C:\Users\a\AppData\Roaming")])),
                Some(PathBuf::from(r"C:\Users\a\AppData\Roaming\GroveMusic"))
            );
            assert_eq!(data_dir_from(env(&[("APPDATA", "relative")])), None);
        }
        #[cfg(target_os = "macos")]
        assert_eq!(
            data_dir_from(env(&[("HOME", "/Users/a")])),
            Some(PathBuf::from(
                "/Users/a/Library/Application Support/GroveMusic"
            ))
        );
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            assert_eq!(
                data_dir_from(env(&[("HOME", "/home/a")])),
                Some(PathBuf::from("/home/a/.config/GroveMusic"))
            );
            assert_eq!(
                data_dir_from(env(&[("HOME", "/home/a"), ("XDG_CONFIG_HOME", "/cfg")])),
                Some(PathBuf::from("/cfg/GroveMusic"))
            );
            assert_eq!(data_dir_from(env(&[("XDG_CONFIG_HOME", "rel")])), None);
        }
        assert_eq!(data_dir_from(env(&[])), None);
    }
    #[test]
    fn missing_preferences_have_safe_defaults_and_no_account_secrets() {
        let prefs: Preferences = serde_json::from_str("{}").unwrap();
        assert!(!prefs.local_history);
        assert!(!prefs.autoplay);
        assert_eq!(prefs.volume, 35.0);
        assert!(!prefs.follow_system && !prefs.close_to_tray);
        assert!(prefs.shortcuts.iter().all(String::is_empty));
        let json = serde_json::to_string(&prefs).unwrap();
        assert!(!json.contains("token"));
        assert!(!json.contains("credential"));
    }
}
