//! Encrypted nearby-room channel keyed from the invite token (jam protocol 3).
//!
//! Handshake (one plaintext JSON line each way, then length-prefixed AEAD frames):
//!
//! ```text
//! C -> S  {"kind":"hello","app":"grove","protocol":3,"min_protocol":3,"key":B64(X25519 pub)}
//! S -> C  {"kind":"hello","app":"grove","protocol":3,"min_protocol":3,"key":B64(X25519 pub)}
//!         or {"kind":"error","app":..,"protocol":..,"min_protocol":..,"message":..}
//! psk  = HKDF-SHA256(salt="grove-jam psk v1", ikm=invite token, info="psk", 32 bytes)
//! th   = SHA-256(client hello line || server hello line)   (exact bytes incl. '\n')
//! prk  = HKDF-Extract(salt=psk, ikm=X25519(eph_c, eph_s))
//! k_cs = HKDF-Expand(prk, "grove-jam/3 c2s" || th)   ChaCha20-Poly1305
//! k_sc = HKDF-Expand(prk, "grove-jam/3 s2c" || th)
//! frame = u32 big-endian length || AEAD(k, nonce = 0u32 || u64 BE counter, json)
//! ```
//!
//! The token itself never crosses the network. A peer without the token cannot
//! derive the keys: the server drops a client whose first frame fails to open,
//! and the client rejects a host whose welcome fails to open. Fresh ephemeral
//! keys give each connection forward secrecy. This is the same shape as Noise
//! `NNpsk0`; Tide must implement this exact derivation to host or join nearby
//! Grove rooms (Tide's current jams are relay-only and use TLS to the relay).
use crate::version::{self, LAN_MIN_PROTOCOL, LAN_PROTOCOL};
use base64::{engine::general_purpose::STANDARD, Engine};
use ring::{
    aead::{self, Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305},
    agreement::{self, EphemeralPrivateKey, UnparsedPublicKey, X25519},
    digest, hkdf,
    rand::SystemRandom,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::time::Duration;
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt,
};
use zeroize::Zeroizing;

pub const MAX_FRAME: usize = 256 * 1024;
const HELLO_LIMIT: usize = 4096;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Hello {
    pub kind: String,
    pub app: String,
    pub protocol: u16,
    pub min_protocol: u16,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
}

/// What the peer announced, for status text ("Grove · protocol 3 · encrypted").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    pub app: String,
    pub protocol: u16,
}

pub struct Sealer {
    key: LessSafeKey,
    counter: u64,
}
pub struct Opener {
    key: LessSafeKey,
    counter: u64,
}

fn nonce(counter: u64) -> Nonce {
    let mut bytes = [0u8; 12];
    bytes[4..].copy_from_slice(&counter.to_be_bytes());
    Nonce::assume_unique_for_key(bytes)
}

impl Sealer {
    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, String> {
        if plaintext.len() > MAX_FRAME {
            return Err("Jam message is too large.".into());
        }
        let mut buffer = plaintext.to_vec();
        self.key
            .seal_in_place_append_tag(nonce(self.counter), Aad::empty(), &mut buffer)
            .map_err(|_| "Could not encrypt the jam update.")?;
        self.counter = self
            .counter
            .checked_add(1)
            .ok_or("Jam connection reached its message limit.")?;
        Ok(buffer)
    }
}
impl Opener {
    pub fn open(&mut self, mut ciphertext: Vec<u8>) -> Result<Vec<u8>, String> {
        let length = self
            .key
            .open_in_place(nonce(self.counter), Aad::empty(), &mut ciphertext)
            .map_err(|_| "This invitation is invalid or expired.")?
            .len();
        ciphertext.truncate(length);
        self.counter = self
            .counter
            .checked_add(1)
            .ok_or("Jam connection reached its message limit.")?;
        Ok(ciphertext)
    }
}

struct Len(usize);
impl hkdf::KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

/// Stretch the invite token into the pre-shared key both sides hold.
pub fn psk(token: &str) -> Zeroizing<[u8; 32]> {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, b"grove-jam psk v1").extract(token.as_bytes());
    let mut out = Zeroizing::new([0u8; 32]);
    prk.expand(&[b"psk"], Len(32))
        .and_then(|okm| okm.fill(out.as_mut()))
        .expect("HKDF output length is valid");
    out
}

fn session_keys(
    psk: &[u8; 32],
    shared: &[u8],
    transcript: &[u8],
) -> Result<(LessSafeKey, LessSafeKey), String> {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, psk).extract(shared);
    let make = |label: &[u8]| -> Result<LessSafeKey, String> {
        let info = [label, transcript];
        let okm = prk
            .expand(&info, &CHACHA20_POLY1305)
            .map_err(|_| "Could not secure the jam connection.")?;
        Ok(LessSafeKey::new(UnboundKey::from(okm)))
    };
    Ok((make(b"grove-jam/3 c2s")?, make(b"grove-jam/3 s2c")?))
}

fn transcript(client: &[u8], server: &[u8]) -> digest::Digest {
    let mut context = digest::Context::new(&digest::SHA256);
    context.update(client);
    context.update(server);
    context.finish()
}

fn ephemeral() -> Result<(EphemeralPrivateKey, String), String> {
    let rng = SystemRandom::new();
    let private = EphemeralPrivateKey::generate(&X25519, &rng)
        .map_err(|_| "Secure randomness is unavailable.")?;
    let public = private
        .compute_public_key()
        .map_err(|_| "Secure randomness is unavailable.")?;
    Ok((private, STANDARD.encode(public.as_ref())))
}

fn agree(private: EphemeralPrivateKey, peer: &str) -> Result<Zeroizing<Vec<u8>>, String> {
    let bytes = STANDARD
        .decode(peer)
        .ok()
        .filter(|b| b.len() == 32)
        .ok_or("Invalid jam handshake.")?;
    agreement::agree_ephemeral(
        private,
        &UnparsedPublicKey::new(&X25519, &bytes),
        |secret| Zeroizing::new(secret.to_vec()),
    )
    .map_err(|_| "Invalid jam handshake.".into())
}

fn hello(app: &str, key: String) -> Hello {
    Hello {
        kind: "hello".into(),
        app: app.into(),
        protocol: LAN_PROTOCOL,
        min_protocol: LAN_MIN_PROTOCOL,
        key,
        message: String::new(),
    }
}

fn line<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| "Could not prepare the jam update.")?;
    bytes.push(b'\n');
    Ok(bytes)
}

async fn write_all<W: AsyncWrite + Unpin>(writer: &mut W, bytes: &[u8]) -> Result<(), String> {
    tokio::time::timeout(Duration::from_secs(2), writer.write_all(bytes))
        .await
        .map_err(|_| "Jam update timed out.")?
        .map_err(|_| "Jam connection lost.".into())
}

/// Bounded newline-delimited reader; returns the exact bytes for the transcript.
pub async fn read_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    limit: usize,
) -> Result<Vec<u8>, String> {
    let mut line = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .await
            .map_err(|_| "Jam connection lost.")?;
        if available.is_empty() {
            return Err("Jam connection closed.".into());
        }
        let newline = available.iter().position(|b| *b == b'\n');
        let count = newline.map_or(available.len(), |n| n + 1);
        if line.len() + count > limit {
            return Err("Jam message is too large.".into());
        }
        line.extend_from_slice(&available[..count]);
        reader.consume(count);
        if newline.is_some() {
            return Ok(line);
        }
    }
}

/// Guest side. Returns keys plus the host's announced app/protocol.
pub async fn client<R, W>(
    reader: &mut R,
    writer: &mut W,
    token: &str,
    app: &str,
) -> Result<(Sealer, Opener, Peer), String>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let (private, key) = ephemeral()?;
    let mine = line(&hello(app, key))?;
    write_all(writer, &mine).await?;
    let theirs = tokio::time::timeout(Duration::from_secs(5), read_line(reader, HELLO_LIMIT))
        .await
        .map_err(|_| "The host did not answer the invitation.")??;
    let value: serde_json::Value =
        serde_json::from_slice(&theirs).map_err(|_| "Invalid jam handshake.")?;
    if value["kind"] != "hello" {
        // A v2 host replies with a plain error and no protocol field.
        let protocol = value["protocol"].as_u64().unwrap_or(2) as u16;
        let min = value["min_protocol"].as_u64().unwrap_or(protocol as u64) as u16;
        if protocol == LAN_PROTOCOL && min <= LAN_PROTOCOL {
            let message = value["message"]
                .as_str()
                .unwrap_or("Could not join the jam.");
            return Err(message.chars().take(200).collect());
        }
        return Err(version::host_mismatch(
            value["app"].as_str().unwrap_or(""),
            protocol,
            min,
        ));
    }
    let peer: Hello = serde_json::from_value(value).map_err(|_| "Invalid jam handshake.")?;
    let protocol = version::negotiate(peer.protocol, peer.min_protocol)
        .ok_or_else(|| version::host_mismatch(&peer.app, peer.protocol, peer.min_protocol))?;
    let shared = agree(private, &peer.key)?;
    let th = transcript(&mine, &theirs);
    let (send, receive) = session_keys(&psk(token), &shared, th.as_ref())?;
    Ok((
        Sealer {
            key: send,
            counter: 0,
        },
        Opener {
            key: receive,
            counter: 0,
        },
        Peer {
            app: peer.app.chars().take(16).collect(),
            protocol,
        },
    ))
}

pub enum Accepted {
    Secure(Box<(Sealer, Opener, Peer)>),
    /// The guest was told why (version mismatch); close the connection.
    Rejected,
}

/// Host side. Old plaintext v2 guests receive a readable update message.
pub async fn server<R, W>(
    reader: &mut R,
    writer: &mut W,
    token: &str,
    app: &str,
) -> Result<Accepted, String>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let theirs = read_line(reader, HELLO_LIMIT).await?;
    let value: serde_json::Value =
        serde_json::from_slice(&theirs).map_err(|_| "Invalid jam handshake.")?;
    if value["kind"] == "join" {
        let protocol = value["version"].as_u64().unwrap_or(0).min(u16::MAX as u64) as u16;
        let message =
            serde_json::json!({"kind":"error","message":version::guest_mismatch(protocol)});
        write_all(writer, &line(&message)?).await?;
        return Ok(Accepted::Rejected);
    }
    let peer: Hello = serde_json::from_value(value).map_err(|_| "Invalid jam handshake.")?;
    if peer.kind != "hello" {
        return Err("Invalid jam handshake.".into());
    }
    let Some(protocol) = version::negotiate(peer.protocol, peer.min_protocol) else {
        let reply = Hello {
            kind: "error".into(),
            message: version::guest_mismatch(peer.protocol),
            ..hello(app, String::new())
        };
        write_all(writer, &line(&reply)?).await?;
        return Ok(Accepted::Rejected);
    };
    let (private, key) = ephemeral()?;
    let shared = agree(private, &peer.key)?;
    let mine = line(&hello(app, key))?;
    write_all(writer, &mine).await?;
    let th = transcript(&theirs, &mine);
    let (receive, send) = session_keys(&psk(token), &shared, th.as_ref())?;
    Ok(Accepted::Secure(Box::new((
        Sealer {
            key: send,
            counter: 0,
        },
        Opener {
            key: receive,
            counter: 0,
        },
        Peer {
            app: peer.app.chars().take(16).collect(),
            protocol,
        },
    ))))
}

pub async fn send<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    sealer: &mut Sealer,
    value: &T,
) -> Result<(), String> {
    let plain =
        Zeroizing::new(serde_json::to_vec(value).map_err(|_| "Could not prepare the jam update.")?);
    let sealed = sealer.seal(&plain)?;
    let mut frame = Vec::with_capacity(sealed.len() + 4);
    frame.extend_from_slice(&(sealed.len() as u32).to_be_bytes());
    frame.extend_from_slice(&sealed);
    write_all(writer, &frame).await
}

pub async fn recv<R: AsyncRead + Unpin, T: DeserializeOwned>(
    reader: &mut R,
    opener: &mut Opener,
) -> Result<T, String> {
    let mut length = [0u8; 4];
    reader
        .read_exact(&mut length)
        .await
        .map_err(|_| "Jam connection closed.")?;
    let length = u32::from_be_bytes(length) as usize;
    if !(aead::MAX_TAG_LEN..=MAX_FRAME + aead::MAX_TAG_LEN).contains(&length) {
        return Err("Jam message is too large.".into());
    }
    let mut buffer = vec![0u8; length];
    reader
        .read_exact(&mut buffer)
        .await
        .map_err(|_| "Jam connection closed.")?;
    let plain = Zeroizing::new(opener.open(buffer)?);
    serde_json::from_slice(&plain).map_err(|_| "Invalid jam message.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{duplex, split, BufReader};

    #[tokio::test]
    async fn same_token_derives_matching_keys_and_hides_plaintext() {
        let token = "abcdefghijklmnopqrstuv";
        let (a, b) = duplex(64 * 1024);
        let (ar, mut aw) = split(a);
        let (br, mut bw) = split(b);
        let guest = tokio::spawn(async move {
            let mut reader = BufReader::new(ar);
            let (mut sealer, mut opener, peer) =
                client(&mut reader, &mut aw, token, "grove").await.unwrap();
            assert_eq!(peer.app, "tide");
            send(
                &mut aw,
                &mut sealer,
                &serde_json::json!({"secret":"queue-title"}),
            )
            .await
            .unwrap();
            let reply: serde_json::Value = recv(&mut reader, &mut opener).await.unwrap();
            reply
        });
        let mut reader = BufReader::new(br);
        let Accepted::Secure(secured) = server(&mut reader, &mut bw, token, "tide").await.unwrap()
        else {
            panic!("rejected")
        };
        let (mut sealer, mut opener, peer) = *secured;
        assert_eq!(peer.protocol, LAN_PROTOCOL);
        let mut head = [0u8; 4];
        reader.read_exact(&mut head).await.unwrap();
        let mut body = vec![0u8; u32::from_be_bytes(head) as usize];
        reader.read_exact(&mut body).await.unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("queue-title"));
        assert!(!String::from_utf8_lossy(&body).contains(token));
        let plain = opener.open(body).unwrap();
        assert!(String::from_utf8_lossy(&plain).contains("queue-title"));
        send(&mut bw, &mut sealer, &serde_json::json!({"ok":true}))
            .await
            .unwrap();
        assert_eq!(guest.await.unwrap()["ok"], true);
    }

    #[tokio::test]
    async fn wrong_token_cannot_open_frames() {
        let (a, b) = duplex(64 * 1024);
        let (ar, mut aw) = split(a);
        let (br, mut bw) = split(b);
        let guest = tokio::spawn(async move {
            let mut reader = BufReader::new(ar);
            let (mut sealer, _, _) =
                client(&mut reader, &mut aw, "wrong-token-wrong-token", "grove")
                    .await
                    .unwrap();
            send(&mut aw, &mut sealer, &serde_json::json!({"kind":"join"}))
                .await
                .unwrap();
        });
        let mut reader = BufReader::new(br);
        let Accepted::Secure(secured) =
            server(&mut reader, &mut bw, "abcdefghijklmnopqrstuv", "grove")
                .await
                .unwrap()
        else {
            panic!("rejected")
        };
        let (_, mut opener, _) = *secured;
        guest.await.unwrap();
        assert!(recv::<_, serde_json::Value>(&mut reader, &mut opener)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn old_plaintext_guest_and_future_protocols_get_update_messages() {
        // A v2 guest sends its token in plain JSON; the host never parses it as a member.
        let (a, b) = duplex(4096);
        let (ar, mut aw) = split(a);
        let (br, mut bw) = split(b);
        aw.write_all(b"{\"kind\":\"join\",\"version\":2,\"token\":\"x\",\"name\":\"Old\"}\n")
            .await
            .unwrap();
        let mut reader = BufReader::new(br);
        assert!(matches!(
            server(&mut reader, &mut bw, "abcdefghijklmnopqrstuv", "grove").await,
            Ok(Accepted::Rejected)
        ));
        let mut old = BufReader::new(ar);
        let reply = read_line(&mut old, 4096).await.unwrap();
        let reply: serde_json::Value = serde_json::from_slice(&reply).unwrap();
        assert!(reply["message"]
            .as_str()
            .unwrap()
            .contains("Update Tide or Grove"));

        // A future host that requires protocol 9 tells this guest to update Grove.
        let (a, b) = duplex(4096);
        let (ar, mut aw) = split(a);
        let (br, mut bw) = split(b);
        let future = tokio::spawn(async move {
            let mut reader = BufReader::new(br);
            let _ = read_line(&mut reader, 4096).await.unwrap();
            let reply = Hello {
                kind: "error".into(),
                app: "tide".into(),
                protocol: 9,
                min_protocol: 9,
                key: String::new(),
                message: "x".into(),
            };
            bw.write_all(&line(&reply).unwrap()).await.unwrap();
        });
        let mut reader = BufReader::new(ar);
        let error = client(&mut reader, &mut aw, "abcdefghijklmnopqrstuv", "grove")
            .await
            .err()
            .unwrap();
        future.await.unwrap();
        assert!(error.contains("Update Grove"), "{error}");

        // An old v2 host answers the hello with a plain error: ask the host to update.
        let (a, b) = duplex(4096);
        let (ar, mut aw) = split(a);
        let (br, mut bw) = split(b);
        let old_host = tokio::spawn(async move {
            let mut reader = BufReader::new(br);
            let _ = read_line(&mut reader, 4096).await.unwrap();
            bw.write_all(
                b"{\"kind\":\"error\",\"message\":\"This invitation is invalid or expired.\"}\n",
            )
            .await
            .unwrap();
        });
        let mut reader = BufReader::new(ar);
        let error = client(&mut reader, &mut aw, "abcdefghijklmnopqrstuv", "grove")
            .await
            .err()
            .unwrap();
        old_host.await.unwrap();
        assert!(error.contains("older jam protocol"), "{error}");
    }

    #[test]
    fn psk_depends_on_token() {
        assert_ne!(*psk("a"), *psk("b"));
        assert_eq!(*psk("same"), *psk("same"));
    }
}
