<div align="center">

# Drawer

**Versioned file storage for agents, modelled on Google Drive.**

One Rust binary serves a REST API, an MCP server and a web UI over the same store.

[![CI](https://github.com/Anxiety471/drawer/actions/workflows/ci.yml/badge.svg)](https://github.com/Anxiety471/drawer/actions/workflows/ci.yml)

<img src="docs/screenshots/browse-light.png" alt="Drawer file browser in light mode" width="860">

</div>

Agents write files all day and sooner or later one of them overwrites something it shouldn't. Drawer keeps every version, puts deleted things in a trash, and lets two agents share a folder without silently clobbering each other's writes.

## Quickstart

Linux and macOS, one command:

```sh
curl -fsSL https://raw.githubusercontent.com/Anxiety471/drawer/main/install.sh | sh
drawer serve                                # http://127.0.0.1:8787
```

The installer downloads the release for your machine, checks its SHA-256, and puts `drawer` in `~/.local/bin` with the UI in `~/.local/share/drawer/ui`. If no release exists for your platform it builds from source with cargo (installing Rust via rustup if needed).

Then point an agent at it. For Hermes Agent, `drawer mcp-config` prints the entry to paste into `~/.hermes/config.yaml`. See [MCP and agent harnesses](#mcp-and-agent-harnesses).

## Features

- **Stable ids.** Every file and folder has an id that survives rename, move and trash. Moving a folder moves everything under it.
- **Revisions.** Every save is a new immutable revision. Rolling back adds the old content as the new head, so nothing is lost.
- **Safe concurrent writes.** Writes accept `if_head_revision`. A stale writer gets `412 Precondition Failed` and nothing changes.
- **Trash.** Deleting moves things to the trash, with their history. Restore by id, or purge for good.
- **Star, search, copy, move.** Star files and folders, search names and contents, and copy without re-uploading: copies share content by hash.
- **Sharing.** Grant `reader`, `writer` or `owner` on a folder and it covers everything below. A node with no grants is open.
- **Custom properties.** Key/value pairs stored with each revision.
- **Any file type.** Stored byte-for-byte, up to 512 MB per upload.
- **Previews in the UI.** PDF, Word (`.docx`), Excel (`.xlsx`, `.xls`, `.ods`), CSV/TSV, Markdown, HTML, images, audio and video. Text and source code are editable with syntax highlighting. Other types (`.doc`, `.pptx`, archives) download.
- **Optional bearer token** for the HTTP API, MCP endpoint and UI.

Content is stored as sha256-addressed blobs on disk, and metadata lives in SQLite.

## Web UI

A Drive-style browser with My Drawer, Starred and Trash, a search box over names and contents, and a version history panel on every file.

<p align="center">
  <img src="docs/screenshots/history.png" alt="Version history for a Markdown file" width="860"><br>
  <img src="docs/screenshots/editor.png" alt="Editing a Markdown file with unsaved changes" width="860">
</p>

There is a dark theme too: <img src="docs/screenshots/browse-dark.png" alt="Drawer file browser in dark mode" width="860">

## Command line

`drawer` with no subcommand runs `serve`. The file commands work on the store directly, so no server has to be running. Paths look like `/notes/todo.md`. Run `drawer --help` or `drawer <command> --help` for every flag.

```sh
drawer put ./report.pdf /work/            # upload into a folder (trailing / or existing folder)
drawer put - /notes/todo.md < todo.md     # from stdin; needs an explicit file path
drawer ls -l /work                        # kind, revision, size, modified, name
drawer cat /notes/todo.md > copy.md       # stdout; --revision N for an old one
drawer get /work/report.pdf               # save to ./report.pdf; --force to overwrite
drawer history /notes/todo.md             # revisions, newest first
drawer rollback /notes/todo.md 2          # adds revision 2's bytes as the new head
drawer search "quarterly roadmap"         # names and contents
drawer rm /notes/old.md                   # to the trash; the id is kept
drawer trash                              # list it, with ids
drawer restore <id>                       # bring it back
drawer purge <id> --yes                   # gone for good
```

| Group | Commands |
| --- | --- |
| Run | `serve` (default), `stdio`, `doctor`, `mcp-config` |
| Files | `ls`, `mkdir`, `put`, `cat`, `get`, `stat`, `mv`, `cp` |
| Versions | `history`, `rollback` (`cp` takes `--revision`, `cat` and `get` too) |
| Trash | `rm`, `trash`, `restore <id>`, `purge <id> --yes` |
| Search and stars | `search`, `star`, `unstar`, `starred` |
| Sharing | `perms`, `share <path> <principal> <role>`, `unshare <path> <principal>` |

Global flags: `--data-dir` (`DRAWER_DATA_DIR`, default `./data`), `--author` (`DRAWER_AUTHOR`, default `$USER`) which is recorded on every change, and `--json` for machine-readable output from commands that return data.

`drawer doctor` checks database integrity, counts, missing blob files and whether the UI is built.

Writes are serialized inside one process. A CLI write while a server is writing to the same store can race it, so route concurrent writes through one or the other.

## MCP and agent harnesses

Drawer speaks MCP two ways:

- **Streamable HTTP** at `http://127.0.0.1:8787/mcp`, served by `drawer serve`. Best when several agents, or agents and people, share files.
- **Stdio** with `drawer --data-dir ./data stdio`. The harness starts Drawer itself.

For Hermes Agent (`~/.hermes/config.yaml`):

```yaml
mcp_servers:
  drawer:
    command: "/usr/local/bin/drawer"
    args: ["--data-dir", "/srv/drawer-data", "stdio"]
```

`drawer mcp-config` prints this for your paths. `drawer mcp-config --format json` prints an `mcpServers` block for Claude Desktop, Cursor and most other clients.

The server exposes 18 tools:

| Tools | |
| --- | --- |
| `drawer_list`, `drawer_stat`, `drawer_search`, `drawer_starred` | browse and find |
| `drawer_mkdir`, `drawer_read`, `drawer_write` | create and change content |
| `drawer_revisions`, `drawer_rollback`, `drawer_copy`, `drawer_move` | history and layout |
| `drawer_delete`, `drawer_trash`, `drawer_restore`, `drawer_purge` | trash lifecycle |
| `drawer_star`, `drawer_share`, `drawer_unshare` | stars and grants |

`drawer_read` and `drawer_write` take `encoding: "base64"` for binary files such as PDF, docx, xlsx or images. `drawer_read` returns PNG, JPEG, GIF and WebP images as image content so vision models can look at them. `drawer_purge` is the only destructive tool.

For concurrent agents, read the file, then write with `if_head_revision` set to the revision you read. On a conflict, re-read and merge.

Setup for Hermes, Claude Code, Claude Desktop, Codex, Gemini CLI, Cursor, VS Code, the OpenAI Agents SDK and LangChain is in [docs/harnesses.md](docs/harnesses.md).

**Mirroring Hermes output.** MCP only saves what the agent chooses to save. [`integrations/hermes/drawer-mirror`](integrations/hermes/drawer-mirror/README.md) is a Hermes plugin that copies every successful `write_file` and `patch` into Drawer after the fact, and records the links Hermes shares. Hermes keeps its own artifacts, and both can run side by side.

## REST API

Base path `/drawer/v1`. Nodes are named by `path=` or `id=`. Mutations name their actor with the `x-drawer-author` header (default `api`). `GET /health` and `GET /drawer/v1/health` both return `{"ok": true}`. With `--token` set, both need the bearer token like everything else.

| Method | Path | Notes |
| --- | --- | --- |
| GET | `/tree?path=/` | live children of a folder |
| POST | `/folders` | `{"path"}`, creates parents too |
| DELETE | `/folders?path=` or `?id=` | trash a folder (`&if_head_revision=N` optional) |
| GET | `/files?path=` or `?id=` | raw bytes; `&revision=N`; `&download=true` for an attachment. Response carries `x-drawer-revision` and `x-drawer-prop-*` |
| PUT | `/files?path=` | raw body; `x-drawer-comment`, `x-drawer-prop-<key>`, `&if_head_revision=N` |
| DELETE | `/files?path=` or `?id=` | trash a file (`&if_head_revision=N` optional) |
| GET | `/files/meta?path=` or `?id=` | id, head revision, size, mime, starred, trashed, properties |
| GET | `/revisions?path=` or `?id=` | revision history, newest first |
| POST | `/rollback` | `{"path"\|"id","revision","author","comment","if_head_revision"}` |
| POST | `/copy` | `{"from"\|"from_id","to","source_revision","author","comment","if_head_revision"}` |
| POST | `/move` | `{"from"\|"from_id","to"}`: rename or move, keeps the id |
| PATCH | `/nodes/{id}` | `{"name","parent_id","starred","trashed"}`: rename, move, star, trash, restore |
| DELETE | `/nodes/{id}` | purge; the node must already be in the trash |
| GET | `/trash` | top-most trashed nodes |
| GET | `/starred` | starred live nodes |
| GET / PUT | `/nodes/{id}/permissions` | list grants; `PUT {"principal","role"}` (owner only) |
| DELETE | `/nodes/{id}/permissions/{principal}` | remove a grant (owner only) |
| GET | `/search?q=` | names and contents |

Files are served with `nosniff`, and everything except PDFs gets a `sandbox` CSP, so stored HTML or SVG never runs scripts. Write responses include `new_revision`: `false` means content and properties matched the head, so nothing was added.

## Architecture

```
src/service.rs   nodes, revisions, grants, trash, search: the one service layer
src/api.rs       REST API at /drawer/v1
src/mcp.rs       MCP tools (rmcp), stdio and streamable HTTP
src/cli.rs       the drawer command
src/blob.rs      sha256 content-addressed blobs on disk
src/schema.sql   SQLite metadata (sqlx)
ui/              React, TypeScript, Vite: the web UI
integrations/    harness plugins, e.g. the Hermes drawer-mirror
```

The REST API, MCP server, CLI and UI all call the same service layer, so they agree on permissions, revisions and the trash.

**Limits.** Real user accounts don't exist yet. Actors are self-declared, so grants are cooperative, not a security boundary. Read grants aren't enforced yet. Copying folders isn't supported yet. There are no signed URLs, no blob garbage collection and no Postgres backend. Treat the optional bearer token as the access control.

**Upgrading.** Stores created on the earlier path-keyed schema are migrated in place on open. Their trashed files land in the trash. Write responses changed: `created` is now `new_revision`, and `version` is now `revision`.

## Configuration

| Variable | Used by | Default |
| --- | --- | --- |
| `DRAWER_DATA_DIR` | all store commands | `./data` |
| `DRAWER_ADDR` | `serve` | `127.0.0.1:8787` |
| `DRAWER_TOKEN` | `serve` | unset (an empty value counts as unset) |
| `DRAWER_UI_DIR` | `serve` | `./ui/dist`, then `../share/drawer/ui` beside the binary |
| `DRAWER_AUTHOR` | CLI writes | `$USER` |
| `DRAWER_PREFIX` | installer | `~/.local` |
| `DRAWER_VERSION` | installer | `latest` |
| `DRAWER_SOURCE=1` | installer | build from source instead of downloading a release |

`serve` warns when it listens beyond loopback without a token.

## Development

```sh
cargo test && cargo clippy --all-targets -- -D warnings
cd ui && npm ci && npm run dev     # UI dev server, proxies /drawer to 127.0.0.1:8787
cd ui && npm run build             # type-check and build ui/dist
cargo run --release -- serve       # API, MCP and UI on http://127.0.0.1:8787
```

CI runs clippy, the Rust tests and the UI build on every push to `main` and every pull request.

Releases: pushing a `v*` tag builds Linux (x86_64, aarch64) and macOS (x86_64, aarch64) binaries and attaches them with `SHA256SUMS` (`.github/workflows/release.yml`).
