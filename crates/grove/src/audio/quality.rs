//! The source format librespot will stream. librespot does not report its
//! choice, so this repeats its documented preference order (player.rs,
//! `load_track`) over the formats Spotify offers for the item.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    Vorbis96,
    Vorbis160,
    Vorbis320,
    Mp3_96,
    Mp3_160,
    Mp3_256,
    Mp3_320,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Self::Vorbis96 => "Ogg Vorbis · 96 kbps",
            Self::Vorbis160 => "Ogg Vorbis · 160 kbps",
            Self::Vorbis320 => "Ogg Vorbis · 320 kbps",
            Self::Mp3_96 => "MP3 · 96 kbps",
            Self::Mp3_160 => "MP3 · 160 kbps",
            Self::Mp3_256 => "MP3 · 256 kbps",
            Self::Mp3_320 => "MP3 · 320 kbps",
        }
    }

    pub fn from_librespot(format: librespot_metadata::audio::AudioFileFormat) -> Option<Self> {
        use librespot_metadata::audio::AudioFileFormat as F;
        Some(match format {
            F::OGG_VORBIS_96 => Self::Vorbis96,
            F::OGG_VORBIS_160 => Self::Vorbis160,
            F::OGG_VORBIS_320 => Self::Vorbis320,
            F::MP3_96 => Self::Mp3_96,
            F::MP3_160 => Self::Mp3_160,
            F::MP3_256 => Self::Mp3_256,
            F::MP3_320 => Self::Mp3_320,
            _ => return None,
        })
    }
}

fn preference(bitrate: u16) -> [Format; 7] {
    use Format::*;
    match bitrate {
        96 => [
            Vorbis96, Mp3_96, Vorbis160, Mp3_160, Mp3_256, Vorbis320, Mp3_320,
        ],
        320 => [
            Vorbis320, Mp3_320, Mp3_256, Vorbis160, Mp3_160, Vorbis96, Mp3_96,
        ],
        _ => [
            Vorbis160, Mp3_160, Vorbis96, Mp3_96, Mp3_256, Vorbis320, Mp3_320,
        ],
    }
}

/// The format librespot picks at `bitrate` from what is `available`.
pub fn chosen(bitrate: u16, available: &[Format]) -> Option<Format> {
    preference(bitrate)
        .into_iter()
        .find(|format| available.contains(format))
}

/// Now Playing text, e.g. "Ogg Vorbis · 320 kbps · 44.1 kHz".
pub fn source_label(bitrate: u16, available: &[Format]) -> String {
    match chosen(bitrate, available) {
        Some(format) => format!("{} · 44.1 kHz", format.label()),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Format::*;

    #[test]
    fn mirrors_librespot_preference_and_podcast_fallback() {
        let all = [Vorbis96, Vorbis160, Vorbis320, Mp3_96];
        assert_eq!(chosen(320, &all), Some(Vorbis320));
        assert_eq!(chosen(160, &all), Some(Vorbis160));
        assert_eq!(chosen(96, &all), Some(Vorbis96));
        // Podcasts often offer only 96 kbps; asking for 320 falls back.
        assert_eq!(chosen(320, &[Vorbis96]), Some(Vorbis96));
        assert_eq!(chosen(96, &[Mp3_320]), Some(Mp3_320));
        assert_eq!(chosen(160, &[]), None);
        assert_eq!(source_label(320, &all), "Ogg Vorbis · 320 kbps · 44.1 kHz");
        assert_eq!(source_label(320, &[]), "");
    }
}
