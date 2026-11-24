//! Spotify feature requests are explicit UI actions with bounded in-flight work.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Songs only, in pages of 10 (Spotify's February 2026 search limit).
    Search {
        query: String,
        offset: u32,
    },
    /// The All view: one multi-type search for songs, albums, artists and playlists.
    SearchAll {
        query: String,
    },
    /// One result type ("album", "artist" or "playlist"), paged by 10.
    SearchKind {
        query: String,
        kind: String,
        offset: u32,
    },
    Home,
    /// A full artist page: profile, top or release-derived songs, release shelves.
    ArtistPage {
        uri: String,
    },
    Follow {
        uri: String,
        follow: bool,
    },
    Radio {
        uri: String,
    },
    Save {
        uri: String,
        saved: bool,
    },
    Contains {
        uri: String,
    },
    TrackInfo {
        uri: String,
    },
    /// Several tracks at once: `/tracks?ids=` where the app may use it, one by one otherwise.
    Tracks {
        uris: Vec<String>,
    },
    Collection {
        uri: String,
        offset: u32,
    },
    Album {
        uri: String,
        offset: u32,
    },
    Playlists {
        offset: u32,
    },
    SavedAlbums {
        offset: u32,
    },
    FollowedArtists {
        after: Option<String>,
    },
    RecentlyPlayed,
    /// Grove's discovery mix, built from top songs, artists' releases and liked songs.
    Mix,
    /// Autoplay refill from the artists of what is playing now.
    Continue {
        artists: Vec<String>,
    },
    PlaylistAdd {
        playlist: String,
        uri: String,
    },
    /// Removes the exact entry at `position`. `copies` lists the other loaded
    /// positions of the same song so they can be restored if Spotify removes them too.
    PlaylistRemoveEntry {
        playlist: String,
        uri: String,
        position: u32,
        copies: Vec<u32>,
    },
}

impl Action {
    /// A key for read-only requests; identical in-flight reads are collapsed.
    pub fn read_key(&self) -> Option<String> {
        match self {
            Action::TrackInfo { uri } => Some(format!("track:{uri}")),
            Action::Contains { uri } => Some(format!("contains:{uri}")),
            Action::SavedAlbums { offset } => Some(format!("albums:{offset}")),
            Action::FollowedArtists { after } => {
                Some(format!("artists:{}", after.as_deref().unwrap_or_default()))
            }
            Action::RecentlyPlayed => Some("recent".into()),
            Action::Playlists { offset } => Some(format!("playlists:{offset}")),
            _ => None,
        }
    }
}
