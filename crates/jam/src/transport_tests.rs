//! Real loopback tests: encrypted nearby rooms, version guard, rejoin and
//! host migration. Relay interop with Tide lives in `tests/tide_interop.rs`.
use super::*;
use tokio::io::AsyncWriteExt;

fn drain(mut rx: tokio::sync::mpsc::Receiver<Internal>) -> tokio::sync::mpsc::Receiver<Internal> {
    // Forward everything so the publish task never blocks on a full channel.
    let (tx, out) = tokio::sync::mpsc::channel(1024);
    tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            let _ = tx.try_send(message);
        }
    });
    out
}

async fn wait_state(
    rx: &mut tokio::sync::mpsc::Receiver<Internal>,
    epoch: u64,
    mut done: impl FnMut(&Snapshot) -> bool,
) -> Snapshot {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(Internal::State(e, state, _, _)) = rx.recv().await {
                if e == epoch && done(&state) {
                    return state;
                }
            }
        }
    })
    .await
    .expect("expected jam state")
}

fn token(invite: &str) -> (SocketAddr, String) {
    match invite::parse(invite).unwrap() {
        Invite::Nearby { address, token } => (address, token),
        _ => panic!("nearby invite expected"),
    }
}

async fn join(
    name: &str,
    invite: &str,
    epoch: u64,
    events: tokio::sync::mpsc::Sender<Internal>,
) -> Result<Guest, String> {
    let (address, secret) = token(invite);
    let (context, link) = guest(name, address, &secret, epoch, events.clone()).await?;
    Ok(start_guest(context, *link, epoch, events))
}

#[tokio::test]
async fn cross_service_frames_and_encrypted_queue_over_real_tcp() {
    let (events, received) = tokio::sync::mpsc::channel(64);
    let mut received = drain(received);
    let host = host("Tide", "127.0.0.1", 1, events.clone()).await.unwrap();
    let alice = join("Alice", &host.invite, 2, events.clone())
        .await
        .unwrap();
    let _bob = join("Bob", &host.invite, 3, events).await.unwrap();
    for (uri, playing, position) in [
        ("youtube:abcdefghijk", true, 12000),
        ("youtube:abcdefghijk", false, 42000),
        ("spotify:track:aaaaaaaaaaaaaaaaaaaaaa", true, 8000),
    ] {
        host.shared
            .change(|room| {
                room.set_playback(Some(Playback {
                    uri: uri.into(),
                    title: "Same recording".into(),
                    artist: "Artist".into(),
                    position_ms: position,
                    playing,
                    sampled_at_ms: jam::unix_ms(),
                    play_id: 1,
                    duration_ms: 200_000,
                    isrc: "USRC17607839".into(),
                    preparing: false,
                }));
                validate(&room.snapshot)
            })
            .await
            .unwrap();
        wait_state(&mut received, 2, |s| {
            s.playback
                .as_ref()
                .is_some_and(|p| p.uri == uri && p.position_ms == position && p.playing == playing)
        })
        .await;
    }
    alice
        .commands
        .send(ClientMessage::Add {
            operation: "one".into(),
            uri: "spotify:track:2JRo0gjbX4GrCqBYdRohoo".into(),
            title: "Shared song".into(),
        })
        .await
        .unwrap();
    let state = wait_state(&mut received, 3, |s| s.queue.len() == 1).await;
    assert_eq!(state.queue[0].member_id, alice.context.member);
    let serialized = serde_json::to_string(&host.shared.updates.borrow().clone()).unwrap();
    assert!(!serialized.contains(&host.shared.token));
    let mut bad = host.shared.updates.borrow().clone();
    bad.playback.as_mut().unwrap().uri = "file:///private.mp3".into();
    assert!(validate(&bad).is_err());
}

#[tokio::test]
async fn old_plaintext_guests_and_wrong_tokens_never_join() {
    let (events, received) = tokio::sync::mpsc::channel(64);
    let _received = drain(received);
    let host = host("Host", "127.0.0.1", 1, events.clone()).await.unwrap();
    let (address, secret) = token(&host.invite);
    // A protocol-2 app sends its token in plain JSON and gets an update message.
    let stream = TcpStream::connect(address).await.unwrap();
    let (read, mut write) = stream.into_split();
    write
        .write_all(
            format!(
                "{{\"kind\":\"join\",\"version\":2,\"token\":\"{secret}\",\"name\":\"Old\"}}\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut read = BufReader::new(read);
    let line = secure::read_line(&mut read, 4096).await.unwrap();
    assert!(String::from_utf8_lossy(&line).contains("Update Tide or Grove"));
    let wrong = invite::nearby(address, "this-is-an-invalid-room-token");
    let error = join("Intruder", &wrong, 2, events.clone())
        .await
        .err()
        .unwrap();
    assert!(error.contains("invalid or expired"), "{error}");
    assert_eq!(host.shared.room.lock().await.snapshot.members.len(), 1);
}

#[tokio::test]
async fn ending_the_room_closes_guests_without_migration() {
    let (events, received) = tokio::sync::mpsc::channel(64);
    let mut received = drain(received);
    let host = host("Host", "127.0.0.1", 1, events.clone()).await.unwrap();
    let _guest = join("Alice", &host.invite, 3, events).await.unwrap();
    wait_state(&mut received, 1, |s| s.members.len() == 2).await;
    host.close().await;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match received.recv().await {
                Some(Internal::Left(3, message)) => {
                    assert!(message.contains("ended"));
                    break;
                }
                Some(Internal::Lost(3)) => panic!("A deliberate end must not look like a crash"),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn clock_votes_buffering_and_stats_reach_the_host() {
    let (events, received) = tokio::sync::mpsc::channel(64);
    let mut received = drain(received);
    let host = host("Host", "127.0.0.1", 1, events.clone()).await.unwrap();
    let alice = join("Alice", &host.invite, 2, events.clone())
        .await
        .unwrap();
    // The writer pings immediately; a Pong yields a clock sample.
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            if let Some(Internal::Clock(2, offset, rtt, _, _)) = received.recv().await {
                assert!(offset.abs() < 1_000 && rtt < 1_000);
                break;
            }
        }
    })
    .await
    .unwrap();
    host.shared
        .change(|room| {
            room.set_playback(Some(Playback {
                uri: "spotify:track:2JRo0gjbX4GrCqBYdRohoo".into(),
                play_id: 5,
                preparing: true,
                sampled_at_ms: jam::unix_ms(),
                ..Playback::default()
            }));
            Ok(())
        })
        .await
        .unwrap();
    wait_state(&mut received, 2, |s| {
        s.playback.as_ref().is_some_and(|p| p.play_id == 5)
    })
    .await;
    alice
        .commands
        .send(ClientMessage::Buffered { play_id: 5 })
        .await
        .unwrap();
    let stats = guest_stats(&alice.context, Some(40));
    alice
        .commands
        .send(ClientMessage::Stats {
            stats,
            following: true,
        })
        .await
        .unwrap();
    let state = wait_state(&mut received, 1, |s| {
        s.members
            .get(1)
            .is_some_and(|m| m.ready_play == 5 && m.sync.is_some())
    })
    .await;
    assert_eq!(state.members[1].sync.as_ref().unwrap().drift_ms, Some(40));
    alice
        .commands
        .send(ClientMessage::VoteSkip { play_id: 5 })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(Internal::Skip(1, 5)) = received.recv().await {
                break;
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn dropped_guest_rejoins_with_the_same_seat() {
    let (events, received) = tokio::sync::mpsc::channel(64);
    let mut received = drain(received);
    let host = host("Host", "127.0.0.1", 1, events.clone()).await.unwrap();
    let alice = join("Alice", &host.invite, 2, events.clone())
        .await
        .unwrap();
    let member = alice.context.member.clone();
    let context = alice.context.clone();
    wait_state(&mut received, 1, |s| s.members.len() == 2).await;
    drop(alice); // Socket closes without a Leave: the host keeps the seat.
    wait_state(&mut received, 1, |s| {
        s.members.get(1).is_some_and(|m| !m.connected)
    })
    .await;
    tokio::spawn(reconnect(context, 2, events));
    let link = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(Internal::Reconnected(2, link)) = received.recv().await {
                return link;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(link.member, member);
    let state = host.shared.room.lock().await.snapshot.clone();
    assert_eq!(state.members.len(), 2);
    assert!(state.members[1].connected);
}

fn next_event(
    rx: &mpsc::Receiver<Event>,
    seconds: u64,
    mut want: impl FnMut(&Event) -> bool,
) -> Event {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "timed out waiting for a jam event");
        if let Ok(event) = rx.recv_timeout(left) {
            if let Event::Error(message) = &event {
                eprintln!("worker error: {message}");
            }
            if want(&event) {
                return event;
            }
        }
    }
}

#[test]
fn host_migration_hands_the_room_to_the_standby_guest() {
    // The host must keep serving between block_on calls.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let (events, received) = tokio::sync::mpsc::channel(64);
    let (host, mut received) = runtime.block_on(async {
        let host = host("Host", "127.0.0.1", 1, events.clone()).await.unwrap();
        (host, drain(received))
    });
    let (b_tx, b_rx, b_thread) = worker();
    let (c_tx, c_rx, c_thread) = worker();
    b_tx.send(Request::Join {
        name: "Bea".into(),
        invite: host.invite.clone(),
    })
    .unwrap();
    next_event(&b_rx, 5, |e| matches!(e, Event::Joined { .. }));
    c_tx.send(Request::Join {
        name: "Cal".into(),
        invite: host.invite.clone(),
    })
    .unwrap();
    next_event(&c_rx, 5, |e| matches!(e, Event::Joined { .. }));
    runtime.block_on(async {
        host.shared
            .change(|room| {
                let host = room.host_id().to_owned();
                room.add(
                    &host,
                    "q",
                    "spotify:track:2JRo0gjbX4GrCqBYdRohoo",
                    "Keep me",
                )
            })
            .await
            .unwrap();
        // Bea joined first with standby, so she is designated and reports a port.
        wait_state(&mut received, 1, |s| s.successor.is_some()).await;
    });
    // Both guests learn who the successor is before the host vanishes.
    next_event(
        &c_rx,
        5,
        |e| matches!(e, Event::Snapshot { state, .. } if state.successor.is_some()),
    );
    drop(host); // A crash: no Closed message.
    drop(runtime);
    let invite = match next_event(&b_rx, 15, |e| matches!(e, Event::Promoted { .. })) {
        Event::Promoted { invite, .. } => invite,
        _ => unreachable!(),
    };
    assert!(invite.starts_with("grove-jam://127.0.0.1:"));
    next_event(&c_rx, 20, |e| matches!(e, Event::Resumed(_)));
    let state = match next_event(
        &b_rx,
        10,
        |e| matches!(e, Event::Snapshot { state, .. } if state.members.len() == 2 && state.members[1].connected),
    ) {
        Event::Snapshot { state, .. } => state,
        _ => unreachable!(),
    };
    assert_eq!(state.members[0].name, "Bea");
    assert_eq!(state.members[1].name, "Cal");
    assert_eq!(
        state.queue.len(),
        1,
        "The shared queue survives the handover"
    );
    for tx in [b_tx, c_tx] {
        let _ = tx.send(Request::Shutdown);
    }
    b_thread.join().unwrap();
    c_thread.join().unwrap();
}

#[test]
fn snapshots_reject_public_successors_and_oversized_fields() {
    let mut room = Room::new("Host").unwrap();
    room.set_successor(Some(Successor {
        member_id: "x".into(),
        address: "8.8.8.8:4000".into(),
    }));
    assert!(validate(&room.snapshot).is_err());
    room.set_successor(Some(Successor {
        member_id: "x".into(),
        address: "192.168.0.4:4000".into(),
    }));
    assert!(validate(&room.snapshot).is_ok());
    room.snapshot.playback = Some(Playback {
        uri: "spotify:track:2JRo0gjbX4GrCqBYdRohoo".into(),
        isrc: "X".repeat(40),
        ..Playback::default()
    });
    assert!(validate(&room.snapshot).is_err());
}
