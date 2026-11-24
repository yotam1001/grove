//! Offline native feature review. No credentials, network, or preference writes.
use super::*;
use slint::Model;
use std::path::PathBuf;
fn track(id: char, title: &str) -> Value {
    json!({"uri":format!("spotify:track:{}",id.to_string().repeat(22)),"name":title,"artists":[{"name":"Review artist","uri":"spotify:artist:aaaaaaaaaaaaaaaaaaaaaa"}],"duration_ms":210_000})
}
fn capture(ui: &GroveWindow, out: &std::path::Path, name: &str) {
    let p = ui
        .window()
        .take_snapshot()
        .expect("Native feature snapshot");
    image::RgbaImage::from_raw(p.width(), p.height(), p.as_bytes().to_vec())
        .unwrap()
        .save(out.join(format!("{name}.png")))
        .unwrap();
}
pub fn start(
    ui: &GroveWindow,
    features: Rc<RefCell<Features>>,
    out: PathBuf,
) -> Result<slint::Timer, std::io::Error> {
    std::fs::create_dir_all(&out)?;
    let timer = slint::Timer::default();
    let weak = ui.as_weak();
    let mut step = 0;
    let mut browse_started = false;
    let mut old_id = 0;
    timer.start(slint::TimerMode::Repeated,Duration::from_millis(300),move||{
        let Some(ui)=weak.upgrade()else{return;};
        match step {
            0=>{ui.set_page(6);},
            1=>{ui.invoke_search("test".into());assert!(ui.get_feature_message().contains("Connect Spotify"));ui.set_library_ready(true);ui.invoke_search("first".into());old_id=features.borrow().serial;},
            2=>{
                ui.invoke_search("second".into());let id=features.borrow().serial;
                features.borrow_mut().response(&ui,old_id,Ok(json!({"kind":"search","tracks":[track('c',"Stale result")]})));
                assert_eq!(ui.get_feature_tracks().row_count(),0,"Stale search replaced newer query");
                features.borrow_mut().response(&ui,id,Ok(json!({"kind":"search","tracks":[track('a',"Forest floor"),track('b',"Evening light")],"has_more":true})));
                assert_eq!(ui.get_feature_tracks().row_count(),2);assert!(!ui.get_feature_busy());
            },
            3=>{
                capture(&ui,&out,"search");ui.invoke_feature_next();let id=features.borrow().serial;
                features.borrow_mut().response(&ui,id,Ok(json!({"kind":"search","tracks":[track('d',"Afterglow")],"has_more":false})));
                assert_eq!(ui.get_feature_tracks().row_count(),3);assert!(!ui.get_feature_more());
                let uri=ui.get_feature_tracks().row_data(0).unwrap().uri;
                ui.invoke_song_action("Next".into(),uri.clone());ui.invoke_song_action("Queue".into(),ui.get_feature_tracks().row_data(1).unwrap().uri);
                assert_eq!(ui.get_local_queue().row_count(),2);
                ui.invoke_queue_edit("down".into(),0);assert_eq!(ui.get_local_queue().row_data(1).unwrap().uri,uri);
                ui.invoke_queue_edit("remove".into(),0);assert_eq!(ui.get_local_queue().row_count(),1);
                ui.invoke_song_action("Hide".into(),uri);assert_eq!(ui.get_feature_tracks().row_count(),2);
                ui.invoke_appearance("reset-hidden".into());assert!(features.borrow().prefs.hidden.is_empty());
                ui.set_link("spotify:track:cccccccccccccccccccccc".into());ui.invoke_enqueue_link();let id=features.borrow().serial;
                features.borrow_mut().response(&ui,id,Ok(json!({"kind":"track_info","track":track('c',"A linked song")})));assert_eq!(ui.get_local_queue().row_count(),2);ui.invoke_queue_edit("remove".into(),1);
                let(sender,mut receiver)=tokio::sync::mpsc::unbounded_channel();ui.set_jam_connected(true);ui.set_jam_invite("review-room-a".into());
                features.borrow_mut().resolve_jam(&ui,"spotify:track:cccccccccccccccccccccc".into(),sender.clone());let id=features.borrow().serial;ui.set_jam_invite("review-room-b".into());features.borrow_mut().response(&ui,id,Ok(json!({"track":track('c',"Old room song")})));assert!(receiver.try_recv().is_err());
                features.borrow_mut().resolve_jam(&ui,"spotify:track:cccccccccccccccccccccc".into(),sender);let id=features.borrow().serial;features.borrow_mut().response(&ui,id,Ok(json!({"track":track('c',"A named shared song")})));assert!(matches!(receiver.try_recv(),Ok(crate::jam_transport::Request::Add{title,..})if title=="A named shared song"));ui.set_jam_connected(false);ui.set_jam_invite("".into());
                ui.set_page(1);
            },
            4=>{
                capture(&ui,&out,"editable-queue");
                let mut state=state::State::default();state.authenticated=true;state.active=true;state.play_request_id=Some(0);
                let mut f=features.borrow_mut();assert!(f.next(&ui,&state));assert_eq!(f.queue.len(),1,"Unconfirmed request consumed queue");
                f.tick(&ui,&state);assert_eq!(f.queue.len(),1);
                state.track=f.queue[0].clone();state.play_request_id=Some(1);f.tick(&ui,&state);assert!(f.queue.is_empty());
                f.queue.push(Track::from_entity(&crate::web_api::entity(&track('b',"Queue survives failure"))));
                f.ended(&ui,&state,Some(0));assert!(f.starting.is_none(),"Stale EOF advanced the queue");
                ui.set_local_active(true);f.ended(&ui,&state,Some(1));assert!(f.starting.is_none(),"Spotify EOF advanced the queue during local audio");ui.set_local_active(false);
                state.loading=true;f.ended(&ui,&state,Some(1));assert!(f.starting.is_none(),"An old EOF advanced the queue while loading");state.loading=false;
                assert!(f.next(&ui,&state));f.starting.as_mut().unwrap().2=Instant::now()-Duration::from_secs(16);f.tick(&ui,&state);assert_eq!(f.queue.len(),1);assert!(f.starting.is_none());
                drop(f);ui.set_message("".into());ui.set_page(3);ui.invoke_browse("liked".into(),"Liked Songs".into());
            },
            5=>{
                let id=features.borrow().serial;features.borrow_mut().response(&ui,id,Ok(json!({"kind":"collection_page","items":[{"track":track('a',"Forest floor")},{"item":track('b',"Evening light")},{"item":null}],"has_more":true})));
                assert_eq!(ui.get_feature_tracks().row_count(),2);
            },
            6=>{
                capture(&ui,&out,"liked-songs");ui.invoke_feature_next();let id=features.borrow().serial;features.borrow_mut().response(&ui,id,Ok(json!({"kind":"collection_page","items":[{"item":track('d',"Afterglow")}],"has_more":false})));
                assert_eq!(ui.get_feature_tracks().row_count(),3);
                let uri=ui.get_feature_tracks().row_data(0).unwrap().uri;ui.invoke_song_action("Unsave".into(),uri.clone());let id=features.borrow().serial;features.borrow_mut().response(&ui,id,Ok(json!({"kind":"saved","uri":uri.as_str(),"saved":false})));assert_eq!(ui.get_feature_tracks().row_count(),2);
                let uri=ui.get_feature_tracks().row_data(0).unwrap().uri;ui.invoke_song_action("Playlist".into(),uri);assert!(!ui.get_edit_track().is_empty());ui.set_edit_playlist("https://example.com/wrong".into());ui.invoke_playlist_edit(false);assert!(ui.get_feature_message().contains("owned"));
            },
            7=>{
                capture(&ui,&out,"playlist-validation");ui.set_edit_track("".into());ui.set_feature_message("".into());ui.set_page(8);
            },
            8=>{capture(&ui,&out,"settings-dark");ui.invoke_appearance("theme".into());assert!(ui.get_light_mode());ui.invoke_appearance("history".into());assert!(ui.get_local_history());ui.invoke_appearance("media".into());assert!(!ui.get_media_keys());},
            9=>{capture(&ui,&out,"settings-light");ui.invoke_appearance("compact".into());assert!(ui.get_compact_mode());},
            10=>{capture(&ui,&out,"settings-compact");ui.invoke_appearance("theme".into());ui.set_page(5);},
            11=>{let id=features.borrow().serial;features.borrow_mut().response(&ui,id,Ok(json!({"kind":"home","tracks":[track('a',"Forest floor"),track('b',"Evening light")]})));assert_eq!(ui.get_feature_tracks().row_count(),2);},
            12=>{capture(&ui,&out,"home-compact");ui.invoke_appearance("compact".into());ui.invoke_song_action("Artist".into(),ui.get_feature_tracks().row_data(0).unwrap().uri);},
            13=>{let id=features.borrow().serial;features.borrow_mut().response(&ui,id,Ok(json!({"kind":"artist","title":"Review artist","tracks":[track('a',"Forest floor")],"note":"Tracks from recent artist releases"})));assert_eq!(ui.get_feature_tracks().row_count(),1);},
            14=>{
                capture(&ui,&out,"artist");
                ui.invoke_feature_back();assert_eq!(ui.get_page(),5);assert_eq!(ui.get_feature_tracks().row_count(),2,"Artist Back lost the previous list");
                let mut f=features.borrow_mut();let mut s=state::State::default();s.active=true;s.playing=true;s.track=Track::from_entity(&crate::web_api::entity(&track('a',"Forest floor")));f.tick(&ui,&s);f.listened=Duration::from_secs(29);f.last_tick=Instant::now()-Duration::from_secs(10);f.tick(&ui,&s);assert!(f.history.is_empty(),"Waiting time counted as listening");s.playing=false;f.listened=Duration::from_secs(30);f.tick(&ui,&s);assert!(f.history.is_empty(),"Paused time recorded history");s.playing=true;f.tick(&ui,&s);assert_eq!(f.history.len(),1);f.tick(&ui,&s);assert_eq!(f.history.len(),1);drop(f);ui.set_page(9);
            },
            15=>{assert_eq!(ui.get_feature_tracks().row_count(),1);capture(&ui,&out,"session-history");features.borrow_mut().clear(&ui);assert_eq!(ui.get_feature_tracks().row_count(),0);assert_eq!(ui.get_local_queue().row_count(),0);assert!(features.borrow().pending.is_empty());},
            16=>{
                ui.set_page(4);ui.set_jam_connected(true);ui.set_jam_host(false);ui.set_jam_invite("cross-room-a".into());ui.set_jam_following(true);ui.set_library_ready(true);
                let remote=crate::jam::Playback{uri:"youtube:abcdefghijk".into(),title:"Forest floor".into(),artist:"Review artist".into(),position_ms:12000,playing:true,sampled_at_ms:crate::jam::unix_ms(),..Default::default()};
                assert!(features.borrow_mut().paired_host(&ui,&remote).is_none(),"Foreign song played without confirmation");let stale=features.borrow().serial;
                let remote2=crate::jam::Playback{uri:"youtube:bcdefghijkl".into(),..remote};assert!(features.borrow_mut().paired_host(&ui,&remote2).is_none());let id=features.borrow().serial;
                features.borrow_mut().response(&ui,stale,Ok(json!({"tracks":[track('d',"Wrong stale recording")]})));assert_eq!(ui.get_jam_matches().row_count(),0);
                features.borrow_mut().response(&ui,id,Ok(json!({"tracks":[track('a',"Forest floor"),track('b',"Forest floor · live")]})));assert_eq!(ui.get_jam_matches().row_count(),2);
                ui.invoke_jam_pair(0);assert_eq!(features.borrow_mut().paired_host(&ui,&remote2),Some("spotify:track:aaaaaaaaaaaaaaaaaaaaaa".into()));
                ui.set_jam_match_link("https://evil.test/song".into());ui.invoke_jam_pair(-1);assert_eq!(features.borrow_mut().paired_host(&ui,&remote2),Some("spotify:track:aaaaaaaaaaaaaaaaaaaaaa".into()));
                ui.set_jam_sync_status("Following Tide · experimental cross-service room".into());
            },
            17=>{
                capture(&ui,&out,"cross-service-match");features.borrow_mut().reset_jam(&ui);ui.set_jam_connected(true);ui.set_jam_invite("isrc-room".into());
                // ISRC + duration auto-match, opt-in remembered choices, album entries, host ISRC lookup.
                let isrc_track=|id:char,title:&str,ms:u64|{let mut t=track(id,title);t["duration_ms"]=json!(ms);t["external_ids"]=json!({"isrc":"USRC17607839"});t};
                let remote=crate::jam::Playback{uri:"youtube:cdefghijklm".into(),title:"Artist - Forest floor (Official Video)".into(),position_ms:1000,playing:true,sampled_at_ms:crate::jam::unix_ms(),duration_ms:211_000,isrc:"USRC17607839".into(),..Default::default()};
                assert!(features.borrow_mut().paired_host(&ui,&remote).is_none());let id=features.borrow().serial;
                features.borrow_mut().response(&ui,id,Ok(json!({"tracks":[isrc_track('e',"Forest floor · live",260_000),isrc_track('f',"Forest floor",210_000)]})));
                assert!(ui.get_jam_match_confirmed()&&ui.get_jam_message().contains("ISRC"),"Exact ISRC with agreeing duration should confirm itself");
                assert_eq!(features.borrow_mut().paired_host(&ui,&remote),Some("spotify:track:ffffffffffffffffffffff".into()));
                features.borrow_mut().set_remember_jam(&ui,true);ui.invoke_jam_pair(0);
                features.borrow_mut().reset_jam(&ui);ui.set_jam_invite("next-room".into());
                assert!(features.borrow_mut().paired_host(&ui,&remote).is_some(),"Opted-in choices carry over to the next room");
                features.borrow_mut().set_remember_jam(&ui,false);features.borrow_mut().reset_jam(&ui);ui.set_jam_invite("third-room".into());
                assert!(features.borrow_mut().paired_host(&ui,&remote).is_none(),"Opting out forgets saved choices");assert!(features.borrow().prefs.jam_matches.is_empty());
                let(sender,mut receiver)=tokio::sync::mpsc::unbounded_channel();
                features.borrow_mut().resolve_jam_collection(&ui,"spotify:album:aaaaaaaaaaaaaaaaaaaaaa".into(),sender);let id=features.borrow().serial;
                features.borrow_mut().response(&ui,id,Ok(json!({"kind":"collection_page","items":[track('a',"One"),{"track":track('b',"Two")},{"item":null}],"has_more":false})));
                assert!(matches!(receiver.try_recv(),Ok(crate::jam_transport::Request::AddItems{group,items}) if group=="From an album"&&items.len()==2));
                ui.set_library_ready(true);assert_eq!(features.borrow_mut().host_isrc(&ui,"spotify:track:ffffffffffffffffffffff"),(String::new(),0));let id=features.borrow().serial;
                features.borrow_mut().response(&ui,id,Ok(json!({"track":isrc_track('f',"Forest floor",210_000)})));
                assert_eq!(features.borrow_mut().host_isrc(&ui,"spotify:track:ffffffffffffffffffffff"),("USRC17607839".into(),210_000));
                capture(&ui,&out,"isrc-match");ui.set_jam_connected(false);
            },
            18=>{features.borrow_mut().reset_jam(&ui);ui.set_jam_invite("cross-room-b".into());
                let remote=crate::jam::Playback{uri:"youtube:bcdefghijkl".into(),title:"Forest floor".into(),artist:"Review artist".into(),position_ms:12000,playing:true,sampled_at_ms:crate::jam::unix_ms(),..Default::default()};
                assert!(features.borrow_mut().paired_host(&ui,&remote).is_none(),"Recording choice leaked across rooms");let id=features.borrow().serial;
                ui.set_jam_connected(false);features.borrow_mut().response(&ui,id,Ok(json!({"tracks":[track('d',"Late response")]})));assert_eq!(ui.get_jam_matches().row_count(),0);
                features.borrow_mut().clear(&ui);
            },
            // Shell (#41): the queue drawer (#27), Hide beside Like (#30) and related songs.
            18=>{
                ui.set_library_ready(true);ui.set_authenticated(true);ui.set_page(1);
                let mut f=features.borrow_mut();
                f.prefs.autoplay=true;ui.set_autoplay(true);
                f.rotation=["Forest floor","Evening light","Afterglow","Quiet chord"].iter().zip(['a','b','c','d']).map(|(t,c)|Track::from_entity(&crate::web_api::entity(&track(c,t)))).collect();
                ui.set_current_uri("spotify:track:aaaaaaaaaaaaaaaaaaaaaa".into());
                f.queue=["Morning light","Roots and rivers","Soft and slow"].iter().zip(['e','f','g']).map(|(t,c)|Track::from_entity(&crate::web_api::entity(&track(c,t)))).collect();
                f.render_queue(&ui);
                assert_eq!(ui.get_autoplay_queue().row_count(),3,"Autoplay preview must skip the current song");
                drop(f);
                ui.invoke_queue_move(0,2);
                let order:Vec<_>=ui.get_local_queue().iter().map(|t|t.title.to_string()).collect();
                assert_eq!(order,["Roots and rivers","Soft and slow","Morning light"],"Drag reorder");
                ui.invoke_queue_move(5,0);assert_eq!(ui.get_local_queue().row_count(),3);
                ui.invoke_queue_filter("slow".into());
                assert_eq!(ui.get_queue_search_results().row_count(),1);assert_eq!(ui.get_queue_search_index().row_data(0),Some(1));
                ui.invoke_queue_filter("light".into());
                assert_eq!(ui.get_queue_search_results().row_count(),2,"Search covers manual and autoplay sections");
                assert_eq!(ui.get_queue_search_index().row_data(1),Some(-1));
                ui.invoke_queue_filter("".into());
                // Hide the playing song, then another: neither may autoplay.
                ui.set_current_uri("spotify:track:bbbbbbbbbbbbbbbbbbbbbb".into());
                ui.invoke_hide_current(true);
                assert!(ui.get_current_hidden());assert!(features.borrow().prefs.hidden.contains("spotify:track:bbbbbbbbbbbbbbbbbbbbbb"));
                ui.set_current_uri("spotify:track:cccccccccccccccccccccc".into());ui.invoke_hide_current(true);
                ui.set_current_uri("spotify:track:aaaaaaaaaaaaaaaaaaaaaa".into());features.borrow().render_queue(&ui);
                let autoplay:Vec<_>=ui.get_autoplay_queue().iter().map(|t|t.uri.to_string()).collect();
                assert_eq!(autoplay,["spotify:track:dddddddddddddddddddddd"],"Hidden songs appeared in autoplay");
                ui.invoke_autoplay_action("queue".into(),0);assert_eq!(ui.get_local_queue().row_count(),4);
                let mut f=features.borrow_mut();f.queue.clear();f.render_queue(&ui);
                let mut s=state::State::default();s.authenticated=true;s.active=true;s.play_request_id=Some(3);s.track=Track::from_entity(&crate::web_api::entity(&track('a',"Forest floor")));
                f.ended(&ui,&s,Some(3));
                assert_eq!(f.queue.first().map(|t|t.uri.as_str()),Some("spotify:track:dddddddddddddddddddddd"),"Autoplay picked a hidden song");
                f.starting=None;f.queue.clear();f.render_queue(&ui);drop(f);
                ui.set_current_uri("spotify:track:bbbbbbbbbbbbbbbbbbbbbb".into());ui.invoke_hide_current(false);assert!(!ui.get_current_hidden());
                ui.set_current_uri("spotify:track:aaaaaaaaaaaaaaaaaaaaaa".into());
                // Related songs: stale responses for an earlier song are ignored.
                ui.invoke_load_related();assert_eq!(ui.get_related_state(),1);let stale=features.borrow().serial;
                ui.set_current_uri("spotify:track:dddddddddddddddddddddd".into());
                features.borrow_mut().response(&ui,stale,Ok(json!({"kind":"radio","tracks":[track('e',"Stale related")]})));
                assert_eq!(ui.get_related_tracks().row_count(),0,"A stale related list replaced the current song's");
                ui.invoke_load_related();let id=features.borrow().serial;
                features.borrow_mut().response(&ui,id,Ok(json!({"kind":"radio","tracks":[track('d',"The current song"),track('e',"Related one"),track('f',"Related two")],"note":"More from this artist"})));
                assert_eq!(ui.get_related_state(),2);assert_eq!(ui.get_related_tracks().row_count(),2,"Related must skip the current song");
                ui.invoke_related_action("queue".into(),1);assert_eq!(ui.get_local_queue().row_data(0).unwrap().title,"Related two");
                ui.set_message("".into());
            },
            19=>{
                assert!(ui.get_queue_open(),"Page 1 did not open the queue drawer");assert_eq!(ui.get_page(),0);
                capture(&ui,&out,"queue-drawer");
                ui.set_queue_open(false);ui.set_page(5);
                assert!(ui.get_can_go_back(),"History did not record pages");
                ui.invoke_navigate_back();assert_eq!(ui.get_page(),0,"Back did not return to Now Playing");
                assert!(ui.get_can_go_forward());ui.invoke_navigate_forward();assert_eq!(ui.get_page(),5);
                ui.invoke_appearance("accent-art".into());assert!(ui.get_accent_follows_art());ui.invoke_appearance("accent-art".into());
                features.borrow_mut().clear(&ui);
                eprintln!("PASS: native feature callbacks, confirmed cross-service matching, stale-song and room guards, queue drawer, hide and related songs");
            },
            _ if browse_started=>return,
            _=>{if crate::app::system_review::step(&ui,&features,&out,step-20){browse_started=true;super::browse_review::start(&ui, features.clone(), out.clone());}}
        }
        eprintln!("Feature review stage {step} passed");step+=1;
    });
    Ok(timer)
}
