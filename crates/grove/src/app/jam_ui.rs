//! Shared jam callbacks and the 100 ms jam timer that hosts or follows a room.
use super::send;
use crate::{
    features::Features,
    jam, jam_sync, jam_transport,
    spotify::{self, command, RequestSender},
    state::State,
    GroveJamTrack, GroveWindow,
};
use serde_json::json;
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};

pub type JamSender = tokio::sync::mpsc::UnboundedSender<jam_transport::Request>;

pub fn bind(ui: &GroveWindow, jam_tx: &JamSender, features: &Rc<RefCell<Features>>) {
    let weak = ui.as_weak();
    let sender = jam_tx.clone();
    ui.on_jam_create(move || {
        if let Some(ui) = weak.upgrade() {
            if ui.get_local_active() {
                ui.set_jam_message("Return to Spotify before starting a jam.".into());
                return;
            }
            if ui.get_jam_busy() || ui.get_jam_connected() {
                return;
            }
            ui.set_jam_busy(true);
            ui.set_jam_message("Starting a nearby jam…".into());
            let _ = sender.send(jam_transport::Request::Host {
                name: ui.get_jam_name().to_string(),
                address: ui.get_jam_address().to_string(),
            });
        }
    });
    let weak = ui.as_weak();
    let sender = jam_tx.clone();
    ui.on_jam_join(move || {
        if let Some(ui) = weak.upgrade() {
            if ui.get_local_active() {
                ui.set_jam_message("Return to Spotify before joining a jam.".into());
                return;
            }
            if ui.get_jam_busy() || ui.get_jam_connected() {
                return;
            }
            ui.set_jam_busy(true);
            ui.set_jam_message("Connecting to the host…".into());
            let _ = sender.send(jam_transport::Request::Join {
                name: ui.get_jam_name().to_string(),
                invite: ui.get_jam_join_invite().to_string(),
            });
        }
    });
    let sender = jam_tx.clone();
    ui.on_jam_leave(move || {
        let _ = sender.send(jam_transport::Request::Leave);
    });
    let weak = ui.as_weak();
    let sender = jam_tx.clone();
    let feature_state = features.clone();
    ui.on_jam_add(move || {
        if let Some(ui) = weak.upgrade() {
            if ui.get_jam_tide_host() {
                ui.set_jam_message("Choose the next song in Tide on the host's device.".into());
                return;
            }
            match spotify::spotify_uri(&ui.get_jam_track_link()) {
                Ok(uri) if uri.starts_with("spotify:track:") => {
                    if ui.get_jam_track_title().trim().is_empty() {
                        feature_state
                            .borrow_mut()
                            .resolve_jam(&ui, uri, sender.clone());
                        return;
                    }
                    ui.set_jam_message(String::new().into());
                    let _ = sender.send(jam_transport::Request::Add {
                        uri,
                        title: ui.get_jam_track_title().to_string(),
                    });
                }
                _ => ui.set_jam_message("Add an individual Spotify track link.".into()),
            }
        }
    });
    let sender = jam_tx.clone();
    ui.on_jam_remove(move |id| {
        let _ = sender.send(jam_transport::Request::Remove(id.to_string()));
    });
    let sender = jam_tx.clone();
    ui.on_jam_next(move || {
        let _ = sender.send(jam_transport::Request::Next);
    });
}

pub fn start_timer(
    ui: &GroveWindow,
    jam_events: mpsc::Receiver<jam_transport::Event>,
    state: &Rc<RefCell<State>>,
    jam_tx: &JamSender,
    tx: &RequestSender,
    features: &Rc<RefCell<Features>>,
) -> slint::Timer {
    let jam_timer = slint::Timer::default();
    let weak = ui.as_weak();
    let current = state.clone();
    let sender = jam_tx.clone();
    let spotify_sender = tx.clone();
    let jam_features = features.clone();
    let mut member_id = String::new();
    let mut room_id = String::new();
    let mut revision = 0u64;
    let mut target: Option<jam_sync::Target> = None;
    let mut follower = jam_sync::Follower::default();
    let mut published = Instant::now() - Duration::from_secs(2);
    let mut was_following = true;
    let mut quiet = false;
    let mut pending: Option<(String, String, Option<u64>, Instant)> = None;
    jam_timer.start(slint::TimerMode::Repeated,Duration::from_millis(100),move||{
        let Some(ui)=weak.upgrade()else{return;};let state=current.borrow();
        while let Ok(event)=jam_events.try_recv(){
            match event {
                jam_transport::Event::Joined{invite,member_id:member,host}=>{
                    jam_features.borrow_mut().reset_jam(&ui);
                    ui.set_jam_busy(false);
                    member_id=member;room_id.clear();revision=0;target=None;follower=jam_sync::Follower::default();pending=None;quiet=false;was_following=true;
                    ui.set_jam_connected(true);ui.set_jam_host(host);ui.set_jam_following(true);ui.set_jam_invite(invite.into());ui.set_jam_message(String::new().into());
                }
                jam_transport::Event::Snapshot{state:room,host_sent_ms,transit_ms}=>{
                    if !room_id.is_empty() && room.room_id!=room_id{continue;}
                    if room.revision<=revision{continue;}room_id=room.room_id.clone();revision=room.revision;
                    if let Some(playback)=&room.playback { ui.set_jam_tide_host(playback.uri.starts_with("youtube:")); }
                    ui.set_jam_members(room.members.iter().map(|m|format!("{}{}",m.name,if m.host{" (host)"}else{""})).collect::<Vec<_>>().join(" · ").into());
                    ui.set_jam_tracks(ModelRc::new(VecModel::from(room.queue.iter().map(|entry|GroveJamTrack{
                        id:entry.id.clone().into(),title:entry.title.clone().into(),
                        subtitle:format!("Added by {} · {}",entry.member_name,match entry.delivery{jam::Delivery::Waiting=>"Waiting",jam::Delivery::Sending=>"Starting on host…",jam::Delivery::Unconfirmed=>"Unconfirmed · check host playback"}).into(),
                        removable:entry.delivery!=jam::Delivery::Sending && (ui.get_jam_host()||entry.member_id==member_id),
                    }).collect::<Vec<_>>())));
                    target=room.playback.map(|playback|jam_sync::Target{playback,host_sent_ms,received:Instant::now(),transit_ms});
                    if target.is_some(){quiet=false;}
                }
                jam_transport::Event::Play{id,uri}=>{
                    if !state.authenticated {let _=sender.send(jam_transport::Request::Complete{id,success:false});ui.set_jam_message("Enable your Spotify playback before starting a shared track.".into());continue;}
                    pending=Some((id,uri.clone(),state.play_request_id,Instant::now()));
                    send(&spotify_sender,json!({"command":"play","uri":uri}));
                }
                jam_transport::Event::Left(message)=>{
                    jam_features.borrow_mut().reset_jam(&ui);
                    ui.set_jam_busy(false);
                    if ui.get_jam_connected()&&!ui.get_jam_host()&&ui.get_jam_following()&&state.playing{send(&spotify_sender,command("pause"));}
                    ui.set_jam_connected(false);ui.set_jam_host(false);ui.set_jam_following(true);ui.set_jam_invite(String::new().into());ui.set_jam_members(String::new().into());ui.set_jam_tracks(ModelRc::default());ui.set_jam_message(message.into());ui.set_jam_sync_status("Not in a jam".into());
                    target=None;pending=None;room_id.clear();revision=0;follower=jam_sync::Follower::default();
                }
                jam_transport::Event::Error(message)=>{ui.set_jam_busy(false);ui.set_jam_message(message.into());},
            }
        }
        if !ui.get_jam_connected(){return;}
        if ui.get_jam_host(){
            let shareable=state.authenticated&&state.active&&state.track.uri.starts_with("spotify:track:");
            ui.set_jam_sync_status(if shareable{"Sharing host playback · listeners follow on their own devices"}else if state.authenticated&&state.active&&!state.track.uri.is_empty(){"Choose a Spotify track on the host · other audio types cannot be shared yet"}else{"Connect Spotify and play a track to start listening together"}.into());
            if published.elapsed()>=Duration::from_secs(1){
                published=Instant::now();
                let frame=if shareable{
                    Some(jam::Playback{uri:state.track.uri.clone(),title:state.track.title.clone(),artist:state.track.artist.clone(),position_ms:state.position().max(0.0) as u64,playing:state.playing,sampled_at_ms:jam::unix_ms()})
                }else{None};let _=sender.send(jam_transport::Request::Publish(frame));
            }
            if let Some((id,uri,before,started))=&pending{
                let confirmed=state.track.uri==*uri&&state.playing&&state.play_request_id!=*before;
                if confirmed||started.elapsed()>Duration::from_secs(15){
                    let _=sender.send(jam_transport::Request::Complete{id:id.clone(),success:confirmed});
                    if !confirmed{ui.set_jam_message("Playback was not confirmed. Check the host player before removing the unconfirmed entry.".into());}
                    pending=None;
                }
            }
        }else{
            if was_following!=ui.get_jam_following(){follower=jam_sync::Follower::default();quiet=false;was_following=ui.get_jam_following();}
            if !ui.get_jam_following(){ui.set_jam_sync_status("Following stopped · your playback is independent".into());return;}
            if !state.authenticated{ui.set_jam_sync_status("Enable Spotify playback in Account to listen".into());return;}
            let Some(remote)=&target else{
                ui.set_jam_matching(false);
                ui.set_jam_sync_status("Waiting for the host to play a track".into());
                if !quiet&&state.playing{send(&spotify_sender,command("pause"));quiet=true;}return;
            };
            if remote.stale(Instant::now()) { ui.set_jam_sync_status("Host updates delayed · waiting to sync".into()); return; }
            let Some(uri)=jam_features.borrow_mut().paired_host(&ui,&remote.playback) else {
                ui.set_jam_sync_status("Choose the matching Spotify song to follow Tide".into());
                if !quiet && state.playing { send(&spotify_sender,command("pause")); quiet=true; }
                return;
            };
            quiet=false;
            let mapped=jam_sync::Target{playback:jam::Playback{uri,..remote.playback.clone()},host_sent_ms:remote.host_sent_ms,received:remote.received,transit_ms:remote.transit_ms};
            let remote=&mapped;
            if (!state.active||state.track.uri!=remote.playback.uri)&&follower.exhausted(&remote.playback.uri,Instant::now()){
                ui.set_jam_sync_status("Could not load the host track. Check Connection, then stop and restart following to retry.".into());return;
            }
            ui.set_jam_sync_status(if remote.stale(Instant::now()){"Host updates delayed · waiting to sync".into()}else if !state.active||state.track.uri!=remote.playback.uri{format!("Loading host track · {}",remote.playback.title).into()}else if state.playing!=remote.playback.playing{"Synchronizing play/pause with the host…".into()}else{format!("Following {} · {}",remote.playback.title,if remote.playback.playing{"Playing"}else{"Paused"}).into()});
            if let Some(correction)=follower.plan(remote,jam_sync::Local{uri:if state.active{&state.track.uri}else{""},position_ms:state.position().max(0.0) as u64,playing:state.playing,ready:state.authenticated},Instant::now()){
                let payload=match correction {
                    jam_sync::Correction::Load{uri,position_ms,playing}=>json!({"command":"jam_load","uri":uri,"position_ms":position_ms,"playing":playing}),
                    jam_sync::Correction::Seek(position_ms)=>json!({"command":"seek","position_ms":position_ms}),
                    jam_sync::Correction::Play=>command("play"),jam_sync::Correction::Pause=>command("pause"),
                };send(&spotify_sender,payload);
            }
        }
    });
    jam_timer
}
