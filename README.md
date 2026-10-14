# Drawer

Versioned filesystem storage for agents. One Rust binary serves a REST API, an MCP server and a web UI over the same service layer.

Short form: `drawer` (crate name, MCP tool prefix, `/drawer/v1` API path).

## What works today

- Folders and files addressed by absolute path (`/notes/todo.md`)
- Immutable versions: every write is a new version, nothing is rewritten
- Rollback (writes an old version's content as a new version)
- Soft delete (history is kept, writing the path again restores it)
- Search over file names and text contents
- Content-addressed blobs on disk (sha256), metadata in SQLite
- REST API, MCP (HTTP and stdio), React UI
- Optional bearer token

Not yet: users/teams/agents permissions, links to agent memories, tasks and workflows, Postgres.

## Run

```sh
cd ui && npm ci && npm run build && cd ..   # optional, for the UI
cargo run --release -- serve                # http://127.0.0.1:8787
```

Options: `--data-dir` (`DRAWER_DATA_DIR`, default `./data`), `--addr` (`DRAWER_ADDR`), `--token` (`DRAWER_TOKEN`), `--ui-dir` (`DRAWER_UI_DIR`).

## MCP (Hermes Agent and others)

Streamable HTTP endpoint: `http://127.0.0.1:8787/mcp`. Stdio: `drawer --data-dir ./data stdio`.

Tools: `drawer_list`, `drawer_mkdir`, `drawer_read`, `drawer_write`, `drawer_delete`, `drawer_stat`, `drawer_versions`, `drawer_rollback`, `drawer_search`.

For Hermes, add an MCP server entry pointing at either the URL above or the stdio command. Check the Hermes docs for the exact config key names for your version.

## REST API (`/drawer/v1`)

| Method | Path | Notes |
| --- | --- | --- |
| GET | `/tree?path=/` | list a folder |
| POST / DELETE | `/folders` | `{"path"}` / `?path=` (empty folders only) |
| GET / PUT / DELETE | `/files?path=` | raw bytes, `&version=N`; PUT takes `x-drawer-author`, `x-drawer-comment` |
| GET | `/files/meta?path=` | metadata |
| GET | `/versions?path=` | history, newest first |
| POST | `/rollback` | `{"path","version","author"}` |
| GET | `/search?q=` | names and contents |

## Develop

```sh
cargo test && cargo clippy --all-targets
cd ui && npm run dev   # proxies /drawer to 127.0.0.1:8787
```
