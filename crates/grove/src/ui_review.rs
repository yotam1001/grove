//! Opt-in native callback review. Uses loopback rooms, never Spotify credentials.
//!
//! Stages cover the original host/join/queue/permission checks, then the jam
//! protocol work: the versioned encrypted handshake (old plaintext apps and
//! wrong tokens are refused), an internet room through an in-process loopback
//! relay (`grove-jam-relay` code), a scheduled start after a buffering ack, and
//! an automatic rejoin after the connection is cut by a loopback proxy. Playback
//! state for the scheduled start is synthetic; no Spotify engine is started.
use crate::{jam_transport as jam, state::State, GroveWindow};
use slint::{ComponentHandle, Model};
use std::{
    cell::RefCell,
    net::SocketAddr,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

const TRACK: &str = "spotify:track:2JRo0gjbX4GrCqBYdRohoo";

/// A loopback TCP proxy whose live connections can be cut on demand.
struct Proxy {
    address: SocketAddr,
    cut: tokio::sync::watch::Sender<u64>,
    stop: tokio::sync::watch::Sender<bool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Proxy {
    fn start(target: SocketAddr) -> std::io::Result<Self> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let (cut, cuts) = tokio::sync::watch::channel(0u64);
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        let thread = std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async move {
                let Ok(listener) = tokio::net::TcpListener::from_std(listener) else {
                    return;
                };
                loop {
                    tokio::select! {
                        _ = stopped.changed() => break,
                        accepted = listener.accept() => {
                            let Ok((mut inbound, _)) = accepted else { break; };
                            let mut cuts = cuts.clone();
                            cuts.borrow_and_update();
                            tokio::spawn(async move {
                                let Ok(mut outbound) = tokio::net::TcpStream::connect(target).await else { return; };
                                tokio::select! {
                                    _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound) => {}
                                    _ = cuts.changed() => {}
                                }
                            });
                        }
                    }
                }
            });
        });
        Ok(Self {
            address,
            cut,
            stop,
            thread: Some(thread),
        })
    }
    fn cut(&self) {
        self.cut.send_modify(|n| *n += 1);
    }
}
impl Drop for Proxy {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Grove's relay backend on loopback, standing in for a self-hosted relay.
struct Relay {
    endpoint: String,
    stop: tokio::sync::watch::Sender<bool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Relay {
    fn start() -> std::io::Result<Self> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let endpoint = format!("http://{}", listener.local_addr()?);
        let (stop, receiver) = tokio::sync::watch::channel(false);
        let thread = std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async move {
                if let Ok(listener) = tokio::net::TcpListener::from_std(listener) {
                    let _ = tide_jam::relay_server::serve(
                        listener,
                        receiver,
                        tide_jam::relay_server::Mode::Local,
                    )
                    .await;
                }
            });
        });
        Ok(Self {
            endpoint,
            stop,
            thread: Some(thread),
        })
    }
}
impl Drop for Relay {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Old (protocol 2) and wrong-token clients against a nearby host.
fn handshake_probes(invite: &str) -> (String, bool) {
    use tokio::io::AsyncWriteExt;
    let tide_jam::invite::Invite::Nearby { address, token } =
        tide_jam::invite::parse(invite).expect("nearby invite")
    else {
        panic!("nearby invite expected");
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("probe runtime");
    runtime.block_on(async move {
        let old = async {
            let stream = tokio::net::TcpStream::connect(address).await.ok()?;
            let (read, mut write) = stream.into_split();
            let line = format!(
                "{{\"kind\":\"join\",\"version\":2,\"token\":\"{token}\",\"name\":\"Old app\"}}\n"
            );
            write.write_all(line.as_bytes()).await.ok()?;
            let mut read = tokio::io::BufReader::new(read);
            let reply = tide_jam::secure::read_line(&mut read, 4096).await.ok()?;
            Some(String::from_utf8_lossy(&reply).into_owned())
        };
        let old = tokio::time::timeout(Duration::from_secs(3), old)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        let wrong = async {
            let stream = tokio::net::TcpStream::connect(address).await.ok()?;
            let (read, mut write) = stream.into_split();
            let mut read = tokio::io::BufReader::new(read);
            let (mut sealer, mut opener, _) = tide_jam::secure::client(
                &mut read,
                &mut write,
                "a-wrong-token-for-this-room",
                "grove",
            )
            .await
            .ok()?;
            tide_jam::secure::send(
                &mut write,
                &mut sealer,
                &serde_json::json!({"kind":"join","name":"Intruder"}),
            )
            .await
            .ok()?;
            // The host cannot open our frame, so it closes without a welcome.
            Some(
                tide_jam::secure::recv::<_, serde_json::Value>(&mut read, &mut opener)
                    .await
                    .is_err(),
            )
        };
        let refused = tokio::time::timeout(Duration::from_secs(6), wrong)
            .await
            .ok()
            .flatten()
            .unwrap_or(false);
        (old, refused)
    })
}

pub struct Review {
    timer: slint::Timer,
    peer: tokio::sync::mpsc::UnboundedSender<jam::Request>,
    task: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Review {
    fn drop(&mut self) {
        self.timer.stop();
        let _ = self.peer.send(jam::Request::Shutdown);
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}

/// With the app state, the scheduled-start stage drives synthetic playback.
pub fn start_with(
    ui: &GroveWindow,
    state: Option<Rc<RefCell<State>>>,
    output: PathBuf,
) -> Result<Review, std::io::Error> {
    std::fs::create_dir_all(&output)?;
    let (peer, events, task) = jam::worker();
    let sender = peer.clone();
    let weak = ui.as_weak();
    let timer = slint::Timer::default();
    let mut stage = 0;
    let mut since = Instant::now();
    let mut peer_invite = String::new();
    let mut peer_joined = false;
    let mut peer_left = false;
    let mut peer_ready = false;
    let mut peer_members = 0usize;
    let mut relay: Option<Relay> = None;
    let mut proxy: Option<Proxy> = None;
    let state = state.unwrap_or_default();
    ui.set_page(4);
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(150), move || {
        let Some(ui) = weak.upgrade() else { return; };
        assert!(since.elapsed() < Duration::from_secs(15), "Native review timed out at stage {stage}");
        while let Ok(event) = events.try_recv() {
            match event {
                jam::Event::Joined {invite, ..} => { peer_invite = invite; peer_joined = true; peer_left = false; },
                jam::Event::Left(_) => { peer_left = true; peer_joined = false; },
                jam::Event::Snapshot { state: room, .. } => {
                    peer_members = room.members.len();
                    peer_ready = room.members.iter().any(|m| !m.host && m.ready_play == 3);
                },
                jam::Event::Error(error) if stage < 16 => panic!("Native review peer: {error}"),
                _ => {},
            }
        }
        let advanced = match stage {
            0 => {
                ui.set_jam_name("Native review host".into());
                ui.set_jam_address("8.8.8.8".into());
                ui.invoke_jam_create();
                assert!(ui.get_jam_busy());
                true
            },
            1 if !ui.get_jam_busy() => {
                assert!(!ui.get_jam_connected());
                assert!(ui.get_jam_message().contains("private LAN"));
                capture(&ui, &output, "invalid-host");
                ui.set_jam_address("127.0.0.1".into());
                ui.invoke_jam_create();
                ui.invoke_jam_create(); // Repeated activation is ignored while busy.
                true
            },
            2 if ui.get_jam_connected() => {
                assert!(ui.get_jam_host());
                sender.send(jam::Request::Join {name: "Review guest".into(), invite: ui.get_jam_invite().to_string()}).unwrap();
                ui.set_jam_track_link("https://example.com/not-spotify".into());
                ui.invoke_jam_add();
                assert!(ui.get_jam_message().contains("individual Spotify"));
                true
            },
            3 if peer_joined && ui.get_jam_members().contains("Review guest") => {
                ui.set_jam_track_link(TRACK.into());
                ui.set_jam_track_title("Host contribution".into());
                ui.invoke_jam_add();
                true
            },
            4 if ui.get_jam_tracks().row_count() == 1 => {
                assert!(ui.get_jam_message().is_empty(), "A successful action retained an old validation error");
                assert!(ui.get_jam_tracks().row_data(0).unwrap().removable);
                sender.send(jam::Request::Add {uri: "spotify:track:3JRo0gjbX4GrCqBYdRohoo".into(), title: "Guest contribution".into()}).unwrap();
                true
            },
            5 if ui.get_jam_tracks().row_count() == 2 => {
                capture(&ui, &output, "host-with-guest");
                ui.invoke_jam_remove(ui.get_jam_tracks().row_data(0).unwrap().id);
                true
            },
            6 if ui.get_jam_tracks().row_count() == 1 => {
                assert_eq!(ui.get_jam_tracks().row_data(0).unwrap().title, "Guest contribution");
                sender.send(jam::Request::Leave).unwrap();
                true
            },
            7 if !ui.get_jam_members().contains("Review guest") => {
                assert_eq!(ui.get_jam_tracks().row_count(), 1, "Leaving must preserve contributions");
                ui.invoke_jam_leave();
                true
            },
            8 if !ui.get_jam_connected() => {
                assert_eq!(ui.get_jam_tracks().row_count(), 0);
                assert!(ui.get_jam_invite().is_empty());
                peer_joined = false;
                sender.send(jam::Request::Host {name: "Review remote host".into(), address: "127.0.0.1".into()}).unwrap();
                true
            },
            9 if peer_joined && !peer_invite.is_empty() => {
                ui.set_jam_name("Native review listener".into());
                ui.set_jam_join_invite(peer_invite.clone().into());
                ui.invoke_jam_join();
                true
            },
            10 if ui.get_jam_connected() => {
                assert!(!ui.get_jam_host());
                assert!(ui.get_jam_following());
                ui.invoke_play_link();
                assert!(ui.get_message().contains("Stop following"));
                ui.set_message("".into());
                sender.send(jam::Request::Add {uri: TRACK.into(), title: "Remote host contribution".into()}).unwrap();
                true
            },
            11 if ui.get_jam_tracks().row_count() == 1 => {
                let row = ui.get_jam_tracks().row_data(0).unwrap();
                assert!(!row.removable);
                ui.invoke_jam_remove(row.id); // Server must also enforce disabled actions.
                true
            },
            12 if ui.get_jam_message().contains("only their own") => {
                ui.window().set_size(slint::LogicalSize::new(700.0,540.0));
                true
            },
            13 => {
                capture(&ui, &output, "guest-permission-error");
                sender.send(jam::Request::Leave).unwrap();
                true
            },
            14 if !ui.get_jam_connected() => {
                assert_eq!(ui.get_jam_tracks().row_count(), 0);
                assert!(ui.get_jam_sync_status().contains("Not in a jam"));
                true
            },
            15 => {
                capture(&ui, &output, "host-disconnected");
                eprintln!("PASS: native callbacks, validation, busy guard, host/join, shared queue, ownership, leave and host disconnect");
                ui.window().set_size(slint::LogicalSize::new(1200.0,800.0));
                ui.set_jam_message("".into());
                ui.set_jam_address("127.0.0.1".into());
                ui.invoke_jam_create();
                true
            },
            // Handshake: versioned hello, encryption from the invite token.
            16 if ui.get_jam_connected() && ui.get_jam_host() => {
                let label = ui.get_jam_security().to_string();
                assert!(label.contains("Encrypted") && label.contains("protocol 3"), "{label}");
                let (old, refused) = handshake_probes(&ui.get_jam_invite());
                assert!(old.contains("Update Tide or Grove"), "Old apps must be told to update: {old}");
                assert!(refused, "A wrong invite token must never receive room state");
                true
            },
            17 if ui.get_jam_member_rows().row_count() == 1 => {
                assert!(!ui.get_jam_members().contains("Intruder") && !ui.get_jam_members().contains("Old app"));
                capture(&ui, &output, "handshake-encrypted");
                eprintln!("PASS: version handshake and invite-keyed encryption");
                ui.invoke_jam_leave();
                true
            },
            // Relay path: an internet room through a loopback grove-jam-relay.
            18 if !ui.get_jam_connected() => {
                let started = Relay::start().expect("loopback relay");
                ui.set_jam_internet(true);
                ui.set_jam_relay("".into());
                ui.invoke_jam_create();
                assert!(ui.get_jam_message().contains("relay"), "An empty relay address must be explained");
                ui.set_jam_relay(started.endpoint.clone().into());
                relay.replace(started);
                ui.invoke_jam_create();
                true
            },
            19 if ui.get_jam_connected() && ui.get_jam_invite().starts_with("grove-jam:v2.") => {
                assert!(ui.get_jam_security().contains("protocol 2"));
                assert!(ui.get_jam_invite_link().starts_with("grove://join?invite="));
                peer_joined = false;
                sender.send(jam::Request::Join {name: "Relay listener".into(), invite: ui.get_jam_invite_link().to_string()}).unwrap();
                true
            },
            20 if peer_joined && ui.get_jam_member_rows().row_count() == 2 => {
                capture(&ui, &output, "relay-host");
                ui.invoke_jam_leave();
                true
            },
            21 if !ui.get_jam_connected() && peer_left => {
                eprintln!("PASS: relay path (protocol 2 room, grove:// invite link, listener sees the end)");
                drop(relay.take());
                ui.set_jam_internet(false);
                peer_joined = false;
                peer_invite.clear();
                sender.send(jam::Request::Host {name: "Review remote host".into(), address: "127.0.0.1".into()}).unwrap();
                true
            },
            // Scheduled start: buffer, acknowledge, then start together.
            22 if peer_joined && !peer_invite.is_empty() => {
                let tide_jam::invite::Invite::Nearby { address, token } = tide_jam::invite::parse(&peer_invite).unwrap() else { unreachable!() };
                let cut = Proxy::start(address).expect("loopback proxy");
                ui.set_jam_join_invite(tide_jam::invite::nearby(cut.address, &token).into());
                proxy.replace(cut);
                {
                    let mut s = state.borrow_mut();
                    s.authenticated = true;
                }
                ui.invoke_jam_join();
                true
            },
            23 if ui.get_jam_connected() && !ui.get_jam_host() && peer_members == 2 => {
                sender.send(jam::Request::Publish(Some(crate::jam::Playback {
                    uri: TRACK.into(), title: "Scheduled song".into(), artist: "Review artist".into(),
                    play_id: 3, preparing: true, sampled_at_ms: crate::jam::unix_ms(), duration_ms: 200_000,
                    ..Default::default()
                }))).unwrap();
                true
            },
            24 if ui.get_jam_sync_status().contains("getting everyone ready") => {
                // The follower asked the engine to load paused; simulate the loaded song.
                let mut s = state.borrow_mut();
                s.active = true;
                s.track.uri = TRACK.into();
                s.track.title = "Scheduled song".into();
                s.track.duration = 200_000.0;
                true
            },
            25 if peer_ready => {
                sender.send(jam::Request::Publish(Some(crate::jam::Playback {
                    uri: TRACK.into(), title: "Scheduled song".into(), artist: "Review artist".into(),
                    play_id: 3, playing: true, sampled_at_ms: crate::jam::unix_ms() + 4_000, duration_ms: 200_000,
                    ..Default::default()
                }))).unwrap();
                true
            },
            26 if ui.get_jam_sync_status().contains("Starting together in") => {
                assert!(ui.get_jam_can_skip());
                capture(&ui, &output, "scheduled-start");
                eprintln!("PASS: buffering acknowledgement and scheduled start");
                // Rejoin: cut the live connection; the guest reconnects with its seat.
                proxy.as_ref().unwrap().cut();
                true
            },
            27 if ui.get_jam_message().contains("Back in the jam") && peer_members == 2 => {
                assert!(ui.get_jam_connected() && !ui.get_jam_host());
                capture(&ui, &output, "rejoined");
                *state.borrow_mut() = State::default();
                ui.invoke_jam_leave();
                true
            },
            28 if !ui.get_jam_connected() => {
                sender.send(jam::Request::Leave).unwrap();
                drop(proxy.take());
                eprintln!("PASS: automatic rejoin after a dropped connection");
                slint::quit_event_loop().unwrap();
                true
            },
            _ => false,
        };
        if advanced { eprintln!("Native review stage {stage} passed"); stage += 1; since = Instant::now(); }
    });
    Ok(Review {
        timer,
        peer,
        task: Some(task),
    })
}

fn capture(ui: &GroveWindow, output: &std::path::Path, name: &str) {
    let pixels = ui.window().take_snapshot().expect("Native snapshot");
    image::RgbaImage::from_raw(pixels.width(), pixels.height(), pixels.as_bytes().to_vec())
        .unwrap()
        .save(output.join(format!("{name}.png")))
        .unwrap();
}
