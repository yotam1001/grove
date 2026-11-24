//! Tide's HTTP jam relay protocol, as a Grove client.
//!
//! Protocol 1 mirrors Tide `experiments/jam/src/{protocol,endpoint,client}.rs`
//! at [`crate::version::TIDE_JAM_REV`] byte for byte on the wire: the same
//! routes, headers, JSON shapes, limits and canonical endpoint rules. The
//! interop tests in `tests/tide_interop.rs` run Tide's own relay and client
//! against this code. Protocol 2 is the cross-service extension served by
//! `grove-jam-relay`: the queue holds provider URIs with title, artist,
//! duration and ISRC instead of bare YouTube IDs.
//!
//! Security properties kept from Tide: HTTPS with certificate verification
//! (rustls, no bypass) or literal `http://127.0.0.1:port` for local labs; no
//! redirects or proxies; bounded bodies and timeouts; capabilities only in an
//! Authorization header, never in URLs; Debug output redacts them.
use crate::version::{RELAY_CROSS, RELAY_TIDE};
use serde::{de, de::DeserializeOwned, Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, time::Duration};
use zeroize::Zeroizing;

pub const MAX_QUEUE: usize = 2000;
pub const MAX_BODY: usize = 32 * 1024;
pub const MAX_REQUESTS: usize = 32;
pub const MAX_POSITION_MS: u64 = 24 * 60 * 60 * 1000;
pub const STALE_MS: u64 = 6_000;
/// Tide's free shared relay (Render free tier; sleeps when idle).
pub const SHARED_RELAY: &str = "https://tide-jam-trial.onrender.com";
/// Cross-service queues carry metadata, so they are shorter than Tide's.
pub const MAX_CROSS_QUEUE: usize = 100;

#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
#[serde(transparent)]
pub struct VideoId(String);
impl VideoId {
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        if value.len() != 11
            || !value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        {
            return Err("Jam accepts only YouTube video IDs, never URLs or files");
        }
        Ok(Self(value.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for VideoId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(de::Error::custom)
    }
}

/// A room capability (64 lowercase hex). Never printed.
#[derive(Clone)]
pub struct Secret(Zeroizing<String>);
impl Secret {
    pub fn random() -> Result<Self, &'static str> {
        let mut bytes = Zeroizing::new([0u8; 32]);
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), bytes.as_mut())
            .map_err(|_| "Secure randomness unavailable")?;
        Self::parse(&hex(bytes.as_ref()))
    }
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        if !valid_hex(value, 64) {
            return Err("Invalid room capability");
        }
        Ok(Self(Zeroizing::new(value.into())))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
    pub fn matches(&self, other: &Self) -> bool {
        let (a, b) = (self.0.as_bytes(), other.0.as_bytes());
        a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[room capability redacted]")
    }
}
impl Serialize for Secret {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.expose())
    }
}
impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Zeroizing::new(String::deserialize(deserializer)?);
        Self::parse(&value).map_err(de::Error::custom)
    }
}
pub fn valid_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub fn room_id() -> Result<String, &'static str> {
    let mut bytes = [0u8; 16];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes)
        .map_err(|_| "Secure randomness unavailable")?;
    Ok(hex(&bytes))
}
pub fn valid_short_code(code: &str) -> bool {
    code.len() == 4 && code.bytes().all(|byte| byte.is_ascii_uppercase())
}

// ---- Protocol 1 (Tide) -------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Timeline {
    pub revision: u64,
    pub play_id: u64,
    pub queue: Vec<VideoId>,
    pub index: usize,
    pub position_ms: u64,
    pub paused: bool,
    pub loading: bool,
}
impl Timeline {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.queue.is_empty()
            || self.queue.len() > MAX_QUEUE
            || self.index >= self.queue.len()
            || self.position_ms > MAX_POSITION_MS
            || self.revision == 0
            || self.play_id == 0
        {
            return Err("Invalid Jam timeline");
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Skip {
    pub play_id: u64,
    pub video: VideoId,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Enqueue {
    pub video: VideoId,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueRequest {
    pub id: u64,
    pub video: VideoId,
}

// ---- Protocol 2 (cross-service extension) ------------------------------------

/// A provider identifier plus the metadata a listener on the other service
/// needs to find the same recording. No URLs, artwork or account data.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TrackRef {
    pub uri: String,
    pub title: String,
    pub artist: String,
    pub duration_ms: u64,
    #[serde(default)]
    pub isrc: String,
}
impl TrackRef {
    pub fn validate(&self) -> Result<(), &'static str> {
        let text = |s: &str| s.chars().count() <= 200 && !s.chars().any(char::is_control);
        if crate::track_uri(&self.uri).ok().as_deref() != Some(self.uri.as_str())
            || !text(&self.title)
            || !text(&self.artist)
            || self.duration_ms > MAX_POSITION_MS
            || !(self.isrc.is_empty()
                || crate::matching::normalize_isrc(&self.isrc).as_deref()
                    == Some(self.isrc.as_str()))
        {
            return Err("Invalid Jam track");
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CrossTimeline {
    pub revision: u64,
    pub play_id: u64,
    pub queue: Vec<TrackRef>,
    pub index: usize,
    pub position_ms: u64,
    pub paused: bool,
    pub loading: bool,
}
impl CrossTimeline {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.queue.is_empty()
            || self.queue.len() > MAX_CROSS_QUEUE
            || self.index >= self.queue.len()
            || self.position_ms > MAX_POSITION_MS
            || self.revision == 0
            || self.play_id == 0
        {
            return Err("Invalid Jam timeline");
        }
        self.queue.iter().try_for_each(TrackRef::validate)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CrossSkip {
    pub play_id: u64,
    pub uri: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrossEnqueue {
    pub track: TrackRef,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrossRequest {
    pub id: u64,
    pub track: TrackRef,
}

// ---- Shared shapes -----------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct View<T> {
    pub timeline: Option<T>,
    pub age_ms: u64,
    pub members: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Created {
    pub room: String,
    pub host: Secret,
    pub invite: Secret,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Joined {
    pub member: Secret,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invite {
    pub endpoint: String,
    pub room: String,
    pub invite: Secret,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShortCode {
    pub code: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeJoined {
    pub room: String,
    pub member: Secret,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostView {
    pub members: Vec<u32>,
    pub open: bool,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListenerId {
    pub id: u32,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Protocols {
    pub protocols: Vec<u8>,
}

/// Tide's validation of a polled view (members ≤ 8, bounded age).
pub fn valid_view<T>(view: &View<T>, timeline: impl Fn(&T) -> bool) -> bool {
    view.members <= 8
        && view.age_ms <= 2 * 60 * 60 * 1000
        && view.timeline.as_ref().is_none_or(timeline)
}

/// `tide-jam:v1.` (Tide) or `grove-jam:v2.` (cross-service) + base64url(JSON).
impl Invite {
    pub fn to_code(&self, protocol: u8) -> Result<Zeroizing<String>, &'static str> {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        let endpoint = Endpoint::parse(&self.endpoint).map_err(|_| "Invalid invite relay")?;
        if !valid_hex(&self.room, 32)
            || endpoint.as_str() != self.endpoint
            || self.endpoint.len() > 512
        {
            return Err("Invalid Jam invite");
        }
        let bytes = Zeroizing::new(serde_json::to_vec(self).map_err(|_| "Invalid Jam invite")?);
        let prefix = if protocol == RELAY_TIDE {
            "tide-jam:v1."
        } else {
            "grove-jam:v2."
        };
        let result = Zeroizing::new(format!("{prefix}{}", URL_SAFE_NO_PAD.encode(&*bytes)));
        if result.len() > 2048 {
            return Err("Jam invite is too long");
        }
        Ok(result)
    }
    /// Returns the invite and its protocol. Unknown versions ask for an update.
    pub fn from_code(code: &str) -> Result<(Self, u8), String> {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        if code.len() > 2048 {
            return Err("Jam invite is too long".into());
        }
        let code = code.trim();
        let (protocol, encoded) = if let Some(rest) = code.strip_prefix("tide-jam:v1.") {
            (RELAY_TIDE, rest)
        } else if let Some(rest) = code.strip_prefix("grove-jam:v2.") {
            (RELAY_CROSS, rest)
        } else if let Some(version) = code
            .strip_prefix("tide-jam:v")
            .or_else(|| code.strip_prefix("grove-jam:v"))
            .and_then(|rest| rest.split('.').next())
            .and_then(|v| v.parse::<u8>().ok())
        {
            return Err(crate::version::relay_mismatch(version.max(RELAY_CROSS + 1)));
        } else {
            return Err("Paste a Tide or Grove jam invite".into());
        };
        let bytes = Zeroizing::new(
            URL_SAFE_NO_PAD
                .decode(encoded)
                .map_err(|_| "Invalid Jam invite code")?,
        );
        let invite: Self = serde_json::from_slice(&bytes).map_err(|_| "Invalid Jam invite code")?;
        invite.to_code(protocol)?;
        Ok((invite, protocol))
    }
}

// ---- Endpoint ----------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    url: String,
    authority: String,
    remote: bool,
}
impl Endpoint {
    /// Tide's canonical rule: `https://host[:port]` or `http://127.0.0.1:port`,
    /// with no repairs, credentials, paths, queries or default ports.
    pub fn parse(input: &str) -> Result<Self, Error> {
        if input.len() > 2048 || !input.is_ascii() || input.contains(['\\', '@']) {
            return Err(Error::Invalid);
        }
        let url = url::Url::parse(input).map_err(|_| Error::Invalid)?;
        let remote = url.scheme() == "https";
        if !(remote
            || (url.scheme() == "http"
                && url.host_str() == Some("127.0.0.1")
                && url.port().is_some()))
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port() == Some(0)
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::Invalid);
        }
        let canonical = url.as_str().strip_suffix('/').ok_or(Error::Invalid)?;
        if input != canonical && input != url.as_str() {
            return Err(Error::Invalid);
        }
        Ok(Self {
            url: canonical.into(),
            authority: url[url::Position::BeforeHost..url::Position::AfterPort].into(),
            remote,
        })
    }
    pub fn as_str(&self) -> &str {
        &self.url
    }
    pub fn authority(&self) -> &str {
        &self.authority
    }
    pub fn is_remote(&self) -> bool {
        self.remote
    }
}

// ---- Client ------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Unavailable,
    Invalid,
    Transport,
    Limited,
    /// The relay or room speaks another protocol (the one it needs).
    Mismatch(u8),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => f.write_str("Jam room or membership is unavailable"),
            Self::Invalid => f.write_str("Jam protocol validation failed"),
            Self::Transport => f.write_str("Jam relay did not respond safely"),
            Self::Limited => f.write_str("Jam server capacity or request limit reached"),
            Self::Mismatch(n) => f.write_str(&crate::version::relay_mismatch(*n)),
        }
    }
}
impl std::error::Error for Error {}

pub struct Client {
    endpoint: Endpoint,
    http: reqwest::Client,
    protocol: u8,
}
impl Client {
    pub fn new(endpoint: &str, protocol: u8) -> Result<Self, Error> {
        if protocol != RELAY_TIDE && protocol != RELAY_CROSS {
            return Err(Error::Invalid);
        }
        let endpoint = Endpoint::parse(endpoint)?;
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .pool_max_idle_per_host(0)
            .https_only(endpoint.is_remote())
            .build()
            .map_err(|_| Error::Transport)?;
        Ok(Self {
            endpoint,
            http,
            protocol,
        })
    }
    pub fn endpoint(&self) -> &str {
        self.endpoint.as_str()
    }
    pub fn protocol(&self) -> u8 {
        self.protocol
    }
    /// Tide's free relay sleeps; wake it with a credential-free request first.
    pub fn needs_wakeup(&self) -> bool {
        self.endpoint.is_remote() && self.endpoint.authority().ends_with(".onrender.com")
    }
    pub async fn ready(&self) -> Result<(), Error> {
        let bytes = self.request("GET", "/healthz", None, Vec::new()).await?;
        if bytes == b"tide-jam-ready-v1" {
            Ok(())
        } else {
            Err(Error::Invalid)
        }
    }
    /// Which relay protocols the server supports. Tide's relay has no such
    /// route and answers 404: that means protocol 1 only.
    pub async fn protocols(&self) -> Result<Vec<u8>, Error> {
        let probe = Self {
            endpoint: self.endpoint.clone(),
            http: self.http.clone(),
            protocol: RELAY_TIDE,
        };
        match probe
            .json::<Protocols>("GET", "/v2/protocols", None, Vec::new())
            .await
        {
            Ok(value) => Ok(value.protocols.into_iter().take(8).collect()),
            Err(Error::Unavailable) => Ok(vec![RELAY_TIDE]),
            Err(error) => Err(error),
        }
    }
    async fn request(
        &self,
        method: &str,
        path: &str,
        auth: Option<&Secret>,
        body: Vec<u8>,
    ) -> Result<Vec<u8>, Error> {
        if body.len() > MAX_BODY {
            return Err(Error::Invalid);
        }
        let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(|_| Error::Invalid)?;
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.endpoint.as_str()))
            .header("x-tide-jam-protocol", self.protocol.to_string())
            .header("content-type", "application/json")
            .body(body);
        if let Some(auth) = auth {
            let value = Zeroizing::new(format!("Bearer {}", auth.expose()));
            let mut header =
                reqwest::header::HeaderValue::from_str(&value).map_err(|_| Error::Invalid)?;
            header.set_sensitive(true);
            request = request.header(reqwest::header::AUTHORIZATION, header);
        }
        let mut response = request.send().await.map_err(|_| Error::Transport)?;
        let status = response.status().as_u16();
        if status == 426 {
            let required = response
                .headers()
                .get("x-grove-jam-room-protocol")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok())
                .unwrap_or(RELAY_CROSS + 1);
            return Err(Error::Mismatch(required));
        }
        match status {
            // Tide's relay rejects an unknown protocol header with 403.
            403 if self.protocol != RELAY_TIDE => return Err(Error::Mismatch(self.protocol)),
            404 | 401 | 403 => return Err(Error::Unavailable),
            429 | 409 => return Err(Error::Limited),
            200..=299 => {}
            _ => return Err(Error::Transport),
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::Transport)? {
            if bytes.len() + chunk.len() > MAX_BODY {
                return Err(Error::Transport);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
    async fn json<T: DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        auth: Option<&Secret>,
        body: Vec<u8>,
    ) -> Result<T, Error> {
        let bytes = Zeroizing::new(self.request(method, path, auth, body).await?);
        serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)
    }
    fn path(room: &str, suffix: &str) -> Result<String, Error> {
        if !valid_hex(room, 32) {
            return Err(Error::Invalid);
        }
        Ok(format!("/v1/rooms/{room}{suffix}"))
    }
    fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, Error> {
        serde_json::to_vec(value).map_err(|_| Error::Invalid)
    }
    /// Shared HTTPS relay (no operator key) or a literal-loopback lab relay.
    pub async fn create(&self) -> Result<Created, Error> {
        let result: Created = self.json("POST", "/v1/rooms", None, b"{}".to_vec()).await?;
        if !valid_hex(&result.room, 32) || result.host.matches(&result.invite) {
            return Err(Error::Invalid);
        }
        Ok(result)
    }
    pub async fn room_code(&self, room: &str, host: &Secret) -> Result<String, Error> {
        let result: ShortCode = self
            .json("GET", &Self::path(room, "/code")?, Some(host), Vec::new())
            .await?;
        if !valid_short_code(&result.code) {
            return Err(Error::Invalid);
        }
        Ok(result.code)
    }
    pub async fn join_code(&self, code: &str) -> Result<CodeJoined, Error> {
        if !valid_short_code(code) {
            return Err(Error::Invalid);
        }
        let body = Self::encode(&ShortCode { code: code.into() })?;
        let result: CodeJoined = self.json("POST", "/v1/join-code", None, body).await?;
        if !valid_hex(&result.room, 32) {
            return Err(Error::Invalid);
        }
        Ok(result)
    }
    pub async fn join(&self, invite: &Invite) -> Result<Joined, Error> {
        // An invite cannot silently redirect an already selected client.
        if Endpoint::parse(&invite.endpoint)? != self.endpoint {
            return Err(Error::Invalid);
        }
        self.json(
            "POST",
            &Self::path(&invite.room, "/join")?,
            Some(&invite.invite),
            b"{}".to_vec(),
        )
        .await
    }
    pub async fn listener_id(&self, room: &str, member: &Secret) -> Result<u32, Error> {
        let value: ListenerId = self
            .json(
                "GET",
                &Self::path(room, "/members/self")?,
                Some(member),
                Vec::new(),
            )
            .await?;
        if value.id == 0 || value.id > i32::MAX as u32 {
            return Err(Error::Invalid);
        }
        Ok(value.id)
    }
    pub async fn publish<T: Serialize>(
        &self,
        room: &str,
        host: &Secret,
        timeline: &T,
    ) -> Result<(), Error> {
        let bytes = Self::encode(timeline)?;
        self.request("PUT", &Self::path(room, "/state")?, Some(host), bytes)
            .await
            .map(|_| ())
    }
    /// Poll the room; also returns the measured round trip in milliseconds.
    pub async fn poll<T: DeserializeOwned>(
        &self,
        room: &str,
        token: &Secret,
    ) -> Result<(View<T>, u64), Error> {
        let began = std::time::Instant::now();
        let view: View<T> = self
            .json("GET", &Self::path(room, "/state")?, Some(token), Vec::new())
            .await?;
        if view.members > 8 || view.age_ms > 2 * 60 * 60 * 1000 {
            return Err(Error::Invalid);
        }
        Ok((view, began.elapsed().as_millis() as u64))
    }
    pub async fn host_view(&self, room: &str, host: &Secret) -> Result<HostView, Error> {
        let view: HostView = self
            .json(
                "GET",
                &Self::path(room, "/members")?,
                Some(host),
                Vec::new(),
            )
            .await?;
        if view.members.len() > 8 {
            return Err(Error::Invalid);
        }
        Ok(view)
    }
    /// Sent once: retrying a POST after a lost reply could duplicate the song.
    pub async fn enqueue<T: Serialize>(
        &self,
        room: &str,
        member: &Secret,
        request: &T,
    ) -> Result<(), Error> {
        let body = Self::encode(request)?;
        self.request("POST", &Self::path(room, "/requests")?, Some(member), body)
            .await
            .map(|_| ())
    }
    pub async fn requests<T: DeserializeOwned>(
        &self,
        room: &str,
        host: &Secret,
    ) -> Result<Vec<T>, Error> {
        let requests: Vec<T> = self
            .json(
                "GET",
                &Self::path(room, "/requests")?,
                Some(host),
                Vec::new(),
            )
            .await?;
        if requests.len() > MAX_REQUESTS {
            return Err(Error::Invalid);
        }
        Ok(requests)
    }
    pub async fn acknowledge(&self, room: &str, host: &Secret, id: u64) -> Result<(), Error> {
        if id == 0 {
            return Err(Error::Invalid);
        }
        self.request(
            "DELETE",
            &Self::path(room, &format!("/requests/{id}"))?,
            Some(host),
            Vec::new(),
        )
        .await
        .map(|_| ())
    }
    pub async fn request_skip<T: Serialize>(
        &self,
        room: &str,
        member: &Secret,
        skip: &T,
    ) -> Result<(), Error> {
        let body = Self::encode(skip)?;
        self.request("POST", &Self::path(room, "/skip")?, Some(member), body)
            .await
            .map(|_| ())
    }
    pub async fn skip_request<T: DeserializeOwned>(
        &self,
        room: &str,
        host: &Secret,
    ) -> Result<Option<T>, Error> {
        self.json("GET", &Self::path(room, "/skip")?, Some(host), Vec::new())
            .await
    }
    pub async fn close(&self, room: &str, host: &Secret) -> Result<(), Error> {
        self.request("DELETE", &Self::path(room, "")?, Some(host), Vec::new())
            .await
            .map(|_| ())
    }
    pub async fn leave(&self, room: &str, member: &Secret) -> Result<(), Error> {
        self.request(
            "DELETE",
            &Self::path(room, "/members/self")?,
            Some(member),
            Vec::new(),
        )
        .await
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoints_follow_tides_canonical_rules() {
        for good in [
            "https://jam.example.test",
            "https://jam.example.test/",
            "https://jam.example.test:8443",
            "http://127.0.0.1:8765",
        ] {
            assert!(Endpoint::parse(good).is_ok(), "{good}");
        }
        for bad in [
            "http://jam.example.test",
            "http://192.168.1.2:80",
            "https://user@jam.example.test",
            "https://JAM.example.test",
            "https://jam.example.test:443",
            "https://jam.example.test/path",
            "https://jam.example.test/?q",
            " https://jam.example.test",
            "http://127.0.0.1",
        ] {
            assert!(Endpoint::parse(bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn invite_codes_round_trip_and_name_versions() {
        let invite = Invite {
            endpoint: "https://jam.example.test".into(),
            room: "0123456789abcdef0123456789abcdef".into(),
            invite: Secret::random().unwrap(),
        };
        let tide = invite.to_code(RELAY_TIDE).unwrap();
        assert!(tide.starts_with("tide-jam:v1."));
        assert_eq!(Invite::from_code(&tide).unwrap().1, RELAY_TIDE);
        let cross = invite.to_code(RELAY_CROSS).unwrap();
        assert!(cross.starts_with("grove-jam:v2."));
        assert_eq!(Invite::from_code(&cross).unwrap().1, RELAY_CROSS);
        assert!(Invite::from_code("tide-jam:v7.abc")
            .unwrap_err()
            .contains("Update Grove"));
        assert!(Invite::from_code("hello").is_err());
        assert!(!format!("{:?}", invite.invite).contains(invite.invite.expose()));
    }
    #[test]
    fn cross_tracks_are_validated() {
        let mut track = TrackRef {
            uri: "spotify:track:2JRo0gjbX4GrCqBYdRohoo".into(),
            title: "Song".into(),
            artist: "Artist".into(),
            duration_ms: 200_000,
            isrc: "USRC17607839".into(),
        };
        assert!(track.validate().is_ok());
        track.isrc = "usrc17607839".into();
        assert!(track.validate().is_err(), "ISRCs travel normalized");
        track.isrc.clear();
        track.uri = "https://evil.test".into();
        assert!(track.validate().is_err());
        assert!(serde_json::from_str::<TrackRef>(
            r#"{"uri":"youtube:abcdefghijk","title":"","artist":"","duration_ms":0,"stream":"x"}"#
        )
        .is_err());
    }
}
