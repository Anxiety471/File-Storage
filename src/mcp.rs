//! MCP server. Tool names carry the `drawer_` prefix.
use std::collections::BTreeMap;

use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerConfig},
    schemars, tool, tool_handler, tool_router,
};
use serde::Deserialize;

use crate::service::{Drawer, WriteOpts};

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
    /// Specific generation to read; defaults to the live one.
    generation: Option<i64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct WriteArg {
    /// Absolute path; parent folders are created automatically.
    path: String,
    /// Full new text content of the file.
    content: String,
    /// Why this change was made (stored with the generation).
    comment: Option<String>,
    /// Who is writing, e.g. the agent's name. Defaults to "agent".
    author: Option<String>,
    /// Custom key/value metadata stored with this generation, e.g. {"source": "web"}.
    metadata: Option<BTreeMap<String, String>>,
    /// Optional precondition. 0 = only if the file does not exist; N = only if its live generation is N. Use it when you edited a file you read earlier: on failure the write is refused and nothing changed, so re-read and merge.
    if_generation_match: Option<i64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct DeleteArg {
    /// Absolute path of the file to delete.
    path: String,
    /// Optional precondition. 0 = only if the file does not exist; N = only if its live generation is N. Use it when you edited a file you read earlier: on failure the write is refused and nothing changed, so re-read and merge.
    if_generation_match: Option<i64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct RollbackArg {
    path: String,
    /// Generation whose content becomes the new live generation.
    generation: i64,
    author: Option<String>,
    /// Optional precondition. 0 = only if the file does not exist; N = only if its live generation is N. Use it when you edited a file you read earlier: on failure the write is refused and nothing changed, so re-read and merge.
    if_generation_match: Option<i64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct CopyArg {
    /// Absolute path to copy from.
    from: String,
    /// Absolute path to copy to. May equal `from`, which adds a new generation with the same content.
    to: String,
    /// Generation of `from` to copy; defaults to the live one.
    source_generation: Option<i64>,
    author: Option<String>,
    comment: Option<String>,
    /// Optional precondition. 0 = only if the file does not exist; N = only if its live generation is N. Use it when you edited a file you read earlier: on failure the write is refused and nothing changed, so re-read and merge.
    if_generation_match: Option<i64>,
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

#[tool_router]
impl DrawerMcp {
    #[tool(description = "List the files and folders directly inside a folder (use / for the root).")]
    async fn drawer_list(&self, Parameters(a): Parameters<PathArg>) -> Result<String, String> {
        json(self.svc.list(&a.path).await.map_err(|e| e.to_string())?)
    }

    #[tool(description = "Create a folder, including any missing parents.")]
    async fn drawer_mkdir(&self, Parameters(a): Parameters<PathArg>) -> Result<String, String> {
        self.svc.mkdir(&a.path).await.map_err(|e| e.to_string())
    }

    #[tool(description = "Read a text file. Returns the content; the live generation unless one is given.")]
    async fn drawer_read(&self, Parameters(a): Parameters<ReadArg>) -> Result<String, String> {
        let (_, data) = self.svc.read(&a.path, a.generation).await.map_err(|e| e.to_string())?;
        String::from_utf8(data).map_err(|_| "file is binary; use the REST API to download it".into())
    }

    #[tool(description = "Create or overwrite a text file. Every change becomes a new immutable generation. Returns new_generation=false when nothing changed.")]
    async fn drawer_write(&self, Parameters(a): Parameters<WriteArg>) -> Result<String, String> {
        let opts = WriteOpts {
            author: a.author.unwrap_or_else(|| "agent".into()),
            comment: a.comment.unwrap_or_default(),
            metadata: a.metadata.unwrap_or_default(),
            if_generation_match: a.if_generation_match,
        };
        let out = self.svc.write(&a.path, a.content.as_bytes(), opts).await.map_err(|e| e.to_string())?;
        json(out)
    }

    #[tool(description = "Delete a file. Its history is kept and writing the path again restores it.")]
    async fn drawer_delete(&self, Parameters(a): Parameters<DeleteArg>) -> Result<String, String> {
        self.svc.delete_file(&a.path, a.if_generation_match).await.map_err(|e| e.to_string())?;
        Ok("deleted".into())
    }

    #[tool(description = "Show file metadata: size, mime, live generation, author, custom metadata.")]
    async fn drawer_stat(&self, Parameters(a): Parameters<ReadArg>) -> Result<String, String> {
        json(self.svc.stat(&a.path, a.generation).await.map_err(|e| e.to_string())?)
    }

    #[tool(description = "List all generations of a file, newest first.")]
    async fn drawer_generations(&self, Parameters(a): Parameters<PathArg>) -> Result<String, String> {
        json(self.svc.generations(&a.path).await.map_err(|e| e.to_string())?)
    }

    #[tool(description = "Make an earlier generation the live one. Adds a new generation with the old content; history is kept.")]
    async fn drawer_rollback(&self, Parameters(a): Parameters<RollbackArg>) -> Result<String, String> {
        let mut opts = WriteOpts::new(a.author.unwrap_or_else(|| "agent".into()), "");
        opts.if_generation_match = a.if_generation_match;
        json(self.svc.rollback(&a.path, a.generation, opts).await.map_err(|e| e.to_string())?)
    }

    #[tool(description = "Copy a file to another path (or to the same path as a new generation). Content is shared, not duplicated. To move a file, copy it then delete the source.")]
    async fn drawer_copy(&self, Parameters(a): Parameters<CopyArg>) -> Result<String, String> {
        let mut opts = WriteOpts::new(a.author.unwrap_or_else(|| "agent".into()), a.comment.unwrap_or_default());
        opts.if_generation_match = a.if_generation_match;
        json(self.svc.copy(&a.from, &a.to, a.source_generation, opts).await.map_err(|e| e.to_string())?)
    }

    #[tool(description = "Search file names and text contents.")]
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
