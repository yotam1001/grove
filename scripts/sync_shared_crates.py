#!/usr/bin/env python3
"""Keep Grove's vendored copies of Tide's shared crates in sync.

The shared crates (see docs/SHARED_CRATES.md) are copied from one pinned Tide
commit. `crates/shared-crates.json` records that commit, the few Grove-only
text replacements applied on top, and the SHA-256 of every vendored file.

    python scripts/sync_shared_crates.py check
        Offline. Fails when a vendored file was edited, added or removed by hand.
    python scripts/sync_shared_crates.py check --upstream [--source DIR]
        Rebuilds the vendored tree from the pinned Tide commit and fails when it
        differs from what is committed. Needs the network unless --source names
        a local Tide clone that has the commit.
    python scripts/sync_shared_crates.py sync [--commit SHA] [--source DIR]
        Replaces the vendored crates with the given (default: pinned) Tide commit,
        reapplies the replacements and rewrites the manifest.
    python scripts/sync_shared_crates.py status
        Reports how far Tide's main branch has moved past the pinned commit.

Only the standard library is used. Downloads are plain HTTPS GETs of public
files; no credentials are sent unless GITHUB_TOKEN is set (as in CI).
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "crates" / "shared-crates.json"
SHA = re.compile(r"^[0-9a-f]{40}$")
MAX_FILE = 4 * 1024 * 1024


def load_manifest() -> dict:
    data = json.loads(MANIFEST.read_text(encoding="utf-8"))
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", data["upstream"]):
        raise SystemExit("Manifest upstream must be owner/repo")
    if not SHA.match(data["commit"]):
        raise SystemExit("Manifest commit must be a full 40-character SHA")
    for crate in data["crates"]:
        if not re.fullmatch(r"[a-z0-9_-]+", crate):
            raise SystemExit(f"Invalid crate name {crate!r}")
    return data


def normalise(data: bytes) -> bytes:
    # Git checkouts on Windows may use CRLF; hashes are always over LF text.
    return data.replace(b"\r\n", b"\n")


def digest(data: bytes) -> str:
    return hashlib.sha256(normalise(data)).hexdigest()


def get(url: str) -> bytes:
    request = urllib.request.Request(url, headers={"User-Agent": "grove-shared-crate-sync"})
    token = os.environ.get("GITHUB_TOKEN")
    if token and url.startswith("https://api.github.com/"):
        request.add_header("Authorization", f"Bearer {token}")
    with urllib.request.urlopen(request, timeout=60) as response:
        body = response.read(MAX_FILE + 1)
    if len(body) > MAX_FILE:
        raise SystemExit(f"Refusing oversized download: {url}")
    return body


def upstream_files(repo: str, commit: str, crates: list[str], source: Path | None) -> dict[str, bytes]:
    """Return {repo-relative path: bytes} for every file of the shared crates."""
    prefixes = tuple(f"crates/{crate}/" for crate in crates)
    files: dict[str, bytes] = {}
    if source is not None:
        listing = subprocess.run(
            ["git", "-C", str(source), "ls-tree", "-r", "--name-only", commit, "--", *[p.rstrip("/") for p in prefixes]],
            check=True, capture_output=True, text=True,
        ).stdout.split()
        for path in listing:
            files[path] = subprocess.run(
                ["git", "-C", str(source), "show", f"{commit}:{path}"], check=True, capture_output=True
            ).stdout
    else:
        tree = json.loads(get(f"https://api.github.com/repos/{repo}/git/trees/{commit}?recursive=1"))
        if tree.get("truncated"):
            raise SystemExit("Upstream tree listing was truncated; use --source with a local clone")
        for entry in tree["tree"]:
            if entry["type"] == "blob" and entry["path"].startswith(prefixes):
                files[entry["path"]] = get(f"https://raw.githubusercontent.com/{repo}/{commit}/{entry['path']}")
    if not files:
        raise SystemExit("No shared-crate files found at the upstream commit")
    for path in files:
        if ".." in Path(path).parts or path.startswith("/"):
            raise SystemExit(f"Unsafe upstream path {path!r}")
    return {path: normalise(body) for path, body in files.items()}


def apply_replacements(files: dict[str, bytes], replacements: list[dict]) -> None:
    for rule in replacements:
        path = rule["path"]
        if path not in files:
            raise SystemExit(f"Replacement target {path} is not in the upstream crates")
        text = files[path].decode("utf-8")
        expected = int(rule.get("count", 1))
        found = text.count(rule["old"])
        if found != expected:
            raise SystemExit(
                f"Replacement {rule['old']!r} in {path} matched {found} times, expected {expected}. "
                "Upstream changed: review the rule in crates/shared-crates.json."
            )
        files[path] = text.replace(rule["old"], rule["new"]).encode("utf-8")


def local_files(crates: list[str]) -> dict[str, bytes]:
    result = {}
    for crate in crates:
        base = ROOT / "crates" / crate
        for path in sorted(base.rglob("*")):
            if path.is_file():
                result[path.relative_to(ROOT).as_posix()] = normalise(path.read_bytes())
    return result


def compare(expected: dict[str, str], actual: dict[str, bytes], label: str) -> list[str]:
    problems = []
    for path in sorted(set(expected) | set(actual)):
        if path not in actual:
            problems.append(f"{label}: missing {path}")
        elif path not in expected:
            problems.append(f"{label}: unexpected {path}")
        elif digest(actual[path]) != expected[path]:
            problems.append(f"{label}: changed {path}")
    return problems


def command_check(args: argparse.Namespace) -> int:
    data = load_manifest()
    problems = compare(data["files"], local_files(data["crates"]), "vendored")
    if args.upstream:
        rebuilt = upstream_files(data["upstream"], data["commit"], data["crates"], args.source)
        apply_replacements(rebuilt, data.get("replacements", []))
        problems += compare(data["files"], rebuilt, f"upstream {data['commit'][:7]}")
    if problems:
        print("Shared crates drifted from crates/shared-crates.json:")
        for problem in problems:
            print("  " + problem)
        print("Fix: run `python scripts/sync_shared_crates.py sync` (see docs/SHARED_CRATES.md).")
        return 1
    print(f"Shared crates {', '.join(data['crates'])} match Tide {data['commit'][:7]}"
          + (" and upstream" if args.upstream else ""))
    return 0


def command_sync(args: argparse.Namespace) -> int:
    data = load_manifest()
    commit = args.commit or data["commit"]
    if args.source is not None and not SHA.match(commit):
        commit = subprocess.run(["git", "-C", str(args.source), "rev-parse", commit],
                                check=True, capture_output=True, text=True).stdout.strip()
    if not SHA.match(commit):
        raise SystemExit("--commit must be a full 40-character SHA (or use --source to resolve a ref)")
    files = upstream_files(data["upstream"], commit, data["crates"], args.source)
    apply_replacements(files, data.get("replacements", []))
    with tempfile.TemporaryDirectory() as staging:
        staging = Path(staging)
        for path, body in files.items():
            target = staging / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(body)
        for crate in data["crates"]:
            destination = ROOT / "crates" / crate
            if destination.resolve().parent != (ROOT / "crates").resolve():
                raise SystemExit(f"Refusing to replace {destination}")
            shutil.rmtree(destination, ignore_errors=True)
            shutil.copytree(staging / "crates" / crate, destination)
    data["commit"] = commit
    data["files"] = {path: digest(body) for path, body in sorted(files.items())}
    MANIFEST.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"Synced {', '.join(data['crates'])} from {data['upstream']}@{commit[:7]} ({len(files)} files)")
    print("Next: cargo test --workspace, then commit crates/ and crates/shared-crates.json together.")
    return 0


def command_status(args: argparse.Namespace) -> int:
    data = load_manifest()
    compare_url = f"https://api.github.com/repos/{data['upstream']}/compare/{data['commit']}...main"
    result = json.loads(get(compare_url))
    touched = sorted({f["filename"] for f in result.get("files", [])
                      if f["filename"].startswith(tuple(f"crates/{c}/" for c in data["crates"]))})
    print(f"Tide main is {result.get('ahead_by', '?')} commits ahead of the pinned {data['commit'][:7]}.")
    print("Shared-crate files changed upstream since the pin:" if touched else "No shared-crate files changed upstream.")
    for path in touched:
        print("  " + path)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    check = sub.add_parser("check")
    check.add_argument("--upstream", action="store_true")
    check.add_argument("--source", type=Path)
    sync = sub.add_parser("sync")
    sync.add_argument("--commit")
    sync.add_argument("--source", type=Path)
    sub.add_parser("status")
    args = parser.parse_args()
    return {"check": command_check, "sync": command_sync, "status": command_status}[args.command](args)


if __name__ == "__main__":
    sys.exit(main())
