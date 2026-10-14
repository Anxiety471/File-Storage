# Drawer

Versioned filesystem storage for agents, modelled on Google Drive. One Rust binary serves a REST API, an MCP server and a web UI over the same service layer.

Short form: `drawer` (crate name, MCP tool prefix, `/drawer/v1` API path).

## Model

- **Nodes with stable ids.** Every file and folder has an id that survives rename, move and trash. A node's path is derived from its parent chain, so moving a folder moves everything under it without rewriting rows.
- **Revisions.** A file's content is a list of immutable revisions. Nothing is rewritten. Rollback makes an old revision the head by adding a new one.
- **Preconditions.** Writes, trashing, copying and rollback accept `if_head_revision`. `0` means only create when no live file is there. `N` means only change the file while its head is still revision `N`. A stale writer gets `412 Precondition Failed` and nothing changes.
- **Properties.** Custom key/value pairs stored with each revision: `x-drawer-prop-<key>` headers over REST (returned on read too), a `properties` object over MCP.
- **Trash.** Trashing a file or folder hides it and everything under it. Restore it by id, or delete it forever with purge. Writing to a trashed path revives that file with a new revision.
- **Star.** Star any file or folder and list the starred items.
- **Copy and move.** Copy shares content by hash, so nothing is re-uploaded. Move or rename keeps the id, history and grants.
- **Sharing.** Role grants (`reader`, `writer`, `owner`) on a node apply to everything below it. A node with no grants anywhere is open.
- **Search** over live file names and text contents.
- Content-addressed blobs on disk (sha256), metadata in SQLite.
- Optional bearer token.

Not yet: real user accounts (actors are self-declared, so grants are cooperative, not a security boundary), enforcement of read grants, copying folders, signed URLs, retention and garbage collection of blobs, Postgres.

Upgrading: stores created on the earlier path-keyed schema are migrated in place on open. Their trashed files land in the trash. Write responses changed: `created` is now `new_revision`, and `version` is now `revision`.

## Run

```sh
cd ui && npm ci && npm run build && cd ..   # optional, for the UI
cargo run --release -- serve                # http://127.0.0.1:8787
```

Options: `--data-dir` (`DRAWER_DATA_DIR`, default `./data`), `--addr` (`DRAWER_ADDR`), `--token` (`DRAWER_TOKEN`), `--ui-dir` (`DRAWER_UI_DIR`).

## MCP (Hermes Agent and others)

Streamable HTTP endpoint: `http://127.0.0.1:8787/mcp`. Stdio: `drawer --data-dir ./data stdio`.

Tools: `drawer_list`, `drawer_mkdir`, `drawer_read`, `drawer_write`, `drawer_stat`, `drawer_revisions`, `drawer_rollback`, `drawer_copy`, `drawer_move`, `drawer_delete` (trash), `drawer_trash`, `drawer_restore`, `drawer_purge`, `drawer_star`, `drawer_starred`, `drawer_share`, `drawer_unshare`, `drawer_search`.

Agents address nodes by path. Trash, restore and purge take the node `id`, which `drawer_trash` and `drawer_stat` return.

For concurrent agents, read the file, then write with `if_head_revision` set to the revision you read. On a conflict, re-read and merge.

For Hermes, add an MCP server entry pointing at either the URL above or the stdio command. Check the Hermes docs for the exact config key names for your version.

## REST API (`/drawer/v1`)

Nodes are named by `path=` or `id=`. Mutations name their actor with `x-drawer-author` (default `api`).

| Method | Path | Notes |
| --- | --- | --- |
| GET | `/tree?path=/` | live children of a folder |
| POST | `/folders` | `{"path"}`, creates parents too |
| DELETE | `/folders?path=` or `?id=` | trash a folder (`&if_head_revision=N` optional) |
| GET | `/files?path=` or `?id=` | raw bytes, `&revision=N`; response has `x-drawer-revision` and `x-drawer-prop-*` |
| PUT | `/files?path=` | raw body; `x-drawer-comment`, `x-drawer-prop-<key>`; `&if_head_revision=N` |
| DELETE | `/files?path=` or `?id=` | trash a file (`&if_head_revision=N` optional) |
| GET | `/files/meta?path=` or `?id=` | the node: id, head revision, size, mime, starred, trashed, properties |
| GET | `/revisions?path=` or `?id=` | revision history, newest first |
| POST | `/rollback` | `{"path"\|"id","revision","author","comment","if_head_revision"}` |
| POST | `/copy` | `{"from"\|"from_id","to","source_revision","author","comment","if_head_revision"}` |
| POST | `/move` | `{"from"\|"from_id","to"}`, renames or moves, keeps the id |
| PATCH | `/nodes/{id}` | `{"name","parent_id","starred","trashed"}`: rename, move, star, trash, restore |
| DELETE | `/nodes/{id}` | purge: permanent, the node must already be in the trash |
| GET | `/trash` | top-most trashed nodes |
| GET | `/starred` | starred live nodes |
| GET | `/nodes/{id}/permissions` | grants on a node |
| PUT | `/nodes/{id}/permissions` | `{"principal","role"}`, owner only |
| DELETE | `/nodes/{id}/permissions/{principal}` | owner only |
| GET | `/search?q=` | names and contents |

Write responses include `new_revision`: `false` means the content and properties matched the head, so nothing was added.

## Develop

```sh
cargo test && cargo clippy --all-targets -- -D warnings
cd ui && npm run dev   # proxies /drawer to 127.0.0.1:8787
```
