//! Shared jam callbacks and the 100 ms jam timer that hosts or follows a room.
//!
//! Hosts start shared songs together: load paused, wait for listeners to
//! acknowledge buffering (or a short timeout), then publish a start time a
//! little in the future and press play at that instant. Guests follow with the
//! NTP-style clock offset from the worker and a seek threshold that tightens
//! once the clock is trusted.
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
    cell::{Cell, RefCell},
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};
use tide_jam::{clock, invite};

pub type JamSender = tokio::sync::mpsc::UnboundedSender<jam_transport::Request>;

thread_local! {
    /// The play the guest currently hears, for skip votes.
    static PLAY: Cell<u64> = const { Cell::new(0) };
}

/// Fill in an invitation opened from a `grove://` link. Never joins by itself.
pub fn open_link(ui: &GroveWindow, link: &str) {
    match invite::unwrap_link(link) {
        Ok(inner) => {
            ui.set_page(4);
            ui.set_jam_join_invite(inner.into());
            ui.set_jam_message(
                "Invitation opened from a link. Check who sent it, then choose Join jam.".into(),
            );
        }
        Err(error) => ui.set_jam_message(error.into()),
    }
}

pub fn bind(ui: &GroveWindow, jam_tx: &JamSender, features: &Rc<RefCell<Features>>) {
    features.borrow_mut().set_jam_sender(jam_tx.clone());
    if let Some(link) = std::env::args()
        .skip(1)
        .find(|a| a.len() < 4096 && a.to_ascii_lowercase().starts_with("grove://"))
    {
        open_link(ui, &link);
    }
    let weak = ui.as_weak();
    let sender = jam_tx.clone();
    ui.on_jam_create(move || {
        let Some(ui) = weak.upgrade() else { return };
        if ui.get_local_active() {
            ui.set_jam_message("Return to Spotify before starting a jam.".into());
            return;
        }
        if ui.get_jam_busy() || ui.get_jam_connected() {
            return;
        }
        let name = ui.get_jam_name().to_string();
        if ui.get_jam_internet() {
            let endpoint = ui.get_jam_relay().trim().to_string();
            if endpoint.is_empty() {
                ui.set_jam_message("Enter your relay's address. Anyone can run grove-jam-relay for free; to join a Tide room, paste its invite instead.".into());
                return;
            }
            ui.set_jam_busy(true);
            ui.set_jam_message("Creating an internet jam…".into());
            let _ = sender.send(jam_transport::Request::HostRelay { name, endpoint });
        } else {
            ui.set_jam_busy(true);
            ui.set_jam_message("Starting a nearby jam…".into());
            let _ = sender.send(jam_transport::Request::Host {
                name,
                address: ui.get_jam_address().to_string(),
            });
        }
    });
    let weak = ui.as_weak();
    let sender = jam_tx.clone();
    ui.on_jam_join(move || {
        let Some(ui) = weak.upgrade() else { return };
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
    });
    let sender = jam_tx.clone();
    ui.on_jam_leave(move || {
        let _ = sender.send(jam_transport::Request::Leave);
    });
    let weak = ui.as_weak();
    let sender = jam_tx.clone();
    let feature_state = features.clone();
    ui.on_jam_add(move || {
        let Some(ui) = weak.upgrade() else { return };
        if ui.get_jam_tide_host() {
            ui.set_jam_message("Choose the next song in Tide on the host's device.".into());
            return;
        }
        let link = ui.get_jam_track_link().to_string();
        if let Ok(uri) = tide_jam::collection_uri(&link) {
            feature_state
                .borrow_mut()
                .resolve_jam_collection(&ui, uri, sender.clone());
            return;
        }
        match spotify::spotify_uri(&link) {
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
            _ => ui
                .set_jam_message("Add an individual Spotify track, album or playlist link.".into()),
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
    let weak = ui.as_weak();
    let sender = jam_tx.clone();
    ui.on_jam_vote_skip(move || {
        let Some(ui) = weak.upgrade() else { return };
        let play_id = PLAY.get();
        if play_id == 0 || !ui.get_jam_connected() || ui.get_jam_host() {
            return;
        }
        let _ = sender.send(jam_transport::Request::VoteSkip { play_id });
        ui.set_jam_message("Skip vote sent.".into());
    });
    let weak = ui.as_weak();
    let sender = jam_tx.clone();
    let feature_state = features.clone();
    ui.on_jam_options_changed(move |kind| {
        let Some(ui) = weak.upgrade() else { return };
        match kind.as_str() {
            "auto-advance" => {
                let _ = sender.send(jam_transport::Request::SetAutoAdvance(
                    ui.get_jam_auto_advance(),
                ));
            }
            "remember" => feature_state
                .borrow_mut()
                .set_remember_jam(&ui, ui.get_jam_remember_matches()),
            _ => {}
        }
    });
}

enum Phase {
    /// Host loads paused while listeners buffer and acknowledge.
    Loading,
    /// Everyone presses play at `at` (host clock `start_at`).
    Scheduled { start_at: u64, at: Instant },
}
struct HostStart {
    entry: String,
    uri: String,
    title: String,
    play_id: u64,
    began: Instant,
    phase: Phase,
}

struct Session {
    member_id: String,
    room_id: String,
    revision: u64,
    snapshot: Option<jam::Snapshot>,
    target: Option<jam_sync::Target>,
    follower: jam_sync::Follower,
    published: Instant,
    reported: Instant,
    was_following: bool,
    quiet: bool,
    stale_paused: bool,
    clock: Option<i64>,
    play_id: u64,
    host_uri: String,
    start: Option<HostStart>,
    near_end: u64,
    ended: u64,
    timer: Option<slint::Timer>,
}
impl Default for Session {
    fn default() -> Self {
        Self {
            member_id: String::new(),
            room_id: String::new(),
            revision: 0,
            snapshot: None,
            target: None,
            follower: jam_sync::Follower::default(),
            published: Instant::now() - Duration::from_secs(2),
            reported: Instant::now(),
            was_following: true,
            quiet: false,
            stale_paused: false,
            clock: None,
            play_id: 0,
            host_uri: String::new(),
            start: None,
            near_end: 0,
            ended: 0,
            timer: None,
        }
    }
}

fn member_rows(snapshot: &jam::Snapshot, me: &str) -> Vec<GroveJamTrack> {
    let play_id = snapshot.playback.as_ref().map_or(0, |p| p.play_id);
    let preparing = snapshot.playback.as_ref().is_some_and(|p| p.preparing);
    snapshot
        .members
        .iter()
        .map(|m| {
            let mut subtitle = if m.host {
                "Host".to_string()
            } else if !m.connected {
                "Reconnecting…".into()
            } else if !m.following {
                "Listening independently".into()
            } else if preparing && m.ready_play == play_id {
                "Ready".into()
            } else if let Some(sync) = &m.sync {
                let drift = sync
                    .drift_ms
                    .map(|d| format!(" · {d:+} ms from host"))
                    .unwrap_or_default();
                format!("{} ms round trip{drift}", sync.rtt_ms)
            } else {
                "Connected".into()
            };
            if snapshot
                .successor
                .as_ref()
                .is_some_and(|s| s.member_id == m.id)
            {
                subtitle.push_str(" · stand-in host");
            }
            GroveJamTrack {
                id: m.id.clone().into(),
                title: if m.id == me {
                    format!("{} (you)", m.name)
                } else {
                    m.name.clone()
                }
                .into(),
                subtitle: subtitle.into(),
                removable: false,
            }
        })
        .collect()
}

fn clear_ui(ui: &GroveWindow) {
    ui.set_jam_connected(false);
    ui.set_jam_host(false);
    ui.set_jam_following(true);
    ui.set_jam_invite(String::new().into());
    ui.set_jam_invite_link(String::new().into());
    ui.set_jam_members(String::new().into());
    ui.set_jam_member_rows(ModelRc::default());
    ui.set_jam_tracks(ModelRc::default());
    ui.set_jam_security(String::new().into());
    ui.set_jam_skip_status(String::new().into());
    ui.set_jam_can_skip(false);
    ui.set_jam_sync_status("Not in a jam".into());
    PLAY.set(0);
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
    let mut s = Session::default();
    jam_timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(100),
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let state = current.borrow();
            while let Ok(event) = jam_events.try_recv() {
                handle(
                    &ui,
                    &state,
                    event,
                    &mut s,
                    &sender,
                    &spotify_sender,
                    &jam_features,
                );
            }
            if !ui.get_jam_connected() {
                return;
            }
            if ui.get_jam_host() {
                host_tick(&ui, &state, &mut s, &sender, &spotify_sender, &jam_features);
            } else {
                guest_tick(&ui, &state, &mut s, &sender, &spotify_sender, &jam_features);
            }
        },
    );
    jam_timer
}

fn handle(
    ui: &GroveWindow,
    state: &State,
    event: jam_transport::Event,
    s: &mut Session,
    sender: &JamSender,
    spotify_sender: &RequestSender,
    features: &Rc<RefCell<Features>>,
) {
    use jam_transport::Event as E;
    match event {
        E::Joined {
            invite: code,
            member_id,
            host,
        } => {
            features.borrow_mut().reset_jam(ui);
            *s = Session {
                member_id,
                ..Session::default()
            };
            ui.set_jam_busy(false);
            ui.set_jam_connected(true);
            ui.set_jam_host(host);
            ui.set_jam_following(true);
            if host {
                ui.set_jam_invite_link(invite::link(&code).into());
                ui.set_jam_invite(code.into());
                let _ = sender.send(jam_transport::Request::SetAutoAdvance(
                    ui.get_jam_auto_advance(),
                ));
            } else {
                // Guests scope song choices to the invitation they joined.
                ui.set_jam_invite(ui.get_jam_join_invite());
                ui.set_jam_invite_link(String::new().into());
            }
            ui.set_jam_message(String::new().into());
        }
        E::Secure(label) => ui.set_jam_security(label.into()),
        E::Snapshot {
            state: room,
            host_sent_ms,
            transit_ms,
        } => {
            if !s.room_id.is_empty() && room.room_id != s.room_id {
                return;
            }
            if room.revision <= s.revision {
                return;
            }
            s.room_id = room.room_id.clone();
            s.revision = room.revision;
            let tide = room.origin == "tide-relay"
                || room
                    .playback
                    .as_ref()
                    .is_some_and(|p| p.uri.starts_with("youtube:"));
            ui.set_jam_tide_host(tide);
            ui.set_jam_members(
                room.members
                    .iter()
                    .map(|m| format!("{}{}", m.name, if m.host { " (host)" } else { "" }))
                    .collect::<Vec<_>>()
                    .join(" · ")
                    .into(),
            );
            ui.set_jam_member_rows(ModelRc::new(VecModel::from(member_rows(
                &room,
                &s.member_id,
            ))));
            let host = ui.get_jam_host();
            ui.set_jam_tracks(ModelRc::new(VecModel::from(
                room.queue
                    .iter()
                    .map(|entry| {
                        let from = if entry.group.is_empty() {
                            String::new()
                        } else {
                            format!(" · {}", entry.group)
                        };
                        GroveJamTrack {
                            id: entry.id.clone().into(),
                            title: entry.title.clone().into(),
                            subtitle: format!(
                                "Added by {}{from} · {}",
                                entry.member_name,
                                match entry.delivery {
                                    jam::Delivery::Waiting => "Waiting",
                                    jam::Delivery::Sending => "Starting for everyone…",
                                    jam::Delivery::Unconfirmed =>
                                        "Unconfirmed · check host playback",
                                }
                            )
                            .into(),
                            removable: !tide
                                && entry.delivery != jam::Delivery::Sending
                                && (host || entry.member_id == s.member_id),
                        }
                    })
                    .collect::<Vec<_>>(),
            )));
            let guests = room
                .members
                .iter()
                .filter(|m| !m.host && m.connected)
                .count();
            let votes = room.skip_votes.len();
            ui.set_jam_skip_status(
                if votes > 0 {
                    format!("{votes} of {guests} listeners want to skip")
                } else {
                    String::new()
                }
                .into(),
            );
            if !host {
                ui.set_jam_auto_advance(room.auto_advance);
            }
            let play_id = room.playback.as_ref().map_or(0, |p| p.play_id);
            PLAY.set(play_id);
            ui.set_jam_can_skip(!host && play_id != 0);
            let nearby = room.origin == "nearby" || room.origin.is_empty();
            s.target = room.playback.clone().map(|playback| jam_sync::Target {
                playback,
                host_sent_ms,
                received: Instant::now(),
                transit_ms,
                clock: s.clock.filter(|_| nearby).map(|o| (o, jam::unix_ms())),
            });
            if s.target.is_some() {
                s.quiet = false;
                s.stale_paused = false;
            }
            s.snapshot = Some(room);
        }
        E::Clock {
            offset_ms,
            confident,
            ..
        } => {
            s.clock = Some(offset_ms);
            s.follower.threshold_ms = if confident {
                clock::TIGHT_DRIFT_MS
            } else {
                clock::LOOSE_DRIFT_MS
            };
        }
        E::Play { id, uri } => {
            if !state.authenticated {
                let _ = sender.send(jam_transport::Request::Complete { id, success: false });
                ui.set_jam_message(
                    "Enable your Spotify playback before starting a shared track.".into(),
                );
                return;
            }
            s.play_id += 1;
            let title = s
                .snapshot
                .as_ref()
                .and_then(|r| r.queue.iter().find(|e| e.id == id))
                .map(|e| e.title.clone())
                .unwrap_or_default();
            send(
                spotify_sender,
                json!({"command":"jam_load","uri":uri,"position_ms":0,"playing":false}),
            );
            s.start = Some(HostStart {
                entry: id,
                uri,
                title,
                play_id: s.play_id,
                began: Instant::now(),
                phase: Phase::Loading,
            });
            s.published = Instant::now() - Duration::from_secs(2);
        }
        E::SkipVoted { play_id } => {
            if ui.get_jam_host() && play_id == s.play_id {
                ui.set_jam_message("Listeners voted to skip this song.".into());
                advance(s, sender, spotify_sender, true);
            }
        }
        E::Reconnecting(message) => {
            if !ui.get_jam_connected() {
                // Still joining (for example while a sleeping relay wakes up).
                ui.set_jam_message(message.into());
                return;
            }
            ui.set_jam_sync_status(message.into());
            if !ui.get_jam_host() && ui.get_jam_following() && state.playing && !s.stale_paused {
                send(spotify_sender, command("pause"));
                s.stale_paused = true;
            }
        }
        E::Resumed(message) => ui.set_jam_message(message.into()),
        E::Promoted {
            invite: code,
            member_id,
        } => {
            s.member_id = member_id;
            s.play_id = s
                .snapshot
                .as_ref()
                .and_then(|r| r.playback.as_ref())
                .map_or(0, |p| p.play_id);
            s.host_uri = state.track.uri.clone();
            s.revision = 0;
            ui.set_jam_host(true);
            ui.set_jam_invite_link(invite::link(&code).into());
            ui.set_jam_invite(code.into());
            ui.set_jam_can_skip(false);
            ui.set_jam_message(
                "The host left, so you're hosting now. Listeners reconnect automatically.".into(),
            );
            let _ = sender.send(jam_transport::Request::SetAutoAdvance(
                ui.get_jam_auto_advance(),
            ));
        }
        E::Left(message) => {
            features.borrow_mut().reset_jam(ui);
            ui.set_jam_busy(false);
            if ui.get_jam_connected()
                && !ui.get_jam_host()
                && ui.get_jam_following()
                && state.playing
            {
                send(spotify_sender, command("pause"));
            }
            clear_ui(ui);
            ui.set_jam_message(message.into());
            *s = Session::default();
        }
        E::Error(message) => {
            ui.set_jam_busy(false);
            ui.set_jam_message(message.into());
        }
    }
}

/// Start the next shared song, or skip in the host's own queue.
fn advance(s: &mut Session, sender: &JamSender, spotify_sender: &RequestSender, skip: bool) {
    let waiting = s.snapshot.as_ref().is_some_and(|r| {
        r.queue
            .first()
            .is_some_and(|e| e.delivery == jam::Delivery::Waiting)
    });
    if waiting && s.start.is_none() {
        let _ = sender.send(jam_transport::Request::Next);
    } else if skip {
        send(spotify_sender, command("skip_next"));
    }
}

fn host_tick(
    ui: &GroveWindow,
    state: &State,
    s: &mut Session,
    sender: &JamSender,
    spotify_sender: &RequestSender,
    features: &Rc<RefCell<Features>>,
) {
    let shareable =
        state.authenticated && state.active && state.track.uri.starts_with("spotify:track:");
    let position = state.position().max(0.0) as u64;
    let duration = state.track.duration.max(0.0) as u64;
    // Auto-advance when the host's song ends and the shared queue has more.
    if s.start.is_none() && shareable && state.track.uri == s.host_uri {
        if state.playing && duration > 5_000 && position + 2_500 >= duration {
            s.near_end = s.play_id;
        } else if s.near_end == s.play_id && !state.playing && s.ended != s.play_id {
            s.ended = s.play_id;
            if s.snapshot.as_ref().is_some_and(|r| r.auto_advance) {
                advance(s, sender, spotify_sender, false);
            }
        }
    }
    if s.start.is_none() && shareable && state.track.uri != s.host_uri {
        s.host_uri = state.track.uri.clone();
        s.play_id += 1;
    }
    enum Step {
        Wait,
        Schedule(u64),
        Done(String, String),
        Fail(String),
    }
    let mut step = Step::Wait;
    if let Some(start) = &s.start {
        let host_ready = state.active && state.track.uri == start.uri && !state.loading;
        match start.phase {
            Phase::Loading => {
                let members = s
                    .snapshot
                    .as_ref()
                    .map(|r| r.members.clone())
                    .unwrap_or_default();
                let waiting = jam_sync::unbuffered(&members, start.play_id);
                ui.set_jam_sync_status(
                    if waiting.is_empty() {
                        "Getting everyone ready…".to_string()
                    } else {
                        format!("Waiting for {} to buffer…", waiting.join(", "))
                    }
                    .into(),
                );
                if host_ready
                    && (waiting.is_empty() || start.began.elapsed() >= jam_sync::BUFFER_WAIT)
                {
                    step = Step::Schedule(jam_sync::start_lead_ms(&members));
                } else if start.began.elapsed() > Duration::from_secs(15) {
                    step = Step::Fail(start.entry.clone());
                }
            }
            Phase::Scheduled { at, .. } => {
                ui.set_jam_sync_status("Starting together…".into());
                if host_ready && state.playing {
                    step = Step::Done(start.entry.clone(), start.uri.clone());
                } else if Instant::now() > at + Duration::from_secs(10) {
                    step = Step::Fail(start.entry.clone());
                }
            }
        }
    } else {
        ui.set_jam_sync_status(
            if shareable {
                "Sharing host playback · listeners follow on their own devices"
            } else if state.authenticated && state.active && !state.track.uri.is_empty() {
                "Choose a Spotify track on the host · other audio types cannot be shared yet"
            } else {
                "Connect Spotify and play a track to start listening together"
            }
            .into(),
        );
    }
    let mut force = false;
    match step {
        Step::Wait => {}
        Step::Schedule(lead) => {
            if let Some(start) = &mut s.start {
                start.phase = Phase::Scheduled {
                    start_at: jam::unix_ms() + lead,
                    at: Instant::now() + Duration::from_millis(lead),
                };
            }
            force = true;
            let play = spotify_sender.clone();
            let weak = ui.as_weak();
            let timer = slint::Timer::default();
            timer.start(
                slint::TimerMode::SingleShot,
                Duration::from_millis(lead),
                move || {
                    if weak
                        .upgrade()
                        .is_some_and(|ui| ui.get_jam_connected() && ui.get_jam_host())
                    {
                        send(&play, command("play"));
                    }
                },
            );
            s.timer = Some(timer);
        }
        Step::Done(id, uri) => {
            let _ = sender.send(jam_transport::Request::Complete { id, success: true });
            s.host_uri = uri;
            s.start = None;
        }
        Step::Fail(id) => {
            let _ = sender.send(jam_transport::Request::Complete { id, success: false });
            ui.set_jam_message("Playback was not confirmed. Check the host player before removing the unconfirmed entry.".into());
            s.start = None;
        }
    }
    if !force && s.published.elapsed() < Duration::from_secs(1) {
        return;
    }
    s.published = Instant::now();
    let frame = match &s.start {
        Some(start) => {
            let (isrc, duration_ms) = features.borrow_mut().host_isrc(ui, &start.uri);
            let (playing, sampled_at_ms, preparing) = match start.phase {
                Phase::Loading => (false, jam::unix_ms(), true),
                Phase::Scheduled { start_at, .. } => (true, start_at, false),
            };
            Some(jam::Playback {
                uri: start.uri.clone(),
                title: start.title.clone(),
                artist: String::new(),
                position_ms: 0,
                playing,
                sampled_at_ms,
                play_id: start.play_id,
                duration_ms,
                isrc,
                preparing,
            })
        }
        None if shareable => {
            let (isrc, _) = features.borrow_mut().host_isrc(ui, &state.track.uri);
            Some(jam::Playback {
                uri: state.track.uri.clone(),
                title: state.track.title.clone(),
                artist: state.track.artist.clone(),
                position_ms: position,
                playing: state.playing,
                sampled_at_ms: jam::unix_ms(),
                play_id: s.play_id.max(1),
                duration_ms: duration,
                isrc,
                preparing: false,
            })
        }
        None => None,
    };
    let _ = sender.send(jam_transport::Request::Publish(frame));
}

fn guest_tick(
    ui: &GroveWindow,
    state: &State,
    s: &mut Session,
    sender: &JamSender,
    spotify_sender: &RequestSender,
    features: &Rc<RefCell<Features>>,
) {
    if s.was_following != ui.get_jam_following() {
        let threshold = s.follower.threshold_ms;
        s.follower = jam_sync::Follower::default();
        s.follower.threshold_ms = threshold;
        s.quiet = false;
        s.was_following = ui.get_jam_following();
    }
    let following = ui.get_jam_following();
    if !following {
        ui.set_jam_sync_status("Following stopped · your playback is independent".into());
        report(s, sender, None, false);
        return;
    }
    if !state.authenticated {
        ui.set_jam_sync_status("Enable Spotify playback in Account to listen".into());
        return;
    }
    let Some(remote) = &s.target else {
        ui.set_jam_matching(false);
        ui.set_jam_sync_status("Waiting for the host to play a track".into());
        if !s.quiet && state.playing {
            send(spotify_sender, command("pause"));
            s.quiet = true;
        }
        return;
    };
    let now = Instant::now();
    if remote.stale(now) {
        ui.set_jam_sync_status("Host updates delayed · paused until they return".into());
        if state.playing && !s.stale_paused {
            send(spotify_sender, command("pause"));
            s.stale_paused = true;
        }
        return;
    }
    let Some(uri) = features.borrow_mut().paired_host(ui, &remote.playback) else {
        ui.set_jam_sync_status("Choose the matching Spotify song to follow Tide".into());
        if !s.quiet && state.playing {
            send(spotify_sender, command("pause"));
            s.quiet = true;
        }
        return;
    };
    s.quiet = false;
    let mapped = jam_sync::Target {
        playback: jam::Playback {
            uri,
            ..remote.playback.clone()
        },
        host_sent_ms: remote.host_sent_ms,
        received: remote.received,
        transit_ms: remote.transit_ms,
        clock: remote.clock,
    };
    let local_uri = if state.active {
        state.track.uri.as_str()
    } else {
        ""
    };
    let local = |state: &State| jam_sync::Local {
        uri: local_uri,
        position_ms: state.position().max(0.0) as u64,
        playing: state.playing,
        ready: state.authenticated && !state.loading,
    };
    if local_uri != mapped.playback.uri && s.follower.exhausted(&mapped.playback.uri, now) {
        ui.set_jam_sync_status("Could not load the host track. Check Connection, then stop and restart following to retry.".into());
        return;
    }
    let drift = jam_sync::Follower::drift(&mapped, &local(state), now);
    let title = &mapped.playback.title;
    ui.set_jam_sync_status(
        if mapped.playback.preparing {
            "Host is getting everyone ready…".to_string()
        } else if let Some(wait) = mapped.starts_in(now) {
            format!("Starting together in {:.1} s", wait as f64 / 1000.0)
        } else if local_uri != mapped.playback.uri {
            format!("Loading host track · {title}")
        } else if state.playing != mapped.playback.playing {
            "Synchronizing play/pause with the host…".into()
        } else if let Some(d) = drift {
            format!("Following {title} · {} ms from host", d.abs())
        } else {
            format!(
                "Following {title} · {}",
                if mapped.playback.playing {
                    "Playing"
                } else {
                    "Paused"
                }
            )
        }
        .into(),
    );
    report(s, sender, drift, true);
    let Some(correction) = s.follower.plan(&mapped, local(state), now) else {
        return;
    };
    let payload = match correction {
        jam_sync::Correction::Load {
            uri,
            position_ms,
            playing,
        } => json!({"command":"jam_load","uri":uri,"position_ms":position_ms,"playing":playing}),
        jam_sync::Correction::Seek(position_ms) => {
            json!({"command":"seek","position_ms":position_ms})
        }
        jam_sync::Correction::Play => command("play"),
        jam_sync::Correction::Pause => command("pause"),
        jam_sync::Correction::Buffered { play_id } => {
            let _ = sender.send(jam_transport::Request::Buffered { play_id });
            return;
        }
        jam_sync::Correction::PlayAt { delay_ms } => {
            let play = spotify_sender.clone();
            let weak = ui.as_weak();
            let timer = slint::Timer::default();
            timer.start(
                slint::TimerMode::SingleShot,
                Duration::from_millis(delay_ms),
                move || {
                    if weak.upgrade().is_some_and(|ui| {
                        ui.get_jam_connected() && !ui.get_jam_host() && ui.get_jam_following()
                    }) {
                        send(&play, command("play"));
                    }
                },
            );
            s.timer = Some(timer);
            return;
        }
    };
    send(spotify_sender, payload);
}

/// Tell the host how this guest is doing, every two seconds.
fn report(s: &mut Session, sender: &JamSender, drift_ms: Option<i64>, following: bool) {
    if s.reported.elapsed() >= Duration::from_secs(2) {
        s.reported = Instant::now();
        let _ = sender.send(jam_transport::Request::Report {
            drift_ms,
            following,
        });
    }
}
