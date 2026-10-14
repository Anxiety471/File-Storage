# Drawer

Versioned filesystem storage for agents. One Rust binary serves a REST API, an MCP server and a web UI over the same service layer.

Short form: `drawer` (crate name, MCP tool prefix, `/drawer/v1` API path).

## What works today

Storage semantics follow Google Cloud Storage, with folders kept explicit:

- Folders and files addressed by absolute path (`/notes/todo.md`)
- **Generations**: every change to a file is a new immutable generation; nothing is rewritten
- **Preconditions** (`if_generation_match`): `0` writes only if the file does not exist, `N` writes only if the live generation is still `N`. A stale writer gets `412 Precondition Failed` and nothing changes. Works on write, delete, copy and rollback.
- **Custom metadata** per generation: `x-drawer-meta-<key>` headers over REST (returned on read too; keys are lowercased and values must be ASCII to fit in a header), a `metadata` object over MCP
- **Copy** between paths, or to the same path as a new generation. Content is shared by hash, so nothing is re-uploaded. Move = copy then delete.
- Rollback (makes an old generation the live one, as a new generation)
- Soft delete (history is kept, writing the path again restores it)
- Search over file names and text contents
- Content-addressed blobs on disk (sha256), metadata in SQLite
- REST API, MCP (HTTP and stdio), React UI (saves are guarded on the generation you opened)
- Optional bearer token

Not yet: buckets, signed URLs, retention/lifecycle rules, prefix/delimiter listing, users/teams/agents permissions, links to agent memories, tasks and workflows, Postgres.

Upgrading from `version` to `generation`: stores created earlier are migrated in place on open (columns renamed, `metadata` added). Old write responses used `created`; they now use `new_generation`.

## Run

```sh
cd ui && npm ci && npm run build && cd ..   # optional, for the UI
cargo run --release -- serve                # http://127.0.0.1:8787
```

Options: `--data-dir` (`DRAWER_DATA_DIR`, default `./data`), `--addr` (`DRAWER_ADDR`), `--token` (`DRAWER_TOKEN`), `--ui-dir` (`DRAWER_UI_DIR`).

## MCP (Hermes Agent and others)

Streamable HTTP endpoint: `http://127.0.0.1:8787/mcp`. Stdio: `drawer --data-dir ./data stdio`.

Tools: `drawer_list`, `drawer_mkdir`, `drawer_read`, `drawer_write`, `drawer_delete`, `drawer_stat`, `drawer_generations`, `drawer_rollback`, `drawer_copy`, `drawer_search`.

For concurrent agents, read the file, then write with `if_generation_match` set to the generation you read. On a conflict, re-read and merge.

For Hermes, add an MCP server entry pointing at either the URL above or the stdio command. Check the Hermes docs for the exact config key names for your version.

## REST API (`/drawer/v1`)

| Method | Path | Notes |
| --- | --- | --- |
| GET | `/tree?path=/` | list a folder |
| POST / DELETE | `/folders` | `{"path"}` / `?path=` (empty folders only) |
| GET | `/files?path=` | raw bytes, `&generation=N`; response has `x-drawer-generation` and `x-drawer-meta-*` |
| PUT | `/files?path=` | raw body; `x-drawer-author`, `x-drawer-comment`, `x-drawer-meta-<key>`; `&if_generation_match=N` |
| DELETE | `/files?path=` | `&if_generation_match=N` |
| GET | `/files/meta?path=` | metadata, `&generation=N` |
| GET | `/generations?path=` | history, newest first |
| POST | `/rollback` | `{"path","generation","author","comment","if_generation_match"}` |
| POST | `/copy` | `{"from","to","source_generation","author","comment","if_generation_match"}` |
| GET | `/search?q=` | names and contents |

Write responses include `new_generation`: `false` means the content and metadata matched the live generation, so nothing was added.

## Develop

```sh
cargo test && cargo clippy --all-targets
cd ui && npm run dev   # proxies /drawer to 127.0.0.1:8787
```
