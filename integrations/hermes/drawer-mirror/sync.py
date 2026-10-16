#!/usr/bin/env python3
"""Sync Hermes artifacts into Drawer -- scan-based, no double-writing.

Hermes already keeps everything it produces: files on disk, and the artifact
records in its session store (state.db) that the Artifacts panel reads. This
tool scans that store and makes sure Drawer has the same files. It does not
watch every write, and it never re-sends a file Drawer already holds -- each
candidate is hashed and compared with Drawer's copy before anything is sent.

Usage:
    python3 sync.py                 Backfill: PUT every file already under the
                                    mirror root(s) (DRAWER_MIRROR_ROOT /
                                    DRAWER_MIRROR_ROOTS).
    python3 sync.py --hermes        Scan Hermes' session store for artifacts and
                                    upload the files that exist, under
                                    /hermes/artifacts/<absolute path>; new links
                                    are recorded in /hermes/links.md.

Both modes read their configuration like the plugin does: the process
environment first, then $HERMES_HOME/.env, so edits apply without a restart.

`--hermes` is deliberate about scope. The Artifacts panel also surfaces paths
that were only *mentioned* in tool output (system files, configs, ...), which
is not the same as files Hermes produced, so the scan only considers files
under the scan roots (DRAWER_SYNC_ROOTS, default ~/.hermes) and skips symlinks,
secret names (auth.json, .env, *.pem, ...), Hermes' own configuration and
runtime logs, databases (*.db*), bytecode, dependency/cache directories and
files over DRAWER_MIRROR_MAX_BYTES.

The scan is idempotent: re-running only uploads what is new or changed, and
links are deduplicated against /hermes/links.md.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import logging
import os
import re
import sqlite3
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent

HERMES_HOME = Path(os.environ.get("HERMES_HOME", "~/.hermes")).expanduser()
STATE_DB = HERMES_HOME / "state.db"

MEDIA_RE = re.compile(r"""MEDIA:\s*(?:"([^"]+)"|'([^']+)'|([^\s"'`]+))""")
PATH_RE = re.compile(r"(?<![\w.])/(?:[\w.-]+/)*[\w.-]+\.[A-Za-z0-9]{1,10}\b")
URL_RE = re.compile(r"https?://[^\s<>\"'`)\]]+")

SECRET_NAMES = frozenset({
    ".env", "auth.json", "credentials", ".netrc", ".npmrc", ".pypirc", "hosts.yml",
    "hosts", "id_rsa", "id_dsa", "id_ecdsa", "id_ed25519", "known_hosts",
})
SECRET_SUFFIXES = (".pem", ".key", ".p12", ".pfx", ".keystore", ".jks", ".token", ".secret")
SKIP_DIRS = frozenset({
    ".git", "__pycache__", "node_modules", ".venv", "venv", ".tox", ".mypy_cache",
    ".pytest_cache", ".ruff_cache", ".cache",
})
SKIP_SUFFIXES = (".pyc", ".pyo", ".db", ".db-shm", ".db-wal", ".sqlite", ".sqlite3")


def _load_dotenv() -> None:
    """Load only Drawer settings from Hermes' env file for standalone timer runs."""
    path = HERMES_HOME / ".env"
    try:
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError:
        return
    for raw in lines:
        line = raw.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        name, _, value = line.partition("=")
        name = name.strip()
        if not name.startswith("DRAWER_") or os.environ.get(name):
            continue
        value = value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "\\\"'":
            value = value[1:-1]
        os.environ[name] = value


def _load_plugin():
    """Load the sibling ``__init__.py`` under its own module name (no package needed)."""
    _load_dotenv()
    spec = importlib.util.spec_from_file_location("drawer_mirror", HERE / "__init__.py")
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _skip_reason(path: Path) -> str | None:
    """Why a candidate path is not synced, or None when it is fine. Symlinks are
    refused by the callers, before the path is resolved."""
    name = path.name
    if name in {"config.yaml", "profile.yaml"} and path.is_relative_to(HERMES_HOME):
        return "Hermes configuration"
    if "logs" in path.parts and path.is_relative_to(HERMES_HOME):
        # Runtime logs grow on every agent turn, so mirroring one stores a full
        # copy per scan as a new revision (agent.log alone reached 406 MB of
        # history). They are not artifacts; they stay local.
        return "Hermes runtime log"
    if name in SECRET_NAMES:
        return "secret file"
    if name.endswith(SECRET_SUFFIXES):
        return "credential file"
    if any(part in SKIP_DIRS for part in path.parts):
        return "dependency or cache directory"
    if name.endswith(SKIP_SUFFIXES):
        return "database or bytecode"
    return None


def _hermes_references() -> tuple[dict[str, None], dict[str, None]]:
    """Scan Hermes' session store for artifact references.

    Returns (file paths, urls), each deduplicated. This is the same data the
    desktop Artifacts panel collects: ``MEDIA:`` tags, markdown links, URLs and
    absolute paths with a file/image extension, from assistant and tool
    messages (system and user messages are not scanned)."""
    if not STATE_DB.is_file():
        return {}, {}
    try:
        conn = sqlite3.connect(f"file:{STATE_DB}?mode=ro", uri=True)
    except sqlite3.Error as exc:
        print(f"drawer-mirror sync: cannot open {STATE_DB}: {exc}", file=sys.stderr)
        return {}, {}
    try:
        try:
            rows = conn.execute(
                "select role, content from messages where role in ('assistant','tool') order by rowid"
            ).fetchall()
        except sqlite3.Error as exc:
            # A changed Hermes schema must not look like "nothing to sync".
            print(f"drawer-mirror sync: cannot read artifact references from {STATE_DB}: {exc}", file=sys.stderr)
            return {}, {}
    finally:
        conn.close()
    files: dict[str, None] = {}
    urls: dict[str, None] = {}
    for _role, content in rows:
        if not content:
            continue
        text = content if isinstance(content, str) else str(content)
        for m in MEDIA_RE.finditer(text):
            raw = (m.group(1) or m.group(2) or m.group(3) or "").strip()
            if raw and not raw.lower().startswith(("http://", "https://")):
                files.setdefault(raw, None)
        for m in PATH_RE.finditer(text):
            files.setdefault(m.group(0), None)
        for m in URL_RE.finditer(text):
            urls.setdefault(m.group(0).rstrip(".,;:!?"), None)
    return files, urls


def _candidate_files(roots: list[Path], files: dict[str, None]) -> dict[Path, None]:
    """Existing, in-scope files among the scanned references."""
    candidates: dict[Path, None] = {}
    for raw in files:
        if raw.startswith("file://"):
            raw = urllib.parse.unquote(raw[len("file://"):])
        if not raw or "://" in raw or raw.lower().startswith(("http://", "https://")):
            continue
        path = Path(raw).expanduser()
        if not path.is_absolute() or path.is_symlink():
            continue
        try:
            resolved = path.resolve()
        except OSError:
            continue
        if not any(resolved == root or root in resolved.parents for root in roots):
            continue
        if not resolved.is_file():
            continue
        candidates.setdefault(resolved, None)
    return candidates


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _already_synced(mirror, cfg: dict, remote: str, local: Path) -> bool:
    """True when Drawer already holds this exact content (by sha256)."""
    digest = _sha256(local)
    url = f"{cfg['url']}/drawer/v1/files/meta?{urllib.parse.urlencode({'path': remote})}"
    headers = {"x-drawer-author": cfg["author"]}
    if cfg.get("token"):
        headers["authorization"] = f"Bearer {cfg['token']}"
    request = urllib.request.Request(url, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=15) as response:
            meta = json.load(response)
    except (urllib.error.URLError, OSError, ValueError):
        return False
    return meta.get("hash") == digest


def _put_file(mirror, cfg: dict, remote: str, local: Path) -> None:
    request = mirror._files_request(cfg, remote, method="PUT", data=local.read_bytes(), headers={
        "content-type": "application/octet-stream",
        "x-drawer-comment": "synced from Hermes artifacts",
    })
    with urllib.request.urlopen(request, timeout=60) as response:
        json.load(response)


def _remote_for_scan(mirror, cfg: dict, path: Path, roots: list[Path]) -> str:
    """Remote path for a scanned file: relative to its scan root, so the Drawer
    tree mirrors Hermes' own folders instead of the absolute system path.
    With several roots each root's basename prefixes the relative path."""
    for root in roots:
        if path.is_relative_to(root):
            rel = path.relative_to(root).as_posix()
            if len(roots) > 1:
                rel = f"{root.name}/{rel}"
            return f"{cfg['prefix']}/artifacts/{rel}"
    return f"{cfg['prefix']}/artifacts/{str(path).lstrip('/')}"


def _scan_hermes(mirror, args) -> int:
    cfg = mirror._settings()
    if cfg is None:
        print("drawer-mirror sync: configure a mirror root and Drawer URL before syncing", file=sys.stderr)
        return 2
    if args.roots:
        roots = [Path(r).expanduser() for r in args.roots]
    else:
        env_roots = os.environ.get("DRAWER_SYNC_ROOTS", "").strip()
        if env_roots:
            roots = [Path(r.strip()).expanduser() for r in env_roots.split(",") if r.strip()]
        else:
            roots = [HERMES_HOME]
    roots = [r.resolve() for r in roots if r.is_dir()]
    if not roots:
        print("drawer-mirror sync: none of the scan roots exist", file=sys.stderr)
        return 2
    files, urls = _hermes_references()
    candidates = _candidate_files(roots, files)
    if not args.quiet:
        print(f"scan: {len(candidates)} candidate files under {', '.join(str(r) for r in roots)}")
    uploaded = skipped = failed = 0
    for path in sorted(candidates):
        reason = _skip_reason(path)
        if reason:
            skipped += 1
            if args.verbose:
                print(f"skipped {path} ({reason})")
            continue
        if path.stat().st_size > cfg["max_bytes"]:
            skipped += 1
            if args.verbose:
                print(f"skipped {path} (over size cap)")
            continue
        remote = _remote_for_scan(mirror, cfg, path, roots)
        if not args.dry_run and _already_synced(mirror, cfg, remote, path):
            skipped += 1
            if args.verbose:
                print(f"skipped {path} (already in Drawer)")
            continue
        try:
            if not args.dry_run:
                _put_file(mirror, cfg, remote, path)
            uploaded += 1
            if args.verbose:
                print(f"uploaded {path} -> {remote}")
        except Exception as exc:  # noqa: BLE001 - one bad file must not stop the run
            failed += 1
            print(f"drawer-mirror sync: failed {path}: {exc}", file=sys.stderr)
    if not args.no_links and cfg["links"]:
        cleaned = mirror._extract_links("\n".join(urls)) if urls else []
        if args.dry_run:
            if cleaned and not args.quiet:
                print(f"links: {len(cleaned)} candidate links (dry run, not recorded)")
        elif cleaned:
            try:
                mirror._record_links(cleaned, "hermes-scan", cfg)
                if not args.quiet:
                    print(f"links: {len(cleaned)} recorded into {cfg['prefix']}/links.md")
            except Exception as exc:  # noqa: BLE001
                print(f"drawer-mirror sync: links failed: {exc}", file=sys.stderr)
    if not args.quiet:
        verb = "would upload" if args.dry_run else "uploaded"
        print(f"drawer-mirror sync: {uploaded} {verb}, {skipped} skipped, {failed} failed")
    return 1 if failed else 0


def _walk_files(root: Path):
    """Files under root, without descending into dependency, cache or hidden folders."""
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = sorted(d for d in dirnames if d not in SKIP_DIRS and not d.startswith("."))
        for name in sorted(filenames):
            yield Path(dirpath, name)


def _backfill(mirror, args) -> int:
    cfg = mirror._settings()
    if cfg is None:
        print("drawer-mirror sync: no mirror root configured (set DRAWER_MIRROR_ROOT or "
              "DRAWER_MIRROR_ROOTS); nothing to do", file=sys.stderr)
        return 2
    root = cfg["root"]
    remote = cfg["prefix"]
    uploaded = skipped = failed = 0
    for path in _walk_files(root):
        if path.is_symlink() or not path.is_file():
            continue
        rel = path.relative_to(root)
        if (any(part.startswith(".") for part in rel.parts) or _skip_reason(rel)
                or path.stat().st_size > cfg["max_bytes"]):
            skipped += 1
            continue
        target = f"{remote}/{rel.as_posix()}"
        if args.dry_run:
            uploaded += 1
            if args.verbose:
                print(f"would upload {path} -> {target}")
            continue
        try:
            mirror._mirror_file(str(path), "backfilled by drawer-mirror sync", cfg)
            uploaded += 1
            if args.verbose:
                print(f"uploaded {path} -> {target}")
        except Exception as exc:  # noqa: BLE001 - one bad file must not stop the run
            failed += 1
            print(f"drawer-mirror sync: failed {path}: {exc}", file=sys.stderr)
    if not args.quiet:
        verb = "would upload" if args.dry_run else "uploaded"
        print(f"drawer-mirror sync: {uploaded} {verb}, {skipped} skipped, {failed} failed")
    return 1 if failed else 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Sync Hermes artifacts into Drawer (scan-based, no double-writing).",
        formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--hermes", action="store_true",
                        help="scan Hermes' session store for artifacts instead of the root backfill")
    parser.add_argument("--roots", nargs="*", metavar="DIR",
                        help="scan roots for --hermes (default: ~/.hermes or DRAWER_SYNC_ROOTS)")
    parser.add_argument("--no-links", action="store_true", help="skip recording links into links.md")
    parser.add_argument("--dry-run", action="store_true", help="list what would be uploaded, upload nothing")
    parser.add_argument("--quiet", action="store_true", help="suppress the summary line")
    parser.add_argument("--verbose", action="store_true", help="one line per file")
    args = parser.parse_args(argv)
    logging.basicConfig(level=logging.INFO if args.verbose else logging.WARNING, format="%(message)s")

    mirror = _load_plugin()
    if args.hermes:
        return _scan_hermes(mirror, args)
    return _backfill(mirror, args)


if __name__ == "__main__":
    raise SystemExit(main())
