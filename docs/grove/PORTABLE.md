# Grove portable preview (Linux and macOS)

This archive is an unsigned preview build of Grove from CI. It has been built,
tested and exercised through Grove's offline review modes on GitHub's Linux and
macOS runners only. Nobody has run it on real hardware with a Spotify account
yet, so treat every live feature as unverified.

## Run it

    tar -xzf grove-<platform>-portable.tar.gz
    cd grove-<platform>-portable
    shasum -a 256 -c SHA256SUMS.txt
    ./grove-music

macOS: the binary is not signed or notarized. If Gatekeeper blocks it, run
`xattr -d com.apple.quarantine grove-music` after checking the checksums.

## What it needs

- Spotify Premium for local playback through librespot; library and browsing
  work with any account.
- A web browser for the Spotify approval (`xdg-open` on Linux, `open` on macOS).
- Linux: a Secret Service keyring (GNOME Keyring or KWallet) to remember the
  sign-in, ALSA or PulseAudio/PipeWire for audio, and a D-Bus session for MPRIS
  media controls.
- `mpv` 0.39 or newer on `PATH` (or `/opt/homebrew/bin`, `/usr/local/bin`) only
  for local files.

Settings and caches live in `~/.config/GroveMusic` on Linux and
`~/Library/Application Support/GroveMusic` on macOS. Sign-ins are kept in the
system keyring. There is no updater or installer on these platforms yet; the
Windows-only features (taskbar buttons, tray, global shortcuts, the System
theme watcher) are absent.
