//! Saved audio choices. Every field is bounded when loaded, so a hand-edited
//! preferences file can never reach the engine with an unsupported value.
use super::dsp::{CUSTOM_PRESET, EQ_FREQUENCIES_10, EQ_FREQUENCIES_5, EQ_PRESETS};
use super::resume::ResumeStore;
use serde::{Deserialize, Serialize};

/// Spotify stream bitrates librespot can request.
pub const BITRATES: [u16; 3] = [96, 160, 320];
/// Fade lengths offered in Settings, in seconds; 0 is off.
pub const FADE_LENGTHS: [u8; 7] = [0, 2, 4, 6, 8, 10, 12];
/// Normalisation pregain choices in dB.
pub const PREGAINS: [i8; 4] = [-6, -3, 0, 3];
/// Largest boost or cut of a custom equalizer band, in dB.
pub const BAND_LIMIT_DB: f32 = 12.0;
/// "auto" follows the Windows default output.
pub const SYSTEM_DEFAULT: &str = "auto";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum NormalisationKind {
    /// Album gain inside albums, track gain elsewhere (librespot's choice).
    #[default]
    Auto,
    Track,
    Album,
}

impl NormalisationKind {
    pub const ALL: [Self; 3] = [Self::Auto, Self::Track, Self::Album];
    pub fn index(self) -> i32 {
        Self::ALL.iter().position(|k| *k == self).unwrap_or(0) as i32
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    pub bitrate: u16,
    pub normalise: bool,
    pub normalisation: NormalisationKind,
    pub pregain_db: i8,
    /// [`SYSTEM_DEFAULT`] or the Windows name of an output device.
    pub device: String,
    /// 0 (off) or one of [`FADE_LENGTHS`].
    pub fade_seconds: u8,
    /// Index into [`EQ_PRESETS`]; [`CUSTOM_PRESET`] uses `custom_bands`.
    pub eq_preset: u8,
    /// 5 or 10 custom bands.
    pub eq_band_count: u8,
    pub custom_bands: Vec<f32>,
    pub remember_positions: bool,
    pub resume: ResumeStore,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            bitrate: 160,
            normalise: false,
            normalisation: NormalisationKind::Auto,
            pregain_db: 0,
            device: SYSTEM_DEFAULT.into(),
            fade_seconds: 0,
            eq_preset: 0,
            eq_band_count: 10,
            custom_bands: vec![0.0; 10],
            remember_positions: true,
            resume: ResumeStore::default(),
        }
    }
}

impl AudioSettings {
    /// Replace anything out of range with its default. Returns true when changed.
    pub fn sanitize(&mut self) -> bool {
        let before = self.clone();
        let defaults = Self::default();
        if !BITRATES.contains(&self.bitrate) {
            self.bitrate = defaults.bitrate;
        }
        if !PREGAINS.contains(&self.pregain_db) {
            self.pregain_db = 0;
        }
        if !valid_device(&self.device) {
            self.device = SYSTEM_DEFAULT.into();
        }
        if !FADE_LENGTHS.contains(&self.fade_seconds) {
            self.fade_seconds = 0;
        }
        if usize::from(self.eq_preset) > CUSTOM_PRESET {
            self.eq_preset = 0;
        }
        if self.eq_band_count != 5 && self.eq_band_count != 10 {
            self.eq_band_count = 10;
        }
        let count = usize::from(self.eq_band_count);
        self.custom_bands.truncate(count);
        self.custom_bands.resize(count, 0.0);
        for gain in &mut self.custom_bands {
            *gain = if gain.is_finite() {
                (gain.clamp(-BAND_LIMIT_DB, BAND_LIMIT_DB) * 2.0).round() / 2.0
            } else {
                0.0
            };
        }
        self.resume.sanitize();
        *self != before
    }

    pub fn band_frequencies(&self) -> &'static [f32] {
        if self.eq_band_count == 5 {
            &EQ_FREQUENCIES_5
        } else {
            &EQ_FREQUENCIES_10
        }
    }

    /// The (frequency, gain dB) bands the equalizer should run, empty when flat.
    pub fn eq_bands(&self) -> Vec<(f32, f32)> {
        let preset = usize::from(self.eq_preset);
        let bands: Vec<(f32, f32)> = if preset == CUSTOM_PRESET {
            self.band_frequencies()
                .iter()
                .copied()
                .zip(self.custom_bands.iter().copied())
                .collect()
        } else {
            EQ_PRESETS
                .get(preset)
                .map(|(_, bands)| bands.to_vec())
                .unwrap_or_default()
        };
        if bands.iter().all(|(_, gain)| gain.abs() < 0.05) {
            Vec::new()
        } else {
            bands
        }
    }

    /// Switch between 5 and 10 custom bands, carrying the curve across.
    pub fn set_band_count(&mut self, count: u8) {
        if count != 5 && count != 10 || count == self.eq_band_count {
            return;
        }
        let old: Vec<(f32, f32)> = self
            .band_frequencies()
            .iter()
            .copied()
            .zip(self.custom_bands.iter().copied())
            .collect();
        self.eq_band_count = count;
        self.custom_bands = self
            .band_frequencies()
            .iter()
            .map(|f| {
                old.iter()
                    .min_by(|a, b| {
                        (a.0.ln() - f.ln())
                            .abs()
                            .total_cmp(&(b.0.ln() - f.ln()).abs())
                    })
                    .map(|band| band.1)
                    .unwrap_or(0.0)
            })
            .collect();
    }

    /// Whether a change from `old` needs librespot's player rebuilt.
    pub fn needs_engine_restart(&self, old: &Self) -> bool {
        self.bitrate != old.bitrate
            || self.normalise != old.normalise
            || self.normalisation != old.normalisation
            || self.pregain_db != old.pregain_db
    }
}

/// A damaged audio section falls back to defaults without discarding the
/// rest of the preferences file.
pub fn lenient<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<AudioSettings, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

pub fn valid_device(device: &str) -> bool {
    device == SYSTEM_DEFAULT
        || (!device.trim().is_empty()
            && device.len() <= 256
            && !device.chars().any(char::is_control))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hand_edited_values_fall_back_to_safe_defaults() {
        let mut settings: AudioSettings = serde_json::from_str(
            r#"{"bitrate":999,"pregain_db":40,"device":"bad\nname","fade_seconds":5,
                "eq_preset":200,"eq_band_count":7}"#,
        )
        .unwrap();
        settings.sanitize();
        assert_eq!(settings, AudioSettings::default());
        let mut settings: AudioSettings = serde_json::from_str(
            r#"{"bitrate":320,"fade_seconds":6,"eq_preset":7,"eq_band_count":5,"custom_bands":[30,-30,1.26]}"#,
        )
        .unwrap();
        assert!(settings.sanitize());
        assert_eq!(settings.bitrate, 320);
        assert_eq!(settings.fade_seconds, 6);
        assert_eq!(settings.custom_bands, vec![12.0, -12.0, 1.5, 0.0, 0.0]);
        assert!(!settings.sanitize(), "sanitize is idempotent");
    }

    #[test]
    fn a_damaged_audio_section_keeps_other_preferences() {
        let prefs: crate::preferences::Preferences = serde_json::from_str(
            r#"{"light":true,"audio":{"normalisation":"loudest","bitrate":"x"}}"#,
        )
        .unwrap();
        assert!(prefs.light);
        assert_eq!(prefs.audio, AudioSettings::default());
    }

    #[test]
    fn presets_and_custom_curves_produce_bounded_bands() {
        let mut settings = AudioSettings::default();
        assert!(settings.eq_bands().is_empty(), "Flat runs no filters");
        settings.eq_preset = 1;
        assert!(!settings.eq_bands().is_empty());
        settings.eq_preset = CUSTOM_PRESET as u8;
        assert!(
            settings.eq_bands().is_empty(),
            "A flat custom curve runs no filters"
        );
        settings.custom_bands[1] = 6.0;
        assert_eq!(settings.eq_bands().len(), 10);
        settings.set_band_count(5);
        assert_eq!(settings.custom_bands.len(), 5);
        assert_eq!(
            settings.custom_bands[0], 6.0,
            "The 62 Hz boost carried to 60 Hz"
        );
        settings.set_band_count(7);
        assert_eq!(settings.eq_band_count, 5);
    }

    #[test]
    fn only_stream_choices_restart_the_engine() {
        let base = AudioSettings::default();
        let mut eq = base.clone();
        eq.eq_preset = 2;
        eq.fade_seconds = 4;
        eq.device = "Headphones".into();
        assert!(!eq.needs_engine_restart(&base));
        for change in [
            |s: &mut AudioSettings| s.bitrate = 320,
            |s: &mut AudioSettings| s.normalise = true,
            |s: &mut AudioSettings| s.normalisation = NormalisationKind::Album,
            |s: &mut AudioSettings| s.pregain_db = -3,
        ] {
            let mut next = base.clone();
            change(&mut next);
            assert!(next.needs_engine_restart(&base));
        }
    }
}
