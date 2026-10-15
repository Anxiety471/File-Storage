# drawer-mirror: every Hermes file write lands in Drawer

A Hermes Agent plugin that copies each file Hermes writes into a running Drawer
server. Hermes keeps its own artifacts, files and memory as they are. Drawer is
extra storage: revisions, trash, sharing and the web UI, on top.

It uses two plugin hooks:

- `post_tool_call`: after a successful `write_file` or `patch` whose target is
  under `DRAWER_MIRROR_ROOT`, the file's bytes are sent to Drawer's REST API
  (`PUT /drawer/v1/files`). Drawer records the author as `hermes`, and an
  unchanged file adds no revision.
- `post_llm_call`: every http(s) link in a Hermes reply is appended to
  `/hermes/links.md`, one line per new link, with the time and session. This is
  the list behind Hermes' Artifacts panel, minus the local files. Links are text,
  not files on disk, so they go in one Markdown file.

The agent does not have to remember anything in either case.

The MCP server in `docs/harnesses.md` is still there. Use it when the agent should
search or browse Drawer. This plugin only handles the one-way copy.

## Install

```sh
# 1. run Drawer (shared server, so the UI and every agent see the same store)
drawer --data-dir /srv/drawer-data serve --token "$DRAWER_TOKEN"

# 2. install the plugin into Hermes' user plugin directory
cp -r integrations/hermes/drawer-mirror ~/.hermes/plugins/

# 3. allow it. Plugins are off until listed in plugins.enabled
```

`~/.hermes/config.yaml`:

```yaml
plugins:
  enabled:
    - drawer-mirror
```

`~/.hermes/.env`:

```sh
DRAWER_URL=http://127.0.0.1:8787
DRAWER_TOKEN=change-me            # only if drawer runs with --token
DRAWER_MIRROR_ROOT=~/.hermes/artifacts   # the only folder that gets mirrored
DRAWER_MIRROR_PREFIX=/hermes      # Drawer folder for that root (default /hermes); links go to <prefix>/links.md
# DRAWER_MIRROR_LINKS=0           # keep files, skip the links list (default on)
# DRAWER_MIRROR_AUTHOR=hermes     # author on each revision (default hermes)
# DRAWER_MIRROR_MAX_BYTES=536870912   # skip bigger files (default 512 MB)
```

Restart Hermes so the plugin loads. `/reload-mcp` only reloads MCP servers.

Then `~/.hermes/artifacts/reports/plan.md` appears in Drawer at
`/hermes/reports/plan.md`.

## Rules

- **Only files under `DRAWER_MIRROR_ROOT` are sent.** Paths outside it, symlinks
  that point outside it, and anything with a dot in its path (`.env`, `.git`,
  caches) stay local.
- **The root may not contain `HERMES_HOME`.** Pointing it at `~/.hermes` or `~`
  would sweep up `config.yaml` and `.env`. Mirroring is then switched off and an
  error is logged.
- **Links are cleaned before they are saved.** Userinfo, query strings and
  fragments are dropped, because query strings often hold signed tokens. The
  path is kept. Loopback links (`localhost`, `127.0.0.1`) are skipped. Each link
  is saved once, however many replies repeat it.
- **Only links in Hermes' replies are recorded.** Links that appear only in
  terminal or tool output are not, so the list is smaller than Hermes' Artifacts
  panel.
- **Failed tool calls are skipped.** A `write_file` or `patch` that returned an
  `error` is not mirrored.
- **Best effort.** If Drawer is down or slow, the hook logs a warning after at
  most 10 seconds. The tool result and the agent turn are unchanged.

## Limits

- Only `write_file` and `patch` are mirrored. Files written with `terminal`
  (shell redirects, scripts, `git`, `cp`) are not. Use `write_file` for anything
  that should be kept.
- The path must exist on the machine running Hermes. If the terminal backend is a
  sandbox such as Docker or Modal, the file lives inside the sandbox and is
  skipped.
- A `patch` call with no `path` argument (a multi-file patch) is skipped.
- Relative paths resolve against the Hermes process's working directory. Use
  absolute paths for anything that matters.
- Files that Hermes only mentions (like a profile's `SOUL.md`) are not copied.
  Only files under the root are.
- Mirroring is one way. Edits made in the Drawer UI are not written back to
  Hermes.
- Drawer's own authorization is cooperative (see `docs/harnesses.md`). Run it
  behind `--token` on a private network.

## Tests

```sh
python3 -m unittest discover -s integrations/hermes/drawer-mirror/tests -v
```

These use the standard library only and a fake Drawer, so Hermes does not need to
be installed. They cover path rules, hidden and symlinked files, failed results,
the root containment check, the size cap, the bearer header, Drawer being
unreachable, link cleaning and dedupe, the `if_head_revision` retry on a 412, and
the links switch.

The hook was also run against `drawer serve` in a scratch directory: a write, an
unchanged rewrite (no revision), a `patch` edit (revision 2), a failed `patch`
(skipped), a `.env` file (skipped) and a PNG (mirrored, served as `image/png`). Link capture was run the same way: three replies
produced three links in `/hermes/links.md` (a repeat was skipped, a query string
was stripped), revision 2, and a stale `if_head_revision` returned 412.

Not verified: a live Hermes Agent run. Hermes was not installed in the environment
where this was written. The hook API used here (`register(ctx)`,
`ctx.register_hook("post_tool_call", fn)`, the `tool_name` / `args` / `result`
keyword arguments) is taken from Hermes' plugin docs and source
(`website/docs/user-guide/features/hooks.md`, `hermes_cli/plugins.py`). Run one
`write_file` against a real Hermes before you rely on it.
