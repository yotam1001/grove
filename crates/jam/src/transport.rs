//! The jam worker: hosts or follows a room, nearby or through a relay.
//!
//! Nearby rooms run over TCP on a private address with the encrypted channel in
//! [`crate::secure`] (protocol 3). Relay rooms use Tide's HTTP relay: protocol 1
//! to follow Tide hosts, protocol 2 for Grove-hosted cross-service rooms. No
//! discovery, accounts or audio cross either path.
//!
//! Resilience: a guest whose connection drops keeps its seat for 30 s and
//! reconnects with backoff using its member ID. The host designates a standby
//! successor; if the host vanishes (rather than ending the room) the successor
//! takes over from the last snapshot and everyone reconnects to it with the same
//! invite token. Relay guests retry bounded polls and rejoin through the invite.
use crate::{
    clock::Estimator,
    invite::{self, Invite},
    jam::{self, Item, Member, Playback, Room, Snapshot, Successor, SyncStats},
    relay::{self, CrossEnqueue, CrossRequest, CrossSkip, CrossTimeline, Timeline, TrackRef},
    secure::{self, Accepted, Opener, Sealer},
    version::{RELAY_CROSS, RELAY_TIDE},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};
use tokio::{
    io::BufReader,
    net::{
        tcp::{OwnedReadHalf, OwnedWriteHalf},
        TcpListener, TcpStream,
    },
    sync::{watch, Mutex},
    task::{JoinHandle, JoinSet},
};

/// Ask the OS which local address it would route from; sends no datagram.
pub fn suggested_address() -> String {
    let address = (|| {
        let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
        socket.connect("192.0.2.1:80").ok()?;
        match socket.local_addr().ok()?.ip() {
            IpAddr::V4(ip) if ip.is_private() => Some(ip.to_string()),
            _ => None,
        }
    })();
    address.unwrap_or_else(|| "127.0.0.1".into())
}

pub enum Request {
    /// Nearby room on a loopback or private address.
    Host {
        name: String,
        address: String,
    },
    /// Internet room on a protocol-2 relay (HTTPS, or loopback for labs).
    HostRelay {
        name: String,
        endpoint: String,
    },
    /// Any invitation: nearby, Tide relay, cross-service relay or short code.
    Join {
        name: String,
        invite: String,
    },
    Add {
        uri: String,
        title: String,
    },
    AddItems {
        group: String,
        items: Vec<Item>,
    },
    Remove(String),
    Next,
    Complete {
        id: String,
        success: bool,
    },
    Publish(Option<Playback>),
    VoteSkip {
        play_id: u64,
    },
    Buffered {
        play_id: u64,
    },
    Report {
        drift_ms: Option<i64>,
        following: bool,
    },
    SetAutoAdvance(bool),
    Leave,
    Shutdown,
}
// Events and internal messages travel through channels one at a time; boxing
// the room snapshot would only add an allocation per update.
#[allow(clippy::large_enum_variant)]
pub enum Event {
    Joined {
        invite: String,
        member_id: String,
        host: bool,
    },
    Snapshot {
        state: Snapshot,
        host_sent_ms: u64,
        transit_ms: u64,
    },
    /// Guest clock estimate (host minus local) from the latest exchange.
    Clock {
        offset_ms: i64,
        rtt_ms: u64,
        jitter_ms: u64,
        confident: bool,
    },
    /// Connection security and protocol, for the status line.
    Secure(String),
    Play {
        id: String,
        uri: String,
    },
    /// Host only: guests voted (or a relay listener asked) to skip this play.
    SkipVoted {
        play_id: u64,
    },
    Reconnecting(String),
    Resumed(String),
    /// This guest took over as host after the host disappeared.
    Promoted {
        invite: String,
        member_id: String,
    },
    Left(String),
    Error(String),
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ClientMessage {
    Join {
        name: String,
        #[serde(default)]
        resume: Option<String>,
        #[serde(default)]
        standby: bool,
    },
    Add {
        operation: String,
        uri: String,
        title: String,
    },
    AddItems {
        operation: String,
        group: String,
        items: Vec<Item>,
    },
    Remove {
        id: String,
    },
    Ping {
        t0: u64,
    },
    Buffered {
        play_id: u64,
    },
    Stats {
        stats: SyncStats,
        following: bool,
    },
    VoteSkip {
        play_id: u64,
    },
    Standby {
        port: u16,
    },
    Leave,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ServerMessage {
    Welcome {
        member_id: String,
        state: Snapshot,
        host_sent_ms: u64,
    },
    State {
        state: Snapshot,
        host_sent_ms: u64,
    },
    Pong {
        t0: u64,
        t1: u64,
        t2: u64,
    },
    Error {
        message: String,
    },
    /// Ask this guest to stand by as successor (bind a listener).
    Designate,
    Closed {
        message: String,
    },
}

struct Task(Option<JoinHandle<()>>);
impl Task {
    fn new(handle: JoinHandle<()>) -> Self {
        Self(Some(handle))
    }
    /// Let the task finish its bounded cleanup instead of aborting it.
    fn detach(mut self) {
        self.0.take();
    }
}
impl Drop for Task {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

type PeerSender = tokio::sync::mpsc::Sender<ServerMessage>;
struct Shared {
    room: Mutex<Room>,
    updates: watch::Sender<Snapshot>,
    token: String,
    peers: Mutex<HashMap<String, (u64, PeerSender)>>,
    skipped: AtomicU64,
    events: tokio::sync::mpsc::Sender<Internal>,
    epoch: u64,
}
impl Shared {
    async fn change<T>(
        &self,
        change: impl FnOnce(&mut Room) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut room = self.room.lock().await;
        let result = change(&mut room)?;
        self.updates.send_replace(room.snapshot.clone());
        Ok(result)
    }
    async fn skip(&self, play_id: u64) {
        if self.skipped.fetch_max(play_id, Ordering::AcqRel) < play_id {
            let _ = self.events.send(Internal::Skip(self.epoch, play_id)).await;
        }
    }
}
struct Host {
    shared: Arc<Shared>,
    invite: String,
    member_id: String,
    _tasks: Vec<Task>,
}
impl Host {
    /// Tell guests the room ended on purpose, so nobody tries to take over.
    async fn close(self) {
        let peers: Vec<PeerSender> = self
            .shared
            .peers
            .lock()
            .await
            .values()
            .map(|(_, s)| s.clone())
            .collect();
        for peer in &peers {
            let _ = peer.try_send(ServerMessage::Closed {
                message: "The host ended the jam.".into(),
            });
        }
        if !peers.is_empty() {
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }
}

struct GuestContext {
    host: SocketAddr,
    token: String,
    name: String,
    member: String,
    standby: Arc<std::sync::Mutex<Option<TcpListener>>>,
    clock: Arc<std::sync::Mutex<Estimator>>,
    last: Arc<std::sync::Mutex<Option<Snapshot>>>,
}
struct Guest {
    commands: tokio::sync::mpsc::Sender<ClientMessage>,
    context: Arc<GuestContext>,
    reconnecting: bool,
    _tasks: Vec<Task>,
}

enum RelayCommand {
    Skip(u64),
    Add(Item),
    Leave,
}
struct RelayGuest {
    commands: tokio::sync::mpsc::Sender<RelayCommand>,
    task: Task,
}
struct RelayHost {
    shared: Arc<Shared>,
    commands: tokio::sync::mpsc::Sender<RelayCommand>,
    member_id: String,
    task: Task,
}

enum Mode {
    Host(Host),
    Guest(Guest),
    RelayGuest(RelayGuest),
    RelayHost(RelayHost),
}
impl Mode {
    fn shared(&self) -> Option<(&Arc<Shared>, &str)> {
        match self {
            Mode::Host(h) => Some((&h.shared, &h.member_id)),
            Mode::RelayHost(h) => Some((&h.shared, &h.member_id)),
            _ => None,
        }
    }
}

struct Link {
    reader: BufReader<OwnedReadHalf>,
    opener: Opener,
    writer: OwnedWriteHalf,
    sealer: Sealer,
    member: String,
    state: Snapshot,
    host_sent: u64,
    transit: u64,
    app: String,
    local_ip: IpAddr,
}

#[allow(clippy::large_enum_variant)]
enum Internal {
    State(u64, Snapshot, u64, u64),
    Clock(u64, i64, u64, u64, bool),
    Secure(u64, String),
    Left(u64, String),
    Failed(u64, String),
    Error(u64, String),
    Status(u64, String),
    Joined(u64, String, String, bool),
    Lost(u64),
    Reconnected(u64, Box<Link>),
    Promote(u64),
    Skip(u64, u64),
    Resumed(u64, String),
}

pub fn worker() -> (
    tokio::sync::mpsc::UnboundedSender<Request>,
    mpsc::Receiver<Event>,
    std::thread::JoinHandle<()>,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (events, out) = mpsc::channel();
    let task = std::thread::spawn(move || {
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime.block_on(run(rx, events)),
            Err(_) => {
                let _ = events.send(Event::Error("Could not start shared jams.".into()));
            }
        }
    });
    (tx, out, task)
}

fn security_label(app: &str) -> String {
    format!(
        "Encrypted nearby room · {} · jam protocol {}",
        crate::version::app_name(app),
        crate::version::LAN_PROTOCOL
    )
}

async fn run(
    mut requests: tokio::sync::mpsc::UnboundedReceiver<Request>,
    events: mpsc::Sender<Event>,
) {
    let (internal, mut incoming) = tokio::sync::mpsc::channel(64);
    let mut mode: Option<Mode> = None;
    let mut epoch = 0u64;
    let mut report: (Option<i64>, bool) = (None, true);
    loop {
        tokio::select! {
            request = requests.recv() => {
                let Some(request) = request else { break; };
                let result: Result<(), String> = match request {
                    Request::Shutdown => break,
                    Request::Leave => {
                        epoch += 1;
                        end(mode.take()).await;
                        let _ = events.send(Event::Left("You left the jam.".into()));
                        Ok(())
                    }
                    Request::Host { name, address } => {
                        if mode.is_some() { Err("Leave the current jam first.".into()) } else {
                            epoch += 1;
                            match host(&name, &address, epoch, internal.clone()).await {
                                Ok(host) => {
                                    let _ = events.send(Event::Joined { invite: host.invite.clone(), member_id: host.member_id.clone(), host: true });
                                    let _ = events.send(Event::Secure(security_label("grove")));
                                    let _ = events.send(Event::Snapshot { state: host.shared.updates.borrow().clone(), host_sent_ms: jam::unix_ms(), transit_ms: 0 });
                                    mode = Some(Mode::Host(host));
                                    Ok(())
                                }
                                Err(error) => Err(error),
                            }
                        }
                    }
                    Request::HostRelay { name, endpoint } => {
                        if mode.is_some() { Err("Leave the current jam first.".into()) } else {
                            epoch += 1;
                            match relay_host(&name, &endpoint, epoch, internal.clone()) {
                                Ok(host) => { mode = Some(Mode::RelayHost(host)); Ok(()) }
                                Err(error) => Err(error),
                            }
                        }
                    }
                    Request::Join { name, invite } => {
                        if mode.is_some() { Err("Leave the current jam first.".into()) } else {
                            epoch += 1;
                            match invite::parse(&invite) {
                                Err(error) => Err(error),
                                Ok(Invite::Nearby { address, token }) => {
                                    match guest(&name, address, &token, epoch, internal.clone()).await {
                                        Ok((guest, link)) => {
                                            let _ = events.send(Event::Joined { invite: String::new(), member_id: link.member.clone(), host: false });
                                            let _ = events.send(Event::Secure(security_label(&link.app)));
                                            let _ = events.send(Event::Snapshot { state: link.state.clone(), host_sent_ms: link.host_sent, transit_ms: link.transit });
                                            mode = Some(Mode::Guest(start_guest(guest, *link, epoch, internal.clone())));
                                            Ok(())
                                        }
                                        Err(error) => Err(error),
                                    }
                                }
                                Ok(Invite::Relay { invite, protocol }) => {
                                    mode = Some(Mode::RelayGuest(relay_guest(RelayTarget::Invite(invite, protocol), epoch, internal.clone())));
                                    Ok(())
                                }
                                Ok(Invite::Code(code)) => {
                                    mode = Some(Mode::RelayGuest(relay_guest(RelayTarget::Code(code), epoch, internal.clone())));
                                    Ok(())
                                }
                            }
                        }
                    }
                    Request::Add { uri, title } => add(&mode, "", vec![Item { uri, title, ..Item::default() }]).await,
                    Request::AddItems { group, items } => add(&mode, &group, items).await,
                    Request::Remove(id) => match &mode {
                        Some(Mode::Guest(guest)) => guest.commands.try_send(ClientMessage::Remove { id }).map_err(|_| "The jam is busy. Try again.".into()),
                        Some(other) => match other.shared() {
                            Some((shared, member)) => shared.change(|room| room.remove(member, &id)).await,
                            None => Err("Only the host can change this queue.".into()),
                        },
                        None => Err("You are not in a jam.".into()),
                    },
                    Request::Next => match mode.as_ref().and_then(Mode::shared) {
                        Some((shared, _)) => match shared.change(Room::stage_next).await {
                            Ok(entry) => { let _ = events.send(Event::Play { id: entry.id, uri: entry.uri }); Ok(()) }
                            Err(error) => Err(error),
                        },
                        None => Err("Only the host can start the next shared track.".into()),
                    },
                    Request::Complete { id, success } => {
                        if let Some((shared, _)) = mode.as_ref().and_then(Mode::shared) {
                            let _ = shared.change(|room| { room.complete(&id, success); Ok(()) }).await;
                        }
                        Ok(())
                    }
                    Request::Publish(playback) => {
                        if let Some((shared, _)) = mode.as_ref().and_then(Mode::shared) {
                            let result = shared.change(|room| {
                                let mut candidate = room.snapshot.clone();
                                candidate.playback = playback.clone();
                                validate(&candidate)?;
                                room.set_playback(playback);
                                Ok(())
                            }).await;
                            if let Err(error) = result { let _ = events.send(Event::Error(error)); }
                        }
                        Ok(())
                    }
                    Request::SetAutoAdvance(enabled) => {
                        if let Some((shared, _)) = mode.as_ref().and_then(Mode::shared) {
                            let _ = shared.change(|room| { room.snapshot.auto_advance = enabled; room.snapshot.revision += 1; Ok(()) }).await;
                        }
                        Ok(())
                    }
                    Request::VoteSkip { play_id } => match &mode {
                        Some(Mode::Guest(guest)) => guest.commands.try_send(ClientMessage::VoteSkip { play_id }).map_err(|_| "The jam is busy. Try again.".into()),
                        Some(Mode::RelayGuest(guest)) => guest.commands.try_send(RelayCommand::Skip(play_id)).map_err(|_| "The jam is busy. Try again.".into()),
                        _ => Err("Only listeners vote to skip.".into()),
                    },
                    Request::Buffered { play_id } => {
                        if let Some(Mode::Guest(guest)) = &mode { let _ = guest.commands.try_send(ClientMessage::Buffered { play_id }); }
                        Ok(())
                    }
                    Request::Report { drift_ms, following } => {
                        report = (drift_ms, following);
                        if let Some(Mode::Guest(guest)) = &mode {
                            let stats = guest_stats(&guest.context, drift_ms);
                            let _ = guest.commands.try_send(ClientMessage::Stats { stats, following });
                        }
                        Ok(())
                    }
                };
                if let Err(error) = result { let _ = events.send(Event::Error(error)); }
            }
            message = incoming.recv() => {
                let Some(message) = message else { continue; };
                match message {
                    Internal::State(e, state, time, transit) if e == epoch => { let _ = events.send(Event::Snapshot { state, host_sent_ms: time, transit_ms: transit }); }
                    Internal::Clock(e, offset_ms, rtt_ms, jitter_ms, confident) if e == epoch => { let _ = events.send(Event::Clock { offset_ms, rtt_ms, jitter_ms, confident }); }
                    Internal::Secure(e, label) if e == epoch => { let _ = events.send(Event::Secure(label)); }
                    Internal::Status(e, message) if e == epoch => { let _ = events.send(Event::Reconnecting(message)); }
                    Internal::Resumed(e, message) if e == epoch => { let _ = events.send(Event::Resumed(message)); }
                    Internal::Joined(e, invite, member, host) if e == epoch => { let _ = events.send(Event::Joined { invite, member_id: member, host }); }
                    Internal::Skip(e, play_id) if e == epoch => { let _ = events.send(Event::SkipVoted { play_id }); }
                    Internal::Error(e, message) if e == epoch => { let _ = events.send(Event::Error(message)); }
                    Internal::Left(e, message) if e == epoch => {
                        epoch += 1;
                        end(mode.take()).await;
                        let _ = events.send(Event::Left(message));
                    }
                    Internal::Failed(e, message) if e == epoch => {
                        epoch += 1;
                        mode = None;
                        let _ = events.send(Event::Error(message));
                    }
                    Internal::Lost(e) if e == epoch => {
                        if let Some(Mode::Guest(guest)) = &mut mode {
                            if !guest.reconnecting {
                                guest.reconnecting = true;
                                guest._tasks.clear();
                                let _ = events.send(Event::Reconnecting("Lost the host · reconnecting…".into()));
                                let context = guest.context.clone();
                                let sender = internal.clone();
                                let current = epoch;
                                guest._tasks.push(Task::new(tokio::spawn(reconnect(context, current, sender))));
                            }
                        }
                    }
                    Internal::Reconnected(e, link) if e == epoch => {
                        if let Some(Mode::Guest(old)) = mode.take() {
                            let context = old.context.clone();
                            drop(old);
                            let _ = events.send(Event::Resumed("Back in the jam".into()));
                            let _ = events.send(Event::Snapshot { state: link.state.clone(), host_sent_ms: link.host_sent, transit_ms: link.transit });
                            let guest = start_guest_with(context, *link, epoch, internal.clone());
                            let stats = guest_stats(&guest.context, report.0);
                            let _ = guest.commands.try_send(ClientMessage::Stats { stats, following: report.1 });
                            mode = Some(Mode::Guest(guest));
                        }
                    }
                    Internal::Promote(e) if e == epoch => {
                        if let Some(Mode::Guest(old)) = mode.take() {
                            match promote(&old.context, epoch, internal.clone()) {
                                Ok(host) => {
                                    let _ = events.send(Event::Promoted { invite: host.invite.clone(), member_id: host.member_id.clone() });
                                    let _ = events.send(Event::Snapshot { state: host.shared.updates.borrow().clone(), host_sent_ms: jam::unix_ms(), transit_ms: 0 });
                                    mode = Some(Mode::Host(host));
                                }
                                Err(message) => {
                                    epoch += 1;
                                    let _ = events.send(Event::Left(message));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    end(mode.take()).await;
}

async fn end(mode: Option<Mode>) {
    match mode {
        Some(Mode::Host(host)) => host.close().await,
        Some(Mode::Guest(guest)) => {
            let _ = guest.commands.try_send(ClientMessage::Leave);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Some(Mode::RelayGuest(guest)) => {
            let _ = guest.commands.try_send(RelayCommand::Leave);
            guest.task.detach();
        }
        Some(Mode::RelayHost(host)) => {
            let _ = host.commands.try_send(RelayCommand::Leave);
            host.task.detach();
        }
        None => {}
    }
}

async fn add(mode: &Option<Mode>, group: &str, items: Vec<Item>) -> Result<(), String> {
    match mode {
        Some(Mode::Guest(guest)) => {
            let message = if group.is_empty() && items.len() == 1 {
                let item = items.into_iter().next().unwrap_or_default();
                ClientMessage::Add {
                    operation: jam::id(),
                    uri: item.uri,
                    title: item.title,
                }
            } else {
                ClientMessage::AddItems {
                    operation: jam::id(),
                    group: group.into(),
                    items,
                }
            };
            guest
                .commands
                .try_send(message)
                .map_err(|_| "The jam is busy. Try again.".into())
        }
        Some(Mode::RelayGuest(guest)) => {
            if items.len() != 1 {
                return Err("Add songs one at a time in an internet jam.".into());
            }
            let item = items.into_iter().next().unwrap_or_default();
            guest
                .commands
                .try_send(RelayCommand::Add(item))
                .map_err(|_| "Wait for the previous song to be sent.".into())
        }
        Some(other) => match other.shared() {
            Some((shared, member)) => {
                let member = member.to_owned();
                let group = group.to_owned();
                shared
                    .change(|room| room.add_items(&member, &jam::id(), &group, items))
                    .await
            }
            None => Err("Start or join a jam first.".into()),
        },
        None => Err("Start or join a jam first.".into()),
    }
}

fn guest_stats(context: &GuestContext, drift_ms: Option<i64>) -> SyncStats {
    let clock = context.clock.lock().map(|c| c.clone()).unwrap_or_default();
    let best = clock.best();
    SyncStats {
        offset_ms: best.map(|s| s.offset_ms).unwrap_or(0),
        rtt_ms: best.map(|s| s.rtt_ms).unwrap_or(0),
        jitter_ms: clock.jitter_ms(),
        drift_ms: drift_ms.map(|d| d.clamp(-86_400_000, 86_400_000)),
    }
}

// ---- Nearby host ---------------------------------------------------------------

async fn host(
    name: &str,
    address: &str,
    epoch: u64,
    events: tokio::sync::mpsc::Sender<Internal>,
) -> Result<Host, String> {
    let ip: IpAddr = address
        .trim()
        .parse()
        .map_err(|_| "Enter 127.0.0.1 or this computer’s private LAN IP.")?;
    if !invite::private(ip) {
        return Err("Nearby jams must bind to a loopback or private LAN address.".into());
    }
    let room = Room::new(name)?;
    let listener = TcpListener::bind(SocketAddr::new(ip, 0))
        .await
        .map_err(|_| "Could not start a jam on that address.")?;
    start_host(listener, room, jam::id(), epoch, events)
}

fn start_host(
    listener: TcpListener,
    room: Room,
    token: String,
    epoch: u64,
    events: tokio::sync::mpsc::Sender<Internal>,
) -> Result<Host, String> {
    let address = listener
        .local_addr()
        .map_err(|_| "Could not find the jam address.")?;
    let invitation = invite::nearby(address, &token);
    let member_id = room.host_id().to_owned();
    let (updates, mut changes) = watch::channel(room.snapshot.clone());
    let shared = Arc::new(Shared {
        room: Mutex::new(room),
        updates,
        token,
        peers: Mutex::new(HashMap::new()),
        skipped: AtomicU64::new(0),
        events: events.clone(),
        epoch,
    });
    let snapshot_events = events.clone();
    let publish = Task::new(tokio::spawn(async move {
        while changes.changed().await.is_ok() {
            let snapshot = changes.borrow_and_update().clone();
            if snapshot_events
                .send(Internal::State(epoch, snapshot, jam::unix_ms(), 0))
                .await
                .is_err()
            {
                break;
            }
        }
    }));
    let state = shared.clone();
    let server = Task::new(tokio::spawn(async move {
        let mut peers = JoinSet::new();
        let mut serial = 0u64;
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((socket, _)) = accepted else { break; };
                    if peers.len() >= 32 { continue; }
                    serial += 1;
                    let room = state.clone();
                    let id = serial;
                    peers.spawn(async move { peer(socket, room, id).await; });
                }
                _ = peers.join_next(), if !peers.is_empty() => {}
            }
        }
    }));
    let state = shared.clone();
    let maintenance = Task::new(tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        let mut designating: Option<(String, Instant)> = None;
        loop {
            tick.tick().await;
            let now = Instant::now();
            let candidate = {
                let mut room = state.room.lock().await;
                if room.expire(now) {
                    state.updates.send_replace(room.snapshot.clone());
                }
                if let Some((member, since)) = &designating {
                    if room.snapshot.successor.is_some() {
                        designating = None;
                    } else if now.saturating_duration_since(*since) > Duration::from_secs(5) {
                        // That guest could not open a standby listener; try another.
                        let member = member.clone();
                        if let Some(m) = room.snapshot.members.iter_mut().find(|m| m.id == member) {
                            m.standby = false;
                        }
                        designating = None;
                    }
                }
                if room.snapshot.successor.is_none() && designating.is_none() {
                    room.successor_candidate()
                } else {
                    None
                }
            };
            if let Some(member) = candidate {
                let sender = state
                    .peers
                    .lock()
                    .await
                    .get(&member)
                    .map(|(_, s)| s.clone());
                if let Some(sender) = sender {
                    if sender.try_send(ServerMessage::Designate).is_ok() {
                        designating = Some((member, now));
                    }
                }
            }
        }
    }));
    Ok(Host {
        shared,
        invite: invitation,
        member_id,
        _tasks: vec![server, publish, maintenance],
    })
}

fn clamp_stats(stats: SyncStats) -> SyncStats {
    SyncStats {
        offset_ms: stats.offset_ms.clamp(-86_400_000, 86_400_000),
        rtt_ms: stats.rtt_ms.min(60_000),
        jitter_ms: stats.jitter_ms.min(60_000),
        drift_ms: stats.drift_ms.map(|d| d.clamp(-86_400_000, 86_400_000)),
    }
}

async fn peer(socket: TcpStream, shared: Arc<Shared>, serial: u64) {
    let peer_ip = socket.peer_addr().ok().map(|a| a.ip());
    let (read, mut write) = socket.into_split();
    let mut reader = BufReader::new(read);
    let accepted = tokio::time::timeout(
        Duration::from_secs(5),
        secure::server(&mut reader, &mut write, &shared.token, "grove"),
    )
    .await;
    let (mut sealer, mut opener) = match accepted {
        Ok(Ok(Accepted::Secure(secured))) => {
            let (sealer, opener, _) = *secured;
            (sealer, opener)
        }
        _ => return,
    };
    // A guest without the token cannot produce a frame that opens: close silently.
    let greeting = tokio::time::timeout(
        Duration::from_secs(5),
        secure::recv::<_, ClientMessage>(&mut reader, &mut opener),
    )
    .await;
    let (name, resume, standby) = match greeting {
        Ok(Ok(ClientMessage::Join {
            name,
            resume,
            standby,
        })) => (name, resume, standby),
        _ => return,
    };
    let member = match shared
        .change(|room| room.join_or_resume(&name, resume.as_deref(), standby))
        .await
    {
        Ok(member) => member,
        Err(error) => {
            let _ = secure::send(
                &mut write,
                &mut sealer,
                &ServerMessage::Error { message: error },
            )
            .await;
            return;
        }
    };
    let (direct_tx, mut direct) = tokio::sync::mpsc::channel(8);
    shared
        .peers
        .lock()
        .await
        .insert(member.clone(), (serial, direct_tx));
    let mut changes = shared.updates.subscribe();
    let welcome = ServerMessage::Welcome {
        member_id: member.clone(),
        state: changes.borrow_and_update().clone(),
        host_sent_ms: jam::unix_ms(),
    };
    let mut left = secure::send(&mut write, &mut sealer, &welcome)
        .await
        .is_err();
    let (commands, mut messages) = tokio::sync::mpsc::channel(8);
    let _reader = Task::new(tokio::spawn(async move {
        loop {
            let result = secure::recv::<_, ClientMessage>(&mut reader, &mut opener).await;
            let stop = result.is_err();
            if commands.send(result).await.is_err() || stop {
                break;
            }
        }
    }));
    let mut closed = false;
    while !left {
        tokio::select! {
            message = messages.recv() => {
                let Some(Ok(message)) = message else { break; };
                let t1 = jam::unix_ms();
                let reply = match message {
                    ClientMessage::Ping { t0 } => Some(ServerMessage::Pong { t0, t1, t2: jam::unix_ms() }),
                    ClientMessage::Leave => { left = true; None }
                    ClientMessage::Standby { port } => {
                        if let (Some(ip), true) = (peer_ip, port != 0) {
                            let address = SocketAddr::new(ip, port).to_string();
                            let _ = shared.change(|room| {
                                if room.snapshot.members.iter().any(|m| m.id == member && m.connected) {
                                    room.set_successor(Some(Successor { member_id: member.clone(), address }));
                                }
                                Ok(())
                            }).await;
                        }
                        None
                    }
                    ClientMessage::VoteSkip { play_id } => match shared.change(|room| room.vote_skip(&member, play_id)).await {
                        Ok(true) => { shared.skip(play_id).await; None }
                        Ok(false) => None,
                        Err(message) => Some(ServerMessage::Error { message }),
                    },
                    ClientMessage::Buffered { play_id } => { let _ = shared.change(|room| { room.buffered(&member, play_id); Ok(()) }).await; None }
                    ClientMessage::Stats { stats, following } => { let _ = shared.change(|room| { room.report(&member, clamp_stats(stats), following); Ok(()) }).await; None }
                    ClientMessage::Add { operation, uri, title } => shared.change(|room| room.add(&member, &operation, &uri, &title)).await.err().map(|message| ServerMessage::Error { message }),
                    ClientMessage::AddItems { operation, group, items } => shared.change(|room| room.add_items(&member, &operation, &group, items)).await.err().map(|message| ServerMessage::Error { message }),
                    ClientMessage::Remove { id } => shared.change(|room| room.remove(&member, &id)).await.err().map(|message| ServerMessage::Error { message }),
                    ClientMessage::Join { .. } => Some(ServerMessage::Error { message: "Join only once per connection.".into() }),
                };
                if let Some(reply) = reply {
                    if secure::send(&mut write, &mut sealer, &reply).await.is_err() { break; }
                }
            }
            message = direct.recv() => {
                let Some(message) = message else { break; };
                let ending = matches!(message, ServerMessage::Closed { .. });
                if secure::send(&mut write, &mut sealer, &message).await.is_err() || ending {
                    closed = ending;
                    break;
                }
            }
            changed = changes.changed() => {
                if changed.is_err() { break; }
                let state = changes.borrow_and_update().clone();
                if secure::send(&mut write, &mut sealer, &ServerMessage::State { state, host_sent_ms: jam::unix_ms() }).await.is_err() { break; }
            }
        }
    }
    {
        let mut peers = shared.peers.lock().await;
        if peers.get(&member).is_some_and(|(s, _)| *s == serial) {
            peers.remove(&member);
        }
    }
    if closed {
        return;
    }
    let still_current = !shared.peers.lock().await.contains_key(&member);
    let _ = shared
        .change(|room| {
            if left {
                room.leave(&member);
            } else if still_current {
                room.away(&member, Instant::now());
            }
            Ok(())
        })
        .await;
}

// ---- Nearby guest --------------------------------------------------------------

async fn connect(
    address: SocketAddr,
    token: &str,
    name: &str,
    resume: Option<&str>,
    standby: bool,
) -> Result<Link, String> {
    let started = Instant::now();
    let socket = tokio::time::timeout(Duration::from_secs(2), TcpStream::connect(address))
        .await
        .map_err(|_| "Jam connection timed out.")?
        .map_err(|_| "Could not reach the host. Check the invitation and network.")?;
    let local_ip = socket
        .local_addr()
        .map(|a| a.ip())
        .map_err(|_| "Jam connection lost.")?;
    let (read, mut writer) = socket.into_split();
    let mut reader = BufReader::new(read);
    let (mut sealer, mut opener, peer) =
        secure::client(&mut reader, &mut writer, token, "grove").await?;
    secure::send(
        &mut writer,
        &mut sealer,
        &ClientMessage::Join {
            name: name.into(),
            resume: resume.map(str::to_owned),
            standby,
        },
    )
    .await?;
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        secure::recv::<_, ServerMessage>(&mut reader, &mut opener),
    )
    .await
    .map_err(|_| "The host did not answer the invitation.")?
    .map_err(|_| "This invitation is invalid or expired.")?;
    let (member, state, host_sent) = match response {
        ServerMessage::Welcome {
            member_id,
            state,
            host_sent_ms,
        } => {
            validate(&state)?;
            (member_id, state, host_sent_ms)
        }
        ServerMessage::Error { message } => return Err(message.chars().take(200).collect()),
        _ => return Err("Invalid jam response.".into()),
    };
    Ok(Link {
        reader,
        opener,
        writer,
        sealer,
        member,
        state,
        host_sent,
        transit: (started.elapsed().as_millis() as u64 / 2).min(500),
        app: peer.app,
        local_ip,
    })
}

async fn guest(
    name: &str,
    address: SocketAddr,
    token: &str,
    _epoch: u64,
    _events: tokio::sync::mpsc::Sender<Internal>,
) -> Result<(GuestContext, Box<Link>), String> {
    let link = connect(address, token, name, None, true).await?;
    let context = GuestContext {
        host: address,
        token: token.into(),
        name: name.into(),
        member: link.member.clone(),
        standby: Arc::new(std::sync::Mutex::new(None)),
        clock: Arc::new(std::sync::Mutex::new(Estimator::default())),
        last: Arc::new(std::sync::Mutex::new(Some(link.state.clone()))),
    };
    Ok((context, Box::new(link)))
}

fn start_guest(
    context: GuestContext,
    link: Link,
    epoch: u64,
    events: tokio::sync::mpsc::Sender<Internal>,
) -> Guest {
    start_guest_with(Arc::new(context), link, epoch, events)
}

fn start_guest_with(
    context: Arc<GuestContext>,
    link: Link,
    epoch: u64,
    events: tokio::sync::mpsc::Sender<Internal>,
) -> Guest {
    let Link {
        mut reader,
        mut opener,
        mut writer,
        mut sealer,
        state,
        local_ip,
        ..
    } = link;
    if let Ok(mut last) = context.last.lock() {
        *last = Some(state);
    }
    let (commands, mut outgoing) = tokio::sync::mpsc::channel::<ClientMessage>(16);
    let errors = events.clone();
    let writer_task = Task::new(tokio::spawn(async move {
        let mut ping = tokio::time::interval(Duration::from_secs(2));
        loop {
            let message = tokio::select! {
                message = outgoing.recv() => match message { Some(m) => m, None => break },
                _ = ping.tick() => ClientMessage::Ping { t0: jam::unix_ms() },
            };
            let leaving = matches!(message, ClientMessage::Leave);
            if secure::send(&mut writer, &mut sealer, &message)
                .await
                .is_err()
            {
                let _ = errors.send(Internal::Lost(epoch)).await;
                break;
            }
            if leaving {
                break;
            }
        }
    }));
    let reply = commands.clone();
    let ctx = context.clone();
    let reader_task = Task::new(tokio::spawn(async move {
        loop {
            match secure::recv::<_, ServerMessage>(&mut reader, &mut opener).await {
                Ok(ServerMessage::State {
                    state,
                    host_sent_ms,
                }) => {
                    if validate(&state).is_err() {
                        break;
                    }
                    if let Ok(mut last) = ctx.last.lock() {
                        *last = Some(state.clone());
                    }
                    if events
                        .send(Internal::State(epoch, state, host_sent_ms, 0))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(ServerMessage::Pong { t0, t1, t2 }) => {
                    let t3 = jam::unix_ms();
                    let update = ctx.clock.lock().ok().and_then(|mut clock| {
                        clock.exchange(t0, t1, t2, t3).then(|| {
                            let best = clock.best().unwrap_or(crate::clock::Sample {
                                offset_ms: 0,
                                rtt_ms: 0,
                            });
                            (
                                best.offset_ms,
                                best.rtt_ms,
                                clock.jitter_ms(),
                                clock.confident(),
                            )
                        })
                    });
                    if let Some((offset, rtt, jitter, confident)) = update {
                        let _ = events
                            .send(Internal::Clock(epoch, offset, rtt, jitter, confident))
                            .await;
                    }
                }
                Ok(ServerMessage::Error { message }) => {
                    let _ = events
                        .send(Internal::Error(epoch, message.chars().take(200).collect()))
                        .await;
                }
                Ok(ServerMessage::Designate) => {
                    let listener = TcpListener::bind(SocketAddr::new(local_ip, 0)).await.ok();
                    let port = listener
                        .as_ref()
                        .and_then(|l| l.local_addr().ok())
                        .map(|a| a.port());
                    if let (Some(listener), Some(port)) = (listener, port) {
                        if let Ok(mut slot) = ctx.standby.lock() {
                            *slot = Some(listener);
                        }
                        let _ = reply.try_send(ClientMessage::Standby { port });
                    }
                }
                Ok(ServerMessage::Closed { message }) => {
                    let _ = events
                        .send(Internal::Left(epoch, message.chars().take(200).collect()))
                        .await;
                    return;
                }
                Ok(ServerMessage::Welcome { .. }) => break,
                Err(_) => break,
            }
        }
        let _ = events.send(Internal::Lost(epoch)).await;
    }));
    Guest {
        commands,
        context,
        reconnecting: false,
        _tasks: vec![writer_task, reader_task],
    }
}

/// Bounded reconnection: the original host first, then the designated successor.
async fn reconnect(
    context: Arc<GuestContext>,
    epoch: u64,
    events: tokio::sync::mpsc::Sender<Internal>,
) {
    let successor = context
        .last
        .lock()
        .ok()
        .and_then(|s| s.as_ref().and_then(|s| s.successor.clone()));
    let me = successor
        .as_ref()
        .is_some_and(|s| s.member_id == context.member);
    let standby = context.standby.lock().map(|s| s.is_some()).unwrap_or(false);
    let began = Instant::now();
    let mut delay = Duration::from_millis(250);
    let mut attempt = 0u32;
    while began.elapsed() < Duration::from_secs(25) {
        // Brief outages are common: give the original host about four seconds.
        let target = match &successor {
            Some(s) if began.elapsed() >= Duration::from_secs(4) && !me => {
                if attempt % 3 == 2 {
                    Some(context.host)
                } else {
                    s.address.parse().ok()
                }
            }
            _ => Some(context.host),
        };
        if me && standby && began.elapsed() >= Duration::from_secs(4) {
            let _ = events.send(Internal::Promote(epoch)).await;
            return;
        }
        if let Some(address) = target {
            let _ = events
                .send(Internal::Status(
                    epoch,
                    if address == context.host {
                        "Lost the host · reconnecting…".into()
                    } else {
                        "Host left · moving to the new host…".into()
                    },
                ))
                .await;
            if let Ok(link) = connect(
                address,
                &context.token,
                &context.name,
                Some(&context.member),
                true,
            )
            .await
            {
                let _ = events
                    .send(Internal::Reconnected(epoch, Box::new(link)))
                    .await;
                return;
            }
        }
        attempt += 1;
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(3));
    }
    let _ = events
        .send(Internal::Left(
            epoch,
            "Lost the connection to the jam. Ask the host for a new invitation if it continues."
                .into(),
        ))
        .await;
}

fn promote(
    context: &GuestContext,
    epoch: u64,
    events: tokio::sync::mpsc::Sender<Internal>,
) -> Result<Host, String> {
    let listener = context
        .standby
        .lock()
        .ok()
        .and_then(|mut s| s.take())
        .ok_or("The host left and no one could take over.")?;
    let snapshot = context
        .last
        .lock()
        .ok()
        .and_then(|s| s.clone())
        .ok_or("The host left and no one could take over.")?;
    let room = Room::restore(snapshot, &context.member, Instant::now())?;
    start_host(listener, room, context.token.clone(), epoch, events)
}

// ---- Relay (internet) ------------------------------------------------------------

enum RelayTarget {
    Invite(relay::Invite, u8),
    Code(String),
}

fn relay_error(error: relay::Error, joining: bool) -> String {
    match error {
        relay::Error::Mismatch(n) => crate::version::relay_mismatch(n),
        relay::Error::Limited if joining => "Too many join attempts, or the room is full. Wait a minute and try again.".into(),
        relay::Error::Limited => "The jam relay is full or rate-limited. Try again in a few minutes, or run your own relay (grove-jam-relay).".into(),
        relay::Error::Unavailable if joining => "Cannot join: the invite may have expired, or the room is locked or full.".into(),
        relay::Error::Unavailable => "This relay does not allow open room creation (it may need an operator key).".into(),
        relay::Error::Invalid => "The jam relay sent an invalid response.".into(),
        relay::Error::Transport => "Could not reach the jam relay. Check your connection and the relay address.".into(),
    }
}

fn relay_label(client: &relay::Client) -> String {
    let link = if client.endpoint().starts_with("https://") {
        "TLS to relay"
    } else {
        "Loopback relay (this computer only)"
    };
    let protocol = if client.protocol() == RELAY_TIDE {
        "Tide protocol 1"
    } else {
        "cross-service protocol 2"
    };
    format!("Internet room · {link} · {protocol}")
}

/// Bounded wake-up for Tide's free relay, which sleeps when idle.
async fn wake(
    client: &relay::Client,
    epoch: u64,
    events: &tokio::sync::mpsc::Sender<Internal>,
    commands: &mut tokio::sync::mpsc::Receiver<RelayCommand>,
) -> Result<(), String> {
    if !client.needs_wakeup() {
        return Ok(());
    }
    let began = Instant::now();
    while began.elapsed() < Duration::from_secs(90) {
        let _ = events
            .send(Internal::Status(
                epoch,
                format!(
                    "Waking the free Jam server… this can take up to a minute ({} s)",
                    began.elapsed().as_secs()
                ),
            ))
            .await;
        if client.ready().await.is_ok() {
            return Ok(());
        }
        tokio::select! {
            command = commands.recv() => if matches!(command, None | Some(RelayCommand::Leave)) { return Err(String::new()); },
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
        }
    }
    Err("The free Jam server did not start in time. Try again shortly.".into())
}

fn relay_guest(
    target: RelayTarget,
    epoch: u64,
    events: tokio::sync::mpsc::Sender<Internal>,
) -> RelayGuest {
    let (commands, receiver) = tokio::sync::mpsc::channel(4);
    let task = Task::new(tokio::spawn(async move {
        if let Err(message) = follow_relay(target, epoch, events.clone(), receiver).await {
            if !message.is_empty() {
                let _ = events.send(Internal::Left(epoch, message)).await;
            }
        }
    }));
    RelayGuest { commands, task }
}

struct Titles {
    lookup: bool,
    known: HashMap<String, String>,
    pending: Option<(String, tokio::task::JoinHandle<Option<String>>)>,
}
impl Titles {
    fn title(&mut self, video: &str) -> String {
        if let Some(title) = self.known.get(video) {
            return title.clone();
        }
        if self.lookup && self.pending.is_none() && self.known.len() < 200 {
            let id = video.to_owned();
            let handle = tokio::spawn(async move { youtube_title(&id).await });
            self.pending = Some((video.into(), handle));
        }
        "YouTube song".into()
    }
    async fn poll(&mut self) {
        if self.pending.as_ref().is_some_and(|(_, h)| h.is_finished()) {
            if let Some((id, handle)) = self.pending.take() {
                let title = handle
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "YouTube song".into());
                self.known.insert(id, title);
            }
        }
    }
}

/// Public oEmbed title for a validated video ID (what Tide does for guests).
async fn youtube_title(video: &str) -> Option<String> {
    relay::VideoId::parse(video).ok()?;
    let mut url = url::Url::parse("https://www.youtube.com/oembed").ok()?;
    url.query_pairs_mut()
        .append_pair("url", &format!("https://www.youtube.com/watch?v={video}"))
        .append_pair("format", "json");
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .https_only(true)
        .timeout(Duration::from_secs(3))
        .build()
        .ok()?;
    let mut response = client.get(url).send().await.ok()?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if bytes.len() + chunk.len() > 16 * 1024 {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let title: String = value["title"]
        .as_str()?
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect();
    let title = title.trim();
    (!title.is_empty()).then(|| title.to_owned())
}

fn relay_members(host_name: &str, count: usize, me: Option<u32>) -> Vec<Member> {
    let mut members = vec![Member {
        id: "relay-host".into(),
        name: host_name.into(),
        host: true,
        connected: true,
        following: false,
        ready_play: 0,
        sync: None,
        standby: false,
    }];
    for n in 0..count.min(8) {
        members.push(Member {
            id: format!("relay-listener-{n}"),
            name: if n == 0 {
                me.map_or("Listener".into(), |id| format!("You · Listener {id}"))
            } else {
                "Listener".into()
            },
            host: false,
            connected: true,
            following: true,
            ready_play: 0,
            sync: None,
            standby: false,
        });
    }
    members
}

#[allow(clippy::too_many_arguments)]
fn relay_snapshot(
    room: &str,
    revision: u64,
    protocol: u8,
    members: usize,
    me: Option<u32>,
    current: Option<(TrackRef, u64, Vec<TrackRef>)>,
    position_ms: u64,
    playing: bool,
    loading: bool,
) -> Snapshot {
    let now = jam::unix_ms();
    let (playback, queue) = match current {
        None => (None, Vec::new()),
        Some((track, play_id, upcoming)) => (
            Some(Playback {
                uri: track.uri,
                title: track.title,
                artist: track.artist,
                position_ms,
                playing: playing && !loading,
                sampled_at_ms: now,
                play_id,
                duration_ms: track.duration_ms,
                isrc: track.isrc,
                preparing: loading,
            }),
            upcoming
                .into_iter()
                .take(jam::MAX_TRACKS)
                .enumerate()
                .map(|(n, t)| jam::Entry {
                    id: format!("relay-{play_id}-{n}"),
                    uri: t.uri,
                    title: t.title,
                    member_id: "relay-host".into(),
                    member_name: "Host".into(),
                    delivery: jam::Delivery::Waiting,
                    isrc: t.isrc,
                    duration_ms: t.duration_ms,
                    group: String::new(),
                })
                .collect(),
        ),
    };
    Snapshot {
        room_id: room.into(),
        revision,
        members: relay_members(
            if protocol == RELAY_TIDE {
                "Tide host"
            } else {
                "Host"
            },
            members,
            me,
        ),
        queue,
        closed: false,
        playback,
        auto_advance: true,
        skip_votes: vec![],
        successor: None,
        origin: if protocol == RELAY_TIDE {
            "tide-relay"
        } else {
            "relay"
        }
        .into(),
    }
}

async fn join_target(
    client: &relay::Client,
    target: &RelayTarget,
) -> Result<(String, relay::Secret), relay::Error> {
    match target {
        RelayTarget::Invite(invite, _) => client
            .join(invite)
            .await
            .map(|j| (invite.room.clone(), j.member)),
        RelayTarget::Code(code) => client.join_code(code).await.map(|j| (j.room, j.member)),
    }
}

async fn follow_relay(
    target: RelayTarget,
    epoch: u64,
    events: tokio::sync::mpsc::Sender<Internal>,
    mut commands: tokio::sync::mpsc::Receiver<RelayCommand>,
) -> Result<(), String> {
    let (endpoint, protocol, invite) = match &target {
        RelayTarget::Invite(invite, protocol) => {
            (invite.endpoint.clone(), *protocol, Some(invite.clone()))
        }
        RelayTarget::Code(_) => (relay::SHARED_RELAY.to_owned(), RELAY_TIDE, None),
    };
    let fail = |message: String| {
        let events = events.clone();
        async move {
            let _ = events.send(Internal::Failed(epoch, message)).await;
            Err::<(), String>(String::new())
        }
    };
    let client = match relay::Client::new(&endpoint, protocol) {
        Ok(client) => client,
        Err(_) => {
            return fail(
                "The invite's relay address is not a valid HTTPS or loopback relay.".into(),
            )
            .await
        }
    };
    if let Err(message) = wake(&client, epoch, &events, &mut commands).await {
        return if message.is_empty() {
            Ok(())
        } else {
            fail(message).await
        };
    }
    let (room, mut member) = match join_target(&client, &target).await {
        Ok(joined) => joined,
        Err(error) => return fail(relay_error(error, true)).await,
    };
    let listener = client.listener_id(&room, &member).await.ok();
    let _ = events
        .send(Internal::Joined(
            epoch,
            String::new(),
            format!("relay-listener-{}", listener.unwrap_or(0)),
            false,
        ))
        .await;
    let _ = events
        .send(Internal::Secure(epoch, relay_label(&client)))
        .await;
    let mut titles = Titles {
        lookup: client.endpoint().starts_with("https://"),
        known: HashMap::new(),
        pending: None,
    };
    let mut clock = Estimator::default();
    let mut failures = 0u32;
    let mut rejoins = 0u32;
    let mut revision = 0u64;
    let mut current: Option<(u64, String)> = None;
    let mut wait = Duration::ZERO;
    let result = loop {
        tokio::select! {
            command = commands.recv() => match command {
                None | Some(RelayCommand::Leave) => break Ok(()),
                Some(RelayCommand::Skip(play_id)) => {
                    let sent = match (&current, protocol) {
                        (Some((id, key)), RELAY_TIDE) if *id == play_id => match relay::VideoId::parse(key.trim_start_matches("youtube:")) {
                            Ok(video) => client.request_skip(&room, &member, &relay::Skip { play_id, video }).await,
                            Err(_) => Err(relay::Error::Invalid),
                        },
                        (Some((id, key)), _) if *id == play_id => client.request_skip(&room, &member, &CrossSkip { play_id, uri: key.clone() }).await,
                        _ => Err(relay::Error::Invalid),
                    };
                    let message = match sent {
                        Ok(()) => "Skip requested for everyone.".into(),
                        Err(relay::Error::Limited) => "Request limit reached. Wait before asking again.".into(),
                        Err(_) => "Skip was not confirmed. The song may have changed.".into(),
                    };
                    let _ = events.send(Internal::Error(epoch, message)).await;
                }
                Some(RelayCommand::Add(item)) => {
                    let message = if protocol == RELAY_TIDE {
                        "In a Tide room, choose songs in Tide on the host's device.".to_owned()
                    } else {
                        let track = TrackRef { uri: item.uri, title: item.title.chars().take(200).collect(), artist: String::new(), duration_ms: item.duration_ms, isrc: crate::matching::normalize_isrc(&item.isrc).unwrap_or_default() };
                        match client.enqueue(&room, &member, &CrossEnqueue { track }).await {
                            Ok(()) => "Song sent to the shared queue.".into(),
                            Err(relay::Error::Limited) => "Queue request limit reached. Wait before adding another song.".into(),
                            Err(_) => "Song addition was not confirmed. Check the queue before retrying.".into(),
                        }
                    };
                    let _ = events.send(Internal::Error(epoch, message)).await;
                }
            },
            _ = tokio::time::sleep(wait) => {
                titles.poll().await;
                let polled = if protocol == RELAY_TIDE {
                    client.poll::<Timeline>(&room, &member).await.and_then(|(view, rtt)| {
                        if !relay::valid_view(&view, |t| t.validate().is_ok()) { return Err(relay::Error::Invalid); }
                        let current = view.timeline.as_ref().map(|t| {
                            let to_ref = |v: &relay::VideoId, titles: &mut Titles| TrackRef { uri: format!("youtube:{}", v.as_str()), title: titles.title(v.as_str()), artist: String::new(), duration_ms: 0, isrc: String::new() };
                            let now = to_ref(&t.queue[t.index], &mut titles);
                            let upcoming = t.queue[t.index + 1..].iter().take(jam::MAX_TRACKS).map(|v| TrackRef { uri: format!("youtube:{}", v.as_str()), title: titles.known.get(v.as_str()).cloned().unwrap_or_else(|| "YouTube song".into()), artist: String::new(), duration_ms: 0, isrc: String::new() }).collect();
                            ((now, t.play_id, upcoming), t.position_ms, !t.paused, t.loading)
                        });
                        Ok((view.members, view.age_ms, rtt, current))
                    })
                } else {
                    client.poll::<CrossTimeline>(&room, &member).await.and_then(|(view, rtt)| {
                        if !relay::valid_view(&view, |t| t.validate().is_ok()) { return Err(relay::Error::Invalid); }
                        let current = view.timeline.as_ref().map(|t| {
                            let upcoming = t.queue[t.index + 1..].to_vec();
                            ((t.queue[t.index].clone(), t.play_id, upcoming), t.position_ms, !t.paused, t.loading)
                        });
                        Ok((view.members, view.age_ms, rtt, current))
                    })
                };
                match polled {
                    Ok((members, age_ms, rtt, timeline)) => {
                        failures = 0;
                        wait = Duration::from_secs(1);
                        clock.round_trip(rtt);
                        let rtt = clock.best().map_or(rtt, |s| s.rtt_ms.min(rtt));
                        let stale = age_ms.saturating_add(rtt / 2) >= relay::STALE_MS;
                        revision += 1;
                        let (current_track, position, playing, loading) = match timeline {
                            Some((track, position, playing, loading)) => {
                                current = Some((track.1, track.0.uri.clone()));
                                let position = if playing && !loading { position.saturating_add(age_ms + rtt / 2) } else { position };
                                (Some(track), position, playing && !stale, loading)
                            }
                            None => { current = None; (None, 0, false, false) }
                        };
                        let snapshot = relay_snapshot(&room, revision, protocol, members, listener, current_track, position, playing, loading);
                        let _ = events.send(Internal::Clock(epoch, 0, rtt, clock.jitter_ms(), clock.confident())).await;
                        if events.send(Internal::State(epoch, snapshot, jam::unix_ms(), 0)).await.is_err() { break Ok(()); }
                    }
                    Err(relay::Error::Mismatch(n)) => break Err(crate::version::relay_mismatch(n)),
                    Err(relay::Error::Invalid) => break Err("The jam relay sent invalid room state.".into()),
                    Err(relay::Error::Unavailable) => {
                        // Membership idles out after 30 s offline; rejoin with the invite.
                        if rejoins >= 3 { break Err("The jam ended, expired, or its relay restarted.".into()); }
                        rejoins += 1;
                        let rejoin = if invite.is_some() { join_target(&client, &target).await } else { Err(relay::Error::Unavailable) };
                        match rejoin {
                            Ok((_, token)) => {
                                member = token;
                                let _ = events.send(Internal::Resumed(epoch, "Rejoined the jam".into())).await;
                                wait = Duration::from_millis(200);
                            }
                            Err(_) => break Err("The jam ended, expired, or its relay restarted.".into()),
                        }
                    }
                    Err(_) => {
                        failures += 1;
                        if failures >= 8 { break Err("Jam disconnected after bounded retries.".into()); }
                        let _ = events.send(Internal::Status(epoch, format!("Jam connection interrupted · retrying ({failures}/8)"))).await;
                        wait = Duration::from_secs(u64::from(failures).min(5));
                    }
                }
            }
        }
    };
    let _ = client.leave(&room, &member).await;
    result
}

fn relay_host(
    name: &str,
    endpoint: &str,
    epoch: u64,
    events: tokio::sync::mpsc::Sender<Internal>,
) -> Result<RelayHost, String> {
    let client = relay::Client::new(endpoint.trim(), RELAY_CROSS).map_err(|_| {
        "Enter a relay address like https://jam.example.org (or http://127.0.0.1:port for testing)."
    })?;
    let mut room = Room::new(name)?;
    room.snapshot.origin = "relay".into();
    let member_id = room.host_id().to_owned();
    let (updates, mut changes) = watch::channel(room.snapshot.clone());
    let shared = Arc::new(Shared {
        room: Mutex::new(room),
        updates,
        token: String::new(),
        peers: Mutex::new(HashMap::new()),
        skipped: AtomicU64::new(0),
        events: events.clone(),
        epoch,
    });
    let (commands, receiver) = tokio::sync::mpsc::channel(4);
    let state = shared.clone();
    let task = Task::new(tokio::spawn(async move {
        let forward = events.clone();
        let publisher = tokio::spawn(async move {
            while changes.changed().await.is_ok() {
                let snapshot = changes.borrow_and_update().clone();
                if forward
                    .send(Internal::State(epoch, snapshot, jam::unix_ms(), 0))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        let result = host_relay(client, state, epoch, events.clone(), receiver).await;
        publisher.abort();
        if let Err(message) = result {
            if !message.is_empty() {
                let _ = events.send(Internal::Left(epoch, message)).await;
            }
        }
    }));
    Ok(RelayHost {
        shared,
        commands,
        member_id,
        task,
    })
}

fn cross_timeline(snapshot: &Snapshot, revision: u64) -> Option<CrossTimeline> {
    let p = snapshot.playback.as_ref()?;
    if p.play_id == 0 {
        return None;
    }
    let now = jam::unix_ms();
    let mut queue = vec![TrackRef {
        uri: p.uri.clone(),
        title: p.title.chars().take(200).collect(),
        artist: p.artist.chars().take(200).collect(),
        duration_ms: p.duration_ms,
        isrc: crate::matching::normalize_isrc(&p.isrc).unwrap_or_default(),
    }];
    queue.extend(
        snapshot
            .queue
            .iter()
            .filter(|e| e.delivery == jam::Delivery::Waiting)
            .take(relay::MAX_CROSS_QUEUE - 1)
            .map(|e| TrackRef {
                uri: e.uri.clone(),
                title: e.title.chars().take(200).collect(),
                artist: String::new(),
                duration_ms: e.duration_ms,
                isrc: e.isrc.clone(),
            }),
    );
    let scheduled = p.playing && p.sampled_at_ms > now;
    let loading = p.preparing || scheduled;
    let position = if p.playing && !loading {
        p.position_ms
            .saturating_add(now.saturating_sub(p.sampled_at_ms))
    } else {
        p.position_ms
    }
    .min(relay::MAX_POSITION_MS);
    Some(CrossTimeline {
        revision,
        play_id: p.play_id,
        queue,
        index: 0,
        position_ms: position,
        paused: !p.playing || loading,
        loading,
    })
}

async fn host_relay(
    client: relay::Client,
    shared: Arc<Shared>,
    epoch: u64,
    events: tokio::sync::mpsc::Sender<Internal>,
    mut commands: tokio::sync::mpsc::Receiver<RelayCommand>,
) -> Result<(), String> {
    let fail = |message: String| {
        let events = events.clone();
        async move {
            let _ = events.send(Internal::Failed(epoch, message)).await;
            Err::<(), String>(String::new())
        }
    };
    if let Err(message) = wake(&client, epoch, &events, &mut commands).await {
        return if message.is_empty() {
            Ok(())
        } else {
            fail(message).await
        };
    }
    match client.protocols().await {
        Ok(protocols) if protocols.contains(&RELAY_CROSS) => {}
        Ok(_) => return fail(crate::version::relay_mismatch(RELAY_CROSS)).await,
        Err(error) => return fail(relay_error(error, false)).await,
    }
    let created = match client.create().await {
        Ok(created) => created,
        Err(error) => return fail(relay_error(error, false)).await,
    };
    let invite = relay::Invite {
        endpoint: client.endpoint().into(),
        room: created.room.clone(),
        invite: created.invite.clone(),
    };
    let code = match invite.to_code(RELAY_CROSS) {
        Ok(code) => code.to_string(),
        Err(message) => return fail(message.into()).await,
    };
    let host_member = shared.room.lock().await.host_id().to_owned();
    let _ = events
        .send(Internal::Joined(epoch, code, host_member.clone(), true))
        .await;
    let _ = events
        .send(Internal::Secure(epoch, relay_label(&client)))
        .await;
    shared.updates.send_modify(|_| {});
    let mut revision = 0u64;
    let mut failures = 0u32;
    let mut accepted = 0u64;
    let mut last_play = 0u64;
    let result = loop {
        tokio::select! {
            command = commands.recv() => if matches!(command, None | Some(RelayCommand::Leave)) { break Ok(()); },
            _ = tokio::time::sleep(Duration::from_secs(1)) => {
                let snapshot = shared.room.lock().await.snapshot.clone();
                let timeline = cross_timeline(&snapshot, revision + 1);
                if let Some(timeline) = &timeline {
                    match client.publish(&created.room, &created.host, timeline).await {
                        Ok(()) => { failures = 0; revision += 1; last_play = timeline.play_id; }
                        Err(relay::Error::Unavailable | relay::Error::Invalid) => break Err("The jam room closed or expired on the relay.".into()),
                        Err(relay::Error::Mismatch(n)) => break Err(crate::version::relay_mismatch(n)),
                        Err(_) => {
                            failures += 1;
                            if failures >= 5 { break Err("Jam relay unavailable after bounded retries.".into()); }
                            let _ = events.send(Internal::Status(epoch, format!("Jam connection interrupted · retrying ({failures}/5)"))).await;
                            continue;
                        }
                    }
                }
                if let Ok(view) = client.host_view(&created.room, &created.host).await {
                    let _ = shared.change(|room| {
                        let mut members = vec![room.snapshot.members[0].clone()];
                        members.extend(view.members.iter().map(|id| Member { id: format!("relay-listener-{id}"), name: format!("Listener {id}"), host: false, connected: true, following: true, ready_play: 0, sync: None, standby: false }));
                        if members.len() != room.snapshot.members.len() || members.iter().zip(&room.snapshot.members).any(|(a, b)| a.id != b.id) {
                            room.snapshot.members = members;
                            room.snapshot.revision += 1;
                        }
                        Ok(())
                    }).await;
                }
                if let Ok(requests) = client.requests::<CrossRequest>(&created.room, &created.host).await {
                    let fresh: Vec<CrossRequest> = requests.into_iter().filter(|r| r.id > accepted).collect();
                    for request in fresh {
                        accepted = request.id;
                        if request.track.validate().is_ok() && request.track.uri.starts_with("spotify:track:") {
                            let member = host_member.clone();
                            let item = Item { uri: request.track.uri, title: request.track.title, isrc: request.track.isrc, duration_ms: request.track.duration_ms };
                            let _ = shared.change(|room| room.add_items(&member, &format!("relay-{}", request.id), "", vec![item])).await;
                        }
                        let _ = client.acknowledge(&created.room, &created.host, request.id).await;
                    }
                }
                if let Ok(Some(skip)) = client.skip_request::<CrossSkip>(&created.room, &created.host).await {
                    let current = snapshot.playback.as_ref().map(|p| (p.play_id, p.uri.clone()));
                    if current == Some((skip.play_id, skip.uri.clone())) && skip.play_id == last_play {
                        shared.skip(skip.play_id).await;
                    }
                }
            }
        }
    };
    let _ = client.close(&created.room, &created.host).await;
    result
}

fn validate(state: &Snapshot) -> Result<(), String> {
    if state.members.is_empty()
        || state.members.len() > jam::MAX_MEMBERS
        || state.queue.len() > jam::MAX_TRACKS
        || state.skip_votes.len() > jam::MAX_MEMBERS
        || state.successor.as_ref().is_some_and(|s| {
            !s.address
                .parse::<SocketAddr>()
                .is_ok_and(|a| invite::private(a.ip()))
        })
    {
        return Err("Invalid jam state.".into());
    }
    if let Some(frame) = &state.playback {
        if crate::track_uri(&frame.uri).ok().as_deref() != Some(frame.uri.as_str())
            || frame.position_ms > 86_400_000
            || frame.duration_ms > 86_400_000
            || frame.isrc.len() > 12
            || frame.title.chars().count() > 300
            || frame.artist.chars().count() > 300
            || frame
                .title
                .chars()
                .chain(frame.artist.chars())
                .any(char::is_control)
        {
            return Err("Invalid shared playback update.".into());
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
