<p align="center"><img src="assets/grove/logo.svg" width="120" alt="Grove logo"></p>

# Grove

**A lightweight, native Spotify player for Windows — the Spotify sibling of [Tide](https://github.com/yotam1001/tide-music).**

Grove is a Rust + [Slint](https://slint.dev/) desktop app. It browses your library through the Spotify Web API and plays audio in-process with [librespot](https://github.com/librespot-org/librespot), so there is no browser, Electron shell or external daemon. It shares Tide's playback, Windows-integration and jam crates so the two apps can host and join each other's listening rooms.

> **Status: early beta, not yet verified against live Spotify playback.** See [docs/BETA_STATUS.md](docs/grove/BETA_STATUS.md) and the [open issues](../../issues). Contributions welcome — the [`tide-parity`](../../labels/tide-parity) label tracks the gap to Tide.

![Grove listening view](docs/grove/screenshots/listen.png)

## Features (today)

- Home, Search, Library, Liked Songs, Now playing, Queue, Shared jams, Settings, Account
- Paste Spotify track / album / playlist / episode links
- Native play/pause, seek, volume, shuffle, repeat; editable Grove queue
- Song menus: Play next, Add to queue, Like/Unlike, View artist, Hide, owned-playlist editing
- Light/dark, Normal/Compact/Mini layouts, Windows media controls, opt-in session history
- Local audio files through Tide's mpv backend (no Spotify login needed)
- Shared jams: encrypted nearby rooms, internet rooms via a free self-hostable relay, and joining **Tide** rooms (Tide ↔ Grove matching by ISRC/duration)
- PKCE sign-in in your browser; credentials in Windows Credential Manager — Grove never sees your password

Full feature/parity table: [docs/grove/TIDE_PARITY.md](docs/grove/TIDE_PARITY.md).

## Requirements

- Windows 10/11 x64
- Rust stable (see CI) — `cargo build --release -p grove-music`
- Spotify **Premium** for playback (browsing works on free accounts)
- `mpv` only for local-file playback

## Build and run

```powershell
cargo test --workspace
cargo run -p grove-music
# offline native UI checks (no Spotify access, no credentials)
./target/debug/grove-music.exe --smoke-ui "$env:TEMP/grove-smoke"
./target/debug/grove-music.exe --review-features "$env:TEMP/grove-features"
./target/debug/grove-music.exe --review-ui "$env:TEMP/grove-jams"
```

Then open **Account → Connect Spotify**, and **Enable playback** for the separate streaming approval. Details: [docs/grove/README.md](docs/grove/README.md).

## Repository layout

| Path | What |
| --- | --- |
| `crates/grove` | The Grove app (UI glue, Spotify auth, Web API, librespot engine, jams) |
| `crates/grove/src/app/` | Window wiring: callbacks, the event poll loop, the jam timer, smoke screenshots |
| `crates/grove/src/features/` | Feature controller, one file per area (search, collections, queue, jams…) |
| `ui/grove.slint` | Native interface entry: `GroveWindow`, sidebar and page switching |
| `ui/theme.slint`, `ui/components.slint`, `ui/model.slint`, `ui/player.slint` | Colour tokens, shared controls, row structs, player bar |
| `ui/pages/` | One file per page (home and search lists, library, queue, now playing, jams, settings, account, playlist editor) |
| `crates/jam` | Jam protocol, sync, encrypted nearby transport, Tide relay client and `grove-jam-relay` (interop-tested against Tide) |
| `crates/playback`, `crates/core`, `crates/windows` | Vendored from Tide: mpv backend, shared types, Windows media integration |
| `installer/`, `scripts/` | Per-user installer and packaging (still being decoupled from Tide's harness) |
| `docs/` | Design notes, beta status, parity audit, jam protocol |

The shared crates are a snapshot of Tide's. Keeping them in sync (or extracting them into a shared crate repo) is tracked in the issues.

## Relationship to Spotify

Grove is an unofficial client. librespot is unofficial and may need updates as Spotify changes. Spotify's terms apply to your account. Grove is not affiliated with or endorsed by Spotify.

## License

MIT — see [LICENSE](LICENSE) and [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
