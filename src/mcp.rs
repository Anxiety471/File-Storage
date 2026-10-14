//! MCP server. Tool names carry the `drawer_` prefix.
//!
//! Agents address nodes by path. Trash, restore and purge take a node `id`, since a trashed
//! node no longer has a live path. `author` names the actor for permission checks.
use std::collections::BTreeMap;

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};

use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerConfig},
    schemars, tool, tool_handler, tool_router,
};
use serde::Deserialize;

use crate::service::{Drawer, NodeUpdate, Ref, WriteOpts};

#[derive(Clone)]
pub struct DrawerMcp {
    svc: Drawer,
    tool_router: ToolRouter<Self>,
}

impl DrawerMcp {
    pub fn new(svc: Drawer) -> Self {
        Self { svc, tool_router: Self::tool_router() }
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
    /// Custom key/value properties stored with this revision, e.g. {"source": "web"}.
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

    #[tool(description = "Read a file. Returns the head revision unless one is given. Text by default; pass encoding \"base64\" for binary files such as PDF, docx, xlsx or images.")]
    async fn drawer_read(&self, Parameters(a): Parameters<ReadArg>) -> Result<String, String> {
        let (rev, data) = self.svc.read(Ref::Path(&a.path), a.revision).await.map_err(|e| e.to_string())?;
        if base64_wanted(&a.encoding)? {
            return Ok(BASE64.encode(data));
        }
        String::from_utf8(data)
            .map_err(|_| format!("{} is binary ({}); read it with encoding \"base64\"", a.path, rev.mime))
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

#[tool_handler(router = self.tool_router)]
impl ServerHandler for DrawerMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }
}
