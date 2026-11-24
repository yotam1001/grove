# Grove private beta status — 2026-11-24

**Release hold: Spotify access and audible playback are not verified.**

Grove 0.1.0-beta.1 has an installable Windows x64 candidate, and Linux and
macOS preview archives come out of CI. None of it is ready to distribute as a
working Spotify player. No public release has been made.

Since the October candidate, the module split (#42) and five parity branches
have merged: the UI shell with Now Playing lyrics and the queue drawer (#47),
the audio engine (#43), browse pages with request budgets (#44), Windows
integration and release infrastructure (#45) and jams (#46). #48 routes the
startup profile and playlists through the same budgets and cache, and adds the
Linux and macOS builds. Everything below "Candidate checks" was verified offline
only.

## What is blocking release

- **Live Spotify account.** The October check saved a library approval, but
  library requests hit HTTP 429. Grove now cools down only the affected
  endpoint group, keeps the last launch's profile and playlists on screen and
  tells `QUOTA_EXCEEDED` apart, but no catalog, library or playlist-write check
  has passed against Spotify yet.
- **Premium.** The test account is not Premium. Streaming needs Premium, so
  audible playback, pause/seek/resume, volume, the audio settings (quality,
  levelling, fades, equalizer, sleep timer) and reconnect after a real network
  drop or sleep remain unverified.
- **Dedicated Spotify Client ID.** A reliable private beta needs an app owner
  who meets Spotify's current development requirements and allowlists the
  testers. Testers never need to share a secret.
- **Jams.** Two-device Spotify jams, audible Tide ↔ Grove synchronization and a
  public `grove-jam-relay` host behind TLS need live testing.
- **Code signing.** The installer, the updates and the macOS archive are
  unsigned. The update check proves a download matches its release, not who
  published it.

Spotify's [quota modes](https://developer.spotify.com/documentation/web-api/concepts/quota-modes)
describe Premium ownership and the five-user limit for a new development app.
The [quota update](https://developer.spotify.com/blog/2026-07-23-web-api-quota-updates)
explains developer-wide quotas and the QUOTA_EXCEEDED response reason.

## Candidate checks (offline)

- `cargo fmt`, `cargo test --workspace` and `cargo clippy --workspace
  --all-targets -D warnings` pass on Windows; CI repeats them on Ubuntu and
  macOS (non-blocking jobs).
- The smoke run (63 stages, 53 captures at 1200×800 and 700×540, light and
  dark), the feature, browse and system review (49 stages), the jam review
  (29 stages over real loopback peers and a local relay) and the audio review
  (15 stages, 12 captures) pass. On Linux and macOS, CI runs the smoke,
  feature and jam reviews and uploads the screenshots.
- `scripts/test_grove_beta.py` covers payload corruption and traversal,
  bootstrap rejections, the install/rollback/uninstall lifecycle and the
  `grove://` handler with a faked registry.
- Package allowlists, SHA-256 checks and a credential/private-path scan pass.
  The shared crates match the pinned Tide commit.

## Remaining acceptance checks

1. Configure an eligible dedicated Spotify app and allowlist a small tester group.
2. Verify fresh consent with the added scopes, catalog/library access, a real
   429 cooldown and reconnect after restarting.
3. With a Premium tester, hear a song and test pause, seek, resume, volume, the
   audio settings and recovery after sleep and a network drop.
4. Verify Like/Unlike, follow/unfollow and a reversible edit to a disposable
   owned playlist.
5. Test installation, update and removal on a clean Windows account, hardware
   media keys, global shortcuts, the tray, and output-device loss and return.
6. Test two-device jams and Tide ↔ Grove rooms, including a public relay host.
7. Run the Linux and macOS archives on real machines: sign-in through the
   browser, keyring storage, MPRIS / Now Playing, audio output and local files
   through mpv.

Use Settings → Save support info for a report without account names, music
history or tokens. Do not send authorization codes or full login URLs.
