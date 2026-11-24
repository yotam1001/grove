//! Offline browse review: Home shelves and the page cache, Search All with stale
//! replies, cooldown and Retry, Library tabs and filtering, exact playlist
//! removal, album and artist pages, credited-artist choice, follow, batched and
//! collapsed lookups, autoplay refill and the Discovery mix. Synthetic JSON only:
//! no credentials, network or preference writes.
use super::{browse::Want, *};
use crate::GroveBrowse;
use std::path::PathBuf;

const ME: &str = "review-account";
const LIST: &str = "spotify:playlist:pppppppppppppppppppppp";
const ALBUM: &str = "spotify:album:llllllllllllllllllllll";
const ARTIST: &str = "spotify:artist:aaaaaaaaaaaaaaaaaaaaaa";
const FEATURED: &str = "spotify:artist:ffffffffffffffffffffff";

fn uri(c: char) -> String {
    format!("spotify:track:{}", c.to_string().repeat(22))
}
fn song(c: char, title: &str) -> Value {
    json!({"uri":uri(c),"name":title,"duration_ms":201_000,
        "artists":[{"name":"Review artist","uri":ARTIST}],
        "album":{"uri":ALBUM,"name":"Synthetic album","images":[]}})
}
fn duet(c: char, title: &str) -> Value {
    let mut t = song(c, title);
    t["artists"] =
        json!([{"name":"Review artist","uri":ARTIST},{"name":"Featured guest","uri":FEATURED}]);
    t
}
fn card(kind: &str, c: char, title: &str, subtitle: &str) -> Value {
    json!({"kind":kind,"uri":format!("spotify:{kind}:{}",c.to_string().repeat(22)),"title":title,"subtitle":subtitle,"image":null})
}
fn capture(ui: &GroveWindow, out: &std::path::Path, name: &str) {
    let p = ui.window().take_snapshot().expect("Native browse snapshot");
    image::RgbaImage::from_raw(p.width(), p.height(), p.as_bytes().to_vec())
        .unwrap()
        .save(out.join(format!("browse-{name}.png")))
        .unwrap();
}
fn last(f: &Rc<RefCell<Features>>) -> u64 {
    f.borrow().serial
}
fn respond(ui: &GroveWindow, f: &Rc<RefCell<Features>>, id: u64, data: Value) {
    f.borrow_mut().response(ui, id, Ok(data));
}
fn size(ui: &GroveWindow, w: f32, h: f32) {
    ui.window().set_size(slint::LogicalSize::new(w, h));
}

pub fn start(ui: &GroveWindow, features: Rc<RefCell<Features>>, out: PathBuf) {
    let timer = Rc::new(slint::Timer::default());
    let keep = timer.clone();
    let weak = ui.as_weak();
    let mut step = 0;
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(300), move || {
        let _keep = &keep;
        let Some(ui) = weak.upgrade() else { return };
        let g = ui.global::<GroveBrowse>();
        let f = &features;
        match step {
            0 => {
                // Earlier stages hide songs; start from a clean list.
                f.borrow_mut().prefs.hidden.clear();
                size(&ui, 1200.0, 800.0);
                ui.set_library_ready(true);
                ui.set_authenticated(true);
                ui.set_account_name("Review listener".into());
                ui.set_owned_playlists(ModelRc::new(VecModel::from(vec![crate::GrovePlaylist {
                    title: "Evening rotation".into(),
                    subtitle: "Your playlist".into(),
                    uri: LIST.into(),
                }])));
                f.borrow_mut().library_ready(&ui, &json!({"account":ME,"items":[
                    {"name":"Evening rotation","uri":LIST,"owner":{"id":ME,"display_name":"Review"},"items":{"total":4},"images":[]},
                    {"name":"Shared finds","uri":"spotify:playlist:qqqqqqqqqqqqqqqqqqqqqq","owner":{"id":"friend","display_name":"Ana"},"images":[]}
                ]}));
                ui.set_page(5);
                f.borrow_mut().view_page = -1;
                f.borrow_mut().show_home(&ui, false);
                assert!(matches!(f.borrow().browse.sent.last(), Some(Action::Home)));
                let id = last(f);
                let tracks: Vec<Value> = "abcdefg".chars().map(|c| song(c, &format!("Rotation {c}"))).collect();
                respond(&ui, f, id, json!({"kind":"home","note":"Recently in your rotation","tracks":tracks,"shelves":[
                    {"id":"recent","title":"Recently played","cards":[card("album",'l',"Synthetic album","2026 · Review artist"),card("album",'m',"Morning set","2024 · Nova")]},
                    {"id":"artists","title":"Your top artists","cards":[card("artist",'a',"Review artist","Artist"),card("artist",'f',"Featured guest","Artist")]},
                    {"id":"albums","title":"Saved albums","cards":[card("album",'n',"Night drive","2023 · Review artist")]}
                ]}));
                assert_eq!(g.get_hero().title, "Rotation a");
                assert_eq!(g.get_hero_label(), "ON REPEAT");
                assert_eq!(g.get_picks().row_count(), 6);
                let titles: Vec<_> = g.get_home_shelves().iter().map(|s| s.title.to_string()).collect();
                assert_eq!(titles, ["Made for you", "Recently played", "Your top artists", "Your playlists", "Saved albums"]);
                // A fresh cached Home is shown again without another request.
                let mark = last(f);
                f.borrow_mut().show_home(&ui, false);
                assert_eq!(last(f), mark, "fresh cached Home was requested again");
                assert_eq!(g.get_picks().row_count(), 6);
            }
            1 => {
                capture(&ui, &out, "home");
                size(&ui, 1200.0, 1500.0);
            }
            2 => {
                capture(&ui, &out, "home-shelves");
                size(&ui, 700.0, 540.0);
            }
            3 => {
                capture(&ui, &out, "home-narrow");
                size(&ui, 1200.0, 800.0);
                ui.set_page(6);
            }
            4 => {
                ui.set_search_text("review artist".into());
                ui.invoke_search("forest flor".into());
                let stale = last(f);
                ui.invoke_search("review artist".into());
                let id = last(f);
                respond(&ui, f, stale, json!({"kind":"search_all","query":"forest flor","top":card("album",'z',"Stale","x"),"tracks":[song('z',"Stale result")]}));
                assert_eq!(g.get_search_songs().row_count(), 0, "a stale search replaced the newer one");
                let page = json!({
                    "tracks":{"items":[song('a',"Forest floor"),duet('b',"Evening light"),song('c',"Afterglow"),song('d',"Moss"),song('e',"Fern")],"next":"more"},
                    "artists":{"items":[{"uri":ARTIST,"name":"Review artist","images":[]}]},
                    "albums":{"items":[{"uri":ALBUM,"name":"Synthetic album","artists":[{"name":"Review artist"}],"release_date":"2026-01-01","images":[]}]},
                    "playlists":{"items":[null,{"uri":LIST,"name":"Evening rotation","owner":{"display_name":"Review"},"images":[]}]}
                });
                respond(&ui, f, id, crate::web_api::search_all("review artist", &page));
                assert_eq!(g.get_search_top().kind, "artist", "exact artist name is the top result");
                assert_eq!(g.get_search_songs().row_count(), 4);
                assert_eq!(g.get_search_shelves().row_count(), 3);
                assert!(ui.get_feature_more());
                assert_eq!(f.borrow().browse.cache.recent_searches[0], "review artist");
                assert_eq!(g.get_recent_searches().row_count(), 2);
            }
            5 => {
                capture(&ui, &out, "search-all");
                size(&ui, 700.0, 540.0);
            }
            6 => {
                capture(&ui, &out, "search-all-narrow");
                size(&ui, 1200.0, 800.0);
                g.invoke_search_tab_changed(2);
                assert_eq!(g.get_search_cards().row_count(), 1);
                g.invoke_search_tab_changed(1);
                assert_eq!(g.get_rows().row_count(), 5);
                // A 429 shows a cooldown with the time left; Retry waits for Retry-After.
                ui.invoke_search("busy".into());
                let id = last(f);
                respond(&ui, f, id, json!({"kind":"throttled","budget":"search","seconds":12,"quota":false,"message":crate::web_api::throttle_message(false, 12)}));
                assert!(g.get_cooldown_visible());
                assert_eq!(g.get_cooldown_seconds(), 12);
                assert!(!g.get_cooldown_quota());
                assert!(!ui.get_feature_busy());
            }
            7 => {
                capture(&ui, &out, "cooldown");
                // Once Retry-After passes, Retry repeats the request.
                f.borrow_mut().browse.cooldown.as_mut().unwrap().until = Instant::now();
                f.borrow_mut().browse_tick(&ui);
                assert_eq!(g.get_cooldown_seconds(), 0);
                let mark = last(f);
                g.invoke_retry();
                assert!(last(f) > mark);
                assert!(matches!(f.borrow().browse.sent.last(), Some(Action::SearchAll { query }) if query == "busy"));
                let id = last(f);
                respond(&ui, f, id, json!({"kind":"throttled","budget":"search","seconds":900,"quota":true,"message":crate::web_api::throttle_message(true, 900)}));
                assert!(g.get_cooldown_quota());
                assert!(g.get_cooldown_text().contains("quota"));
                ui.set_page(3);
            }
            8 => {
                assert!(!g.get_cooldown_visible() || f.borrow().browse.cooldown.is_some());
                f.borrow_mut().clear_cooldown(&ui);
                // The startup playlists page (issue #3): a 429 keeps the last launch's
                // list with the cooldown banner and Retry; a stale list says so.
                g.set_library_tab(0);
                let busy = json!({"account":ME,"items":[{"name":"Saved list"}],"cached":true,"stale":true,
                    "cooldown":{"kind":"throttled","budget":"playlists","seconds":40,"quota":false,"message":crate::web_api::throttle_message(false, 40)}});
                assert!(f.borrow_mut().playlists_note(&ui, &busy).contains("from last time"));
                assert!(g.get_cooldown_visible());
                assert_eq!(g.get_cooldown_seconds(), 40);
                assert!(matches!(f.borrow().browse.cooldown.as_ref().and_then(|c| c.retry.as_ref()), Some(browse::Retry::Tab(0))));
                let empty = json!({"account":ME,"items":[],"load_failed":true,
                    "cooldown":{"kind":"throttled","budget":"playlists","seconds":900,"quota":true,"message":crate::web_api::throttle_message(true, 900)}});
                assert!(f.borrow_mut().playlists_note(&ui, &empty).contains("load when the cooldown ends"));
                assert!(g.get_cooldown_quota());
                let stale = json!({"account":ME,"items":[{"name":"Saved list"}],"cached":true,"stale":true});
                assert!(f.borrow_mut().playlists_note(&ui, &stale).contains("while Grove refreshes"));
                assert!(g.get_cooldown_visible(), "a cached page must not hide a live cooldown");
                let live = json!({"account":ME,"items":[{"name":"Saved list"}],"next":null});
                assert_eq!(f.borrow_mut().playlists_note(&ui, &live), "Your saved playlists");
                assert!(!g.get_cooldown_visible());
                // Playlists tab: Liked Songs and the mix are pinned above playlists.
                assert_eq!(g.get_library_items().row_count(), 4);
                g.set_library_filter("shared".into());
                g.invoke_library_filter_changed("shared".into());
                assert_eq!(g.get_library_items().row_count(), 1);
                g.set_library_filter("".into());
                g.invoke_library_filter_changed("".into());
                g.invoke_library_tab_changed(1);
                assert!(matches!(f.borrow().browse.sent.last(), Some(Action::SavedAlbums { offset: 0 })));
                let id = last(f);
                respond(&ui, f, id, json!({"kind":"library_albums","offset":0,"has_more":true,"cards":[card("album",'l',"Synthetic album","2026 · Review artist"),card("album",'n',"Night drive","2023 · Review artist")]}));
                assert_eq!(g.get_library_items().row_count(), 2);
                assert!(g.get_library_items_more());
                g.invoke_library_next();
                assert!(matches!(f.borrow().browse.sent.last(), Some(Action::SavedAlbums { offset: 2 })));
                let id = last(f);
                respond(&ui, f, id, json!({"kind":"library_albums","offset":2,"has_more":false,"cards":[card("album",'m',"Morning set","2024 · Nova")]}));
                assert_eq!(g.get_library_items().row_count(), 3);
            }
            9 => {
                capture(&ui, &out, "library-albums");
                g.invoke_library_tab_changed(2);
                let id = last(f);
                f.borrow_mut().response(&ui, id, Err("Spotify does not make this available to Grove.".into()));
                assert!(g.get_library_note().contains("Sign in again"));
                // History: this session first, then Spotify's recently played.
                f.borrow_mut().history = vec![Track::from_entity(&crate::web_api::entity(&song('h', "Heard this session")))];
                g.invoke_library_tab_changed(3);
                assert_eq!(g.get_rows().row_count(), 1);
                let id = last(f);
                respond(&ui, f, id, json!({"kind":"recently_played","tracks":[song('h',"Heard this session"),song('i',"Earlier today")]}));
                assert_eq!(g.get_rows().row_count(), 2, "history repeats were not removed");
                // A fresh cached tab is not requested again.
                let mark = last(f);
                g.invoke_library_tab_changed(1);
                assert_eq!(last(f), mark);
                g.invoke_library_tab_changed(3);
            }
            10 => {
                capture(&ui, &out, "library-history");
                g.invoke_open("playlist".into(), LIST.into(), "Evening rotation".into());
                assert_eq!(ui.get_page(), 10);
                assert!(g.get_header().owned);
                let id = last(f);
                respond(&ui, f, id, json!({"kind":"collection_page","uri":LIST,"offset":0,"total":5,"has_more":false,"items":[
                    {"item":song('a',"Forest floor")},{"item":song('b',"Evening light")},{"item":{"uri":"spotify:episode:eeeeeeeeeeeeeeeeeeeeee"}},
                    {"item":song('a',"Forest floor")},{"item":duet('c',"Afterglow")}
                ]}));
                assert_eq!(g.get_rows().row_count(), 4);
                assert_eq!(f.borrow().browse.positions, vec![0, 1, 3, 4]);
                assert!(g.get_header().subtitle.contains("5 songs"));
            }
            11 => {
                capture(&ui, &out, "playlist");
                size(&ui, 700.0, 540.0);
            }
            12 => {
                capture(&ui, &out, "playlist-narrow");
                size(&ui, 1200.0, 800.0);
                // Remove the second copy of "Forest floor": its exact position, not every copy.
                g.invoke_row_action("Remove".into(), 2);
                assert!(ui.get_removing_from_playlist());
                ui.invoke_playlist_edit(true);
                assert!(
                    matches!(f.borrow().browse.sent.last(), Some(Action::PlaylistRemoveEntry { position: 3, copies, .. }) if copies == &vec![0]),
                    "removal did not target the exact entry"
                );
                let id = last(f);
                respond(&ui, f, id, json!({"kind":"playlist_changed","uri":LIST,"removed":{"uri":uri('a'),"position":3},"restored":0,"message":"Removed this entry from your playlist."}));
                assert_eq!(g.get_rows().row_count(), 3);
                assert_eq!(f.borrow().browse.positions, vec![0, 1, 3]);
                assert_eq!(f.borrow().rows[0].uri, uri('a'), "the other copy was removed");
                // Several credited artists: choose one from the row's menu.
                g.invoke_row_action("Artists".into(), 2);
                assert_eq!(g.get_choice_row(), 2);
                assert_eq!(g.get_artist_choices().row_count(), 2);
                g.invoke_open("album".into(), ALBUM.into(), "Synthetic album".into());
                let id = last(f);
                respond(&ui, f, id, json!({"kind":"album","uri":ALBUM,"offset":0,"title":"Synthetic album","album_type":"album","artists":[{"name":"Review artist"}],"release_date":"2026-03-01","total":3,"image":null,"has_more":false,
                    "tracks":[song('a',"Forest floor"),song('j',"Canopy"),song('k',"Clearing")]}));
                assert_eq!(g.get_header().label, "ALBUM");
                assert!(g.get_header().subtitle.contains("2026"));
                assert_eq!(g.get_rows().row_count(), 3);
            }
            13 => {
                capture(&ui, &out, "album");
                g.invoke_open("artist".into(), ARTIST.into(), "Review artist".into());
                assert_eq!(ui.get_page(), 11);
                let id = last(f);
                respond(&ui, f, id, json!({"kind":"artist_page","uri":ARTIST,"title":"Review artist","image":null,"genres":["ambient","folk"],
                    "tracks":[song('a',"Forest floor"),song('j',"Canopy")],"note":"From recent releases","following":false,
                    "albums":[card("album",'l',"Synthetic album","2026 · Review artist")],"singles":[card("album",'s',"Single file","2025 · Review artist")]}));
                assert!(g.get_header().can_follow && !g.get_header().following);
                assert_eq!(g.get_artist_shelves().row_count(), 2);
                g.invoke_follow();
                assert!(matches!(f.borrow().browse.sent.last(), Some(Action::Follow { follow: true, .. })));
                let id = last(f);
                respond(&ui, f, id, json!({"kind":"followed","uri":ARTIST,"following":true}));
                assert!(g.get_header().following);
            }
            14 => {
                capture(&ui, &out, "artist");
                size(&ui, 700.0, 540.0);
            }
            15 => {
                capture(&ui, &out, "artist-narrow");
                size(&ui, 1200.0, 800.0);
                // Back returns to the album, then the playlist, with their rows.
                ui.invoke_feature_back();
                assert_eq!(ui.get_page(), 10);
                assert_eq!(g.get_header().label, "ALBUM");
                ui.invoke_feature_back();
                assert_eq!(g.get_header().title, "Evening rotation");
                assert_eq!(g.get_rows().row_count(), 3);
                // Lookups in one poll share a request; identical reads collapse.
                let mut fm = f.borrow_mut();
                fm.browse.sent.clear();
                let a = fm.request(Action::Contains { uri: uri('a') }, Pending::Saved(uri('a'))).unwrap();
                let b = fm.request(Action::Contains { uri: uri('a') }, Pending::Saved(uri('a'))).unwrap();
                assert_eq!(a, b, "identical reads were not collapsed");
                fm.pending.remove(&a);
                ui.set_current_uri(uri('c').into());
                fm.request(Action::TrackInfo { uri: uri('c') }, Pending::Metadata(uri('c')));
                fm.request(Action::TrackInfo { uri: uri('d') }, Pending::Enqueue);
                let queued = fm.queue.len();
                fm.flush_batch();
                assert!(matches!(fm.browse.sent.last(), Some(Action::Tracks { uris }) if uris.len() == 2));
                let id = fm.serial;
                assert!(matches!(fm.pending.get(&id), Some(Pending::Browse(Want::Batch(w))) if w.len() == 2));
                fm.response(&ui, id, Ok(json!({"kind":"tracks","tracks":[duet('c',"Afterglow"),song('d',"Moss")]})));
                assert_eq!(fm.queue.len(), queued + 1, "batched lookup did not reach the queue");
                assert_eq!(fm.metadata.title, "Afterglow");
                assert!(
                    !fm.pending.values().any(|p| matches!(p, Pending::Metadata(_) | Pending::Enqueue | Pending::Browse(Want::Batch(_)))),
                    "batched lookups were left pending"
                );
                fm.queue.clear();
                fm.render_queue(&ui);
            }
            16 => {
                // Autoplay continues from the artists the listener chose, skipping heard songs.
                let mut fm = f.borrow_mut();
                fm.prefs.autoplay = true;
                fm.remember_seeds(Some(0));
                assert_eq!(fm.browse.seeds.first().map(String::as_str), Some(ARTIST));
                let rotation = fm.rows.clone();
                for t in &rotation {
                    fm.browse.heard.insert(t.uri.clone());
                }
                fm.rotation = rotation;
                // The poll clears the synthetic current song; set it for this check.
                ui.set_current_uri(uri('c').into());
                fm.browse_tick(&ui);
                assert!(matches!(fm.browse.sent.last(), Some(Action::Continue { artists }) if artists[0] == ARTIST));
                let id = fm.serial;
                fm.browse_tick(&ui);
                assert_eq!(fm.serial, id, "a second refill started while one was pending");
                fm.response(&ui, id, Ok(json!({"kind":"continue","tracks":[song('a',"Already heard"),song('x',"New find"),song('y',"Another find")]})));
                assert_eq!(fm.rotation[0].uri, uri('x'));
                assert_eq!(fm.unheard(), 2);
                assert!(!fm.rotation.iter().take(2).any(|t| t.uri == uri('a')));
                fm.prefs.autoplay = false;
                drop(fm);
                g.invoke_open("mix".into(), "mix".into(), "Discovery mix".into());
                let id = last(f);
                assert!(matches!(f.borrow().browse.sent.last(), Some(Action::Mix)));
                respond(&ui, f, id, json!({"kind":"mix","uri":"mix","title":"Discovery mix","note":"Built from your top songs, your artists' newest releases and liked songs.","tracks":[song('m',"Mix one"),duet('n',"Mix two"),song('o',"Mix three")]}));
                assert_eq!(g.get_header().kind, "mix");
                assert_eq!(g.get_rows().row_count(), 3);
            }
            17 => {
                capture(&ui, &out, "mix");
                ui.set_light_mode(true);
            }
            18 => {
                capture(&ui, &out, "mix-light");
                ui.set_light_mode(false);
                features.borrow_mut().clear(&ui);
                assert!(features.borrow().browse.cache.recent_searches.is_empty());
                assert_eq!(g.get_recent_searches().row_count(), 0);
                assert_eq!(g.get_rows().row_count(), 0);
                eprintln!("PASS: browse pages, page cache, search, cooldown, library tabs, exact removal, artist pages, batching and autoplay refill");
                slint::quit_event_loop().unwrap();
            }
            _ => {}
        }
        eprintln!("Browse review stage {step} passed");
        step += 1;
    });
}
