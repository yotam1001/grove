//! Offline Web API tests: a synthetic responder stands in for Spotify.
use super::*;
use crate::library::Action;
use std::sync::{Arc, Mutex};

const ME: &str = "review-account";
const PLAYLIST: &str = "spotify:playlist:pppppppppppppppppppppp";
const A: &str = "spotify:track:aaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "spotify:track:bbbbbbbbbbbbbbbbbbbbbb";
const C: &str = "spotify:track:cccccccccccccccccccccc";
const ARTIST: &str = "spotify:artist:rrrrrrrrrrrrrrrrrrrrrr";

fn ok(body: Value) -> Option<Reply> {
    Some(Reply {
        status: 200,
        retry_after: None,
        body: Some(body),
    })
}
fn status(status: u16) -> Option<Reply> {
    Some(Reply {
        status,
        retry_after: None,
        body: Some(json!({"error":{"status":status}})),
    })
}
fn track(uri: &str, name: &str) -> Value {
    json!({"uri":uri,"name":name,"duration_ms":200_000,"artists":[{"name":"Review artist","uri":ARTIST}],"album":{"uri":"spotify:album:llllllllllllllllllllll","name":"Album","images":[{"url":"https://i.scdn.co/image/a","width":300}]}})
}

/// A tiny owned playlist that behaves like Spotify's items endpoints.
struct Playlist {
    items: Vec<String>,
    snapshot: u32,
    /// Spotify might ignore `positions` and remove every copy of the URI.
    honour_positions: bool,
    /// Apply the next write but lose its reply.
    lose_reply: bool,
    /// Fail the next write before it applies.
    fail_before: bool,
    writes: usize,
}

fn playlist_api(playlist: Arc<Mutex<Playlist>>) -> WebApi {
    WebApi::fake(
        ME,
        Box::new(move |method, path, body| {
            let mut p = playlist.lock().unwrap();
            if path.starts_with("/playlists/pppppppppppppppppppppp?") {
                return ok(json!({"owner":{"id":ME},"snapshot_id":format!("s{}",p.snapshot)}));
            }
            if let Some(query) = path.strip_prefix("/playlists/pppppppppppppppppppppp/items?limit=")
            {
                let (limit, offset) = query.split_once("&offset=").unwrap();
                let (limit, offset): (usize, usize) =
                    (limit.parse().unwrap(), offset.parse().unwrap());
                let items: Vec<_> = p
                    .items
                    .iter()
                    .skip(offset)
                    .take(limit)
                    .map(|u| json!({"item":{"uri":u}}))
                    .collect();
                return ok(json!({"items":items}));
            }
            assert_eq!(
                path, "/playlists/pppppppppppppppppppppp/items",
                "{method} {path}"
            );
            let body = body.unwrap();
            if p.fail_before {
                p.fail_before = false;
                return None;
            }
            p.writes += 1;
            match method {
                "DELETE" => {
                    assert_eq!(body["snapshot_id"], format!("s{}", p.snapshot));
                    let uri = body["items"][0]["uri"].as_str().unwrap().to_owned();
                    let position = body["items"][0]["positions"][0].as_u64().unwrap() as usize;
                    if p.honour_positions {
                        assert_eq!(p.items[position], uri);
                        p.items.remove(position);
                    } else {
                        p.items.retain(|u| *u != uri);
                    }
                }
                "POST" => {
                    let uri = body["uris"][0].as_str().unwrap().to_owned();
                    match body["position"].as_u64() {
                        Some(at) => p.items.insert(at as usize, uri),
                        None => p.items.push(uri),
                    }
                }
                other => panic!("unexpected {other}"),
            }
            p.snapshot += 1;
            if p.lose_reply {
                p.lose_reply = false;
                return None;
            }
            ok(json!({"snapshot_id":format!("s{}",p.snapshot)}))
        }),
    )
}

fn playlist(items: &[&str], honour_positions: bool) -> Arc<Mutex<Playlist>> {
    Arc::new(Mutex::new(Playlist {
        items: items.iter().map(|s| s.to_string()).collect(),
        snapshot: 1,
        honour_positions,
        lose_reply: false,
        fail_before: false,
        writes: 0,
    }))
}

fn remove(position: u32, copies: Vec<u32>, uri: &str) -> Action {
    Action::PlaylistRemoveEntry {
        playlist: PLAYLIST.into(),
        uri: uri.into(),
        position,
        copies,
    }
}

#[tokio::test]
async fn removal_targets_the_exact_entry_whichever_way_spotify_applies_it() {
    for honour in [true, false] {
        let state = playlist(&[A, B, A, C, A], honour);
        let mut api = playlist_api(state.clone());
        let result = api.feature(remove(2, vec![0, 4], A)).await.unwrap();
        assert_eq!(
            state.lock().unwrap().items,
            vec![A, B, C, A],
            "positions honoured: {honour}"
        );
        assert_eq!(result["removed"]["position"], 2);
        assert_eq!(result["restored"], if honour { 0 } else { 2 });
    }
}

#[tokio::test]
async fn removal_refuses_when_the_playlist_moved_under_the_user() {
    let state = playlist(&[A, B], true);
    let mut api = playlist_api(state.clone());
    let error = api.feature(remove(1, vec![], A)).await.unwrap_err();
    assert!(error.contains("changed"));
    assert_eq!(state.lock().unwrap().writes, 0);
}

#[tokio::test]
async fn unconfirmed_writes_reread_before_one_safe_retry() {
    // Lost before applying: the re-read shows no change, so Grove retries once.
    let state = playlist(&[A, B, C], true);
    state.lock().unwrap().fail_before = true;
    let mut api = playlist_api(state.clone());
    let result = api.feature(remove(1, vec![], B)).await.unwrap();
    assert!(result["message"].as_str().unwrap().contains("retry"));
    assert_eq!(state.lock().unwrap().items, vec![A, C]);
    // Applied but the reply was lost: no second removal of a neighbour.
    let state = playlist(&[A, B, B, C], true);
    state.lock().unwrap().lose_reply = true;
    let mut api = playlist_api(state.clone());
    api.feature(remove(1, vec![2], B)).await.unwrap();
    assert_eq!(state.lock().unwrap().items, vec![A, B, C]);
    assert_eq!(state.lock().unwrap().writes, 1);
    // Adds: an applied-but-unconfirmed add is not repeated; an unapplied one is.
    let state = playlist(&[A], true);
    state.lock().unwrap().lose_reply = true;
    let mut api = playlist_api(state.clone());
    let add = Action::PlaylistAdd {
        playlist: PLAYLIST.into(),
        uri: B.into(),
    };
    api.feature(add.clone()).await.unwrap();
    assert_eq!(state.lock().unwrap().items, vec![A, B]);
    state.lock().unwrap().fail_before = true;
    let result = api.feature(add).await.unwrap();
    assert!(result["message"].as_str().unwrap().contains("retry"));
    assert_eq!(state.lock().unwrap().items, vec![A, B, B]);
}

#[tokio::test]
async fn a_429_cools_down_only_its_budget_and_quota_is_distinct() {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let log = calls.clone();
    let mut api = WebApi::fake(
        ME,
        Box::new(move |_, path, _| {
            log.lock().unwrap().push(path.to_owned());
            if path.starts_with("/search") {
                return Some(Reply {
                    status: 429,
                    retry_after: Some(12),
                    body: None,
                });
            }
            if path.starts_with("/me/tracks") {
                return Some(Reply {
                    status: 429,
                    retry_after: None,
                    body: Some(
                        json!({"error":{"status":429,"message":"Too many requests","reason":"QUOTA_EXCEEDED"}}),
                    ),
                });
            }
            ok(json!({"items":[track(A,"Forest floor")]}))
        }),
    );
    let all = Action::SearchAll {
        query: "forest".into(),
    };
    let first = api.feature(all.clone()).await.unwrap();
    assert_eq!(first["kind"], "throttled");
    assert_eq!(first["budget"], "search");
    assert_eq!(first["seconds"], 12);
    assert_eq!(first["quota"], false);
    // The cooldown is served locally without another request.
    let second = api.feature(all).await.unwrap();
    assert_eq!(second["kind"], "throttled");
    assert!(second["seconds"].as_u64().unwrap() <= 12);
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.starts_with("/search"))
            .count(),
        1
    );
    // Other budgets keep working.
    let home = api.feature(Action::Home).await.unwrap();
    assert_eq!(home["kind"], "home");
    assert_eq!(home["tracks"].as_array().unwrap().len(), 1);
    let liked = api
        .feature(Action::Collection {
            uri: "liked".into(),
            offset: 0,
        })
        .await
        .unwrap();
    assert_eq!(liked["kind"], "throttled");
    assert_eq!(liked["quota"], true);
    assert!(liked["message"].as_str().unwrap().contains("quota"));
    assert!(!liked["message"].as_str().unwrap().contains("seconds"));
}

#[tokio::test]
async fn closed_endpoints_degrade_once_per_session() {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let log = calls.clone();
    let mut api = WebApi::fake(
        ME,
        Box::new(move |_, path, _| {
            log.lock().unwrap().push(path.to_owned());
            if path.starts_with("/tracks?ids=") || path.contains("/top-tracks") {
                return status(403);
            }
            if let Some(id) = path.strip_prefix("/tracks/") {
                return ok(track(&format!("spotify:track:{id}"), "Single"));
            }
            if path.starts_with("/artists/rrrrrrrrrrrrrrrrrrrrrr/albums") {
                return ok(json!({"items":[
                    {"uri":"spotify:album:llllllllllllllllllllll","name":"Older","album_type":"album","release_date":"2019-01-01","images":[]},
                    {"uri":"spotify:album:mmmmmmmmmmmmmmmmmmmmmm","name":"Newest","album_type":"single","release_date":"2026-05-01","images":[]}
                ]}));
            }
            if path.starts_with("/albums/") {
                let id = &path[8..30];
                let digit = if id.starts_with('l') { "1" } else { "2" };
                let first = format!("spotify:track:{}", digit.repeat(22));
                return ok(
                    json!({"items":[{"uri":first,"name":format!("From {id}"),"artists":[{"name":"R","uri":ARTIST}]}]}),
                );
            }
            if path.starts_with("/me/library/contains") {
                return ok(json!([true]));
            }
            ok(
                json!({"name":"Review artist","images":[{"url":"https://i.scdn.co/image/x","width":320}]}),
            )
        }),
    );
    let tracks = Action::Tracks {
        uris: vec![A.into(), B.into(), A.into()],
    };
    let first = api.feature(tracks.clone()).await.unwrap();
    assert_eq!(
        first["tracks"].as_array().unwrap().len(),
        2,
        "duplicates collapse"
    );
    api.feature(tracks).await.unwrap();
    let log = calls.lock().unwrap().clone();
    assert_eq!(
        log.iter().filter(|p| p.starts_with("/tracks?ids=")).count(),
        1
    );
    assert_eq!(log.iter().filter(|p| p.starts_with("/tracks/")).count(), 4);
    let page = api
        .feature(Action::ArtistPage { uri: ARTIST.into() })
        .await
        .unwrap();
    assert_eq!(page["kind"], "artist_page");
    assert_eq!(page["note"], "From recent releases");
    assert_eq!(page["following"], true);
    assert_eq!(page["albums"].as_array().unwrap().len(), 1);
    assert_eq!(page["singles"].as_array().unwrap().len(), 1);
    assert!(!page["tracks"].as_array().unwrap().is_empty());
    api.feature(Action::ArtistPage { uri: ARTIST.into() })
        .await
        .unwrap();
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.contains("/top-tracks"))
            .count(),
        1,
        "a closed endpoint is not retried every page"
    );
}

#[tokio::test]
async fn home_and_restricted_playlists_degrade_without_failing() {
    let mut api = WebApi::fake(
        ME,
        Box::new(|_, path, _| {
            if path.starts_with("/me/player/recently-played") || path.starts_with("/playlists/") {
                return status(403);
            }
            if path.starts_with("/me/top/artists") {
                return ok(json!({"items":[{"uri":ARTIST,"name":"Review artist","images":[]}]}));
            }
            if path.starts_with("/me/albums") {
                return ok(
                    json!({"items":[{"album":{"uri":"spotify:album:llllllllllllllllllllll","name":"Saved","artists":[{"name":"R"}],"release_date":"2024-02-02","images":[]}}]}),
                );
            }
            ok(json!({"items":[track(A,"Forest floor"),track(B,"Evening light")]}))
        }),
    );
    let home = api.feature(Action::Home).await.unwrap();
    let shelves = home["shelves"].as_array().unwrap();
    assert_eq!(shelves.len(), 2);
    assert_eq!(shelves[0]["id"], "artists");
    assert_eq!(shelves[1]["cards"][0]["subtitle"], "2024 · R");
    let page = api
        .feature(Action::Collection {
            uri: PLAYLIST.into(),
            offset: 0,
        })
        .await
        .unwrap();
    assert_eq!(page["restricted"], true);
    let mix = api.feature(Action::Mix).await.unwrap();
    assert_eq!(mix["kind"], "mix");
    assert!(!mix["tracks"].as_array().unwrap().is_empty());
}

#[test]
fn all_view_picks_an_exact_artist_or_the_first_song() {
    let page = json!({
        "tracks":{"items":[track(A,"Nova"),track(B,"Other")],"next":"x"},
        "artists":{"items":[{"uri":ARTIST,"name":"Nova Wells","images":[]}]},
        "albums":{"items":[]},
        "playlists":{"items":[null,{"uri":"spotify:playlist:pppppppppppppppppppppp","name":"Mix","owner":{"display_name":"Ana"},"images":[]}]}
    });
    let all = search_all("nova wells", &page);
    assert_eq!(all["top"]["kind"], "artist");
    assert_eq!(all["playlists"].as_array().unwrap().len(), 1);
    assert_eq!(all["playlists"][0]["subtitle"], "By Ana");
    let all = search_all("nova", &page);
    assert_eq!(all["top"]["kind"], "track");
    assert_eq!(all["top"]["uri"], A);
    assert_eq!(all["has_more"], true);
}

#[test]
fn budgets_group_endpoints_and_images_prefer_card_size() {
    assert_eq!(budget("/search?q=a"), "search");
    assert_eq!(budget("/me/player/queue"), "player");
    assert_eq!(budget("/me/player/recently-played?limit=5"), "recent");
    assert_eq!(budget("/me/albums"), "saved");
    assert_eq!(budget("/playlists/x/items"), "playlists");
    assert_eq!(budget("/me"), "profile");
    let images =
        json!([{"url":"big","width":640},{"url":"mid","width":300},{"url":"small","width":64}]);
    assert_eq!(pick_image(&images), "mid");
    assert_eq!(pick_image(&json!([{"url":"only"}])), "only");
    assert_eq!(pick_image(&Value::Null), Value::Null);
}

/// A responder that logs each path and answers `/me` and `/me/playlists` with
/// the given statuses (429 replies carry `Retry-After` or `QUOTA_EXCEEDED`).
fn startup_api(me: u16, playlists: u16, quota: bool) -> (WebApi, Arc<Mutex<Vec<String>>>) {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let log = calls.clone();
    let reply = move |code: u16, body: Value| match code {
        429 if quota => Some(Reply {
            status: 429,
            retry_after: None,
            body: Some(json!({"error":{"status":429,"reason":"QUOTA_EXCEEDED"}})),
        }),
        429 => Some(Reply {
            status: 429,
            retry_after: Some(40),
            body: None,
        }),
        200 => ok(body),
        code => status(code),
    };
    let api = WebApi::fake(
        "",
        Box::new(move |_, path, _| {
            log.lock().unwrap().push(path.to_owned());
            if path == "/me" {
                return reply(
                    me,
                    json!({"id":ME,"display_name":"Live name","product":"premium"}),
                );
            }
            if path.starts_with("/me/playlists") {
                return reply(
                    playlists,
                    json!({"items":[{"name":"Live list","uri":PLAYLIST}],"next":null}),
                );
            }
            ok(json!({"items":[]}))
        }),
    );
    (api, calls)
}

fn cached_profile() -> Value {
    json!({"id":ME,"display_name":"Cached name","product":"premium"})
}

fn cached_page() -> Value {
    json!({"account":ME,"items":[{"name":"Cached list","uri":PLAYLIST}],"next":null})
}

#[tokio::test]
async fn a_fresh_startup_cache_shows_the_library_without_any_request() {
    let (api, calls) = startup_api(500, 500, false);
    let mut api = api
        .identify(Some(ME), Some(Hit::Fresh(cached_profile())), |_| {
            panic!("nothing should wait")
        })
        .await
        .unwrap();
    assert_eq!(
        (api.account_id.as_str(), api.name.as_str()),
        (ME, "Cached name")
    );
    assert_eq!(api.premium, Some(true));
    assert!(
        api.verified_profile.is_none(),
        "a cached profile is not re-cached as new"
    );
    let page = api.library(Some(Hit::Fresh(cached_page())), false).await;
    assert!(!page.refresh && !page.store);
    assert_eq!(page.page["cached"], true);
    assert_eq!(page.page["account"], ME);
    assert_eq!(page.page["items"][0]["name"], "Cached list");
    assert!(
        calls.lock().unwrap().is_empty(),
        "{:?}",
        calls.lock().unwrap()
    );
}

#[tokio::test]
async fn a_startup_429_uses_the_last_launch_instead_of_waiting() {
    let (api, calls) = startup_api(429, 429, false);
    let mut api = api
        .identify(Some(ME), Some(Hit::Stale(cached_profile())), |_| {
            panic!("a cached profile must not wait out the cooldown")
        })
        .await
        .unwrap();
    assert_eq!(api.name, "Cached name");
    assert!(api.verified_profile.is_none());
    // A stale page is shown at once and refreshed afterwards.
    let shown = api.library(Some(Hit::Stale(cached_page())), false).await;
    assert!(shown.refresh && !shown.store);
    assert_eq!(shown.page["stale"], true);
    // The refresh hits the playlists budget's 429: the cached page stays, with the
    // cooldown and Retry-After the UI counts down.
    let refreshed = api.library(Some(Hit::Stale(cached_page())), true).await;
    assert!(!refreshed.store && !refreshed.refresh);
    assert_eq!(refreshed.page["items"][0]["name"], "Cached list");
    assert_eq!(refreshed.page["cooldown"]["kind"], "throttled");
    assert_eq!(refreshed.page["cooldown"]["budget"], "playlists");
    assert_eq!(refreshed.page["cooldown"]["seconds"], 40);
    assert_eq!(refreshed.page["cooldown"]["quota"], false);
    // A second Refresh during the cooldown is answered locally.
    let again = api.library(None, true).await;
    assert_eq!(again.page["load_failed"], true);
    assert_eq!(again.page["cooldown"]["budget"], "playlists");
    // Both budgets are cooling down independently; others still work.
    assert!(matches!(
        api.get("/me").await,
        Err(ApiError::Throttled {
            budget: "profile",
            ..
        })
    ));
    assert!(api.get("/me/top/tracks?limit=5").await.is_ok());
    let calls = calls.lock().unwrap();
    assert_eq!(calls.iter().filter(|p| p.as_str() == "/me").count(), 1);
    assert_eq!(
        calls
            .iter()
            .filter(|p| p.starts_with("/me/playlists"))
            .count(),
        1
    );
}

#[tokio::test]
async fn startup_quota_exhaustion_is_reported_as_quota() {
    let (api, _) = startup_api(200, 429, true);
    let mut api = api.identify(Some(ME), None, |_| {}).await.unwrap();
    assert_eq!(api.name, "Live name");
    assert_eq!(api.verified_profile.as_ref().unwrap()["id"], ME);
    let page = api.library(None, false).await;
    assert_eq!(page.page["load_failed"], true);
    assert_eq!(page.page["cooldown"]["quota"], true);
    assert!(page.page["cooldown"]["message"]
        .as_str()
        .unwrap()
        .contains("quota"));
}

#[tokio::test]
async fn startup_cache_never_vouches_for_another_account() {
    let (api, calls) = startup_api(200, 200, false);
    let other = json!({"id":"someone-else","display_name":"Other"});
    let mut api = api
        .identify(Some(ME), Some(Hit::Fresh(other)), |_| {})
        .await
        .unwrap();
    assert_eq!(
        api.name, "Live name",
        "another account's profile was trusted"
    );
    // Without a cache a 429 quota answer on /me fails instead of guessing.
    let (quota, _) = startup_api(429, 200, true);
    assert!(quota.identify(Some(ME), None, |_| {}).await.is_err());
    // A live page is cached for the next launch.
    let page = api.library(None, true).await;
    assert!(page.store);
    assert_eq!(page.page["items"][0]["name"], "Live list");
    assert_eq!(page.page["account"], ME);
    assert_eq!(calls.lock().unwrap().len(), 2);
}
