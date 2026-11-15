# Contributing to Grove

- Pick an issue (`good first issue` and `tide-parity` are good starting points) and comment so work isn't duplicated.
- Run `cargo fmt --all`, `cargo test --workspace` and `cargo clippy -p grove-music --all-targets -- -D warnings` before opening a PR.
- Never commit Spotify tokens, client secrets, login URLs, `credentials*.json` or session files. Run `python scripts/check_public_files.py` on staged content before pushing.
- Keep Grove and jams free for users, and keep a self-hostable path for any relay.
- Offline review modes (`--smoke-ui`, `--review-features`, `--review-ui`, `--review-local-audio`) must never touch real credentials or Spotify.
- Be honest in PRs about what was verified live vs. only offline.
