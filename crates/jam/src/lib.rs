//! Jam rooms shared by Tide and Grove. Only track metadata and timing travel.
//!
//! * [`transport`]: the worker that hosts or follows a room, nearby or relayed.
//! * [`secure`]: encrypted nearby channel keyed from the invite (protocol 3).
//! * [`relay`] / [`relay_server`]: Tide's HTTP relay protocol (1) plus the
//!   cross-service extension (2), and a free self-hostable relay.
//! * [`clock`] / [`sync`]: NTP-style offsets, scheduled starts, drift policy.
//! * [`matching`]: ISRC/duration-aware Tide ↔ Grove recording matching.
pub mod clock;
pub mod invite;
pub mod jam;
pub mod matching;
pub mod relay;
pub mod relay_server;
pub mod secure;
pub mod sync;
pub mod transport;
pub mod version;

/// Spotify album or playlist identifiers, for adding a whole collection.
pub fn collection_uri(input: &str) -> Result<String, String> {
    let input = input.trim();
    for kind in ["album", "playlist"] {
        if let Some(id) = input.strip_prefix(&format!("spotify:{kind}:")) {
            if id.len() == 22 && id.bytes().all(|c| c.is_ascii_alphanumeric()) {
                return Ok(input.into());
            }
        }
    }
    if let Ok(url) = url::Url::parse(input) {
        if url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && url.host_str() == Some("open.spotify.com")
        {
            let parts: Vec<_> = url.path_segments().into_iter().flatten().collect();
            if let [kind @ ("album" | "playlist"), id] = parts.as_slice() {
                return collection_uri(&format!("spotify:{kind}:{id}"));
            }
        }
    }
    Err("Use a Spotify album or playlist link.".into())
}

/// Canonical provider identifiers only; arbitrary URLs and files never enter a room.
pub fn track_uri(input: &str) -> Result<String, String> {
    let input = input.trim();
    if let Some(id) = input.strip_prefix("spotify:track:") {
        if id.len() == 22 && id.bytes().all(|c| c.is_ascii_alphanumeric()) {
            return Ok(input.into());
        }
    }
    if let Some(id) = input.strip_prefix("youtube:") {
        if youtube_id(id) {
            return Ok(input.into());
        }
    }
    if let Ok(url) = url::Url::parse(input) {
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
        {
            return Err("Use a Spotify song or YouTube video link.".into());
        }
        match url.host_str().unwrap_or("") {
            "open.spotify.com" => {
                let parts: Vec<_> = url.path_segments().into_iter().flatten().collect();
                if let ["track", id] = parts.as_slice() {
                    return track_uri(&format!("spotify:track:{id}"));
                }
            }
            "youtube.com" | "www.youtube.com" | "music.youtube.com" if url.path() == "/watch" => {
                if let Some((_, id)) = url.query_pairs().find(|(k, _)| k == "v") {
                    if youtube_id(&id) {
                        return Ok(format!("youtube:{id}"));
                    }
                }
            }
            "youtu.be" => {
                let id = url.path().trim_start_matches('/');
                if youtube_id(id) {
                    return Ok(format!("youtube:{id}"));
                }
            }
            _ => {}
        }
    }
    Err("Use a Spotify song or YouTube video link.".into())
}
fn youtube_id(id: &str) -> bool {
    id.len() == 11
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifiers_are_provider_scoped_and_never_arbitrary_sources() {
        assert_eq!(
            track_uri("https://music.youtube.com/watch?v=abcdefghijk&list=x").unwrap(),
            "youtube:abcdefghijk"
        );
        assert_eq!(
            track_uri("https://open.spotify.com/track/aaaaaaaaaaaaaaaaaaaaaa?si=x").unwrap(),
            "spotify:track:aaaaaaaaaaaaaaaaaaaaaa"
        );
        for bad in [
            "file:///C:/song.mp3",
            "https://evil.test/watch?v=abcdefghijk",
            "https://user@youtube.com/watch?v=abcdefghijk",
            "youtube:../whatever",
            "spotify:album:aaaaaaaaaaaaaaaaaaaaaa",
        ] {
            assert!(track_uri(bad).is_err());
        }
        assert_eq!(
            collection_uri("https://open.spotify.com/album/aaaaaaaaaaaaaaaaaaaaaa?si=1").unwrap(),
            "spotify:album:aaaaaaaaaaaaaaaaaaaaaa"
        );
        assert!(collection_uri("spotify:playlist:bbbbbbbbbbbbbbbbbbbbbb").is_ok());
        assert!(collection_uri("spotify:track:bbbbbbbbbbbbbbbbbbbbbb").is_err());
        assert!(collection_uri("https://evil.test/album/aaaaaaaaaaaaaaaaaaaaaa").is_err());
    }
}
