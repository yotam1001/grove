//! Spotify Web API access for browsing, library and playlist edits.
//!
//! Requests are grouped into budgets (search, library, artists, …). A 429 on one
//! budget cools down only that budget, honours `Retry-After`, and never touches
//! playback, which runs through the native engine. Endpoints that Spotify closed
//! to development-mode apps in February 2026 (`/tracks?ids=`, artist top tracks)
//! are tried once and then replaced by the per-item routes for the session.
use crate::auth::{self, Tokens};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

#[derive(Serialize, Deserialize)]
pub struct SavedLibrary {
    pub client_id: String,
    pub refresh_token: String,
    pub account_id: String,
}

impl Drop for SavedLibrary {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.refresh_token.zeroize();
    }
}

/// Why a request did not produce data.
#[derive(Debug, Clone, PartialEq)]
pub enum ApiError {
    /// A 429. `quota` is Spotify's `QUOTA_EXCEEDED`, which a short wait does not fix.
    Throttled {
        budget: &'static str,
        seconds: u64,
        quota: bool,
    },
    /// 403/404/410: the endpoint or item is not available to this app or grant.
    Unavailable(String),
    /// A write whose outcome is unknown (network failure, timeout or 5xx).
    Unconfirmed,
    Message(String),
}

impl From<String> for ApiError {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}
impl From<&str> for ApiError {
    fn from(message: &str) -> Self {
        Self::Message(message.into())
    }
}
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Throttled { seconds, quota, .. } => {
                f.write_str(&throttle_message(*quota, *seconds))
            }
            Self::Unavailable(message) | Self::Message(message) => f.write_str(message),
            Self::Unconfirmed => {
                f.write_str("Spotify did not confirm the change. Refresh before retrying.")
            }
        }
    }
}
type ApiResult<T> = Result<T, ApiError>;

/// A raw HTTP reply. `body` is `None` when the response was empty or unreadable.
pub struct Reply {
    pub status: u16,
    pub retry_after: Option<u64>,
    pub body: Option<Value>,
}

/// Synthetic responder used by offline tests: `(method, path, body)` to a reply,
/// or `None` for a network failure.
#[cfg(test)]
pub type Responder = Box<dyn FnMut(&str, &str, Option<&Value>) -> Option<Reply> + Send>;

enum Transport {
    Http(reqwest::Client),
    #[cfg(test)]
    Fake(std::sync::Mutex<Responder>),
}

pub struct WebApi {
    pub client_id: String,
    pub tokens: Tokens,
    pub account_id: String,
    pub name: String,
    pub premium: Option<bool>,
    transport: Transport,
    /// Per-budget cooldowns: budget → (until, quota exhausted).
    cooldowns: HashMap<&'static str, (Instant, bool)>,
    /// Endpoints this app cannot use, learned from 403/404 replies this session.
    unavailable: HashSet<&'static str>,
    pub grant_dirty: bool,
}

/// The budget a path belongs to. Cooldowns apply per budget.
pub fn budget(path: &str) -> &'static str {
    let path = path.split('?').next().unwrap_or_default();
    for (prefix, name) in [
        ("/search", "search"),
        ("/me/top", "top"),
        ("/me/player/recently-played", "recent"),
        ("/me/player", "player"),
        ("/me/library", "library"),
        ("/me/tracks", "saved"),
        ("/me/albums", "saved"),
        ("/me/following", "saved"),
        ("/me/playlists", "playlists"),
        ("/playlists", "playlists"),
        ("/artists", "artists"),
        ("/albums", "albums"),
        ("/tracks", "tracks"),
    ] {
        if path.starts_with(prefix) {
            return name;
        }
    }
    "profile"
}

impl WebApi {
    fn with_transport(client_id: String, tokens: Tokens, transport: Transport) -> Self {
        Self {
            client_id,
            tokens,
            account_id: String::new(),
            name: String::new(),
            premium: None,
            transport,
            cooldowns: HashMap::new(),
            unavailable: HashSet::new(),
            grant_dirty: false,
        }
    }

    #[cfg(test)]
    pub fn fake(account: &str, responder: Responder) -> Self {
        let tokens = Tokens {
            access: "test-access".into(),
            refresh: "test-refresh".into(),
            expires: Instant::now() + Duration::from_secs(3600),
        };
        let mut api = Self::with_transport(
            "test-client".into(),
            tokens,
            Transport::Fake(std::sync::Mutex::new(responder)),
        );
        api.account_id = account.into();
        api
    }

    /// Runs one UI action. Throttling becomes a `throttled` result the UI shows
    /// as a cooldown with a Retry button, instead of an opaque error.
    pub async fn feature(&mut self, action: crate::library::Action) -> Result<Value, String> {
        match self.run(action).await {
            Ok(value) => Ok(value),
            Err(ApiError::Throttled {
                budget,
                seconds,
                quota,
            }) => Ok(throttled_value(budget, seconds, quota)),
            Err(error) => Err(error.to_string()),
        }
    }

    async fn run(&mut self, action: crate::library::Action) -> ApiResult<Value> {
        use crate::library::Action;
        match action {
            Action::TrackInfo { uri } => {
                let id = entity_id(&uri, "track")?;
                let track = self.get(&format!("/tracks/{id}")).await?;
                Ok(json!({"kind":"track_info","track":track}))
            }
            Action::Tracks { uris } => {
                let tracks = self.tracks(&uris).await?;
                Ok(json!({"kind":"tracks","tracks":tracks}))
            }
            Action::Search { query, offset } => {
                let query = valid_query(&query)?;
                let page = self.search(&query, "track", offset).await?;
                Ok(
                    json!({"kind":"search","query":query,"offset":offset,"tracks":page["tracks"]["items"],"has_more":!page["tracks"]["next"].is_null()}),
                )
            }
            Action::SearchAll { query } => {
                let query = valid_query(&query)?;
                let page = self
                    .search(&query, "track,album,artist,playlist", 0)
                    .await?;
                Ok(search_all(&query, &page))
            }
            Action::SearchKind {
                query,
                kind,
                offset,
            } => {
                let query = valid_query(&query)?;
                if !["album", "artist", "playlist"].contains(&kind.as_str()) {
                    return Err("Choose albums, artists or playlists.".into());
                }
                let page = self.search(&query, &kind, offset).await?;
                let key = format!("{kind}s");
                let cards = cards(&page[&key]["items"], &kind);
                Ok(
                    json!({"kind":"search_kind","query":query,"type":kind,"offset":offset,"cards":cards,"has_more":!page[&key]["next"].is_null()}),
                )
            }
            Action::Home => self.home().await,
            Action::ArtistPage { uri } => self.artist_page(&uri).await,
            Action::Follow { uri, follow } => {
                entity_id(&uri, "artist")?;
                self.save_uri(&uri, follow).await?;
                Ok(json!({"kind":"followed","uri":uri,"following":follow}))
            }
            Action::Radio { uri } => {
                let id = entity_id(&uri, "track")?;
                let track = self.get(&format!("/tracks/{id}")).await?;
                let artist = track["artists"][0]["uri"]
                    .as_str()
                    .ok_or("Spotify did not return an artist for this song.")?;
                let artist_id = entity_id(artist, "artist")?;
                let mut tracks = self.artist_tracks(&artist_id, 3).await?;
                tracks.retain(|t| t["uri"].as_str() != Some(&uri));
                Ok(
                    json!({"kind":"radio","tracks":tracks,"seed":uri,"title":"Artist discovery","note":"More from this artist's recent releases. Spotify personalized radio is not exposed to Grove."}),
                )
            }
            Action::Save { uri, saved } => {
                entity_id(&uri, "track")?;
                self.save_uri(&uri, saved).await?;
                Ok(json!({"kind":"saved","uri":uri,"saved":saved}))
            }
            Action::Contains { uri } => {
                if !uri.starts_with("spotify:track:") {
                    entity_id(&uri, "artist")?;
                }
                let saved = self.contains(&uri).await?;
                Ok(json!({"kind":"saved","uri":uri,"saved":saved}))
            }
            Action::Collection { uri, offset } => {
                let path = if uri == "liked" {
                    format!("/me/tracks?limit=50&offset={}", offset.min(100_000))
                } else {
                    format!(
                        "/playlists/{}/items?limit=50&offset={}",
                        entity_id(&uri, "playlist")?,
                        offset.min(100_000)
                    )
                };
                let page = match self.get(&path).await {
                    Ok(page) => page,
                    // February 2026: only owned or collaborative playlists list their items.
                    Err(ApiError::Unavailable(_)) if uri != "liked" => {
                        return Ok(
                            json!({"kind":"collection_page","uri":uri,"offset":offset,"items":[],"has_more":false,"restricted":true,"note":"Spotify only shares the songs of playlists you own or collaborate on with apps like Grove. Open it in Spotify to listen."}),
                        )
                    }
                    Err(error) => return Err(error),
                };
                Ok(
                    json!({"kind":"collection_page","uri":uri,"offset":offset,"items":page["items"],"total":page["total"],"has_more":!page["next"].is_null()}),
                )
            }
            Action::Album { uri, offset } => self.album(&uri, offset).await,
            Action::Playlists { offset } => {
                let page = self
                    .get(&format!(
                        "/me/playlists?limit=50&offset={}",
                        offset.min(100_000)
                    ))
                    .await?;
                Ok(
                    json!({"kind":"playlists_page","offset":offset,"items":page["items"],"has_more":!page["next"].is_null(),"account":self.account_id}),
                )
            }
            Action::SavedAlbums { offset } => {
                let page = self
                    .get(&format!(
                        "/me/albums?limit=50&offset={}",
                        offset.min(100_000)
                    ))
                    .await?;
                let albums: Vec<_> = page["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|entry| card(&entry["album"], "album"))
                    .collect();
                Ok(
                    json!({"kind":"library_albums","offset":offset,"cards":albums,"has_more":!page["next"].is_null()}),
                )
            }
            Action::FollowedArtists { after } => {
                let mut path = "/me/following?type=artist&limit=50".to_owned();
                if let Some(after) = after.filter(|a| entity_id_bare(a)) {
                    path.push_str(&format!("&after={after}"));
                }
                let page = self.get(&path).await?;
                let artists = cards(&page["artists"]["items"], "artist");
                Ok(
                    json!({"kind":"library_artists","cards":artists,"after":page["artists"]["cursors"]["after"]}),
                )
            }
            Action::RecentlyPlayed => {
                let page = self.get("/me/player/recently-played?limit=50").await?;
                Ok(json!({"kind":"recently_played","tracks":recent_tracks(&page)}))
            }
            Action::Mix => self.mix().await,
            Action::Continue { artists } => {
                let mut tracks = Vec::new();
                for uri in artists.iter().take(2) {
                    let id = entity_id(uri, "artist")?;
                    match self.artist_tracks(&id, 2).await {
                        Ok(more) => tracks.extend(more),
                        Err(error @ ApiError::Throttled { .. }) if tracks.is_empty() => {
                            return Err(error)
                        }
                        Err(_) => break,
                    }
                }
                Ok(json!({"kind":"continue","tracks":dedupe(tracks, 60)}))
            }
            Action::PlaylistAdd { playlist, uri } => self.playlist_add(&playlist, &uri).await,
            Action::PlaylistRemoveEntry {
                playlist,
                uri,
                position,
                copies,
            } => {
                self.playlist_remove(&playlist, &uri, position, &copies)
                    .await
            }
        }
    }

    async fn search(&mut self, query: &str, types: &str, offset: u32) -> ApiResult<Value> {
        let parameters = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("q", query)
            .append_pair("type", types)
            .append_pair("limit", "10")
            .append_pair("offset", &offset.min(1000).to_string())
            .finish();
        self.get(&format!("/search?{parameters}")).await
    }

    /// Home shelves from whatever this grant can read. Each source is optional.
    async fn home(&mut self) -> ApiResult<Value> {
        let mut shelves = Vec::new();
        let mut throttle = None;
        let mut note = "Recently in your rotation";
        let mut tracks = match self
            .get("/me/top/tracks?limit=50&time_range=short_term")
            .await
        {
            Ok(page) => page["items"].as_array().cloned().unwrap_or_default(),
            Err(error) => {
                remember_throttle(&mut throttle, &error);
                vec![]
            }
        };
        if tracks.is_empty() {
            note = "From your liked songs";
            match self.get("/me/tracks?limit=50").await {
                Ok(page) => tracks = saved_tracks(&page),
                Err(error) => remember_throttle(&mut throttle, &error),
            }
        }
        match self.get("/me/player/recently-played?limit=50").await {
            Ok(page) => {
                let recent = recent_tracks(&page);
                let mut seen = HashSet::new();
                // Recent listening as album covers, newest first, one card per album.
                let albums: Vec<_> = recent
                    .iter()
                    .filter_map(|t| card(&t["album"], "album"))
                    .filter(|c| seen.insert(c["uri"].as_str().unwrap_or_default().to_owned()))
                    .take(12)
                    .collect();
                if !albums.is_empty() {
                    shelves.push(json!({"id":"recent","title":"Recently played","cards":albums}));
                }
                if tracks.is_empty() {
                    note = "From your recent listening";
                    tracks = recent;
                }
            }
            Err(error) => remember_throttle(&mut throttle, &error),
        }
        match self
            .get("/me/top/artists?limit=12&time_range=medium_term")
            .await
        {
            Ok(page) => {
                let artists = cards(&page["items"], "artist");
                if !artists.is_empty() {
                    shelves
                        .push(json!({"id":"artists","title":"Your top artists","cards":artists}));
                }
            }
            Err(error) => remember_throttle(&mut throttle, &error),
        }
        match self.get("/me/albums?limit=12").await {
            Ok(page) => {
                let albums: Vec<_> = page["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|entry| card(&entry["album"], "album"))
                    .collect();
                if !albums.is_empty() {
                    shelves.push(json!({"id":"albums","title":"Saved albums","cards":albums}));
                }
            }
            Err(error) => remember_throttle(&mut throttle, &error),
        }
        if tracks.is_empty() && shelves.is_empty() {
            if let Some(error) = throttle {
                return Err(error);
            }
        }
        let mut value = json!({"kind":"home","tracks":tracks,"shelves":shelves,"note":note});
        if let Some(ApiError::Throttled {
            budget,
            seconds,
            quota,
        }) = throttle
        {
            value["cooldown"] = throttled_value(budget, seconds, quota);
        }
        Ok(value)
    }

    async fn artist_page(&mut self, uri: &str) -> ApiResult<Value> {
        let id = entity_id(uri, "artist")?;
        let artist = self.get(&format!("/artists/{id}")).await?;
        let releases = self
            .get(&format!(
                "/artists/{id}/albums?include_groups=album,single&limit=10"
            ))
            .await
            .map(|page| page["items"].as_array().cloned().unwrap_or_default())
            .unwrap_or_default();
        let mut top = Vec::new();
        let mut note = "Popular songs";
        if !self.unavailable.contains("artist-top-tracks") {
            match self
                .get(&format!("/artists/{id}/top-tracks?market=from_token"))
                .await
            {
                Ok(page) => top = page["tracks"].as_array().cloned().unwrap_or_default(),
                Err(ApiError::Unavailable(_)) => {
                    self.unavailable.insert("artist-top-tracks");
                }
                Err(_) => {}
            }
        }
        if top.is_empty() {
            // Top tracks are closed to new apps: take songs from the newest releases.
            note = "From recent releases";
            let mut newest: Vec<&Value> = releases.iter().collect();
            newest.sort_by(|a, b| {
                b["release_date"]
                    .as_str()
                    .unwrap_or_default()
                    .cmp(a["release_date"].as_str().unwrap_or_default())
            });
            for album in newest.into_iter().take(2) {
                match self.release_tracks(album, 10).await {
                    Ok(tracks) => top.extend(tracks),
                    Err(_) => break,
                }
            }
            top = interleave_by_album(dedupe(top, 50));
        }
        top.truncate(10);
        let albums: Vec<_> = releases
            .iter()
            .filter(|a| a["album_type"] != "single")
            .filter_map(|a| card(a, "album"))
            .collect();
        let singles: Vec<_> = releases
            .iter()
            .filter(|a| a["album_type"] == "single")
            .filter_map(|a| card(a, "album"))
            .collect();
        let following = self.contains(uri).await.ok();
        Ok(json!({
            "kind":"artist_page","uri":uri,"title":artist["name"],
            "image":pick_image(&artist["images"]),
            "genres":artist["genres"],
            "tracks":top,"note":note,
            "albums":albums,"singles":singles,"following":following
        }))
    }

    async fn album(&mut self, uri: &str, offset: u32) -> ApiResult<Value> {
        let id = entity_id(uri, "album")?;
        let (album, page) = if offset == 0 {
            let album = self.get(&format!("/albums/{id}")).await?;
            let page = album["tracks"].clone();
            (album, page)
        } else {
            let page = self
                .get(&format!(
                    "/albums/{id}/tracks?limit=50&offset={}",
                    offset.min(10_000)
                ))
                .await?;
            (Value::Null, page)
        };
        let summary = json!({"uri":uri,"name":album["name"],"images":album["images"]});
        let tracks: Vec<_> = page["items"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|t| {
                let mut t = t.clone();
                if t["album"].is_null() {
                    t["album"] = summary.clone();
                }
                t
            })
            .collect();
        Ok(json!({
            "kind":"album","uri":uri,"offset":offset,"tracks":tracks,
            "title":album["name"],"image":pick_image(&album["images"]),
            "artists":album["artists"],"release_date":album["release_date"],
            "total":album["total_tracks"],"album_type":album["album_type"],
            "has_more":!page["next"].is_null()
        }))
    }

    /// Discovery mix: Spotify's recommendation endpoints are closed to new apps,
    /// so mix top songs, top artists' newest releases and their liked songs.
    async fn mix(&mut self) -> ApiResult<Value> {
        let mut throttle = None;
        let top = match self
            .get("/me/top/tracks?limit=20&time_range=short_term")
            .await
        {
            Ok(page) => page["items"].as_array().cloned().unwrap_or_default(),
            Err(error) => {
                remember_throttle(&mut throttle, &error);
                vec![]
            }
        };
        let artists: Vec<String> = match self.get("/me/top/artists?limit=5").await {
            Ok(page) => page["items"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|a| a["uri"].as_str().map(str::to_owned))
                .collect(),
            Err(error) => {
                remember_throttle(&mut throttle, &error);
                // Fall back to the artists of the top songs.
                top.iter()
                    .filter_map(|t| t["artists"][0]["uri"].as_str().map(str::to_owned))
                    .collect()
            }
        };
        let saved = match self.get("/me/tracks?limit=50").await {
            Ok(page) => saved_tracks(&page),
            Err(error) => {
                remember_throttle(&mut throttle, &error);
                vec![]
            }
        };
        let mut releases = Vec::new();
        let mut seen = HashSet::new();
        for uri in artists.iter().filter(|u| seen.insert(u.as_str())).take(2) {
            let Ok(id) = entity_id(uri, "artist") else {
                continue;
            };
            match self.artist_tracks(&id, 1).await {
                Ok(tracks) => releases.extend(tracks.into_iter().take(8)),
                Err(error) => {
                    remember_throttle(&mut throttle, &error);
                    break;
                }
            }
        }
        // Liked songs by the same artists count as "related" listening.
        let related: Vec<_> = saved
            .iter()
            .filter(|t| {
                t["artists"].as_array().into_iter().flatten().any(|a| {
                    a["uri"]
                        .as_str()
                        .is_some_and(|u| artists.iter().any(|x| x == u))
                })
            })
            .cloned()
            .collect();
        let rest: Vec<_> = saved.into_iter().take(20).collect();
        let tracks = dedupe(round_robin(vec![top, releases, related, rest]), 60);
        if tracks.is_empty() {
            if let Some(error) = throttle {
                return Err(error);
            }
        }
        Ok(json!({
            "kind":"mix","uri":"mix","title":"Discovery mix","tracks":tracks,
            "note":"Built from your top songs, your artists' newest releases and liked songs. Spotify's recommendations aren't available to Grove."
        }))
    }

    async fn playlist_add(&mut self, playlist: &str, uri: &str) -> ApiResult<Value> {
        entity_id(uri, "track")?;
        let id = entity_id(playlist, "playlist")?;
        let snapshot = self.require_owned_playlist(&id).await?;
        let path = format!("/playlists/{id}/items");
        let body = json!({"uris":[uri]});
        let mut message = "Song added to your playlist.";
        if let Err(error) = self.write(reqwest::Method::POST, &path, Some(&body)).await {
            if error != ApiError::Unconfirmed {
                return Err(error);
            }
            // Re-read before retrying: an unconfirmed add may already have applied.
            let fresh = self.require_owned_playlist(&id).await?;
            if fresh != snapshot {
                message =
                    "Spotify changed the playlist while confirming. Check it before adding again.";
            } else {
                self.write(reqwest::Method::POST, &path, Some(&body))
                    .await
                    .map_err(|_| "Spotify still has not confirmed the change. Refresh the playlist before trying again.")?;
                message = "Song added to your playlist after a safe retry.";
            }
        }
        Ok(json!({"kind":"playlist_changed","uri":playlist,"added":uri,"message":message}))
    }

    async fn playlist_remove(
        &mut self,
        playlist: &str,
        uri: &str,
        position: u32,
        copies: &[u32],
    ) -> ApiResult<Value> {
        entity_id(uri, "track")?;
        let id = entity_id(playlist, "playlist")?;
        let snapshot = self.require_owned_playlist(&id).await?;
        // The entry and its neighbour, so an unconfirmed removal can be checked.
        let (entry, next) = self.window(&id, position).await?;
        if entry.as_deref() != Some(uri) {
            return Err(
                "This playlist changed since Grove loaded it. Refresh it before removing songs."
                    .into(),
            );
        }
        let path = format!("/playlists/{id}/items");
        // The entry's position plus the revision Grove saw: Spotify checks both.
        let body = json!({"items":[{"uri":uri,"positions":[position]}],"snapshot_id":snapshot});
        let mut retried = false;
        if let Err(error) = self
            .write(reqwest::Method::DELETE, &path, Some(&body))
            .await
        {
            if error != ApiError::Unconfirmed {
                return Err(error);
            }
            // Re-read before retrying: the removal may have applied.
            let fresh = self.require_owned_playlist(&id).await?;
            let now = self.window(&id, position).await?.0;
            if fresh == snapshot && now.as_deref() == Some(uri) {
                // Nothing changed, so the first attempt did not apply: retry once.
                self.write(reqwest::Method::DELETE, &path, Some(&body))
                    .await
                    .map_err(|_| "Spotify still has not confirmed the removal. Refresh the playlist before trying again.")?;
                retried = true;
            } else if fresh == snapshot || now != next {
                return Err("This playlist changed while Spotify was confirming. Refresh it before trying again.".into());
            }
        }
        let restored = self.restore_copies(&id, uri, position, copies).await?;
        let message = match (restored, retried) {
            (0, false) => "Removed this entry from your playlist.".to_owned(),
            (0, true) => "Removed this entry after a safe retry.".to_owned(),
            (n, _) => format!("Removed this entry. Spotify also removed {n} other cop{} of the song, so Grove put {} back.", if n == 1 { "y" } else { "ies" }, if n == 1 { "it" } else { "them" }),
        };
        Ok(json!({
            "kind":"playlist_changed","uri":playlist,
            "removed":{"uri":uri,"position":position},
            "restored":restored,"message":message
        }))
    }

    /// If Spotify removed every copy of the song, re-insert the copies the user kept.
    async fn restore_copies(
        &mut self,
        id: &str,
        uri: &str,
        removed: u32,
        copies: &[u32],
    ) -> ApiResult<usize> {
        let mut expected: Vec<u32> = copies
            .iter()
            .filter(|&&p| p != removed)
            .map(|&p| if p > removed { p - 1 } else { p })
            .collect();
        expected.sort_unstable();
        expected.dedup();
        expected.truncate(20);
        let Some(&first) = expected.first() else {
            return Ok(0);
        };
        if self.window(id, first).await?.0.as_deref() == Some(uri) {
            return Ok(0);
        }
        let path = format!("/playlists/{id}/items");
        for (done, position) in expected.iter().enumerate() {
            let body = json!({"uris":[uri],"position":position});
            if self
                .write(reqwest::Method::POST, &path, Some(&body))
                .await
                .is_err()
            {
                return Err(format!("Spotify removed every copy of this song and Grove could restore only {done}. Check the playlist in Spotify.").into());
            }
        }
        Ok(expected.len())
    }

    /// The entries at `position` and `position + 1`, read in one request.
    async fn window(
        &mut self,
        id: &str,
        position: u32,
    ) -> ApiResult<(Option<String>, Option<String>)> {
        let page = self
            .get(&format!(
                "/playlists/{id}/items?limit=2&offset={}",
                position.min(100_000)
            ))
            .await?;
        let uri = |entry: &Value| {
            entry
                .get("item")
                .or_else(|| entry.get("track"))
                .and_then(|t| t["uri"].as_str())
                .map(str::to_owned)
        };
        Ok((uri(&page["items"][0]), uri(&page["items"][1])))
    }

    async fn require_owned_playlist(&mut self, id: &str) -> ApiResult<String> {
        let playlist = self
            .get(&format!("/playlists/{id}?fields=owner(id),snapshot_id"))
            .await?;
        if playlist["owner"]["id"].as_str() != Some(&self.account_id) {
            return Err("Choose a playlist owned by your Spotify account.".into());
        }
        playlist["snapshot_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or("Spotify did not return the playlist revision. Refresh before editing.".into())
    }

    async fn save_uri(&mut self, uri: &str, saved: bool) -> ApiResult<()> {
        let parameters = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("uris", uri)
            .finish();
        self.write(
            if saved {
                reqwest::Method::PUT
            } else {
                reqwest::Method::DELETE
            },
            &format!("/me/library?{parameters}"),
            None,
        )
        .await
        .map(|_| ())
    }

    async fn contains(&mut self, uri: &str) -> ApiResult<bool> {
        let parameters = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("uris", uri)
            .finish();
        let result = self
            .get(&format!("/me/library/contains?{parameters}"))
            .await?;
        result[0]
            .as_bool()
            .ok_or_else(|| "Spotify did not say whether this is saved.".into())
    }

    /// Batched lookups where allowed; `/tracks?ids=` is closed to development-mode
    /// apps, so the first refusal switches to single-track requests for the session.
    async fn tracks(&mut self, uris: &[String]) -> ApiResult<Vec<Value>> {
        let mut ids = Vec::new();
        for uri in uris {
            let id = entity_id(uri, "track")?;
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        ids.truncate(50);
        if ids.len() > 1 && !self.unavailable.contains("tracks-batch") {
            match self.get(&format!("/tracks?ids={}", ids.join(","))).await {
                Ok(page) => {
                    return Ok(page["tracks"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|t| t.is_object())
                        .cloned()
                        .collect())
                }
                Err(ApiError::Unavailable(_)) | Err(ApiError::Message(_)) => {
                    self.unavailable.insert("tracks-batch");
                }
                Err(error) => return Err(error),
            }
        }
        let mut tracks = Vec::new();
        for id in ids.iter().take(20) {
            match self.get(&format!("/tracks/{id}")).await {
                Ok(track) => tracks.push(track),
                Err(error @ ApiError::Throttled { .. }) => {
                    if tracks.is_empty() {
                        return Err(error);
                    }
                    break;
                }
                Err(_) => {}
            }
        }
        Ok(tracks)
    }

    /// Up to 50 tracks from an artist's newest `releases` releases.
    async fn artist_tracks(&mut self, id: &str, releases: usize) -> ApiResult<Vec<Value>> {
        let albums = self
            .get(&format!(
                "/artists/{id}/albums?include_groups=album,single&limit={}",
                releases.clamp(1, 10)
            ))
            .await?;
        let mut tracks = Vec::new();
        for album in albums["items"]
            .as_array()
            .into_iter()
            .flatten()
            .take(releases)
        {
            tracks.extend(self.release_tracks(album, 50).await?);
        }
        Ok(dedupe(tracks, 50))
    }

    async fn release_tracks(&mut self, album: &Value, limit: usize) -> ApiResult<Vec<Value>> {
        let Some(uri) = album["uri"].as_str() else {
            return Ok(vec![]);
        };
        let album_id = entity_id(uri, "album")?;
        let page = self
            .get(&format!("/albums/{album_id}/tracks?limit=50"))
            .await?;
        Ok(page["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| t["uri"].as_str().is_some())
            .take(limit)
            .map(|t| {
                let mut t = t.clone();
                t["album"] = album.clone();
                t
            })
            .collect())
    }

    pub async fn verify(
        client_id: String,
        tokens: Tokens,
        expected: Option<&str>,
        waiting: impl Fn(u64),
    ) -> Result<Self, String> {
        let mut api =
            Self::with_transport(client_id, tokens, Transport::Http(auth::http_client()?));
        let mut attempts = 0;
        let mut waited = Duration::ZERO;
        let profile = loop {
            match api.get("/me").await {
                Ok(profile) => break profile,
                Err(ApiError::Throttled {
                    seconds,
                    quota: false,
                    ..
                }) if verification_retry(attempts, waited, Duration::from_secs(seconds)) => {
                    let delay = Duration::from_secs(seconds);
                    waiting(seconds.saturating_add(1));
                    tokio::time::sleep(delay).await;
                    api.cooldowns.clear();
                    attempts += 1;
                    waited += delay;
                }
                Err(error) => return Err(error.to_string()),
            }
        };
        let id = profile["id"]
            .as_str()
            .filter(|v| !v.is_empty())
            .ok_or("Spotify did not return an account ID.")?;
        if expected.is_some_and(|expected| expected != id) {
            return Err(
                "Saved approval belongs to another Spotify account. Sign out and try again.".into(),
            );
        }
        api.account_id = id.to_owned();
        api.name = profile["display_name"].as_str().unwrap_or(id).to_owned();
        // Development-mode profiles omit product. Defer unknown eligibility to
        // the streaming session instead of incorrectly rejecting every account.
        api.premium = profile["product"].as_str().map(|p| p == "premium");
        Ok(api)
    }

    pub fn saved(&self) -> SavedLibrary {
        SavedLibrary {
            client_id: self.client_id.clone(),
            refresh_token: self.tokens.refresh.clone(),
            account_id: self.account_id.clone(),
        }
    }

    pub async fn ensure_token(&mut self) -> Result<(), String> {
        if self.tokens.expires <= Instant::now() + Duration::from_secs(60) {
            self.tokens = auth::refresh(&self.client_id, &self.tokens.refresh).await?;
            // Persist only on the orchestrator after checking the task generation.
            // A cancelled refresh must never recreate a signed-out credential.
            self.grant_dirty = true;
        }
        Ok(())
    }

    /// Fails fast while this request's budget is cooling down.
    fn check_budget(&mut self, path: &str) -> ApiResult<&'static str> {
        let name = budget(path);
        if let Some(&(until, quota)) = self.cooldowns.get(name) {
            let now = Instant::now();
            if now < until {
                return Err(ApiError::Throttled {
                    budget: name,
                    seconds: until
                        .saturating_duration_since(now)
                        .as_secs()
                        .saturating_add(1),
                    quota,
                });
            }
            self.cooldowns.remove(name);
        }
        Ok(name)
    }

    fn throttle(&mut self, name: &'static str, reply: &Reply) -> ApiError {
        let quota = reply
            .body
            .as_ref()
            .is_some_and(|b| b["error"]["reason"].as_str() == Some("QUOTA_EXCEEDED"));
        let seconds = reply
            .retry_after
            .unwrap_or(if quota { 900 } else { 30 })
            .clamp(1, 3600);
        self.cooldowns
            .insert(name, (Instant::now() + Duration::from_secs(seconds), quota));
        ApiError::Throttled {
            budget: name,
            seconds,
            quota,
        }
    }

    async fn send(
        &mut self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Option<Reply> {
        match &self.transport {
            Transport::Http(http) => {
                let mut request = http
                    .request(method, format!("https://api.spotify.com/v1{path}"))
                    .bearer_auth(&self.tokens.access);
                if let Some(body) = body {
                    request = request.json(body);
                }
                let response = request.send().await.ok()?;
                let status = response.status().as_u16();
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse::<u64>().ok());
                let bytes = response.bytes().await.ok()?;
                Some(Reply {
                    status,
                    retry_after,
                    body: serde_json::from_slice(&bytes).ok(),
                })
            }
            #[cfg(test)]
            Transport::Fake(responder) => (responder.lock().unwrap())(method.as_str(), path, body),
        }
    }

    pub async fn get(&mut self, path: &str) -> ApiResult<Value> {
        self.ensure_token().await?;
        let name = self.check_budget(path)?;
        let reply = self
            .send(reqwest::Method::GET, path, None)
            .await
            .ok_or("Could not reach Spotify. Check your connection and retry.")?;
        match reply.status {
            429 => Err(self.throttle(name, &reply)),
            401 => Err("Spotify rejected library access. Sign in again.".into()),
            403 | 404 | 410 => Err(ApiError::Unavailable(
                "Spotify does not make this available to Grove. Sign in again for updated permissions, or open it in Spotify.".into(),
            )),
            200..=299 => reply
                .body
                .ok_or_else(|| "Spotify returned an unreadable response.".into()),
            status => Err(format!("Spotify could not load this content (HTTP {status}).").into()),
        }
    }

    /// A write. Network failures, timeouts and 5xx replies are `Unconfirmed`.
    async fn write(
        &mut self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> ApiResult<Value> {
        self.ensure_token().await?;
        let name = self.check_budget(path)?;
        let Some(reply) = self.send(method, path, body).await else {
            return Err(ApiError::Unconfirmed);
        };
        match reply.status {
            200..=299 => Ok(reply.body.unwrap_or(Value::Null)),
            401 | 403 => Err("Spotify rejected this action. Sign in again to approve library and playlist editing, or check this app's access.".into()),
            429 => Err(self.throttle(name, &reply)),
            500..=599 => Err(ApiError::Unconfirmed),
            _ => Err("Spotify refused the change. Refresh before retrying.".into()),
        }
    }

    pub async fn playlists(&mut self) -> Result<Value, String> {
        let mut page = self
            .get("/me/playlists?limit=50")
            .await
            .map_err(|e| e.to_string())?;
        page["account"] = json!(self.account_id);
        Ok(page)
    }

    pub async fn queue(&mut self) -> Result<Value, String> {
        let result = self
            .get("/me/player/queue")
            .await
            .map_err(|e| e.to_string())?;
        let queue: Vec<_> = result["queue"]
            .as_array()
            .into_iter()
            .flatten()
            .take(50)
            .map(|item| json!({"item":entity(item)}))
            .collect();
        Ok(json!({"type":"queue_changed","upcoming":queue}))
    }

    pub async fn enqueue(&mut self, uri: &str, device: &str) -> Result<(), String> {
        let parameters = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("uri", uri)
            .append_pair("device_id", device)
            .finish();
        match self
            .write(
                reqwest::Method::POST,
                &format!("/me/player/queue?{parameters}"),
                None,
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(error @ ApiError::Throttled { .. }) => Err(error.to_string()),
            Err(ApiError::Unconfirmed) => {
                Err("Could not add the track to Spotify’s queue.".to_string())
            }
            Err(_) => Err(
                "Spotify refused the queue update. Make Grove the active device and retry.".into(),
            ),
        }
    }
}

pub fn entity(item: &Value) -> Value {
    let creators: Vec<_> = item["artists"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|artist| json!({"entity":{"uri":artist["uri"],"decorations":{"identity":{"name":artist["name"]}}}}))
        .collect();
    json!({"uri":item["uri"],"artwork_url":pick_image(&item["album"]["images"]),"decorations":{"identity":{"name":item["name"]},"creators":creators,"playback":{"duration_ms":item["duration_ms"]}}})
}

/// The smallest image that still fills a 160 px card, or the largest available.
pub fn pick_image(images: &Value) -> Value {
    let list = images.as_array().map(Vec::as_slice).unwrap_or_default();
    list.iter()
        .filter(|i| i["url"].is_string())
        .filter(|i| i["width"].as_u64().is_none_or(|w| w >= 160))
        .min_by_key(|i| i["width"].as_u64().unwrap_or(u64::MAX))
        .or_else(|| list.iter().find(|i| i["url"].is_string()))
        .map(|i| i["url"].clone())
        .unwrap_or(Value::Null)
}

/// A shelf or grid card for an album, artist, playlist or track.
pub fn card(item: &Value, kind: &str) -> Option<Value> {
    let uri = item["uri"].as_str()?;
    if !uri.starts_with(&format!("spotify:{kind}:")) {
        return None;
    }
    let title = item["name"].as_str().filter(|t| !t.is_empty())?;
    let artists = item["artists"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a["name"].as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let year = item["release_date"]
        .as_str()
        .and_then(|d| d.get(..4))
        .unwrap_or_default();
    let subtitle = match kind {
        "artist" => "Artist".to_owned(),
        "playlist" => item["owner"]["display_name"]
            .as_str()
            .map(|o| format!("By {o}"))
            .unwrap_or_else(|| "Playlist".into()),
        "album" if !year.is_empty() && !artists.is_empty() => format!("{year} · {artists}"),
        _ => artists,
    };
    let image = if kind == "track" {
        pick_image(&item["album"]["images"])
    } else {
        pick_image(&item["images"])
    };
    Some(json!({"kind":kind,"uri":uri,"title":title,"subtitle":subtitle,"image":image}))
}

fn cards(items: &Value, kind: &str) -> Vec<Value> {
    items
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| card(item, kind))
        .collect()
}

/// The All view. The top result is an exact-name artist or album, else the first song.
pub fn search_all(query: &str, page: &Value) -> Value {
    let tracks: Vec<Value> = page["tracks"]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| {
            t["uri"]
                .as_str()
                .is_some_and(|u| u.starts_with("spotify:track:"))
        })
        .cloned()
        .collect();
    let albums = cards(&page["albums"]["items"], "album");
    let artists = cards(&page["artists"]["items"], "artist");
    // Playlist results can contain null entries; `card` skips them.
    let playlists = cards(&page["playlists"]["items"], "playlist");
    let needle = query.trim().to_lowercase();
    let exact = |cards: &[Value]| {
        cards
            .iter()
            .find(|c| {
                c["title"]
                    .as_str()
                    .is_some_and(|t| t.to_lowercase() == needle)
            })
            .cloned()
    };
    let top = exact(&artists)
        .or_else(|| exact(&albums))
        .or_else(|| tracks.first().and_then(|t| card(t, "track")))
        .or_else(|| artists.first().cloned())
        .or_else(|| albums.first().cloned())
        .or_else(|| playlists.first().cloned());
    json!({
        "kind":"search_all","query":query,"top":top,"tracks":tracks,
        "albums":albums,"artists":artists,"playlists":playlists,
        "has_more":!page["tracks"]["next"].is_null()
    })
}

fn saved_tracks(page: &Value) -> Vec<Value> {
    page["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.get("track").or_else(|| entry.get("item")))
        .filter(|t| t.is_object())
        .cloned()
        .collect()
}

fn recent_tracks(page: &Value) -> Vec<Value> {
    dedupe(saved_tracks(page), 50)
}

/// Keeps the first copy of each song, up to `limit`.
fn dedupe(tracks: Vec<Value>, limit: usize) -> Vec<Value> {
    let mut seen = HashSet::new();
    tracks
        .into_iter()
        .filter(|t| {
            t["uri"]
                .as_str()
                .is_some_and(|u| u.starts_with("spotify:track:") && seen.insert(u.to_owned()))
        })
        .take(limit)
        .collect()
}

fn round_robin(sources: Vec<Vec<Value>>) -> Vec<Value> {
    let mut iters: Vec<_> = sources.into_iter().map(Vec::into_iter).collect();
    let mut out = Vec::new();
    loop {
        let mut any = false;
        for it in iters.iter_mut() {
            if let Some(v) = it.next() {
                out.push(v);
                any = true;
            }
        }
        if !any {
            return out;
        }
    }
}

/// Alternates songs from different releases so one album does not fill the list.
fn interleave_by_album(tracks: Vec<Value>) -> Vec<Value> {
    let mut groups: Vec<(String, Vec<Value>)> = Vec::new();
    for t in tracks {
        let key = t["album"]["uri"].as_str().unwrap_or_default().to_owned();
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, list)) => list.push(t),
            None => groups.push((key, vec![t])),
        }
    }
    round_robin(groups.into_iter().map(|(_, v)| v).collect())
}

fn remember_throttle(slot: &mut Option<ApiError>, error: &ApiError) {
    if slot.is_none() && matches!(error, ApiError::Throttled { .. }) {
        *slot = Some(error.clone());
    }
}

fn throttled_value(budget: &str, seconds: u64, quota: bool) -> Value {
    json!({"kind":"throttled","budget":budget,"seconds":seconds,"quota":quota,"message":throttle_message(quota, seconds)})
}

pub fn throttle_message(quota: bool, seconds: u64) -> String {
    if quota {
        "Spotify's request quota for this account is used up. A short wait will not fix this: try again later, or use a dedicated Spotify app in Account → Connection options. Playback keeps working.".into()
    } else {
        format!("Spotify is busy. Try again in {seconds} seconds. Playback keeps working.")
    }
}

fn valid_query(query: &str) -> ApiResult<String> {
    let query = query.trim();
    if query.is_empty() || query.len() > 300 {
        return Err("Enter a search of 1–300 characters.".into());
    }
    Ok(query.to_owned())
}

fn verification_retry(attempts: u8, waited: Duration, delay: Duration) -> bool {
    attempts < 3
        && !delay.is_zero()
        && delay <= Duration::from_secs(30)
        && waited + delay <= Duration::from_secs(60)
}

fn entity_id_bare(id: &str) -> bool {
    id.len() == 22 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn entity_id(uri: &str, kind: &str) -> Result<String, String> {
    let id = uri
        .strip_prefix(&format!("spotify:{kind}:"))
        .filter(|id| entity_id_bare(id));
    id.map(str::to_owned)
        .ok_or_else(|| format!("Choose a valid Spotify {kind}."))
}

#[cfg(test)]
mod retry_tests {
    use super::*;
    #[test]
    fn quota_exhaustion_is_not_presented_as_a_short_cooldown() {
        assert!(throttle_message(true, 8).contains("quota"));
        assert!(!throttle_message(true, 8).contains("8 seconds"));
        assert!(throttle_message(false, 8).contains("8 seconds"));
    }
    #[test]
    fn profile_retries_keep_short_server_cooldowns_bounded() {
        assert!(verification_retry(
            0,
            Duration::ZERO,
            Duration::from_secs(13)
        ));
        assert!(verification_retry(
            2,
            Duration::from_secs(26),
            Duration::from_secs(13)
        ));
        assert!(!verification_retry(
            3,
            Duration::from_secs(39),
            Duration::from_secs(13)
        ));
        assert!(!verification_retry(0, Duration::ZERO, Duration::ZERO));
        assert!(!verification_retry(
            0,
            Duration::ZERO,
            Duration::from_secs(3600)
        ));
        assert!(!verification_retry(
            2,
            Duration::from_secs(45),
            Duration::from_secs(20)
        ));
    }
}

#[cfg(test)]
#[path = "web_api_tests.rs"]
mod tests;
