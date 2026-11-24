# Tide + Grove jams

Tide (YouTube) and Grove (Spotify) can listen together. Each listener plays
music through their own app and account; a room carries song identifiers,
metadata and timing, never audio or credentials. Jams stay free: rooms run
peer to peer on a local network, or through a relay anyone can host.

**Status:** implemented and covered by offline loopback tests. Two-PC,
two-account audible sync and a real public relay deployment are *live check
pending*.

## Room types

| Room | Invite | Transport | Who can host | Who can join |
| --- | --- | --- | --- | --- |
| Nearby | `grove-jam://192.168.x.y:port/<token>` | TCP on a private address, encrypted (protocol 3) | Grove | Grove (Tide needs the same protocol, see below) |
| Tide relay | `tide-jam:v1.…` or a four-letter code | Tide's HTTP relay, protocol 1 (TLS to the relay) | Tide | Tide, Grove |
| Cross-service relay | `grove-jam:v2.…` | Same relay protocol, cross-service timeline (protocol 2) | Grove | Grove (Tide after it adopts protocol 2) |

Any of these can be wrapped in a `grove://join?invite=…` link. The Grove
installer registers that scheme for the current user only; opening a link fills
in the invitation on **Shared jams**, and the listener still chooses **Join**.
If Grove is already open, Windows starts a second window with the link (single-
instance forwarding is not implemented yet).

## Staying compatible with Tide (issue #7, #35)

Tide's jam code lives in `experiments/jam` (crate `tide-jam-lab`) and the app's
`crates/app/src/jam*.rs`. Tide no longer has a LAN room protocol, so Grove's
nearby rooms and Tide's relay rooms are separate paths.

**Sync method: pinned git dev-dependency.** `crates/jam/Cargo.toml` depends on
Tide's `tide-jam-lab` (relay + client) at an exact commit, for tests only:

```toml
tide-jam-lab = { git = "https://github.com/yotam1001/tide-music", rev = "<sha>", default-features = false, features = ["relay"] }
```

Grove's runtime code (`crates/jam/src/relay.rs`) is its own mirror of Tide's
protocol types, endpoint rules and client; nothing from Tide is vendored. The
interop tests in `crates/jam/tests/tide_interop.rs` run in the normal
`cargo test --workspace` CI job:

- a Grove listener follows a Tide host on **Tide's own relay** (snapshot,
  upcoming songs, buffering flag, skip request, leave);
- **Tide's own client** runs the full host/guest script against Grove's relay
  (`grove-jam-relay`), so Tide users can use a Grove-operated relay unchanged;
- a Grove host and Grove listener share a protocol-2 room, and Tide's client is
  refused from it;
- hosting a protocol-2 room on a Tide-only relay gives the "update the relay"
  message.

To re-sync: bump `rev` in `crates/jam/Cargo.toml` **and** `TIDE_JAM_REV` in
`crates/jam/src/version.rs` (a test checks they match), run the tests, and port
any protocol change into `relay.rs`. The `jam-interop` workflow also runs the
interop tests against Tide's current `main` weekly; a failure there means Tide's
protocol has drifted from the pinned revision.

### Version handshake and messages

- Nearby: the first line each way is a plaintext hello with `app`, `protocol`
  and `min_protocol`. Mismatches produce "The host's Tide uses an older jam
  protocol (2). Ask them to update Tide." or "This jam needs a newer app (jam
  protocol N). Update Grove to join." Old protocol-2 apps that send their join
  in plain JSON get "This jam needs jam protocol 3; your app has 2. Update Tide
  or Grove to join." and their token is never accepted.
- Relay: invites name their version (`tide-jam:v1.`, `grove-jam:v2.`); unknown
  versions say "Update Grove". Relays advertise protocols at `GET /v2/protocols`
  (Tide's relay answers 404, meaning protocol 1 only), and a room created with
  one protocol answers `426` with `x-grove-jam-room-protocol` to the other.

## Nearby encryption (issue #36)

Tide has no LAN rooms today, so this is a new shared design for both apps:

```text
hello (plaintext JSON line): {"kind":"hello","app":..,"protocol":3,"min_protocol":3,"key":B64(X25519 public)}
psk   = HKDF-SHA256(salt="grove-jam psk v1", ikm=invite token, info="psk")
th    = SHA-256(client hello line || server hello line)
prk   = HKDF-Extract(salt=psk, ikm=X25519(ephemeral_c, ephemeral_s))
keys  = HKDF-Expand(prk, "grove-jam/3 c2s"||th) and ("grove-jam/3 s2c"||th), ChaCha20-Poly1305
frame = u32 BE length || AEAD(key, nonce = 0u32 || u64 BE counter, JSON)
```

The token never crosses the network; a peer without it cannot open frames, and
each connection has fresh ephemeral keys (forward secrecy). This is the Noise
`NNpsk0` shape built from `ring` primitives. **Tide needs the same change** to
host or join nearby rooms: implement exactly this derivation (see
`crates/jam/src/secure.rs` and its tests).

## Internet rooms (issue #34)

Grove joins Tide rooms on any Tide relay, including Tide's free shared relay
(with its wake-up wait). Connections use HTTPS with certificate verification, or
literal `http://127.0.0.1:port` for labs; capabilities travel only in the
Authorization header. Polls retry with backoff (eight attempts) and a lost
membership is rejoined through the invite (three times at most); a closed or
restarted room ends the jam with a clear message.

Grove-hosted internet rooms need protocol 2 because Tide's timeline only holds
YouTube video IDs. `grove-jam-relay` (in `crates/jam`) serves both protocols:

```powershell
cargo run -p tide-jam --bin grove-jam-relay -- --port 8765                                   # this computer
cargo run -p tide-jam --bin grove-jam-relay -- --endpoint https://jam.example.org --port 8765 # behind TLS
```

It binds 127.0.0.1 only. For internet use put a same-machine TLS proxy in front
that overwrites `x-tide-jam-peer` with the client address; Tide's
`experiments/jam/nginx-shared.example.conf` should work unchanged (not yet
validated on a real host). Limits match Tide's
relay (32 rooms, 8 listeners, 32 KiB bodies, 512 requests/s, two rooms per
address). It is free software for a machine you control; no paid hosting is set
up or required. **Tide needs** to accept `grove-jam:v2.` invites and the
protocol-2 timeline to follow Grove hosts.

## Sync (issue #37)

- **Continuous clock:** nearby guests ping every 2 s; the NTP-style estimator
  keeps the lowest-RTT sample of the last eight. Relay guests track RTT per poll
  (the relay's `age_ms` already removes clock offsets).
- **Tighter drift threshold:** seek when off by more than 300 ms once the clock
  is trusted (three samples, best RTT ≤ 400 ms), otherwise 1.5 s.
- **Scheduled starts:** the host loads the next shared song paused, guests load
  it paused and acknowledge, then the host publishes a start time 600 ms plus the
  slowest listener's RTT ahead and everyone presses play at that instant. The
  host waits at most 6 s for acknowledgements.
- **Visible to the host:** each listener's round trip and drift appear under
  People.

## Room features (issue #38)

- Auto-advance (default on): when the host's song ends, the next waiting shared
  song starts for everyone.
- Voting: listeners vote to skip the current play; a majority of connected
  listeners skips (one listener alone counts). In Tide rooms the vote is sent as
  Tide's skip request. Tide has no reactions, so Grove adds none.
- Host migration (nearby): the host asks the first standby-capable listener to
  open a stand-in listener. If the host disappears (rather than ending the jam),
  the stand-in takes over the room, queue and playback after about 4 s, and the
  others reconnect to it with the same invitation.
- Auto-rejoin: a dropped listener keeps its seat for 30 s and reconnects with
  backoff (about 25 s in total).

## Matching (issue #39)

- Grove publishes the host song's ISRC and duration. A listener's search uses
  `isrc:` when an ISRC is known; an identical ISRC whose duration agrees is
  treated as a confirmed match.
- Otherwise candidates are ranked by duration (within 3 s or 2 %), title words,
  and penalties for live/remix/cover versions, and the listener confirms one.
  YouTube titles are cleaned ("(Official Video)", "| Label") before searching.
- Choices are per room unless **Remember my choices on this computer** is on;
  then up to 200 pairs are saved in Grove's preferences. Turning it off forgets
  them.
- Albums and playlists: paste a Spotify album or playlist link to add its first
  50 songs, grouped under "From an album/playlist". Guests may have 20 single
  songs or 50 entries waiting.

## Verification

```powershell
cargo test -p tide-jam            # unit + loopback + Tide interop tests
./target/debug/grove-music.exe --review-ui "$env:TEMP/grove-jams"         # adds handshake, encryption, relay, scheduled start, rejoin stages
./target/debug/grove-music.exe --review-features "$env:TEMP/grove-features" # ISRC match, remembered choices, album entries
```

Live checks pending: two PCs with two Spotify accounts (audible drift, scheduled
start), Tide ↔ Grove audio on real services, a public relay behind TLS, host
migration across real Wi-Fi, and grove:// links from a browser.
