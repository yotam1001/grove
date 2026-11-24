//! Tide ↔ Grove relay interop over loopback, against Tide's own code.
//!
//! `tide-jam-lab` is Tide's jam crate pinned by commit in Cargo.toml (see
//! `tide_jam::version::TIDE_JAM_REV`). These tests run in CI with the normal
//! workspace tests, so a protocol change on either side fails the build.
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};
use tide_jam::{
    relay_server::{self, Mode},
    transport::{self, Event, Request},
    version,
};
use tide_jam_lab::{client::Client as TideClient, protocol as tide};

struct Relay {
    endpoint: String,
    stop: tokio::sync::watch::Sender<bool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Relay {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Run a relay backend (Tide's or Grove's) on its own runtime thread.
fn relay(tide_relay: bool) -> Relay {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            if tide_relay {
                tide_jam_lab::relay::serve(listener, receiver)
                    .await
                    .unwrap();
            } else {
                relay_server::serve(listener, receiver, Mode::Local)
                    .await
                    .unwrap();
            }
        });
    });
    Relay {
        endpoint,
        stop,
        thread: Some(thread),
    }
}

fn next(rx: &mpsc::Receiver<Event>, seconds: u64, mut want: impl FnMut(&Event) -> bool) -> Event {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "timed out waiting for a jam event");
        if let Ok(event) = rx.recv_timeout(left) {
            if want(&event) {
                return event;
            }
        }
    }
}

fn timeline(revision: u64, play_id: u64, position_ms: u64) -> tide::Timeline {
    tide::Timeline {
        revision,
        play_id,
        queue: vec![
            tide::VideoId::parse("abcdefghijk").unwrap(),
            tide::VideoId::parse("lmnopqrstuv").unwrap(),
        ],
        index: 0,
        position_ms,
        paused: false,
        loading: false,
    }
}

#[test]
fn grove_guest_follows_a_tide_host_on_tides_relay() {
    let relay = relay(true);
    let tide_host = TideClient::new(&relay.endpoint).unwrap();
    let room = tide_host.create().unwrap();
    tide_host
        .publish(&room.room, &room.host, &timeline(1, 7, 30_000))
        .unwrap();
    let code = tide::Invite {
        endpoint: relay.endpoint.clone(),
        room: room.room.clone(),
        invite: room.invite.clone(),
    }
    .to_code()
    .unwrap();
    let (tx, rx, thread) = transport::worker();
    // Opened from a grove:// link, exactly as the URL handler passes it.
    tx.send(Request::Join {
        name: "Grove".into(),
        invite: tide_jam::invite::link(&code),
    })
    .unwrap();
    next(&rx, 5, |e| matches!(e, Event::Joined { host: false, .. }));
    next(
        &rx,
        5,
        |e| matches!(e, Event::Secure(label) if label.contains("Tide protocol 1")),
    );
    let state = match next(
        &rx,
        5,
        |e| matches!(e, Event::Snapshot { state, .. } if state.playback.is_some()),
    ) {
        Event::Snapshot { state, .. } => state,
        _ => unreachable!(),
    };
    let playback = state.playback.unwrap();
    assert_eq!(playback.uri, "youtube:abcdefghijk");
    assert_eq!(playback.play_id, 7);
    assert!(playback.playing && playback.position_ms >= 30_000);
    assert_eq!(state.queue.len(), 1, "Upcoming Tide songs are listed");
    assert_eq!(state.origin, "tide-relay");
    assert_eq!(
        tide_host
            .host_view(&room.room, &room.host)
            .unwrap()
            .members
            .len(),
        1
    );
    // A Grove listener's skip request reaches the Tide host as Tide's Skip.
    tx.send(Request::VoteSkip { play_id: 7 }).unwrap();
    next(
        &rx,
        5,
        |e| matches!(e, Event::Error(m) if m.contains("Skip requested")),
    );
    let skip = tide_host
        .skip_request(&room.room, &room.host)
        .unwrap()
        .unwrap();
    assert_eq!(skip.play_id, 7);
    // Tide's loading flag becomes a buffering frame Grove guests wait on.
    std::thread::sleep(Duration::from_millis(150));
    let mut loading = timeline(2, 8, 0);
    loading.loading = true;
    loading.paused = true;
    tide_host.publish(&room.room, &room.host, &loading).unwrap();
    next(
        &rx,
        5,
        |e| matches!(e, Event::Snapshot { state, .. } if state.playback.as_ref().is_some_and(|p| p.play_id == 8 && p.preparing)),
    );
    tx.send(Request::Add {
        uri: "spotify:track:2JRo0gjbX4GrCqBYdRohoo".into(),
        title: "x".into(),
    })
    .unwrap();
    next(
        &rx,
        5,
        |e| matches!(e, Event::Error(m) if m.contains("choose songs in Tide")),
    );
    tx.send(Request::Leave).unwrap();
    next(&rx, 5, |e| matches!(e, Event::Left(_)));
    let deadline = Instant::now() + Duration::from_secs(5);
    while tide_host
        .host_view(&room.room, &room.host)
        .unwrap()
        .members
        .len()
        == 1
    {
        assert!(
            Instant::now() < deadline,
            "Grove's leave never reached the relay"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    tide_host.close(&room.room, &room.host).unwrap();
    let _ = tx.send(Request::Shutdown);
    thread.join().unwrap();
}

#[test]
fn tide_clients_work_unchanged_against_grove_relay() {
    let relay = relay(false);
    let host = TideClient::new(&relay.endpoint).unwrap();
    host.ready().unwrap();
    let room = host.create().unwrap();
    let invite = tide::Invite {
        endpoint: relay.endpoint.clone(),
        room: room.room.clone(),
        invite: room.invite.clone(),
    };
    let guest = TideClient::new(&relay.endpoint).unwrap();
    let joined = guest.join(&invite).unwrap();
    assert!(guest.listener_id(&room.room, &joined.member).unwrap() >= 1);
    host.publish(&room.room, &room.host, &timeline(1, 3, 1_000))
        .unwrap();
    let view = guest.poll(&room.room, &joined.member).unwrap();
    assert_eq!(view.timeline.unwrap().play_id, 3);
    assert_eq!(view.members, 1);
    guest
        .enqueue(
            &room.room,
            &joined.member,
            tide::VideoId::parse("zyxwvutsrqp").unwrap(),
        )
        .unwrap();
    let requests = host.requests(&room.room, &room.host).unwrap();
    assert_eq!(requests.len(), 1);
    host.acknowledge(&room.room, &room.host, requests[0].id)
        .unwrap();
    assert!(host.requests(&room.room, &room.host).unwrap().is_empty());
    guest
        .request_skip(
            &room.room,
            &joined.member,
            &tide::Skip {
                play_id: 3,
                video: tide::VideoId::parse("abcdefghijk").unwrap(),
            },
        )
        .unwrap();
    assert_eq!(
        host.skip_request(&room.room, &room.host)
            .unwrap()
            .unwrap()
            .play_id,
        3
    );
    // Guests can never publish; stale revisions are refused.
    assert!(guest
        .publish(&room.room, &joined.member, &timeline(9, 9, 0))
        .is_err());
    assert!(host
        .publish(&room.room, &room.host, &timeline(1, 3, 0))
        .is_err());
    host.set_admission(&room.room, &room.host, false).unwrap();
    assert!(!host.host_view(&room.room, &room.host).unwrap().open);
    assert!(TideClient::new(&relay.endpoint)
        .unwrap()
        .join(&invite)
        .is_err());
    let member = host.host_view(&room.room, &room.host).unwrap().members[0];
    let rotated = host.remove_member(&room.room, &room.host, member).unwrap();
    assert!(!rotated.matches(&room.invite));
    assert!(guest.poll(&room.room, &joined.member).is_err());
    host.close(&room.room, &room.host).unwrap();
    assert!(host.host_view(&room.room, &room.host).is_err());
}

#[test]
fn grove_hosts_cross_service_rooms_on_grove_relay() {
    let relay = relay(false);
    let (host_tx, host_rx, host_thread) = transport::worker();
    host_tx
        .send(Request::HostRelay {
            name: "Grove host".into(),
            endpoint: relay.endpoint.clone(),
        })
        .unwrap();
    let code = match next(&host_rx, 5, |e| {
        matches!(e, Event::Joined { host: true, .. })
    }) {
        Event::Joined { invite, .. } => invite,
        _ => unreachable!(),
    };
    assert!(code.starts_with("grove-jam:v2."));
    host_tx
        .send(Request::Publish(Some(tide_jam::jam::Playback {
            uri: "spotify:track:2JRo0gjbX4GrCqBYdRohoo".into(),
            title: "Forest Floor".into(),
            artist: "Artist".into(),
            position_ms: 5_000,
            playing: true,
            sampled_at_ms: tide_jam::jam::unix_ms(),
            play_id: 1,
            duration_ms: 200_000,
            isrc: "USRC17607839".into(),
            preparing: false,
        })))
        .unwrap();
    let (guest_tx, guest_rx, guest_thread) = transport::worker();
    guest_tx
        .send(Request::Join {
            name: "Listener".into(),
            invite: code.clone(),
        })
        .unwrap();
    next(
        &guest_rx,
        5,
        |e| matches!(e, Event::Secure(label) if label.contains("protocol 2")),
    );
    let state = match next(
        &guest_rx,
        6,
        |e| matches!(e, Event::Snapshot { state, .. } if state.playback.is_some()),
    ) {
        Event::Snapshot { state, .. } => state,
        _ => unreachable!(),
    };
    let playback = state.playback.unwrap();
    assert_eq!(playback.isrc, "USRC17607839");
    assert_eq!(playback.duration_ms, 200_000);
    assert!(playback.position_ms >= 5_000);
    // Tide's v1 client cannot join a cross-service room by mistake.
    let (invite, protocol) = tide_jam::relay::Invite::from_code(&code).unwrap();
    assert_eq!(protocol, version::RELAY_CROSS);
    let tide_guest = TideClient::new(&relay.endpoint).unwrap();
    assert!(tide_guest
        .join(&tide::Invite {
            endpoint: invite.endpoint.clone(),
            room: invite.room.clone(),
            invite: tide::Secret::parse(invite.invite.expose()).unwrap(),
        })
        .is_err());
    guest_tx
        .send(Request::Add {
            uri: "spotify:track:3JRo0gjbX4GrCqBYdRohoo".into(),
            title: "Guest pick".into(),
        })
        .unwrap();
    next(
        &guest_rx,
        5,
        |e| matches!(e, Event::Error(m) if m.contains("sent to the shared queue")),
    );
    next(
        &host_rx,
        6,
        |e| matches!(e, Event::Snapshot { state, .. } if state.queue.iter().any(|q| q.title == "Guest pick")),
    );
    next(
        &host_rx,
        6,
        |e| matches!(e, Event::Snapshot { state, .. } if state.members.len() == 2),
    );
    guest_tx.send(Request::VoteSkip { play_id: 1 }).unwrap();
    next(&host_rx, 6, |e| {
        matches!(e, Event::SkipVoted { play_id: 1 })
    });
    host_tx.send(Request::Leave).unwrap();
    next(
        &guest_rx,
        15,
        |e| matches!(e, Event::Left(m) if m.contains("ended") || m.contains("closed")),
    );
    for tx in [host_tx, guest_tx] {
        let _ = tx.send(Request::Shutdown);
    }
    host_thread.join().unwrap();
    guest_thread.join().unwrap();
}

#[test]
fn hosting_on_a_tide_only_relay_explains_the_version_gap() {
    let relay = relay(true);
    let (tx, rx, thread) = transport::worker();
    tx.send(Request::HostRelay {
        name: "Grove".into(),
        endpoint: relay.endpoint.clone(),
    })
    .unwrap();
    match next(&rx, 5, |e| matches!(e, Event::Error(_))) {
        Event::Error(message) => {
            assert_eq!(message, version::relay_mismatch(version::RELAY_CROSS));
        }
        _ => unreachable!(),
    }
    // The worker is free again afterwards.
    tx.send(Request::Host {
        name: "Grove".into(),
        address: "127.0.0.1".into(),
    })
    .unwrap();
    next(&rx, 5, |e| matches!(e, Event::Joined { host: true, .. }));
    let _ = tx.send(Request::Shutdown);
    thread.join().unwrap();
}

#[test]
fn pinned_tide_revision_is_recorded() {
    assert_eq!(version::TIDE_JAM_REV.len(), 40);
    let manifest = include_str!("../Cargo.toml");
    assert!(
        manifest.contains(version::TIDE_JAM_REV),
        "Cargo.toml's tide-jam-lab rev and TIDE_JAM_REV must match"
    );
}
