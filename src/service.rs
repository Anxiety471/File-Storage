//! The service layer shared by the REST API and the MCP server.
use std::path::Path;

use chrono::Utc;
use serde::Serialize;
use sqlx::{Row, SqlitePool, sqlite::SqliteConnectOptions};

use crate::blob::BlobStore;
use crate::error::{Error, Result};

const MAX_CONTENT_SCAN: i64 = 1024 * 1024;

#[derive(Clone)]
pub struct Mnemo {
    pool: SqlitePool,
    blobs: BlobStore,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub kind: &'static str,
    pub size: Option<i64>,
    pub version: Option<i64>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct VersionInfo {
    pub path: String,
    pub version: i64,
    pub hash: String,
    pub size: i64,
    pub mime: String,
    pub author: String,
    pub comment: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WriteOutcome {
    #[serde(flatten)]
    pub info: VersionInfo,
    /// False when the content matched the current version, so nothing new was stored.
    pub created: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub path: String,
    pub matched: &'static str,
    pub snippet: String,
}

/// Normalize a user path to `/a/b/c` form. Rejects `..` and control characters.
pub fn normalize(path: &str) -> Result<String> {
    let mut parts = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => return Err(Error::Invalid("'..' is not allowed in paths".into())),
            s if s.chars().any(|c| c.is_control()) => {
                return Err(Error::Invalid("control characters in path".into()));
            }
            s => parts.push(s),
        }
    }
    Ok(format!("/{}", parts.join("/")))
}

fn parent_of(path: &str) -> String {
    match path.rfind('/') {
        Some(0) | None => "/".into(),
        Some(i) => path[..i].to_string(),
    }
}

fn name_of(path: &str) -> String {
    path.rsplit('/').next().unwrap_or("").to_string()
}

fn guess_mime(path: &str, data: &[u8]) -> String {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "md" => "text/markdown",
        "json" => "application/json",
        "html" => "text/html",
        "csv" => "text/csv",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "pdf" => "application/pdf",
        _ if std::str::from_utf8(data).is_ok() => "text/plain",
        _ => "application/octet-stream",
    }
    .into()
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn version_from_row(path: &str, r: &sqlx::sqlite::SqliteRow) -> VersionInfo {
    VersionInfo {
        path: path.to_string(),
        version: r.get("version"),
        hash: r.get("hash"),
        size: r.get("size"),
        mime: r.get("mime"),
        author: r.get("author"),
        comment: r.get("comment"),
        created_at: r.get("created_at"),
    }
}

impl Mnemo {
    /// Open (or create) a store rooted at `data_dir`: `mnemo.db` plus `blobs/`.
    pub async fn open(data_dir: &Path) -> Result<Self> {
        tokio::fs::create_dir_all(data_dir).await?;
        let opts = SqliteConnectOptions::new()
            .filename(data_dir.join("mnemo.db"))
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
        let pool = SqlitePool::connect_with(opts).await?;
        sqlx::raw_sql(include_str!("schema.sql")).execute(&pool).await?;
        Ok(Self { pool, blobs: BlobStore::new(data_dir.join("blobs")) })
    }

    pub async fn mkdir(&self, path: &str) -> Result<String> {
        let path = normalize(path)?;
        if path == "/" {
            return Ok(path);
        }
        if self.file_row(&path).await?.is_some() {
            return Err(Error::Conflict(format!("{path} is a file")));
        }
        self.ensure_parents(&path).await?;
        self.insert_folder(&path).await?;
        Ok(path)
    }

    async fn insert_folder(&self, path: &str) -> Result<()> {
        sqlx::query("INSERT OR IGNORE INTO folders (path, parent, created_at) VALUES (?, ?, ?)")
            .bind(path)
            .bind(parent_of(path))
            .bind(now())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Create every ancestor folder of `path`, refusing to nest under a file.
    async fn ensure_parents(&self, path: &str) -> Result<()> {
        let mut chain = Vec::new();
        let mut p = parent_of(path);
        while p != "/" {
            chain.push(p.clone());
            p = parent_of(&p);
        }
        for dir in chain.into_iter().rev() {
            if self.file_row(&dir).await?.is_some() {
                return Err(Error::Conflict(format!("{dir} is a file")));
            }
            self.insert_folder(&dir).await?;
        }
        Ok(())
    }

    async fn folder_exists(&self, path: &str) -> Result<bool> {
        if path == "/" {
            return Ok(true);
        }
        Ok(sqlx::query("SELECT 1 FROM folders WHERE path = ?")
            .bind(path)
            .fetch_optional(&self.pool)
            .await?
            .is_some())
    }

    /// Returns (id, current_version) for a live (not deleted) file.
    async fn file_row(&self, path: &str) -> Result<Option<(i64, i64)>> {
        Ok(sqlx::query("SELECT id, current_version FROM files WHERE path = ? AND deleted = 0")
            .bind(path)
            .fetch_optional(&self.pool)
            .await?
            .map(|r| (r.get("id"), r.get("current_version"))))
    }

    pub async fn list(&self, path: &str) -> Result<Vec<Entry>> {
        let path = normalize(path)?;
        if !self.folder_exists(&path).await? {
            return Err(Error::NotFound(format!("folder {path}")));
        }
        let mut out = Vec::new();
        for r in sqlx::query("SELECT path, created_at FROM folders WHERE parent = ? ORDER BY path")
            .bind(&path)
            .fetch_all(&self.pool)
            .await?
        {
            let p: String = r.get("path");
            out.push(Entry {
                name: name_of(&p),
                path: p,
                kind: "folder",
                size: None,
                version: None,
                updated_at: r.get("created_at"),
            });
        }
        let files = sqlx::query(
            "SELECT f.path, f.current_version, f.updated_at, v.size FROM files f \
             JOIN versions v ON v.file_id = f.id AND v.version = f.current_version \
             WHERE f.parent = ? AND f.deleted = 0 ORDER BY f.path",
        )
        .bind(&path)
        .fetch_all(&self.pool)
        .await?;
        for r in files {
            let p: String = r.get("path");
            out.push(Entry {
                name: name_of(&p),
                path: p,
                kind: "file",
                size: Some(r.get("size")),
                version: Some(r.get("current_version")),
                updated_at: r.get("updated_at"),
            });
        }
        Ok(out)
    }

    pub async fn write(
        &self,
        path: &str,
        data: &[u8],
        author: &str,
        comment: &str,
    ) -> Result<WriteOutcome> {
        let path = normalize(path)?;
        if path == "/" {
            return Err(Error::Invalid("cannot write to the root".into()));
        }
        if self.folder_exists(&path).await? {
            return Err(Error::Conflict(format!("{path} is a folder")));
        }
        self.ensure_parents(&path).await?;
        let hash = self.blobs.put(data).await?;
        let mime = guess_mime(&path, data);
        let ts = now();

        let mut tx = self.pool.begin().await?;
        let row = sqlx::query("SELECT id, current_version, deleted FROM files WHERE path = ?")
            .bind(&path)
            .fetch_optional(&mut *tx)
            .await?;
        let (file_id, next) = match row {
            Some(r) => {
                let id: i64 = r.get("id");
                let cur: i64 = r.get("current_version");
                let deleted: i64 = r.get("deleted");
                if deleted == 0 {
                    let same: Option<String> = sqlx::query_scalar(
                        "SELECT hash FROM versions WHERE file_id = ? AND version = ?",
                    )
                    .bind(id)
                    .bind(cur)
                    .fetch_optional(&mut *tx)
                    .await?;
                    if same.as_deref() == Some(hash.as_str()) {
                        drop(tx);
                        let info = self.version_info(&path, cur).await?;
                        return Ok(WriteOutcome { info, created: false });
                    }
                }
                (id, cur + 1)
            }
            None => {
                let id = sqlx::query(
                    "INSERT INTO files (path, parent, current_version, deleted, created_at, updated_at) \
                     VALUES (?, ?, 0, 0, ?, ?)",
                )
                .bind(&path)
                .bind(parent_of(&path))
                .bind(&ts)
                .bind(&ts)
                .execute(&mut *tx)
                .await?
                .last_insert_rowid();
                (id, 1)
            }
        };
        sqlx::query(
            "INSERT INTO versions (file_id, version, hash, size, mime, author, comment, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(file_id)
        .bind(next)
        .bind(&hash)
        .bind(data.len() as i64)
        .bind(&mime)
        .bind(author)
        .bind(comment)
        .bind(&ts)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE files SET current_version = ?, deleted = 0, updated_at = ? WHERE id = ?")
            .bind(next)
            .bind(&ts)
            .bind(file_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        let info = self.version_info(&path, next).await?;
        Ok(WriteOutcome { info, created: true })
    }

    async fn version_info(&self, path: &str, version: i64) -> Result<VersionInfo> {
        let r = sqlx::query(
            "SELECT v.* FROM versions v JOIN files f ON f.id = v.file_id \
             WHERE f.path = ? AND v.version = ?",
        )
        .bind(path)
        .bind(version)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| Error::NotFound(format!("{path} version {version}")))?;
        Ok(version_from_row(path, &r))
    }

    /// Metadata for the current (or a specific) version of a live file.
    pub async fn stat(&self, path: &str, version: Option<i64>) -> Result<VersionInfo> {
        let path = normalize(path)?;
        let (_, cur) = self
            .file_row(&path)
            .await?
            .ok_or_else(|| Error::NotFound(format!("file {path}")))?;
        self.version_info(&path, version.unwrap_or(cur)).await
    }

    pub async fn read(&self, path: &str, version: Option<i64>) -> Result<(VersionInfo, Vec<u8>)> {
        let info = self.stat(path, version).await?;
        let data = self.blobs.get(&info.hash).await?;
        Ok((info, data))
    }

    pub async fn delete_file(&self, path: &str) -> Result<()> {
        let path = normalize(path)?;
        let n = sqlx::query("UPDATE files SET deleted = 1, updated_at = ? WHERE path = ? AND deleted = 0")
            .bind(now())
            .bind(&path)
            .execute(&self.pool)
            .await?
            .rows_affected();
        if n == 0 {
            return Err(Error::NotFound(format!("file {path}")));
        }
        Ok(())
    }

    /// Remove an empty folder.
    pub async fn rmdir(&self, path: &str) -> Result<()> {
        let path = normalize(path)?;
        if path == "/" {
            return Err(Error::Invalid("cannot remove the root".into()));
        }
        if !self.folder_exists(&path).await? {
            return Err(Error::NotFound(format!("folder {path}")));
        }
        if !self.list(&path).await?.is_empty() {
            return Err(Error::Conflict(format!("folder {path} is not empty")));
        }
        sqlx::query("DELETE FROM folders WHERE path = ?").bind(&path).execute(&self.pool).await?;
        Ok(())
    }

    /// All versions of a live file, newest first.
    pub async fn versions(&self, path: &str) -> Result<Vec<VersionInfo>> {
        let path = normalize(path)?;
        let (id, _) = self
            .file_row(&path)
            .await?
            .ok_or_else(|| Error::NotFound(format!("file {path}")))?;
        let rows = sqlx::query("SELECT * FROM versions WHERE file_id = ? ORDER BY version DESC")
            .bind(id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(|r| version_from_row(&path, r)).collect())
    }

    /// Roll back by writing an old version's content as a new version. History is never rewritten.
    pub async fn rollback(&self, path: &str, version: i64, author: &str) -> Result<WriteOutcome> {
        let (info, data) = self.read(path, Some(version)).await?;
        let comment = format!("rollback to v{version}");
        let mut out = self.write(&info.path, &data, author, &comment).await?;
        if !out.created {
            out.info = self.stat(&info.path, None).await?;
        }
        Ok(out)
    }

    /// Case-insensitive search over file paths and the text of current versions.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Err(Error::Invalid("empty query".into()));
        }
        let rows = sqlx::query(
            "SELECT f.path, v.hash, v.size, v.mime FROM files f \
             JOIN versions v ON v.file_id = f.id AND v.version = f.current_version \
             WHERE f.deleted = 0 ORDER BY f.path",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut hits = Vec::new();
        for r in rows {
            if hits.len() >= limit {
                break;
            }
            let path: String = r.get("path");
            if path.to_lowercase().contains(&q) {
                hits.push(SearchHit { path, matched: "name", snippet: String::new() });
                continue;
            }
            let size: i64 = r.get("size");
            let mime: String = r.get("mime");
            if size > MAX_CONTENT_SCAN || mime.starts_with("image/") || mime == "application/pdf" {
                continue;
            }
            let data = self.blobs.get(&r.get::<String, _>("hash")).await?;
            let Ok(text) = std::str::from_utf8(&data) else { continue };
            if let Some(line) = text.lines().find(|l| l.to_lowercase().contains(&q)) {
                hits.push(SearchHit {
                    path,
                    matched: "content",
                    snippet: line.chars().take(200).collect(),
                });
            }
        }
        Ok(hits)
    }
}
