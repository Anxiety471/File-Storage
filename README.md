# Mnemosyne

Versioned filesystem storage for agents. One Rust binary serves a REST API, an MCP server and a web UI over the same service layer.

Short form: `mnemo` (crate name, MCP tool prefix, `/mnemo/v1` API path).

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

Options: `--data-dir` (`MNEMO_DATA_DIR`, default `./data`), `--addr` (`MNEMO_ADDR`), `--token` (`MNEMO_TOKEN`), `--ui-dir` (`MNEMO_UI_DIR`).

## MCP (Hermes Agent and others)

Streamable HTTP endpoint: `http://127.0.0.1:8787/mcp`. Stdio: `mnemo --data-dir ./data stdio`.

Tools: `mnemo_list`, `mnemo_mkdir`, `mnemo_read`, `mnemo_write`, `mnemo_delete`, `mnemo_stat`, `mnemo_versions`, `mnemo_rollback`, `mnemo_search`.

For Hermes, add an MCP server entry pointing at either the URL above or the stdio command. Check the Hermes docs for the exact config key names for your version.

## REST API (`/mnemo/v1`)

| Method | Path | Notes |
| --- | --- | --- |
| GET | `/tree?path=/` | list a folder |
| POST / DELETE | `/folders` | `{"path"}` / `?path=` (empty folders only) |
| GET / PUT / DELETE | `/files?path=` | raw bytes, `&version=N`; PUT takes `x-mnemo-author`, `x-mnemo-comment` |
| GET | `/files/meta?path=` | metadata |
| GET | `/versions?path=` | history, newest first |
| POST | `/rollback` | `{"path","version","author"}` |
| GET | `/search?q=` | names and contents |

## Develop

```sh
cargo test && cargo clippy --all-targets
cd ui && npm run dev   # proxies /mnemo to 127.0.0.1:8787
```
