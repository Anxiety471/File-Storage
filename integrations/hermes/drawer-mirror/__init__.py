"""Mirror the files Hermes writes into Drawer.

Hooks ``post_tool_call``. After a successful ``write_file`` or ``patch`` whose
target sits under ``DRAWER_MIRROR_ROOT``, the file's bytes are PUT to Drawer's
REST API at ``DRAWER_MIRROR_PREFIX`` plus the path relative to the root. Drawer
keeps every revision, and an unchanged file adds none.

Hermes keeps its own files, memory and artifacts exactly as before. Mirroring is
best-effort: failures are logged and never change what the tool returned.
"""

from __future__ import annotations

import json
import logging
import os
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any, Optional

logger = logging.getLogger(__name__)

MIRRORED_TOOLS = frozenset({"write_file", "patch"})
DEFAULT_URL = "http://127.0.0.1:8787"
DEFAULT_PREFIX = "/hermes"
DEFAULT_AUTHOR = "hermes"
DEFAULT_MAX_BYTES = 512 * 1024 * 1024  # Drawer's per-upload limit
TIMEOUT_S = 10.0


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
    return {
        "root": root,
        "url": os.environ.get("DRAWER_URL", DEFAULT_URL).rstrip("/"),
        "token": os.environ.get("DRAWER_TOKEN", "").strip(),
        "prefix": prefix.rstrip("/"),
        "author": os.environ.get("DRAWER_MIRROR_AUTHOR", DEFAULT_AUTHOR).strip() or DEFAULT_AUTHOR,
        "max_bytes": int(max_bytes) if max_bytes.isdigit() else DEFAULT_MAX_BYTES,
    }


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
    url = f"{cfg['url']}/drawer/v1/files?{urllib.parse.urlencode({'path': remote})}"
    headers = {
        "content-type": "application/octet-stream",
        "x-drawer-author": cfg["author"],
        "x-drawer-comment": f"mirrored from Hermes {tool_name}",
    }
    if cfg["token"]:
        headers["authorization"] = f"Bearer {cfg['token']}"
    request = urllib.request.Request(url, data=local.read_bytes(), method="PUT", headers=headers)
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


def register(ctx: Any) -> None:
    ctx.register_hook("post_tool_call", _on_post_tool_call)
