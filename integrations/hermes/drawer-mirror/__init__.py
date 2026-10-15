"""Mirror what Hermes produces into Drawer.

Two observer hooks:

- ``post_tool_call``: after a successful ``write_file`` or ``patch`` whose target
  sits under ``DRAWER_MIRROR_ROOT``, the file's bytes are PUT to Drawer's REST API
  at ``DRAWER_MIRROR_PREFIX`` plus the path relative to the root. Drawer keeps every
  revision, and an unchanged file adds none.
- ``post_llm_call``: every http(s) link in Hermes' replies is appended to
  ``<prefix>/links.md`` (one line per new link, deduplicated). Links are not
  files on disk, so they are recorded as text. Set ``DRAWER_MIRROR_LINKS=0`` to
  turn this off.

Hermes keeps its own files, memory and artifacts exactly as before. Mirroring is
best-effort: failures are logged and never change what the tool or the turn returned.
"""

from __future__ import annotations

import json
import logging
import os
import re
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional

logger = logging.getLogger(__name__)

MIRRORED_TOOLS = frozenset({"write_file", "patch"})
DEFAULT_URL = "http://127.0.0.1:8787"
DEFAULT_PREFIX = "/hermes"
DEFAULT_AUTHOR = "hermes"
DEFAULT_MAX_BYTES = 512 * 1024 * 1024  # Drawer's per-upload limit
LINKS_FILE = "links.md"
LINKS_HEADER = "# Links shared by Hermes\n\n"
TIMEOUT_S = 10.0

# Stops at whitespace, angle brackets, quotes, backticks and closing brackets.
_URL_RE = re.compile(r"https?://[^\s<>\"'`)\]]+")
_LOOPBACK = frozenset({"localhost", "127.0.0.1", "::1", "0.0.0.0"})


def _settings() -> Optional[dict[str, Any]]:
    """Read the environment on every call, so a changed .env applies without code changes."""
    raw_root = os.environ.get("DRAWER_MIRROR_ROOT", "").strip()
    if not raw_root:
        return None
    root = Path(raw_root).expanduser().resolve()
    hermes_home = Path(os.environ.get("HERMES_HOME", "~/.hermes")).expanduser().resolve()
    # The root must not contain Hermes' own home: that would sweep up config.yaml and .env.
    if root == hermes_home or root in hermes_home.parents:
        logger.error("drawer-mirror: DRAWER_MIRROR_ROOT=%s contains HERMES_HOME; mirroring is off", root)
        return None
    prefix = os.environ.get("DRAWER_MIRROR_PREFIX", DEFAULT_PREFIX).strip() or DEFAULT_PREFIX
    if not prefix.startswith("/"):
        prefix = "/" + prefix
    max_bytes = os.environ.get("DRAWER_MIRROR_MAX_BYTES", "").strip()
    links = os.environ.get("DRAWER_MIRROR_LINKS", "1").strip().lower() not in ("0", "false", "no", "off")
    return {
        "root": root,
        "url": os.environ.get("DRAWER_URL", DEFAULT_URL).rstrip("/"),
        "token": os.environ.get("DRAWER_TOKEN", "").strip(),
        "prefix": prefix.rstrip("/"),
        "author": os.environ.get("DRAWER_MIRROR_AUTHOR", DEFAULT_AUTHOR).strip() or DEFAULT_AUTHOR,
        "max_bytes": int(max_bytes) if max_bytes.isdigit() else DEFAULT_MAX_BYTES,
        "links": links,
    }


def _files_request(cfg: dict[str, Any], remote: str, method: str = "GET", data: Optional[bytes] = None,
                   headers: Optional[dict[str, str]] = None,
                   if_head_revision: Optional[int] = None) -> urllib.request.Request:
    query = {"path": remote}
    if if_head_revision is not None:
        query["if_head_revision"] = str(if_head_revision)
    url = f"{cfg['url']}/drawer/v1/files?{urllib.parse.urlencode(query)}"
    all_headers = {"x-drawer-author": cfg["author"], **(headers or {})}
    if cfg["token"]:
        all_headers["authorization"] = f"Bearer {cfg['token']}"
    return urllib.request.Request(url, data=data, method=method, headers=all_headers)


def _tool_failed(result: Any) -> bool:
    """A tool result is a JSON string. Mirror only results that did not report an error."""
    try:
        parsed = json.loads(result) if isinstance(result, str) else result
    except (TypeError, ValueError):
        return False
    return isinstance(parsed, dict) and bool(parsed.get("error"))


def _mirror_file(path_arg: str, tool_name: str, cfg: dict[str, Any]) -> None:
    local = Path(path_arg).expanduser()
    if not local.is_absolute():
        local = Path.cwd() / local
    local = local.resolve()
    root = cfg["root"]
    if not local.is_file() or not local.is_relative_to(root):
        return
    rel = local.relative_to(root)
    # Hidden files and folders stay local: .env, .git, caches and the like.
    if any(part.startswith(".") for part in rel.parts):
        return
    size = local.stat().st_size
    if size > cfg["max_bytes"]:
        logger.warning("drawer-mirror: skipping %s, %d bytes is over DRAWER_MIRROR_MAX_BYTES", rel, size)
        return

    remote = f"{cfg['prefix']}/{rel.as_posix()}"
    request = _files_request(cfg, remote, method="PUT", data=local.read_bytes(), headers={
        "content-type": "application/octet-stream",
        "x-drawer-comment": f"mirrored from Hermes {tool_name}",
    })
    with urllib.request.urlopen(request, timeout=TIMEOUT_S) as response:
        outcome = json.load(response)
    if outcome.get("new_revision"):
        logger.info("drawer-mirror: %s -> %s revision %s", rel, remote, outcome.get("revision"))


def _on_post_tool_call(tool_name: str = "", args: Optional[dict[str, Any]] = None,
                       result: Any = "", **_: Any) -> None:
    """Observer: runs after every tool call and never raises, so it cannot break a turn."""
    if tool_name not in MIRRORED_TOOLS or not isinstance(args, dict) or _tool_failed(result):
        return
    path_arg = args.get("path")
    if not isinstance(path_arg, str) or not path_arg:
        return
    try:
        cfg = _settings()
        if cfg is not None:
            _mirror_file(path_arg, tool_name, cfg)
    except Exception as exc:  # noqa: BLE001 - observer hooks must not fail the agent loop
        logger.warning("drawer-mirror: could not mirror %s: %s", path_arg, exc)


def _clean_url(raw: str) -> Optional[str]:
    """Keep scheme, host and path. Drop userinfo, the query and the fragment: query strings
    often carry signed tokens. Loopback links point at this machine, so they are skipped."""
    parts = urllib.parse.urlsplit(raw.rstrip(".,;:!?*_"))
    host = parts.hostname
    if not host or host in _LOOPBACK:
        return None
    netloc = parts.netloc.rsplit("@", 1)[-1]
    return urllib.parse.urlunsplit((parts.scheme, netloc, parts.path, "", ""))


def _extract_links(text: str) -> list[str]:
    """Unique cleaned http(s) links in the order they first appear."""
    seen: dict[str, None] = {}
    for match in _URL_RE.finditer(text):
        try:
            url = _clean_url(match.group(0))
        except ValueError:
            continue
        if url:
            seen.setdefault(url, None)
    return list(seen)


def _read_links(cfg: dict[str, Any], remote: str) -> tuple[str, int]:
    """Current text and head revision of links.md. A missing file is ("", 0)."""
    try:
        with urllib.request.urlopen(_files_request(cfg, remote), timeout=TIMEOUT_S) as response:
            return response.read().decode("utf-8", errors="replace"), int(response.headers.get("x-drawer-revision", "0"))
    except urllib.error.HTTPError as exc:
        if exc.code == 404:
            return "", 0
        raise


def _record_links(urls: list[str], session_id: str, cfg: dict[str, Any]) -> None:
    """Append the links not already listed. Drawer has no append, so read the head, add
    lines, and write with if_head_revision. A concurrent write gets a 412, and we retry once."""
    remote = f"{cfg['prefix']}/{LINKS_FILE}"
    stamp = datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%MZ")
    for _attempt in range(2):
        text, head = _read_links(cfg, remote)
        known = set(_extract_links(text))
        fresh = [url for url in urls if url not in known]
        if not fresh:
            return
        body = text or LINKS_HEADER
        if body and not body.endswith("\n"):
            body += "\n"
        body += "".join(f"- {stamp} · {url} · session {session_id or 'unknown'}\n" for url in fresh)
        try:
            with urllib.request.urlopen(
                _files_request(cfg, remote, method="PUT", data=body.encode("utf-8"), headers={
                    "content-type": "text/markdown; charset=utf-8",
                    "x-drawer-comment": "links shared by Hermes",
                }, if_head_revision=head),
                timeout=TIMEOUT_S,
            ):
                return
        except urllib.error.HTTPError as exc:
            if exc.code != 412:
                raise
    logger.warning("drawer-mirror: %s changed twice during the write; %d link(s) not recorded", remote, len(urls))


def _on_post_llm_call(assistant_response: str = "", session_id: str = "", **_: Any) -> None:
    """Observer: record the links Hermes gave in a reply. Never raises."""
    if not isinstance(assistant_response, str) or "http" not in assistant_response:
        return
    try:
        cfg = _settings()
        if cfg is None or not cfg["links"]:
            return
        urls = _extract_links(assistant_response)
        if urls:
            _record_links(urls, str(session_id or ""), cfg)
    except Exception as exc:  # noqa: BLE001 - observer hooks must not fail the agent loop
        logger.warning("drawer-mirror: could not record links: %s", exc)


def register(ctx: Any) -> None:
    ctx.register_hook("post_tool_call", _on_post_tool_call)
    ctx.register_hook("post_llm_call", _on_post_llm_call)
