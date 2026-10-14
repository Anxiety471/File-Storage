//! MCP server. Tool names carry the `mnemo_` prefix.
use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerConfig},
    schemars, tool, tool_handler, tool_router,
};
use serde::Deserialize;

use crate::service::Mnemo;

#[derive(Clone)]
pub struct MnemoMcp {
    svc: Mnemo,
    tool_router: ToolRouter<Self>,
}

impl MnemoMcp {
    pub fn new(svc: Mnemo) -> Self {
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
    /// Specific version to read; defaults to the latest.
    version: Option<i64>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct WriteArg {
    /// Absolute path; parent folders are created automatically.
    path: String,
    /// Full new text content of the file.
    content: String,
    /// Why this change was made (stored with the version).
    comment: Option<String>,
    /// Who is writing, e.g. the agent's name. Defaults to "agent".
    author: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct RollbackArg {
    path: String,
    /// Version number whose content becomes the new latest version.
    version: i64,
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

#[tool_router]
impl MnemoMcp {
    #[tool(description = "List the files and folders directly inside a folder (use / for the root).")]
    async fn mnemo_list(&self, Parameters(a): Parameters<PathArg>) -> Result<String, String> {
        json(self.svc.list(&a.path).await.map_err(|e| e.to_string())?)
    }

    #[tool(description = "Create a folder, including any missing parents.")]
    async fn mnemo_mkdir(&self, Parameters(a): Parameters<PathArg>) -> Result<String, String> {
        self.svc.mkdir(&a.path).await.map_err(|e| e.to_string())
    }

    #[tool(description = "Read a text file. Returns the content; the version is the latest unless given.")]
    async fn mnemo_read(&self, Parameters(a): Parameters<ReadArg>) -> Result<String, String> {
        let (_, data) = self.svc.read(&a.path, a.version).await.map_err(|e| e.to_string())?;
        String::from_utf8(data).map_err(|_| "file is binary; use the REST API to download it".into())
    }

    #[tool(description = "Create or overwrite a text file. Every change becomes a new immutable version.")]
    async fn mnemo_write(&self, Parameters(a): Parameters<WriteArg>) -> Result<String, String> {
        let author = a.author.unwrap_or_else(|| "agent".into());
        let out = self
            .svc
            .write(&a.path, a.content.as_bytes(), &author, &a.comment.unwrap_or_default())
            .await
            .map_err(|e| e.to_string())?;
        json(out)
    }

    #[tool(description = "Delete a file. Its history is kept and writing the path again restores it.")]
    async fn mnemo_delete(&self, Parameters(a): Parameters<PathArg>) -> Result<String, String> {
        self.svc.delete_file(&a.path).await.map_err(|e| e.to_string())?;
        Ok("deleted".into())
    }

    #[tool(description = "Show file metadata (size, mime, current version, author).")]
    async fn mnemo_stat(&self, Parameters(a): Parameters<ReadArg>) -> Result<String, String> {
        json(self.svc.stat(&a.path, a.version).await.map_err(|e| e.to_string())?)
    }

    #[tool(description = "List all versions of a file, newest first.")]
    async fn mnemo_versions(&self, Parameters(a): Parameters<PathArg>) -> Result<String, String> {
        json(self.svc.versions(&a.path).await.map_err(|e| e.to_string())?)
    }

    #[tool(description = "Roll a file back to an earlier version. Creates a new version with the old content.")]
    async fn mnemo_rollback(&self, Parameters(a): Parameters<RollbackArg>) -> Result<String, String> {
        let author = a.author.unwrap_or_else(|| "agent".into());
        json(self.svc.rollback(&a.path, a.version, &author).await.map_err(|e| e.to_string())?)
    }

    #[tool(description = "Search file names and text contents.")]
    async fn mnemo_search(&self, Parameters(a): Parameters<SearchArg>) -> Result<String, String> {
        json(self.svc.search(&a.query, a.limit.unwrap_or(20)).await.map_err(|e| e.to_string())?)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for MnemoMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }
}
