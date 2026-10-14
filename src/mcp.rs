//! MCP server. Tool names carry the `drawer_` prefix.
//!
//! Agents address nodes by path. Trash, restore and purge take a node `id`, since a trashed
//! node no longer has a live path. `author` names the actor for permission checks.
use std::{collections::BTreeMap, sync::Arc};

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};

use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig, ToolAnnotations},
    schemars, tool, tool_handler, tool_router,
};
use serde::Deserialize;
use serde_json::Value;

use crate::service::{Drawer, NodeUpdate, Ref, WriteOpts};

#[derive(Clone)]
pub struct DrawerMcp {
    svc: Drawer,
    tool_router: ToolRouter<Self>,
}

impl DrawerMcp {
    pub fn new(svc: Drawer) -> Self {
        let mut tool_router = Self::tool_router();
        for route in tool_router.map.values_mut() {
            let tool = &mut route.attr;
            let schema = portable_schema(Value::Object((*tool.input_schema).clone()));
            if let Value::Object(schema) = schema {
                tool.input_schema = Arc::new(schema);
            }
            tool.annotations = Some(annotations_for(&tool.name));
        }
        Self { svc, tool_router }
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
struct PathArg {
    /// Absolute path such as /notes/todo.md
    path: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ReadArg {
    /// Absolute path such as /notes/todo.md
    path: String,
    /// Specific revision to read; defaults to the head revision.
    revision: Option<i64>,
    /// "text" (default) or "base64". Binary files such as PDFs, Word or Excel documents and images must be read as base64.
    encoding: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct WriteArg {
    /// Absolute path; parent folders are created automatically.
    path: String,
    /// Full new content of the file: plain text, or base64 when encoding is "base64".
    content: String,
    /// "text" (default) or "base64". Use base64 to store binary files such as PDF, docx, xlsx or images.
    encoding: Option<String>,
    /// Why this change was made (stored with the revision).
    comment: Option<String>,
    /// Who is writing, e.g. the agent's name. Defaults to "agent". Also the name checked against sharing grants.
    author: Option<String>,
    /// Custom key/value properties stored with this revision; values are strings, e.g. {"source": "web"}.
    properties: Option<BTreeMap<String, String>>,
    /// Optional precondition: 0 = only if no live file is here; N = only if the head revision is N. On failure nothing changes, so re-read and merge.
    if_head_revision: Option<i64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct DeleteArg {
    /// Absolute path of the file or folder to move to the trash.
    path: String,
    /// Optional precondition on the head revision, as for drawer_write.
    if_head_revision: Option<i64>,
    author: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct RollbackArg {
    path: String,
    /// Revision whose content becomes the new head revision.
    revision: i64,
    author: Option<String>,
    if_head_revision: Option<i64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct CopyArg {
    /// Absolute path to copy from.
    from: String,
    /// Absolute path to copy to. May equal `from`, which adds a revision with the same content.
    to: String,
    /// Revision of `from` to copy; defaults to the head revision.
    source_revision: Option<i64>,
    author: Option<String>,
    comment: Option<String>,
    if_head_revision: Option<i64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct MoveArg {
    /// Current absolute path.
    from: String,
    /// New absolute path. Renames if only the last segment differs; moves otherwise.
    to: String,
    author: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct StarArg {
    path: String,
    /// true to star, false to unstar.
    starred: bool,
    author: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct IdArg {
    /// Node id, as shown by drawer_trash or drawer_stat.
    id: String,
    author: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct ShareArg {
    path: String,
    /// Who gets access: an agent name, or "anyone".
    principal: String,
    /// reader, writer or owner.
    role: String,
    author: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct UnshareArg {
    path: String,
    principal: String,
    author: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct SearchArg {
    /// Case-insensitive text matched against file paths and file contents.
    query: String,
    limit: Option<usize>,
}

/// Tool input schemas in the subset every major harness and model API accepts.
///
/// schemars emits `"type": ["string", "null"]` for optional fields, `format` hints and a
/// `$schema` key. OpenAI accepts them, but Gemini (and harnesses that forward schemas to it)
/// rejects type arrays and unknown keywords. Optional fields are already optional because they
/// are absent from `required`, so dropping `null` loses nothing.
fn portable_schema(v: Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                match k.as_str() {
                    "$schema" | "format" | "additionalProperties" | "default" => continue,
                    "type" => {
                        let ty = match v {
                            Value::Array(types) => {
                                let mut types: Vec<Value> = types.into_iter().filter(|t| t != "null").collect();
                                if types.len() == 1 { types.remove(0) } else { Value::Array(types) }
                            }
                            other => other,
                        };
                        out.insert(k, ty);
                    }
                    _ => {
                        out.insert(k, portable_schema(v));
                    }
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(portable_schema).collect()),
        other => other,
    }
}

/// Hints that let harnesses auto-approve safe calls and confirm risky ones.
fn annotations_for(name: &str) -> ToolAnnotations {
    let base = ToolAnnotations::new().open_world(false);
    match name {
        "drawer_list" | "drawer_read" | "drawer_stat" | "drawer_revisions" | "drawer_trash" | "drawer_starred"
        | "drawer_search" => base.read_only(true),
        // gone for good: history included
        "drawer_purge" => base.read_only(false).destructive(true).idempotent(false),
        "drawer_unshare" => base.read_only(false).destructive(true).idempotent(true),
        // reversible: trash can be restored, writes and moves keep history
        "drawer_mkdir" | "drawer_delete" | "drawer_restore" | "drawer_star" | "drawer_share" | "drawer_move" => {
            base.read_only(false).destructive(false).idempotent(true)
        }
        _ => base.read_only(false).destructive(false).idempotent(false),
    }
}

/// Images small enough to hand to a vision model directly.
const MAX_INLINE_IMAGE: usize = 5 * 1024 * 1024;

fn json<T: serde::Serialize>(v: T) -> Result<String, String> {
    serde_json::to_string_pretty(&v).map_err(|e| e.to_string())
}

fn base64_wanted(encoding: &Option<String>) -> Result<bool, String> {
    match encoding.as_deref() {
        None | Some("text") | Some("utf8") | Some("utf-8") => Ok(false),
        Some("base64") => Ok(true),
        Some(other) => Err(format!("unknown encoding {other:?}; use \"text\" or \"base64\"")),
    }
}

fn actor(author: Option<String>) -> String {
    author.unwrap_or_else(|| "agent".into())
}

#[tool_router]
impl DrawerMcp {
    #[tool(description = "List the live files and folders directly inside a folder (use / for the root).")]
    async fn drawer_list(&self, Parameters(a): Parameters<PathArg>) -> Result<String, String> {
        json(self.svc.list(&a.path).await.map_err(|e| e.to_string())?)
    }

    #[tool(description = "Create a folder, including any missing parents. Returns the folder's node id.")]
    async fn drawer_mkdir(&self, Parameters(a): Parameters<PathArg>) -> Result<String, String> {
        json(self.svc.mkdir(&a.path, "agent").await.map_err(|e| e.to_string())?)
    }

    #[tool(description = "Read a file. Returns the head revision unless one is given. Text files come back as text and PNG, JPEG, GIF or WebP images as image content. Pass encoding \"base64\" for other binary files such as PDF, docx or xlsx.")]
    async fn drawer_read(&self, Parameters(a): Parameters<ReadArg>) -> Result<CallToolResult, String> {
        let (rev, data) = self.svc.read(Ref::Path(&a.path), a.revision).await.map_err(|e| e.to_string())?;
        if base64_wanted(&a.encoding)? {
            return Ok(CallToolResult::success(vec![ContentBlock::text(BASE64.encode(data))]));
        }
        // Raster images come back as MCP image content, so vision models can look at them.
        let viewable = matches!(rev.mime.as_str(), "image/png" | "image/jpeg" | "image/gif" | "image/webp");
        if a.encoding.is_none() && viewable && data.len() <= MAX_INLINE_IMAGE {
            return Ok(CallToolResult::success(vec![
                ContentBlock::text(format!("{} (revision {}, {}, {} bytes)", a.path, rev.revision, rev.mime, rev.size)),
                ContentBlock::image(BASE64.encode(data), rev.mime),
            ]));
        }
        let text = String::from_utf8(data)
            .map_err(|_| format!("{} is binary ({}); read it with encoding \"base64\"", a.path, rev.mime))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }

    #[tool(description = "Create or overwrite a file. Every change becomes a new revision. Pass encoding \"base64\" to store binary files such as PDF, docx, xlsx or images. Returns new_revision=false when nothing changed.")]
    async fn drawer_write(&self, Parameters(a): Parameters<WriteArg>) -> Result<String, String> {
        let opts = WriteOpts {
            author: actor(a.author),
            comment: a.comment.unwrap_or_default(),
            properties: a.properties.unwrap_or_default(),
            if_head_revision: a.if_head_revision,
        };
        let data = if base64_wanted(&a.encoding)? {
            BASE64.decode(a.content.trim()).map_err(|e| format!("content is not valid base64: {e}"))?
        } else {
            a.content.into_bytes()
        };
        let out = self.svc.write(&a.path, &data, opts).await.map_err(|e| e.to_string())?;
        json(out)
    }

    #[tool(description = "Move a file or folder to the trash. Restore it with drawer_restore, or delete it forever with drawer_purge.")]
    async fn drawer_delete(&self, Parameters(a): Parameters<DeleteArg>) -> Result<String, String> {
        json(
            self.svc
                .trash(Ref::Path(&a.path), a.if_head_revision, &actor(a.author))
                .await
                .map_err(|e| e.to_string())?,
        )
    }

    #[tool(description = "Show a node's metadata: id, kind, head revision, size, mime, starred, trashed, properties.")]
    async fn drawer_stat(&self, Parameters(a): Parameters<PathArg>) -> Result<String, String> {
        json(self.svc.stat(Ref::Path(&a.path)).await.map_err(|e| e.to_string())?)
    }

    #[tool(description = "List all revisions of a file, newest first.")]
    async fn drawer_revisions(&self, Parameters(a): Parameters<PathArg>) -> Result<String, String> {
        json(self.svc.revisions(Ref::Path(&a.path)).await.map_err(|e| e.to_string())?)
    }

    #[tool(description = "Make an earlier revision the head revision. Adds a new revision with the old content; history is kept.")]
    async fn drawer_rollback(&self, Parameters(a): Parameters<RollbackArg>) -> Result<String, String> {
        let mut opts = WriteOpts::new(actor(a.author), "");
        opts.if_head_revision = a.if_head_revision;
        json(self.svc.rollback(Ref::Path(&a.path), a.revision, opts).await.map_err(|e| e.to_string())?)
    }

    #[tool(description = "Copy a file to another path, or to its own path as a new revision. Content is shared, not duplicated.")]
    async fn drawer_copy(&self, Parameters(a): Parameters<CopyArg>) -> Result<String, String> {
        let mut opts = WriteOpts::new(actor(a.author), a.comment.unwrap_or_default());
        opts.if_head_revision = a.if_head_revision;
        json(
            self.svc
                .copy(Ref::Path(&a.from), &a.to, a.source_revision, opts)
                .await
                .map_err(|e| e.to_string())?,
        )
    }

    #[tool(description = "Rename or move a file or folder. Keeps its id, history and sharing. Creates missing destination folders.")]
    async fn drawer_move(&self, Parameters(a): Parameters<MoveArg>) -> Result<String, String> {
        json(self.svc.move_to(Ref::Path(&a.from), &a.to, &actor(a.author)).await.map_err(|e| e.to_string())?)
    }

    #[tool(description = "Star or unstar a file or folder.")]
    async fn drawer_star(&self, Parameters(a): Parameters<StarArg>) -> Result<String, String> {
        let node = self.svc.stat(Ref::Path(&a.path)).await.map_err(|e| e.to_string())?;
        let u = NodeUpdate { starred: Some(a.starred), ..NodeUpdate::default() };
        json(self.svc.update(&node.id, u, &actor(a.author)).await.map_err(|e| e.to_string())?)
    }

    #[tool(description = "List the top-most items in the trash, with their ids.")]
    async fn drawer_trash(&self) -> Result<String, String> {
        json(self.svc.trash_list().await.map_err(|e| e.to_string())?)
    }

    #[tool(description = "Restore a trashed file or folder by id. Its parent folder must not be in the trash.")]
    async fn drawer_restore(&self, Parameters(a): Parameters<IdArg>) -> Result<String, String> {
        let u = NodeUpdate { trashed: Some(false), ..NodeUpdate::default() };
        json(self.svc.update(&a.id, u, &actor(a.author)).await.map_err(|e| e.to_string())?)
    }

    #[tool(description = "Permanently delete a trashed file or folder by id, with its history. This cannot be undone.")]
    async fn drawer_purge(&self, Parameters(a): Parameters<IdArg>) -> Result<String, String> {
        self.svc.purge(&a.id, &actor(a.author)).await.map_err(|e| e.to_string())?;
        Ok("deleted forever".into())
    }

    #[tool(description = "Grant a principal a role (reader, writer or owner) on a file or folder. Applies to everything below it.")]
    async fn drawer_share(&self, Parameters(a): Parameters<ShareArg>) -> Result<String, String> {
        let node = self.svc.stat(Ref::Path(&a.path)).await.map_err(|e| e.to_string())?;
        json(
            self.svc
                .share(&node.id, &a.principal, &a.role, &actor(a.author))
                .await
                .map_err(|e| e.to_string())?,
        )
    }

    #[tool(description = "Remove a principal's grant on a file or folder.")]
    async fn drawer_unshare(&self, Parameters(a): Parameters<UnshareArg>) -> Result<String, String> {
        let node = self.svc.stat(Ref::Path(&a.path)).await.map_err(|e| e.to_string())?;
        json(self.svc.unshare(&node.id, &a.principal, &actor(a.author)).await.map_err(|e| e.to_string())?)
    }

    #[tool(description = "List starred files and folders.")]
    async fn drawer_starred(&self) -> Result<String, String> {
        json(self.svc.starred().await.map_err(|e| e.to_string())?)
    }

    #[tool(description = "Search live file names and text contents.")]
    async fn drawer_search(&self, Parameters(a): Parameters<SearchArg>) -> Result<String, String> {
        json(self.svc.search(&a.query, a.limit.unwrap_or(20)).await.map_err(|e| e.to_string())?)
    }
}

/// Sent to the client on connect; most harnesses add it to the model's context.
const INSTRUCTIONS: &str = "Drawer is versioned file storage shared by agents and people. \
Paths are absolute (/notes/todo.md); parent folders are created on write. Every write adds a \
revision, so nothing is lost: use drawer_revisions and drawer_rollback to go back. When other \
agents may edit the same file, read it, then write with if_head_revision set to the revision you \
read; on a precondition failure, re-read and merge. Binary files (PDF, Office, images) travel as \
base64 with encoding \"base64\". Deleting moves to the trash; drawer_purge is the only permanent delete.";

#[tool_handler(router = self.tool_router)]
impl ServerHandler for DrawerMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("drawer", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}
