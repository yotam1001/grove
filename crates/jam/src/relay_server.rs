//! Self-hostable jam relay speaking Tide's protocol 1 and the cross-service
//! protocol 2. A port of Tide `experiments/jam/src/relay.rs` (same routes,
//! limits, headers and expiry) generalised so a room created with
//! `x-tide-jam-protocol: 2` stores provider-URI timelines. Tide clients work
//! against it unchanged (see `tests/tide_interop.rs`).
//!
//! Like Tide's, the backend binds literal 127.0.0.1 only. Expose it through a
//! same-machine TLS proxy (Tide's `nginx-shared.example.conf` works as is) that
//! overwrites `x-tide-jam-peer` with the socket address. Rooms live in memory;
//! there are no accounts, logs, audio or paid services.
use crate::relay::{
    valid_hex, valid_short_code, CodeJoined, Created, CrossEnqueue, CrossSkip, CrossTimeline,
    Endpoint, Enqueue, HostView, Joined, ListenerId, Secret, ShortCode, Skip, Timeline, MAX_BODY,
    MAX_REQUESTS,
};
use crate::version::{RELAY_CROSS, RELAY_TIDE};
use http_body_util::{BodyExt, Full, Limited};
use hyper::{body::Bytes, header, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde_json::{json, Value};
use std::{
    collections::{hash_map::RandomState, HashMap, VecDeque},
    hash::BuildHasher,
    net::{IpAddr, Ipv6Addr},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{net::TcpListener, sync::Semaphore};

const MAX_ROOMS: usize = 32;
const ROOM_LIFETIME_MS: u64 = 8 * 60 * 60 * 1000;
const ROOM_IDLE_MS: u64 = 120_000;
const MEMBER_IDLE_MS: u64 = 30_000;
const PEER_HEADER: &str = "x-tide-jam-peer";
type Failure = (StatusCode, &'static str);
const MISSING: Failure = (StatusCode::NOT_FOUND, "Room or membership unavailable");

/// How room creation is authorised.
#[derive(Debug)]
pub enum Mode {
    /// Literal loopback for labs and tests; Host must equal the listener address.
    Local,
    /// Public shared relay behind a TLS proxy; creation is rate-limited per peer.
    Shared(Endpoint),
}

struct Member {
    id: u32,
    token: Secret,
    seen: u64,
    requests: VecDeque<u64>,
}
struct PendingRequest {
    member: u32,
    id: u64,
    item: Value,
}
struct Stored {
    revision: u64,
    play_id: u64,
    current: String,
    json: Value,
}
struct PendingSkip {
    member: u32,
    play_id: u64,
    current: String,
    json: Value,
}
struct Room {
    protocol: u8,
    open: bool,
    next_member: u32,
    peer: Option<u64>,
    host: Secret,
    invite: Secret,
    short_code: Option<String>,
    born: u64,
    updated: u64,
    timeline: Option<Stored>,
    members: Vec<Member>,
    pending: Vec<PendingRequest>,
    next_request: u64,
    skip: Option<PendingSkip>,
}

#[derive(Default)]
struct Store {
    rooms: HashMap<String, Room>,
    creates: VecDeque<u64>,
    peer_creates: HashMap<u64, VecDeque<u64>>,
    code_window: u64,
    code_attempts: usize,
    peer_code_attempts: HashMap<u64, usize>,
}

fn internal() -> Failure {
    (StatusCode::INTERNAL_SERVER_ERROR, "Room setup unavailable")
}
fn prune(times: &mut VecDeque<u64>, now: u64) {
    while times
        .front()
        .is_some_and(|t| now.saturating_sub(*t) >= 60_000)
    {
        times.pop_front();
    }
}

impl Store {
    fn clean(&mut self, now: u64) {
        self.rooms.retain(|_, room| {
            now.saturating_sub(room.born) < ROOM_LIFETIME_MS
                && now.saturating_sub(room.updated) < ROOM_IDLE_MS
        });
        for room in self.rooms.values_mut() {
            room.members
                .retain(|m| now.saturating_sub(m.seen) < MEMBER_IDLE_MS);
            room.pending
                .retain(|p| room.members.iter().any(|m| m.id == p.member));
            if room
                .skip
                .as_ref()
                .is_some_and(|s| !room.members.iter().any(|m| m.id == s.member))
            {
                room.skip = None;
            }
        }
        prune(&mut self.creates, now);
    }
    fn new_short_code(&self) -> Result<String, Failure> {
        for _ in 0..16 {
            let mut random = [0u8; 4];
            ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut random)
                .map_err(|_| internal())?;
            let mut value = u32::from_le_bytes(random);
            let code: String = (0..4)
                .map(|_| {
                    let letter = (b'A' + (value % 26) as u8) as char;
                    value /= 26;
                    letter
                })
                .collect();
            if !self
                .rooms
                .values()
                .any(|room| room.short_code.as_deref() == Some(&code))
            {
                return Ok(code);
            }
        }
        Err(internal())
    }
    fn create(&mut self, protocol: u8, peer: Option<u64>, now: u64) -> Result<Created, Failure> {
        self.clean(now);
        self.peer_creates.retain(|_, times| {
            prune(times, now);
            !times.is_empty()
        });
        if let Some(peer) = peer {
            let active = self.rooms.values().filter(|r| r.peer == Some(peer)).count();
            if active >= 2
                || self.peer_creates.get(&peer).is_some_and(|t| t.len() >= 2)
                || (!self.peer_creates.contains_key(&peer) && self.peer_creates.len() >= 1024)
            {
                return Err((StatusCode::TOO_MANY_REQUESTS, "Room creation limit reached"));
            }
        }
        if self.rooms.len() >= MAX_ROOMS || self.creates.len() >= 16 {
            return Err((StatusCode::TOO_MANY_REQUESTS, "Room capacity reached"));
        }
        let room_id = crate::relay::room_id().map_err(|_| internal())?;
        let created = Created {
            room: room_id.clone(),
            host: Secret::random().map_err(|_| internal())?,
            invite: Secret::random().map_err(|_| internal())?,
        };
        let short_code = peer.map(|_| self.new_short_code()).transpose()?;
        self.rooms.insert(
            room_id,
            Room {
                protocol,
                open: true,
                next_member: 1,
                peer,
                host: created.host.clone(),
                invite: created.invite.clone(),
                short_code,
                born: now,
                updated: now,
                timeline: None,
                members: Vec::new(),
                pending: Vec::new(),
                next_request: 1,
                skip: None,
            },
        );
        self.creates.push_back(now);
        if let Some(peer) = peer {
            self.peer_creates.entry(peer).or_default().push_back(now);
        }
        Ok(created)
    }
    fn admit(room: &mut Room, now: u64) -> Result<Joined, Failure> {
        if !room.open {
            return Err((StatusCode::CONFLICT, "Room is locked"));
        }
        if room.members.len() >= 8 {
            return Err((StatusCode::CONFLICT, "Room is full"));
        }
        let token = Secret::random().map_err(|_| internal())?;
        let id = room.next_member;
        room.next_member = id
            .checked_add(1)
            .filter(|id| *id <= i32::MAX as u32)
            .ok_or(internal())?;
        room.members.push(Member {
            id,
            token: token.clone(),
            seen: now,
            requests: VecDeque::new(),
        });
        Ok(Joined { member: token })
    }
    fn join_short(&mut self, code: &str, peer: u64, now: u64) -> Result<(CodeJoined, u8), Failure> {
        if self.code_window != now / 60_000 {
            self.code_window = now / 60_000;
            self.code_attempts = 0;
            self.peer_code_attempts.clear();
        }
        if self.code_attempts >= 96 {
            return Err((StatusCode::TOO_MANY_REQUESTS, "Join code limit reached"));
        }
        let attempts = self.peer_code_attempts.entry(peer).or_default();
        if *attempts >= 6 {
            return Err((StatusCode::TOO_MANY_REQUESTS, "Join code limit reached"));
        }
        *attempts += 1;
        self.code_attempts += 1;
        self.clean(now);
        let (id, room) = self
            .rooms
            .iter_mut()
            .find(|(_, room)| room.short_code.as_deref() == Some(code))
            .ok_or(MISSING)?;
        let joined = Self::admit(room, now)?;
        Ok((
            CodeJoined {
                room: id.clone(),
                member: joined.member,
            },
            room.protocol,
        ))
    }
    /// Look up a room, enforcing that the request speaks the room's protocol.
    fn room(&mut self, id: &str, protocol: u8, now: u64) -> Result<&mut Room, Failure> {
        self.clean(now);
        let room = self.rooms.get_mut(id).ok_or(MISSING)?;
        if room.protocol != protocol {
            return Err((StatusCode::UPGRADE_REQUIRED, "Room protocol mismatch"));
        }
        Ok(room)
    }
    fn hosted(
        &mut self,
        id: &str,
        protocol: u8,
        host: &Secret,
        now: u64,
    ) -> Result<&mut Room, Failure> {
        let room = self.room(id, protocol, now)?;
        if !room.host.matches(host) {
            return Err(MISSING);
        }
        Ok(room)
    }
}

fn member<'a>(room: &'a mut Room, token: &Secret) -> Result<&'a mut Member, Failure> {
    room.members
        .iter_mut()
        .find(|m| m.token.matches(token))
        .ok_or(MISSING)
}
fn limit_member(member: &mut Member, now: u64) -> Result<(), Failure> {
    prune(&mut member.requests, now);
    if member.requests.len() >= 5 {
        return Err((StatusCode::TOO_MANY_REQUESTS, "Jam request limit reached"));
    }
    member.requests.push_back(now);
    member.seen = now;
    Ok(())
}

struct Shared {
    store: Mutex<Store>,
    epoch: Instant,
    host: String,
    shared_relay: bool,
    peer_hash: RandomState,
    requests: Mutex<VecDeque<u64>>,
}
impl Shared {
    fn now(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }
    fn with<T>(
        &self,
        action: impl FnOnce(&mut Store, u64) -> Result<T, Failure>,
    ) -> Result<T, Failure> {
        let mut store = self.store.lock().map_err(|_| internal())?;
        action(&mut store, self.now())
    }
}

type Reply = Response<Full<Bytes>>;
fn reply(status: StatusCode, body: Vec<u8>, protocol: Option<u8>) -> Reply {
    let mut response = Response::new(Full::new(Bytes::from(body)));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    headers.insert("x-content-type-options", "nosniff".parse().unwrap());
    if status == StatusCode::UPGRADE_REQUIRED {
        if let Some(p) = protocol {
            headers.insert("x-grove-jam-room-protocol", u16::from(p).into());
        }
    }
    response
}
fn status(status: StatusCode) -> Reply {
    reply(status, Vec::new(), None)
}
fn ok_json(value: &impl serde::Serialize) -> Result<Reply, Failure> {
    let mut response = reply(
        StatusCode::OK,
        serde_json::to_vec(value).map_err(|_| internal())?,
        None,
    );
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    Ok(response)
}

fn token(headers: &hyper::HeaderMap) -> Result<Secret, Failure> {
    let text = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .ok_or(MISSING)?;
    Secret::parse(text).map_err(|_| MISSING)
}
fn checked_peer(headers: &hyper::HeaderMap) -> Result<IpAddr, Failure> {
    if headers.get_all(PEER_HEADER).iter().count() != 1 {
        return Err(MISSING);
    }
    let value = headers
        .get(PEER_HEADER)
        .and_then(|h| h.to_str().ok())
        .ok_or(MISSING)?;
    let ip: IpAddr = value.parse().map_err(|_| MISSING)?;
    if ip.to_string() != value {
        return Err(MISSING);
    }
    Ok(match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(Ipv6Addr::from(u128::from(v6) & (u128::MAX << 64))),
        },
        other => other,
    })
}
fn parse<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, Failure> {
    serde_json::from_slice(body).map_err(|_| (StatusCode::BAD_REQUEST, "Invalid JSON"))
}
fn empty(body: &[u8]) -> Result<(), Failure> {
    let value: Value = parse(body)?;
    if value.as_object().is_some_and(|o| o.is_empty()) {
        Ok(())
    } else {
        Err((StatusCode::BAD_REQUEST, "Invalid JSON"))
    }
}

async fn handle(request: Request<hyper::body::Incoming>, shared: Arc<Shared>) -> Reply {
    let headers = request.headers().clone();
    let host_ok = headers.get_all(header::HOST).iter().count() == 1
        && headers.get(header::HOST).and_then(|h| h.to_str().ok()) == Some(shared.host.as_str());
    if request.method() == hyper::Method::GET && request.uri().path() == "/healthz" && host_ok {
        // Tide's client wakes sleeping relays with this exact body.
        return reply(StatusCode::OK, b"tide-jam-ready-v1".to_vec(), None);
    }
    let protocol = headers
        .get("x-tide-jam-protocol")
        .and_then(|h| h.to_str().ok())
        .and_then(|v| match v {
            "1" => Some(RELAY_TIDE),
            "2" => Some(RELAY_CROSS),
            _ => None,
        });
    let path = request.uri().path().to_owned();
    if (shared.shared_relay && checked_peer(&headers).is_err())
        || (!shared.shared_relay && headers.contains_key(PEER_HEADER))
        || headers.contains_key(header::ORIGIN)
        || headers.contains_key("forwarded")
        || headers.contains_key("x-forwarded-host")
        || headers.contains_key("x-forwarded-proto")
        || !host_ok
        || headers.get_all("x-tide-jam-protocol").iter().count() != 1
        || protocol.is_none()
        || headers.get_all(header::AUTHORIZATION).iter().count() > 1
        || headers.get_all(header::CONTENT_TYPE).iter().count() > 1
        || request.uri().scheme().is_some()
        || request.uri().authority().is_some()
        || path.contains(['%', '\\'])
        || path.contains("//")
        || request.uri().query().is_some()
    {
        return status(StatusCode::FORBIDDEN);
    }
    let protocol = protocol.unwrap_or(RELAY_TIDE);
    {
        let Ok(mut requests) = shared.requests.lock() else {
            return status(StatusCode::SERVICE_UNAVAILABLE);
        };
        let now = shared.now();
        while requests
            .front()
            .is_some_and(|t| now.saturating_sub(*t) >= 1000)
        {
            requests.pop_front();
        }
        if requests.len() >= 512 {
            return status(StatusCode::TOO_MANY_REQUESTS);
        }
        requests.push_back(now);
    }
    let method = request.method().clone();
    if (method == hyper::Method::POST || method == hyper::Method::PUT)
        && headers
            .get(header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            != Some("application/json")
    {
        return status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    let body = match Limited::new(request.into_body(), MAX_BODY).collect().await {
        Ok(body) => body.to_bytes(),
        Err(_) => return status(StatusCode::PAYLOAD_TOO_LARGE),
    };
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let result = route(&shared, &method, &segments, &headers, protocol, &body);
    match result {
        Ok(response) => response,
        Err((code, _)) => reply(code, Vec::new(), Some(room_protocol(&shared, &segments))),
    }
}

fn room_protocol(shared: &Shared, segments: &[&str]) -> u8 {
    segments
        .get(2)
        .and_then(|id| {
            shared
                .store
                .lock()
                .ok()
                .and_then(|s| s.rooms.get(*id).map(|r| r.protocol))
        })
        .unwrap_or(RELAY_CROSS)
}

fn route(
    s: &Shared,
    method: &hyper::Method,
    segments: &[&str],
    headers: &hyper::HeaderMap,
    protocol: u8,
    body: &[u8],
) -> Result<Reply, Failure> {
    use hyper::Method as M;
    let peer = |headers: &hyper::HeaderMap| -> Result<u64, Failure> {
        Ok(s.peer_hash.hash_one(checked_peer(headers)?))
    };
    match (method, segments) {
        (&M::GET, ["v2", "protocols"]) => ok_json(&json!({"protocols":[RELAY_TIDE, RELAY_CROSS]})),
        (&M::POST, ["v1", "rooms"]) => {
            empty(body)?;
            if headers.contains_key(header::AUTHORIZATION) {
                return Err(MISSING);
            }
            let peer = if s.shared_relay {
                Some(peer(headers)?)
            } else {
                None
            };
            ok_json(&s.with(|store, now| store.create(protocol, peer, now))?)
        }
        (&M::POST, ["v1", "join-code"]) => {
            let value: ShortCode = parse(body)?;
            if !s.shared_relay
                || headers.contains_key(header::AUTHORIZATION)
                || !valid_short_code(&value.code)
            {
                return Err(MISSING);
            }
            let peer = peer(headers)?;
            let (joined, room_protocol) =
                s.with(|store, now| store.join_short(&value.code, peer, now))?;
            if room_protocol != protocol {
                // Seat was taken; release it so a mismatched app wastes nothing.
                let _ = s.with(|store, _| {
                    if let Some(room) = store.rooms.get_mut(&joined.room) {
                        room.members.retain(|m| !m.token.matches(&joined.member));
                    }
                    Ok(())
                });
                return Err((StatusCode::UPGRADE_REQUIRED, "Room protocol mismatch"));
            }
            ok_json(&joined)
        }
        (_, ["v1", "rooms", id, rest @ ..]) => {
            if !valid_hex(id, 32) {
                return Err(MISSING);
            }
            let auth = token(headers)?;
            room_route(s, method, id, rest, &auth, protocol, body)
        }
        _ => Err(MISSING),
    }
}

fn room_route(
    s: &Shared,
    method: &hyper::Method,
    id: &str,
    rest: &[&str],
    auth: &Secret,
    protocol: u8,
    body: &[u8],
) -> Result<Reply, Failure> {
    use hyper::Method as M;
    let no_content = || Ok(status(StatusCode::NO_CONTENT));
    match (method, rest) {
        (&M::DELETE, []) => {
            s.with(|store, now| {
                store.hosted(id, protocol, auth, now)?;
                store.rooms.remove(id);
                Ok(())
            })?;
            no_content()
        }
        (&M::GET, ["code"]) => {
            let code = s.with(|store, now| {
                store
                    .hosted(id, protocol, auth, now)?
                    .short_code
                    .clone()
                    .ok_or(MISSING)
            })?;
            ok_json(&ShortCode { code })
        }
        (&M::POST, ["join"]) => {
            empty(body)?;
            ok_json(&s.with(|store, now| {
                let room = store.room(id, protocol, now)?;
                if !room.invite.matches(auth) {
                    return Err(MISSING);
                }
                Store::admit(room, now)
            })?)
        }
        (&M::GET, ["state"]) => {
            let view = s.with(|store, now| {
                let room = store.room(id, protocol, now)?;
                if !room.host.matches(auth) {
                    member(room, auth)?.seen = now;
                }
                Ok(json!({
                    "timeline": room.timeline.as_ref().map(|t| t.json.clone()),
                    "age_ms": now.saturating_sub(room.updated),
                    "members": room.members.len(),
                }))
            })?;
            ok_json(&view)
        }
        (&M::PUT, ["state"]) => {
            let stored = match protocol {
                RELAY_TIDE => {
                    let t: Timeline = parse(body)?;
                    t.validate()
                        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid timeline"))?;
                    Stored {
                        revision: t.revision,
                        play_id: t.play_id,
                        current: t.queue[t.index].as_str().into(),
                        json: serde_json::to_value(&t).map_err(|_| internal())?,
                    }
                }
                _ => {
                    let t: CrossTimeline = parse(body)?;
                    t.validate()
                        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid timeline"))?;
                    Stored {
                        revision: t.revision,
                        play_id: t.play_id,
                        current: t.queue[t.index].uri.clone(),
                        json: serde_json::to_value(&t).map_err(|_| internal())?,
                    }
                }
            };
            s.with(|store, now| {
                let room = store.hosted(id, protocol, auth, now)?;
                if let Some(previous) = &room.timeline {
                    if stored.revision <= previous.revision || stored.play_id < previous.play_id {
                        return Err((StatusCode::CONFLICT, "Outdated timeline"));
                    }
                    if now.saturating_sub(room.updated) < 100 {
                        return Err((StatusCode::TOO_MANY_REQUESTS, "Publish rate exceeded"));
                    }
                }
                if room
                    .skip
                    .as_ref()
                    .is_some_and(|k| k.play_id != stored.play_id || k.current != stored.current)
                {
                    room.skip = None;
                }
                room.timeline = Some(stored);
                room.updated = now;
                Ok(())
            })?;
            no_content()
        }
        (&M::GET, ["members"]) => {
            let view = s.with(|store, now| {
                let room = store.hosted(id, protocol, auth, now)?;
                Ok(HostView {
                    members: room.members.iter().map(|m| m.id).collect(),
                    open: room.open,
                })
            })?;
            ok_json(&view)
        }
        (&M::GET, ["members", "self"]) => {
            let value = s.with(|store, now| {
                let room = store.room(id, protocol, now)?;
                Ok(ListenerId {
                    id: member(room, auth)?.id,
                })
            })?;
            ok_json(&value)
        }
        (&M::DELETE, ["members", "self"]) => {
            s.with(|store, now| {
                let room = store.room(id, protocol, now)?;
                let index = room
                    .members
                    .iter()
                    .position(|m| m.token.matches(auth))
                    .ok_or(MISSING)?;
                let gone = room.members.remove(index).id;
                room.pending.retain(|p| p.member != gone);
                if room.skip.as_ref().is_some_and(|k| k.member == gone) {
                    room.skip = None;
                }
                Ok(())
            })?;
            no_content()
        }
        (&M::DELETE, ["members", member_id]) => {
            let target: u32 = member_id.parse().map_err(|_| MISSING)?;
            let invite = s.with(|store, now| {
                let room = store.hosted(id, protocol, auth, now)?;
                let index = room
                    .members
                    .iter()
                    .position(|m| m.id == target)
                    .ok_or(MISSING)?;
                let short = room.short_code.is_some();
                room.members.remove(index);
                room.pending.retain(|p| p.member != target);
                if room.skip.as_ref().is_some_and(|k| k.member == target) {
                    room.skip = None;
                }
                let code = short.then(|| store.new_short_code()).transpose()?;
                let room = store.rooms.get_mut(id).ok_or(MISSING)?;
                room.invite = Secret::random().map_err(|_| internal())?;
                room.short_code = code;
                Ok(room.invite.clone())
            })?;
            ok_json(&json!({ "invite": invite }))
        }
        (&M::PUT, ["admission"]) => {
            let value: Value = parse(body)?;
            let open = match value.as_object() {
                Some(o) if o.len() == 1 => o.get("open").and_then(Value::as_bool),
                _ => None,
            }
            .ok_or((StatusCode::BAD_REQUEST, "Invalid JSON"))?;
            s.with(|store, now| {
                store.hosted(id, protocol, auth, now)?.open = open;
                Ok(())
            })?;
            no_content()
        }
        (&M::POST, ["invite"]) => {
            empty(body)?;
            let invite = s.with(|store, now| {
                let short = store.hosted(id, protocol, auth, now)?.short_code.is_some();
                let code = short.then(|| store.new_short_code()).transpose()?;
                let room = store.rooms.get_mut(id).ok_or(MISSING)?;
                room.invite = Secret::random().map_err(|_| internal())?;
                room.short_code = code;
                Ok(room.invite.clone())
            })?;
            ok_json(&json!({ "invite": invite }))
        }
        (&M::POST, ["requests"]) => {
            let item = match protocol {
                RELAY_TIDE => json!({ "video": parse::<Enqueue>(body)?.video }),
                _ => {
                    let value: CrossEnqueue = parse(body)?;
                    value
                        .track
                        .validate()
                        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid track"))?;
                    json!({ "track": value.track })
                }
            };
            s.with(|store, now| {
                let room = store.room(id, protocol, now)?;
                if room.pending.len() >= MAX_REQUESTS {
                    return Err((StatusCode::TOO_MANY_REQUESTS, "Queue request limit reached"));
                }
                let next = room.next_request;
                let who = member(room, auth)?;
                limit_member(who, now)?;
                let member_id = who.id;
                room.next_request = next.checked_add(1).ok_or(internal())?;
                room.pending.push(PendingRequest {
                    member: member_id,
                    id: next,
                    item,
                });
                Ok(())
            })?;
            no_content()
        }
        (&M::GET, ["requests"]) => {
            let list = s.with(|store, now| {
                let room = store.hosted(id, protocol, auth, now)?;
                Ok(room
                    .pending
                    .iter()
                    .map(|p| {
                        let mut value = p.item.clone();
                        value["id"] = json!(p.id);
                        value
                    })
                    .collect::<Vec<_>>())
            })?;
            ok_json(&list)
        }
        (&M::DELETE, ["requests", request]) => {
            let request: u64 = request.parse().map_err(|_| MISSING)?;
            s.with(|store, now| {
                store
                    .hosted(id, protocol, auth, now)?
                    .pending
                    .retain(|p| p.id != request);
                Ok(())
            })?;
            no_content()
        }
        (&M::POST, ["skip"]) => {
            let (play_id, current, json) = match protocol {
                RELAY_TIDE => {
                    let skip: Skip = parse(body)?;
                    (
                        skip.play_id,
                        skip.video.as_str().to_owned(),
                        serde_json::to_value(&skip).map_err(|_| internal())?,
                    )
                }
                _ => {
                    let skip: CrossSkip = parse(body)?;
                    (
                        skip.play_id,
                        skip.uri.clone(),
                        serde_json::to_value(&skip).map_err(|_| internal())?,
                    )
                }
            };
            s.with(|store, now| {
                let room = store.room(id, protocol, now)?;
                let matches = play_id != 0
                    && room
                        .timeline
                        .as_ref()
                        .is_some_and(|t| t.play_id == play_id && t.current == current);
                let who_index = room
                    .members
                    .iter()
                    .position(|m| m.token.matches(auth))
                    .ok_or(MISSING)?;
                if !matches {
                    return Err((StatusCode::CONFLICT, "Song has already changed"));
                }
                if room
                    .skip
                    .as_ref()
                    .is_some_and(|k| k.play_id == play_id && k.current == current)
                {
                    return Ok(());
                }
                let who = &mut room.members[who_index];
                limit_member(who, now)?;
                let member_id = who.id;
                room.skip = Some(PendingSkip {
                    member: member_id,
                    play_id,
                    current,
                    json,
                });
                Ok(())
            })?;
            no_content()
        }
        (&M::GET, ["skip"]) => {
            let skip = s.with(|store, now| {
                Ok(store
                    .hosted(id, protocol, auth, now)?
                    .skip
                    .as_ref()
                    .map(|k| k.json.clone()))
            })?;
            ok_json(&skip)
        }
        _ => Err(MISSING),
    }
}

/// Serve until `stop` changes. The listener must be bound to 127.0.0.1.
pub async fn serve(
    listener: TcpListener,
    mut stop: tokio::sync::watch::Receiver<bool>,
    mode: Mode,
) -> std::io::Result<()> {
    let address = listener.local_addr()?;
    if address.ip() != std::net::Ipv4Addr::LOCALHOST {
        return Err(std::io::Error::other(
            "The jam relay backend only listens on 127.0.0.1; put a TLS proxy in front.",
        ));
    }
    let (host, shared_relay) = match &mode {
        Mode::Local => (address.to_string(), false),
        Mode::Shared(endpoint) => {
            if !endpoint.is_remote() {
                return Err(std::io::Error::other(
                    "Shared relays need an HTTPS endpoint.",
                ));
            }
            (endpoint.authority().to_owned(), true)
        }
    };
    let shared = Arc::new(Shared {
        store: Mutex::new(Store::default()),
        epoch: Instant::now(),
        host,
        shared_relay,
        peer_hash: RandomState::new(),
        requests: Mutex::new(VecDeque::new()),
    });
    let slots = Arc::new(Semaphore::new(16));
    let mut tasks = tokio::task::JoinSet::new();
    let mut cleanup = tokio::time::interval(Duration::from_secs(10));
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            _ = cleanup.tick() => { let _ = shared.with(|store, now| { store.clean(now); Ok(()) }); },
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let Ok(permit) = slots.clone().try_acquire_owned() else { drop(stream); continue; };
                while tasks.len() >= 32 { let _ = tasks.join_next().await; }
                let state = shared.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    let service = hyper::service::service_fn(move |request| {
                        let state = state.clone();
                        async move { Ok::<_, std::convert::Infallible>(handle(request, state).await) }
                    });
                    let connection = hyper::server::conn::http1::Builder::new()
                        .timer(TokioTimer::new())
                        .header_read_timeout(Duration::from_secs(2))
                        .keep_alive(false)
                        .max_buf_size(MAX_BODY)
                        .serve_connection(TokioIo::new(stream), service);
                    let _ = tokio::time::timeout(Duration::from_secs(4), connection).await;
                });
            }
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rooms_are_bound_to_their_protocol_and_rate_limited_per_peer() {
        let mut store = Store::default();
        let tide = store.create(RELAY_TIDE, None, 0).unwrap();
        assert!(store.room(&tide.room, RELAY_CROSS, 1).is_err());
        assert_eq!(
            store.room(&tide.room, RELAY_CROSS, 1).err().unwrap().0,
            StatusCode::UPGRADE_REQUIRED
        );
        assert!(store.room(&tide.room, RELAY_TIDE, 1).is_ok());
        let a = store.create(RELAY_CROSS, Some(7), 2).unwrap();
        store.create(RELAY_CROSS, Some(7), 3).unwrap();
        assert!(store.create(RELAY_CROSS, Some(7), 4).is_err());
        assert!(store.create(RELAY_CROSS, Some(8), 4).is_ok());
        store.rooms.remove(&a.room);
        assert!(
            store.create(RELAY_CROSS, Some(7), 5).is_err(),
            "Closing does not reset the rate"
        );
    }
    #[test]
    fn idle_members_and_rooms_expire() {
        let mut store = Store::default();
        let room = store.create(RELAY_CROSS, None, 0).unwrap();
        let r = store.room(&room.room, RELAY_CROSS, 0).unwrap();
        Store::admit(r, 0).unwrap();
        store.clean(MEMBER_IDLE_MS);
        assert!(store.rooms[&room.room].members.is_empty());
        store.clean(ROOM_IDLE_MS);
        assert!(store.rooms.is_empty());
    }
}
