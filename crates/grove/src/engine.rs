//! In-process librespot playback. No external daemon, keys, or plaintext login cache.
use crate::{
    audio::{
        control::control,
        fade::{self, AlbumSlot},
        quality,
        reconnect::RestorePoint,
        settings::{AudioSettings, NormalisationKind},
        sink::GroveSink,
    },
    spotify::Event,
};
use librespot_connect::{ConnectConfig, LoadRequest, LoadRequestOptions, PlayingTrack, Spirc};
use librespot_core::{
    authentication::Credentials,
    config::{DeviceType, SessionConfig},
    Session,
};
use librespot_playback::{
    config::{Bitrate, NormalisationType, PlayerConfig},
    mixer::{self, MixerConfig},
    player::{Player, PlayerEvent},
};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::task::JoinHandle;

/// What the worker needs to bring playback back after a lost session.
#[derive(Default)]
struct Tracking {
    point: RestorePoint,
    /// Album position of the current song, for the queue lookahead.
    slot: Option<AlbumSlot>,
    /// When `point.position_ms` was sampled, to extrapolate while playing.
    sampled: Option<Instant>,
    /// The current song was loaded by a jam: everyone in the room hears the
    /// same unfaded audio, so local fades must not apply.
    jam: bool,
}

pub struct Engine {
    pub generation: u64,
    pub session: Session,
    pub spirc: Option<Spirc>,
    tasks: Vec<JoinHandle<()>>,
    active: Arc<AtomicBool>,
    ended: Arc<AtomicBool>,
    closing: Arc<AtomicBool>,
    tracking: Arc<Mutex<Tracking>>,
    events: Sender<Event>,
}

impl Drop for Engine {
    fn drop(&mut self) {
        // An intentional shutdown is not a lost session.
        self.closing.store(true, Ordering::Release);
        if let Some(spirc) = &self.spirc {
            let _ = spirc.shutdown();
        }
        self.session.shutdown();
        for task in &self.tasks {
            task.abort();
        }
    }
}

fn player_config(settings: &AudioSettings) -> PlayerConfig {
    PlayerConfig {
        position_update_interval: Some(Duration::from_secs(1)),
        bitrate: match settings.bitrate {
            96 => Bitrate::Bitrate96,
            320 => Bitrate::Bitrate320,
            _ => Bitrate::Bitrate160,
        },
        normalisation: settings.normalise,
        normalisation_type: match settings.normalisation {
            NormalisationKind::Auto => NormalisationType::Auto,
            NormalisationKind::Track => NormalisationType::Track,
            NormalisationKind::Album => NormalisationType::Album,
        },
        normalisation_pregain_db: f64::from(settings.pregain_db),
        ..PlayerConfig::default()
    }
}

/// Look up Grove's next queued song (the drawer or autoplay refill) so a
/// queued album run keeps its gapless joins and anything else fades.
async fn lookahead(session: Session, tracking: Arc<Mutex<Tracking>>) {
    let mut checked = (String::new(), String::new());
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tick.tick().await;
        let next = control().next_uri();
        let (current, slot) = {
            let t = lock(&tracking);
            (t.point.track_uri.clone(), t.slot.clone())
        };
        if (current.clone(), next.clone()) == checked {
            continue;
        }
        checked = (current, next.clone());
        let gapless = if next.starts_with("spotify:track:") && slot.is_some() {
            match librespot_core::SpotifyUri::from_uri(&next) {
                Ok(id) => {
                    use librespot_metadata::Metadata;
                    librespot_metadata::Track::get(&session, &id)
                        .await
                        .ok()
                        .map(|track| {
                            let following = AlbumSlot {
                                album: track.album.name.clone(),
                                disc: u32::try_from(track.disc_number).unwrap_or(0),
                                number: u32::try_from(track.number).unwrap_or(0),
                            };
                            fade::consecutive(slot.as_ref(), Some(&following))
                        })
                }
                Err(_) => None,
            }
        } else if next.is_empty() {
            None
        } else {
            Some(false)
        };
        control().set_next_gapless(&next, gapless);
    }
}

fn lock(tracking: &Mutex<Tracking>) -> std::sync::MutexGuard<'_, Tracking> {
    tracking.lock().unwrap_or_else(|p| p.into_inner())
}

impl Engine {
    /// `lost` receives this generation if the session ends on its own.
    pub async fn start(
        credentials: Credentials,
        account: &str,
        events: Sender<Event>,
        generation: u64,
        lost: tokio::sync::mpsc::UnboundedSender<u64>,
    ) -> Result<(Self, Credentials), String> {
        // Passing no Cache prevents librespot from writing credentials.json.
        let session = Session::new(SessionConfig::default(), None);
        let mut engine = Self {
            generation,
            session,
            spirc: None,
            tasks: vec![],
            active: Arc::new(AtomicBool::new(false)),
            ended: Arc::new(AtomicBool::new(false)),
            closing: Arc::new(AtomicBool::new(false)),
            tracking: Arc::new(Mutex::new(Tracking::default())),
            events: events.clone(),
        };
        let settings = control().settings();
        let mixer = mixer::find(Some("softvol")).ok_or("Software mixer unavailable.")?(
            MixerConfig::default(),
        )
        .map_err(|_| "Could not initialize volume control.")?;
        let player = Player::new(
            player_config(&settings),
            engine.session.clone(),
            mixer.get_soft_volume(),
            move || Box::new(GroveSink::new()),
        );
        let mut player_events = player.get_player_event_channel();
        let config = ConnectConfig {
            name: "Grove".into(),
            device_type: DeviceType::Computer,
            initial_volume: (u16::MAX as f32 * 0.35) as u16,
            ..ConnectConfig::default()
        };
        let (spirc, task) = tokio::time::timeout(
            Duration::from_secs(75),
            Spirc::new(config, engine.session.clone(), credentials, player, mixer),
        )
        .await
        .map_err(|_| "Spotify playback connection timed out. Retry when online.".to_string())?
        .map_err(|_| {
            "Spotify refused playback approval. Premium is required; try setting up playback again."
                .to_string()
        })?;
        if engine.session.username() != account {
            let _ = spirc.shutdown();
            return Err("Playback approval belongs to a different Spotify account. Use the same account for both approvals.".into());
        }
        let reusable = Credentials {
            username: Some(engine.session.username()),
            auth_type: librespot_protocol::authentication::AuthenticationType::AUTHENTICATION_STORED_SPOTIFY_CREDENTIALS,
            auth_data: engine.session.auth_data(),
        };
        engine.spirc = Some(spirc);
        let ended = engine.ended.clone();
        let active = engine.active.clone();
        let closing = engine.closing.clone();
        engine.tasks.push(tokio::spawn(async move {
            task.await;
            active.store(false, Ordering::Relaxed);
            ended.store(true, Ordering::Relaxed);
            if !closing.load(Ordering::Acquire) {
                let _ = lost.send(generation);
            }
        }));
        let active_flag = engine.active.clone();
        let tracking = engine.tracking.clone();
        let bitrate = settings.bitrate;
        engine.tasks.push(tokio::spawn(async move {
            let mut item = json!({}); let mut playing = false; let mut active = false;
            let mut position = 0u32; let mut volume = 35u32; let mut request_id:Option<u64>=None;
            let mut shuffled=false;let mut repeated=false;
            let mut loading=false;
            let mut natural_end: Option<Instant> = None;
            let mut slot: Option<AlbumSlot> = None;
            let mut duration_ms = 0u32;
            let mut title = String::new();
            let mut uri = String::new();
            while let Some(event) = player_events.recv().await {
                // Only real playhead samples update a saved resume position.
                let mut sampled = false;
                match event {
                    PlayerEvent::TrackChanged { audio_item } => {
                        let names = match &audio_item.unique_fields {
                            librespot_metadata::audio::item::UniqueFields::Track { artists, .. } => artists.iter().map(|a| a.name.clone()).collect::<Vec<_>>(),
                            librespot_metadata::audio::item::UniqueFields::Episode { show_name, .. } => vec![show_name.clone()],
                            _ => vec![],
                        };
                        let next_slot = match &audio_item.unique_fields {
                            librespot_metadata::audio::item::UniqueFields::Track { album, number, disc_number, .. } => Some(AlbumSlot { album: album.clone(), disc: *disc_number, number: *number }),
                            _ => None,
                        };
                        let creators = names.iter().map(|name| json!({"entity":{"decorations":{"identity":{"name":name}}}})).collect::<Vec<_>>();
                        item = json!({"uri":audio_item.uri,"artwork_url":audio_item.covers.first().map(|cover|&cover.url),"decorations":{"identity":{"name":audio_item.name},"creators":creators,"playback":{"duration_ms":audio_item.duration_ms}}});
                        position = 0;
                        duration_ms = audio_item.duration_ms;
                        title = audio_item.name.clone();
                        uri = audio_item.uri.clone();
                        // Fades for this song: in only after a natural end, never inside an album played in order.
                        let audio = control().settings();
                        let context = lock(&tracking).point.context_uri.clone();
                        // Jam-loaded songs never fade: listeners start together and a
                        // guest's sync seeks must not land in a local fade.
                        let jam = lock(&tracking).jam;
                        let fade_seconds = if jam { 0 } else { audio.fade_seconds };
                        let (fade_in, fade_out) = fade::plan(
                            fade_seconds,
                            fade::natural(natural_end, Instant::now()),
                            fade::gapless_context(&context, shuffled),
                            fade::consecutive(slot.as_ref(), next_slot.as_ref()),
                            f64::from(duration_ms),
                        );
                        natural_end = None;
                        slot = next_slot;
                        lock(&tracking).slot = slot.clone();
                        let eligible = fade::plan(fade_seconds, false, false, false, f64::from(duration_ms)).1;
                        control().update_clock(|clock| {
                            clock.eligible = eligible;
                            clock.anchor_ms = 0.0;
                            clock.duration_ms = f64::from(duration_ms);
                            clock.fade_ms = f64::from(fade_seconds) * 1000.0;
                            clock.fade_in = fade_in;
                            clock.fade_out = fade_out;
                        });
                        let formats: Vec<quality::Format> = audio_item.files.keys().copied().filter_map(quality::Format::from_librespot).collect();
                        let _ = events.send(Event::Payload(generation,json!({"type":"source","uri":uri,"label":quality::source_label(bitrate,&formats)})));
                        let mut t = lock(&tracking);
                        t.point.track_uri = uri.clone(); t.point.position_ms = 0; t.sampled = Some(Instant::now());
                    }
                    PlayerEvent::Playing { position_ms, play_request_id, .. } => {
                        playing = true; loading=false; active = true; position = position_ms; request_id=Some(play_request_id);
                        control().update_clock(|clock| clock.anchor_ms = f64::from(position_ms));
                    }
                    PlayerEvent::Paused { position_ms, .. } => {
                        playing = false; loading=false; position = position_ms; sampled = true;
                        control().update_clock(|clock| clock.anchor_ms = f64::from(position_ms));
                    }
                    PlayerEvent::Seeked { position_ms, .. } | PlayerEvent::PositionCorrection { position_ms, .. } => {
                        position = position_ms; sampled = true;
                        control().update_clock(|clock| clock.anchor_ms = f64::from(position_ms));
                    }
                    PlayerEvent::PositionChanged { position_ms, .. } => { position = position_ms; sampled = true; }
                    PlayerEvent::VolumeChanged { volume: level } => {
                        volume = (level as f32 / u16::MAX as f32 * 100.0).round() as u32;
                        // A volume event carries no new position. Re-emitting the last
                        // full sample here would rewind the UI's playback clock.
                        let _ = events.send(Event::Payload(generation,json!({"type":"volume_changed","volume":volume})));
                        continue;
                    }
                    PlayerEvent::EndOfTrack { play_request_id, .. } => {
                        natural_end = Some(Instant::now());
                        if control().with_settings(|s| s.resume.forget(&uri)) { control().mark_resume_dirty(); }
                        let _=events.send(Event::Payload(generation,json!({"type":"end_of_track","play_request_id":play_request_id})));playing=false;
                    }
                    PlayerEvent::Stopped { .. } => { playing = false; }
                    PlayerEvent::Loading { .. } => {
                        loading=true;playing=false;
                        let _=events.send(Event::Payload(generation,json!({"type":"loading"})));continue;
                    }
                    PlayerEvent::ShuffleChanged {shuffle} => {shuffled=shuffle;let _=events.send(Event::Payload(generation,json!({"type":"options_changed","options":{"shuffle":shuffle}})));continue;}
                    PlayerEvent::RepeatChanged {context,..} => {repeated=context;let _=events.send(Event::Payload(generation,json!({"type":"options_changed","options":{"repeat_context":context}})));continue;}
                    PlayerEvent::SessionConnected { .. } => { active = true; }
                    PlayerEvent::SessionDisconnected { .. } => { playing = false; active = false; }
                    PlayerEvent::Unavailable { .. } => {
                        let _ = events.send(Event::Error("Spotify could not load this track. Try another song or reconnect playback.".into()));
                        playing = false;
                    }
                    _ => continue,
                }
                {
                    let mut t = lock(&tracking);
                    t.point.position_ms = position;
                    t.point.playing = playing;
                    t.sampled = Some(Instant::now());
                }
                if sampled && !loading && control().with_settings(|s| s.remember_positions && s.resume.record(&uri, &title, u64::from(position), u64::from(duration_ms))) {
                    control().mark_resume_dirty();
                }
                active_flag.store(active, Ordering::Relaxed);
                let actions = if item["uri"].as_str().is_some() {
                    json!({"play":{},"pause":{},"seek":{},"skip_next":{},"skip_prev":{},"add_to_queue":{},"shuffle":{},"set_repeat":{}})
                } else { json!({"play":{}}) };
                let _ = events.send(Event::Payload(generation,json!({"type":"playback_state","loading":loading,"play_request_id":request_id,"status":if playing {"playing"} else {"paused"},"item":item,"is_active":active,"position":{"position_ms":position,"speed":if playing {1} else {0}},"volume":volume,"options":{"shuffle":shuffled,"repeat_context":repeated},"available_actions":actions})));
            }
        }));
        engine.tasks.push(tokio::spawn(lookahead(
            engine.session.clone(),
            engine.tracking.clone(),
        )));
        Ok((engine, reusable))
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    pub fn has_ended(&self) -> bool {
        self.ended.load(Ordering::Relaxed)
    }

    /// Where playback stood, for restoring it on a new session.
    pub fn restore_point(&self) -> RestorePoint {
        let t = lock(&self.tracking);
        let mut point = t.point.clone();
        if point.playing {
            let elapsed = t.sampled.map(|s| s.elapsed().as_millis()).unwrap_or(0);
            point.position_ms = point
                .position_ms
                .saturating_add(elapsed.min(u128::from(u32::MAX)) as u32);
        }
        point
    }

    /// Load a restore point on this (new) session.
    pub fn restore(&self, point: &RestorePoint) -> Result<(), String> {
        if point.is_empty() {
            return Ok(());
        }
        let mut payload = json!({"command":"play","uri":point.track_uri,"position_ms":point.resume_position(),"playing":point.playing});
        if !point.context_uri.is_empty() && point.context_uri != point.track_uri {
            payload["context_uri"] = json!(point.context_uri);
        }
        self.command(&payload)
    }

    pub fn command(&self, payload: &Value) -> Result<(), String> {
        let spirc = self.spirc.as_ref().ok_or("Playback is not connected.")?;
        let result = match payload["command"].as_str().unwrap_or_default() {
            "play" | "jam_load" => {
                if let Some(uri) = payload["uri"].as_str() {
                    spirc.activate().map_err(|_| "Could not activate Grove.")?;
                    let single =
                        uri.starts_with("spotify:track:") || uri.starts_with("spotify:episode:");
                    // Episodes and long tracks continue where they stopped,
                    // unless the caller chose a position (jams, restores).
                    let saved = (payload["command"] == "play"
                        && single
                        && payload.get("position_ms").is_none())
                    .then(|| {
                        control().with_settings(|s| {
                            s.remember_positions.then(|| s.resume.get(uri)).flatten()
                        })
                    })
                    .flatten();
                    if let Some(position) = saved {
                        let _ = self.events.send(Event::Payload(
                            self.generation,
                            json!({"type":"resumed","uri":uri,"position_ms":position}),
                        ));
                    }
                    let options = LoadRequestOptions {
                        start_playing: payload["playing"].as_bool().unwrap_or(true),
                        seek_to: saved
                            .unwrap_or_else(|| payload["position_ms"].as_u64().unwrap_or(0))
                            .min(u32::MAX as u64) as u32,
                        playing_track: payload["context_uri"]
                            .as_str()
                            .map(|_| PlayingTrack::Uri(uri.to_owned())),
                        ..LoadRequestOptions::default()
                    };
                    let context =
                        payload["context_uri"]
                            .as_str()
                            .unwrap_or(if single { "" } else { uri });
                    {
                        let mut tracking = lock(&self.tracking);
                        tracking.point.context_uri = context.to_owned();
                        tracking.jam = payload["command"] == "jam_load";
                    }
                    let request = if let Some(context) = payload["context_uri"].as_str() {
                        LoadRequest::from_context_uri(context.to_owned(), options)
                    } else if single {
                        LoadRequest::from_tracks(vec![uri.to_owned()], options)
                    } else {
                        LoadRequest::from_context_uri(uri.to_owned(), options)
                    };
                    spirc.load(request)
                } else {
                    spirc.play()
                }
            }
            "pause" => spirc.pause(),
            "skip_next" => spirc.next(),
            "skip_prev" => spirc.prev(),
            "seek" => spirc.set_position_ms(
                payload["position_ms"]
                    .as_u64()
                    .unwrap_or(0)
                    .min(u32::MAX as u64) as u32,
            ),
            "set_volume" => spirc.set_volume(
                (payload["volume"].as_f64().unwrap_or(35.0).clamp(0.0, 100.0) / 100.0
                    * u16::MAX as f64)
                    .round() as u16,
            ),
            "activate" => spirc.activate(),
            "set_shuffle" => spirc.shuffle(payload["enabled"].as_bool().unwrap_or(false)),
            "set_repeat_context" => spirc.repeat(payload["enabled"].as_bool().unwrap_or(false)),
            _ => return Ok(()),
        };
        result.map_err(|_| "Spotify playback command failed. Reconnect playback and retry.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_settings_map_onto_librespot_player_config() {
        let mut settings = AudioSettings::default();
        let config = player_config(&settings);
        assert_eq!(config.bitrate, Bitrate::Bitrate160);
        assert!(!config.normalisation);
        settings.bitrate = 320;
        settings.normalise = true;
        settings.normalisation = NormalisationKind::Album;
        settings.pregain_db = -3;
        let config = player_config(&settings);
        assert_eq!(config.bitrate, Bitrate::Bitrate320);
        assert!(config.normalisation);
        assert_eq!(config.normalisation_type, NormalisationType::Album);
        assert_eq!(config.normalisation_pregain_db, -3.0);
        assert_eq!(
            config.position_update_interval,
            Some(Duration::from_secs(1))
        );
        settings.bitrate = 96;
        assert_eq!(player_config(&settings).bitrate, Bitrate::Bitrate96);
    }
}
