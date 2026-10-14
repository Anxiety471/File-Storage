//! The service layer shared by the REST API and the MCP server.
//!
//! The model follows Google Cloud Storage: a file has a sequence of immutable generations,
//! each carrying its content, custom metadata and audit fields. Writes can carry an
//! `if_generation_match` precondition so a stale writer fails instead of clobbering.
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use chrono::Utc;
use serde::Serialize;
use sqlx::{Row, SqlitePool, sqlite::SqliteConnectOptions, sqlite::SqlitePoolOptions, sqlite::SqliteRow};
use tokio::sync::Mutex;

use crate::blob::BlobStore;
use crate::error::{Error, Result};

const MAX_CONTENT_SCAN: i64 = 1024 * 1024;
const MAX_METADATA_ENTRIES: usize = 32;
const MAX_METADATA_KEY: usize = 64;
const MAX_METADATA_VALUE: usize = 1024;

pub type Metadata = BTreeMap<String, String>;

#[derive(Clone)]
pub struct Drawer {
    pool: SqlitePool,
    blobs: BlobStore,
    // Makes each precondition check and the write it guards one atomic step. Fine for the
    // single-process server this crate is; several processes sharing a store would need
    // `BEGIN IMMEDIATE` instead.
    write_lock: Arc<Mutex<()>>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub kind: &'static str,
    pub size: Option<i64>,
    pub generation: Option<i64>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Generation {
    pub path: String,
    pub generation: i64,
    pub hash: String,
    pub size: i64,
    pub mime: String,
    pub author: String,
    pub comment: String,
    pub metadata: Metadata,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WriteOutcome {
    #[serde(flatten)]
    pub info: Generation,
    /// False when nothing changed (same content and metadata), so no generation was added.
    pub new_generation: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub path: String,
    pub matched: &'static str,
    pub snippet: String,
}

/// Who is changing a file, why, with what metadata, and under which precondition.
#[derive(Debug, Clone, Default)]
pub struct WriteOpts {
    pub author: String,
    pub comment: String,
    pub metadata: Metadata,
    /// `Some(0)`: only create when the file does not exist.
    /// `Some(n)`: only change the file while its live generation is `n`.
    pub if_generation_match: Option<i64>,
}

impl WriteOpts {
    pub fn new(author: impl Into<String>, comment: impl Into<String>) -> Self {
        Self { author: author.into(), comment: comment.into(), ..Self::default() }
    }
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

/// Keys are header-safe tokens; values are bounded text.
pub fn validate_metadata(m: &Metadata) -> Result<()> {
    if m.len() > MAX_METADATA_ENTRIES {
        return Err(Error::Invalid(format!("at most {MAX_METADATA_ENTRIES} metadata entries")));
    }
    for (k, v) in m {
        let key_ok = !k.is_empty()
            && k.len() <= MAX_METADATA_KEY
            && k.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
        if !key_ok {
            return Err(Error::Invalid(format!("bad metadata key {k:?}: use 1-64 of [A-Za-z0-9-_.]")));
        }
        if v.len() > MAX_METADATA_VALUE || v.chars().any(|c| c.is_control()) {
            return Err(Error::Invalid(format!("metadata {k:?} must be at most {MAX_METADATA_VALUE} bytes, no control characters")));
        }
    }
    Ok(())
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

fn generation_from_row(path: &str, r: &SqliteRow) -> Generation {
    let metadata: String = r.get("metadata");
    Generation {
        path: path.to_string(),
        generation: r.get("generation"),
        hash: r.get("hash"),
        size: r.get("size"),
        mime: r.get("mime"),
        author: r.get("author"),
        comment: r.get("comment"),
        metadata: serde_json::from_str(&metadata).unwrap_or_default(),
        created_at: r.get("created_at"),
    }
}

/// `if_generation_match` against the live generation (`None` when the file does not exist).
fn check_precondition(path: &str, live: Option<i64>, expected: Option<i64>) -> Result<()> {
    let Some(expected) = expected else { return Ok(()) };
    let ok = match live {
        None => expected == 0,
        Some(cur) => expected == cur,
    };
    if ok {
        return Ok(());
    }
    Err(Error::PreconditionFailed(match live {
        Some(cur) => format!("{path} is at generation {cur}, not {expected}"),
        None => format!("{path} does not exist, expected generation {expected}"),
    }))
}

async fn columns(pool: &SqlitePool, table: &str) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar("SELECT name FROM pragma_table_info(?)").bind(table).fetch_all(pool).await?)
}

/// Bring a store created before generations and metadata existed up to the current schema.
/// Runs after `schema.sql`, and does nothing on a store that is already current.
async fn migrate(pool: &SqlitePool) -> Result<()> {
    let files = columns(pool, "files").await?;
    if files.iter().any(|c| c == "current_version") {
        sqlx::raw_sql("ALTER TABLE files RENAME COLUMN current_version TO current_generation")
            .execute(pool)
            .await?;
    }
    let versions = columns(pool, "versions").await?;
    if versions.iter().any(|c| c == "version") {
        sqlx::raw_sql("ALTER TABLE versions RENAME COLUMN version TO generation").execute(pool).await?;
    }
    if !versions.iter().any(|c| c == "metadata") {
        sqlx::raw_sql("ALTER TABLE versions ADD COLUMN metadata TEXT NOT NULL DEFAULT '{}'")
            .execute(pool)
            .await?;
    }
    Ok(())
}

impl Drawer {
    /// Open (or create) a store rooted at `data_dir`: `drawer.db` plus `blobs/`.
    pub async fn open(data_dir: &Path) -> Result<Self> {
        tokio::fs::create_dir_all(data_dir).await?;
        let opts = SqliteConnectOptions::new()
            .filename(data_dir.join("drawer.db"))
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
        // Schema and migrations run on a single connection that is closed before the shared
        // pool opens, so no pooled connection ever holds a plan from the old schema.
        let setup = SqlitePoolOptions::new().max_connections(1).connect_with(opts.clone()).await?;
        sqlx::raw_sql(include_str!("schema.sql")).execute(&setup).await?;
        migrate(&setup).await?;
        setup.close().await;
        let pool = SqlitePool::connect_with(opts).await?;
        Ok(Self {
            pool,
            blobs: BlobStore::new(data_dir.join("blobs")),
            write_lock: Arc::new(Mutex::new(())),
        })
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

    /// Returns (id, current generation) for a live (not deleted) file.
    async fn file_row(&self, path: &str) -> Result<Option<(i64, i64)>> {
        Ok(sqlx::query("SELECT id, current_generation FROM files WHERE path = ? AND deleted = 0")
            .bind(path)
            .fetch_optional(&self.pool)
            .await?
            .map(|r| (r.get("id"), r.get("current_generation"))))
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
                generation: None,
                updated_at: r.get("created_at"),
            });
        }
        let files = sqlx::query(
            "SELECT f.path, f.current_generation, f.updated_at, v.size FROM files f \
             JOIN versions v ON v.file_id = f.id AND v.generation = f.current_generation \
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
                generation: Some(r.get("current_generation")),
                updated_at: r.get("updated_at"),
            });
        }
        Ok(out)
    }

    /// Store `data` as the next generation of `path`, unless `opts` is refused or nothing changed.
    pub async fn write(&self, path: &str, data: &[u8], opts: WriteOpts) -> Result<WriteOutcome> {
        let path = normalize(path)?;
        if path == "/" {
            return Err(Error::Invalid("cannot write to the root".into()));
        }
        validate_metadata(&opts.metadata)?;
        let hash = self.blobs.put(data).await?;
        let mime = guess_mime(&path, data);
        self.commit(&path, &hash, data.len() as i64, &mime, &opts.metadata, &opts).await
    }

    /// Core write path, shared by `write`, `copy` and `rollback`. The blob must already exist.
    async fn commit(
        &self,
        path: &str,
        hash: &str,
        size: i64,
        mime: &str,
        metadata: &Metadata,
        opts: &WriteOpts,
    ) -> Result<WriteOutcome> {
        let _guard = self.write_lock.lock().await;
        if self.folder_exists(path).await? {
            return Err(Error::Conflict(format!("{path} is a folder")));
        }
        let live = self.file_row(path).await?;
        check_precondition(path, live.map(|(_, g)| g), opts.if_generation_match)?;
        let meta_json = serde_json::to_string(metadata).expect("string map serializes");

        if let Some((id, cur)) = live {
            let same = sqlx::query("SELECT hash, metadata FROM versions WHERE file_id = ? AND generation = ?")
                .bind(id)
                .bind(cur)
                .fetch_one(&self.pool)
                .await?;
            if same.get::<String, _>("hash") == hash && same.get::<String, _>("metadata") == meta_json {
                let info = self.generation_info(path, cur).await?;
                return Ok(WriteOutcome { info, new_generation: false });
            }
        }

        self.ensure_parents(path).await?;
        let ts = now();
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query("SELECT id, current_generation FROM files WHERE path = ?")
            .bind(path)
            .fetch_optional(&mut *tx)
            .await?;
        let (file_id, next) = match row {
            Some(r) => (r.get::<i64, _>("id"), r.get::<i64, _>("current_generation") + 1),
            None => {
                let id = sqlx::query(
                    "INSERT INTO files (path, parent, current_generation, deleted, created_at, updated_at) \
                     VALUES (?, ?, 0, 0, ?, ?)",
                )
                .bind(path)
                .bind(parent_of(path))
                .bind(&ts)
                .bind(&ts)
                .execute(&mut *tx)
                .await?
                .last_insert_rowid();
                (id, 1)
            }
        };
        sqlx::query(
            "INSERT INTO versions (file_id, generation, hash, size, mime, author, comment, metadata, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(file_id)
        .bind(next)
        .bind(hash)
        .bind(size)
        .bind(mime)
        .bind(&opts.author)
        .bind(&opts.comment)
        .bind(&meta_json)
        .bind(&ts)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE files SET current_generation = ?, deleted = 0, updated_at = ? WHERE id = ?")
            .bind(next)
            .bind(&ts)
            .bind(file_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        let info = self.generation_info(path, next).await?;
        Ok(WriteOutcome { info, new_generation: true })
    }

    async fn generation_info(&self, path: &str, generation: i64) -> Result<Generation> {
        let r = sqlx::query(
            "SELECT v.* FROM versions v JOIN files f ON f.id = v.file_id \
             WHERE f.path = ? AND v.generation = ?",
        )
        .bind(path)
        .bind(generation)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| Error::NotFound(format!("{path} generation {generation}")))?;
        Ok(generation_from_row(path, &r))
    }

    /// Metadata for the live (or a specific) generation of a live file.
    pub async fn stat(&self, path: &str, generation: Option<i64>) -> Result<Generation> {
        let path = normalize(path)?;
        let (_, cur) = self
            .file_row(&path)
            .await?
            .ok_or_else(|| Error::NotFound(format!("file {path}")))?;
        self.generation_info(&path, generation.unwrap_or(cur)).await
    }

    pub async fn read(&self, path: &str, generation: Option<i64>) -> Result<(Generation, Vec<u8>)> {
        let info = self.stat(path, generation).await?;
        let data = self.blobs.get(&info.hash).await?;
        Ok((info, data))
    }

    /// Copy one generation of a file to another path, or to the same path as a new generation.
    /// Content is shared by hash, so nothing is re-uploaded. The source metadata carries over.
    pub async fn copy(
        &self,
        from: &str,
        to: &str,
        source_generation: Option<i64>,
        opts: WriteOpts,
    ) -> Result<WriteOutcome> {
        let to = normalize(to)?;
        if to == "/" {
            return Err(Error::Invalid("cannot write to the root".into()));
        }
        let src = self.stat(from, source_generation).await?;
        self.commit(&to, &src.hash, src.size, &src.mime, &src.metadata, &opts).await
    }

    /// Soft-delete a live file. History is kept; writing the path again starts a new generation.
    pub async fn delete_file(&self, path: &str, if_generation_match: Option<i64>) -> Result<()> {
        let path = normalize(path)?;
        let _guard = self.write_lock.lock().await;
        let (_, cur) = self
            .file_row(&path)
            .await?
            .ok_or_else(|| Error::NotFound(format!("file {path}")))?;
        check_precondition(&path, Some(cur), if_generation_match)?;
        sqlx::query("UPDATE files SET deleted = 1, updated_at = ? WHERE path = ? AND deleted = 0")
            .bind(now())
            .bind(&path)
            .execute(&self.pool)
            .await?;
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

    /// All generations of a live file, newest first.
    pub async fn generations(&self, path: &str) -> Result<Vec<Generation>> {
        let path = normalize(path)?;
        let (id, _) = self
            .file_row(&path)
            .await?
            .ok_or_else(|| Error::NotFound(format!("file {path}")))?;
        let rows = sqlx::query("SELECT * FROM versions WHERE file_id = ? ORDER BY generation DESC")
            .bind(id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(|r| generation_from_row(&path, r)).collect())
    }

    /// Roll back by making an old generation the newest one. History is never rewritten.
    pub async fn rollback(&self, path: &str, generation: i64, mut opts: WriteOpts) -> Result<WriteOutcome> {
        if opts.comment.is_empty() {
            opts.comment = format!("rollback to generation {generation}");
        }
        self.copy(path, path, Some(generation), opts).await
    }

    /// Case-insensitive search over file paths and the text of live generations.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Err(Error::Invalid("empty query".into()));
        }
        let rows = sqlx::query(
            "SELECT f.path, v.hash, v.size, v.mime FROM files f \
             JOIN versions v ON v.file_id = f.id AND v.generation = f.current_generation \
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
