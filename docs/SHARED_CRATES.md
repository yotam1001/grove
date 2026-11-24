# Shared crates: how Grove stays in sync with Tide

Grove builds on four crates that started in [Tide](https://github.com/yotam1001/tide-music):
`tide-core` (shared types), `tide-windows` (Windows media controls, taskbar,
tray, global shortcuts), `tide-playback` (the mpv backend) and `tide-jam`
(shared-jam protocol). This page records how each one is kept in step.

## Decision

**Vendor from a pinned Tide commit, with a sync script and a CI drift check.**

Options considered (issue #7):

| Option | Why not (yet) |
| --- | --- |
| Extract a shared-crates repository consumed as git dependencies by both apps | Needs Tide to move its crates out and both apps to coordinate releases. Worth revisiting once the crates' APIs settle; the manifest below makes that move mechanical. |
| Publish to crates.io | Public API and semver promises for crates that still change weekly; publishing credentials in CI. |
| **Vendor + sync script + recorded upstream commit** | Chosen. Builds stay offline and reproducible, Grove can review every upstream change as a normal diff, and Tide is not asked to change anything. |

The cost of vendoring is drift. Two rules keep that visible:

1. **Vendored crates are never edited by hand.** `crates/shared-crates.json` lists
   the pinned Tide commit and the SHA-256 of every vendored file. A hand edit, a
   stray file or a missing file fails CI.
2. **Grove-only differences are explicit replacement rules** in the same
   manifest. The sync script applies them after copying, and fails loudly if an
   upstream change means a rule no longer matches exactly. Each rule is a
   candidate to upstream into Tide so the list shrinks over time.

## What is synced today

| Crate | Status |
| --- | --- |
| `crates/core` | Synced from Tide by `scripts/sync_shared_crates.py`. No Grove changes. |
| `crates/windows` | Synced. Grove replacement rules: the tray tooltip and menu say "Grove" instead of "Tide", and the tray icon is shown only while "Keep playing in the tray" is on (Tide always shows it). |
| `crates/playback` | Vendored snapshot with one Grove change: the YouTube path (`yt-dlp`, next-stream preparation, cookie leases) sits behind a `youtube` cargo feature that Grove leaves off (issue #8). Once Tide has the same feature, add `playback` to the manifest's `crates` list. |
| `crates/jam` | Owned by the shared-jams work (Tide moved its jam work to `experiments/jam` with a relay). Not under the sync script yet; protocol compatibility is tested by the jam stream. |

## Everyday use

```powershell
# Offline: fail if a vendored file was edited, added or removed by hand.
python scripts/sync_shared_crates.py check

# Rebuild the vendored tree from the pinned Tide commit and compare (needs the network,
# or --source <local Tide clone that has the commit>). CI runs this on every pull request.
python scripts/sync_shared_crates.py check --upstream

# How far has Tide's main branch moved past the pin, and which shared files changed?
python scripts/sync_shared_crates.py status
```

## Updating to a newer Tide commit

1. `python scripts/sync_shared_crates.py status` to see what changed upstream.
2. `python scripts/sync_shared_crates.py sync --commit <40-character SHA>`
   (or `--source <path to a Tide clone> --commit origin/main`).
3. If a replacement rule no longer matches, update it in
   `crates/shared-crates.json` and run `sync` again.
4. `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`
   and the offline review modes, then fix Grove call sites for API changes.
5. Commit the crates and `crates/shared-crates.json` together, with the Tide
   commit in the message.

To change shared behaviour, change it in Tide first, then sync. Only Grove
branding or opt-in differences belong in replacement rules.
