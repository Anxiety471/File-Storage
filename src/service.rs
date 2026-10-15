//! The service layer shared by the REST API and the MCP server.
//!
//! The model follows Google Drive. Every file and folder is a node with a stable id and a
//! parent. Names and paths are views over the tree, so rename, move and trash never rewrite
//! other rows. A file's content is a list of immutable revisions. Role grants sit on nodes
//! and apply to everything below them.
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use chrono::Utc;
use serde::Serialize;
use sqlx::{Row, Sqlite, SqlitePool, Transaction, sqlite::SqliteConnectOptions, sqlite::SqlitePoolOptions, sqlite::SqliteRow};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::blob::BlobStore;
use crate::error::{Error, Result};

const MAX_CONTENT_SCAN: i64 = 1024 * 1024;
const MAX_PROPERTY_ENTRIES: usize = 32;
const MAX_PROPERTY_KEY: usize = 64;
const MAX_PROPERTY_VALUE: usize = 1024;
pub const ROOT_ID: &str = "root";

const READER: u8 = 1;
const WRITER: u8 = 2;
const OWNER: u8 = 3;

pub type Properties = std::collections::BTreeMap<String, String>;

#[derive(Clone)]
pub struct Drawer {
    pool: SqlitePool,
    blobs: BlobStore,
    // Makes each check-then-write sequence one atomic step. Fine for the single-process server
    // this crate is; several processes sharing a store would need `BEGIN IMMEDIATE`.
    write_lock: Arc<Mutex<()>>,
}

/// How a caller names a node: by path (`/notes/todo.md`) or by its stable id.
#[derive(Debug, Clone, Copy)]
pub enum Ref<'a> {
    Path(&'a str),
    Id(&'a str),
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Node {
    pub id: String,
    pub parent_id: Option<String>,
    pub name: String,
    /// Derived from the parent chain at read time, so it changes when the node moves.
    pub path: String,
    pub kind: &'static str,
    pub owner: String,
    pub head_revision: Option<i64>,
    pub hash: Option<String>,
    pub size: Option<i64>,
    pub mime: Option<String>,
    pub properties: Properties,
    pub trashed: bool,
    pub starred: bool,
    pub created_at: String,
    pub modified_at: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Revision {
    pub node_id: String,
    pub revision: i64,
    pub hash: String,
    pub size: i64,
    pub mime: String,
    pub author: String,
    pub comment: String,
    pub properties: Properties,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WriteOutcome {
    pub node: Node,
    pub revision: i64,
    /// False when nothing changed (same content and properties), so no revision was added.
    pub new_revision: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Permission {
    pub principal: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub id: String,
    pub path: String,
    pub matched: &'static str,
    pub snippet: String,
}

/// Who is changing a file, why, with what properties, and under which precondition.
#[derive(Debug, Clone, Default)]
pub struct WriteOpts {
    pub author: String,
    pub comment: String,
    pub properties: Properties,
    /// `Some(0)`: only create when there is no live file here.
    /// `Some(n)`: only change the file while its head revision is `n`.
    pub if_head_revision: Option<i64>,
}

impl WriteOpts {
    pub fn new(author: impl Into<String>, comment: impl Into<String>) -> Self {
        Self { author: author.into(), comment: comment.into(), ..Self::default() }
    }
}

/// Counts for `drawer doctor` and the like.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Stats {
    pub files: i64,
    pub folders: i64,
    /// Top-most trashed nodes, the same things the trash lists.
    pub trashed: i64,
    pub revisions: i64,
}

/// Fields that `Drawer::update` changes. `None` leaves a field as it is.
#[derive(Debug, Clone, Default)]
pub struct NodeUpdate {
    pub name: Option<String>,
    pub parent_id: Option<String>,
    pub starred: Option<bool>,
    pub trashed: Option<bool>,
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

fn segments(norm: &str) -> Vec<&str> {
    norm.split('/').filter(|s| !s.is_empty()).collect()
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.chars().any(|c| c.is_control()) {
        return Err(Error::Invalid(format!("bad name {name:?}")));
    }
    Ok(())
}

/// Keys are header-safe tokens; values are bounded text.
pub fn validate_properties(m: &Properties) -> Result<()> {
    if m.len() > MAX_PROPERTY_ENTRIES {
        return Err(Error::Invalid(format!("at most {MAX_PROPERTY_ENTRIES} properties")));
    }
    for (k, v) in m {
        let key_ok = !k.is_empty()
            && k.len() <= MAX_PROPERTY_KEY
            && k.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
        if !key_ok {
            return Err(Error::Invalid(format!("bad property key {k:?}: use 1-64 of [A-Za-z0-9-_.]")));
        }
        if v.len() > MAX_PROPERTY_VALUE || v.chars().any(|c| c.is_control()) {
            return Err(Error::Invalid(format!(
                "property {k:?} must be at most {MAX_PROPERTY_VALUE} bytes, no control characters"
            )));
        }
    }
    Ok(())
}

fn child_path(parent: &str, name: &str) -> String {
    if parent == "/" { format!("/{name}") } else { format!("{parent}/{name}") }
}

fn parent_of(path: &str) -> String {
    match path.rfind('/') {
        Some(0) | None => "/".into(),
        Some(i) => path[..i].to_string(),
    }
}

fn name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or("")
}

/// Text-like media types whose content search can scan.
fn is_text(mime: &str) -> bool {
    mime.starts_with("text/") || matches!(mime, "application/json" | "application/xml" | "application/yaml" | "application/toml")
}

/// Media type from the file extension, falling back to a UTF-8 check for unknown extensions.
fn guess_mime(path: &str, data: &[u8]) -> String {
    let name = name_of(path).to_ascii_lowercase();
    let ext = match name.rsplit_once('.') {
        Some((_, ext)) => ext,
        None => name.as_str(), // Dockerfile, Makefile, ...
    };
    let mime = match ext {
        // documents
        "pdf" => "application/pdf",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "odp" => "application/vnd.oasis.opendocument.presentation",
        "rtf" => "application/rtf",
        "epub" => "application/epub+zip",
        // text and data
        "md" | "markdown" => "text/markdown",
        "txt" | "log" => "text/plain",
        "csv" => "text/csv",
        "tsv" => "text/tab-separated-values",
        "json" => "application/json",
        "xml" => "application/xml",
        "yaml" | "yml" => "application/yaml",
        "toml" => "application/toml",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        // images, audio, video
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "m4a" => "audio/mp4",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        // archives
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "tar" => "application/x-tar",
        "7z" => "application/x-7z-compressed",
        _ if std::str::from_utf8(data).is_ok() => "text/plain",
        _ => "application/octet-stream",
    };
    // Source files (.rs, .py, .ts, ...) fall through to text/plain; the UI picks a highlighter from the extension.
    mime.into()
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn rank(role: &str) -> u8 {
    match role {
        "reader" => READER,
        "writer" => WRITER,
        "owner" => OWNER,
        _ => 0,
    }
}

const NODE_SELECT: &str = "SELECT n.id, n.parent_id, n.name, n.kind, n.owner, n.head_revision, n.trashed, \
    n.starred, n.created_at, n.modified_at, r.hash, r.size, r.mime, r.properties \
    FROM nodes n LEFT JOIN revisions r ON r.node_id = n.id AND r.revision = n.head_revision";

const REVISION_SELECT: &str =
    "SELECT node_id, revision, hash, size, mime, author, comment, properties, created_at FROM revisions";

fn node_from_row(r: &SqliteRow, path: String) -> Node {
    let props: Option<String> = r.get("properties");
    Node {
        id: r.get("id"),
        parent_id: r.get("parent_id"),
        name: r.get("name"),
        path,
        kind: if r.get::<String, _>("kind") == "folder" { "folder" } else { "file" },
        owner: r.get("owner"),
        head_revision: r.get("head_revision"),
        hash: r.get("hash"),
        size: r.get("size"),
        mime: r.get("mime"),
        properties: props.and_then(|p| serde_json::from_str(&p).ok()).unwrap_or_default(),
        trashed: r.get::<i64, _>("trashed") != 0,
        starred: r.get::<i64, _>("starred") != 0,
        created_at: r.get("created_at"),
        modified_at: r.get("modified_at"),
    }
}

fn revision_from_row(r: &SqliteRow) -> Revision {
    let props: String = r.get("properties");
    Revision {
        node_id: r.get("node_id"),
        revision: r.get("revision"),
        hash: r.get("hash"),
        size: r.get("size"),
        mime: r.get("mime"),
        author: r.get("author"),
        comment: r.get("comment"),
        properties: serde_json::from_str(&props).unwrap_or_default(),
        created_at: r.get("created_at"),
    }
}

/// `if_head_revision` against the live head (`None` when there is no live file).
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
        Some(cur) => format!("{path} is at revision {cur}, not {expected}"),
        None => format!("{path} does not exist, expected revision {expected}"),
    }))
}

/// Where a write lands: an existing live node, a trashed file it revives, or a new path.
enum Slot {
    Live(String),
    Revive(String),
    Create { anchor: String, missing: Vec<String> },
}

async fn columns(pool: &SqlitePool, table: &str) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar("SELECT name FROM pragma_table_info(?)").bind(table).fetch_all(pool).await?)
}

async fn insert_node(
    tx: &mut Transaction<'_, Sqlite>,
    parent: &str,
    name: &str,
    kind: &str,
    owner: &str,
    ts: &str,
) -> Result<String> {
    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO nodes (id, parent_id, name, kind, owner, trashed, starred, created_at, modified_at) \
         VALUES (?, ?, ?, ?, ?, 0, 0, ?, ?)",
    )
    .bind(&id)
    .bind(parent)
    .bind(name)
    .bind(kind)
    .bind(owner)
    .bind(ts)
    .bind(ts)
    .execute(&mut **tx)
    .await?;
    Ok(id)
}

/// Bring a store on the pre-revision schema (`files`, `folders`, `versions` keyed by path) into
/// the node model. Runs once: the legacy tables are dropped in the same transaction.
async fn migrate_legacy(pool: &SqlitePool) -> Result<()> {
    if columns(pool, "files").await?.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    let mut ids: HashMap<String, String> = HashMap::from([("/".to_string(), ROOT_ID.to_string())]);

    let folders = sqlx::query("SELECT path, created_at FROM folders ORDER BY length(path)")
        .fetch_all(&mut *tx)
        .await?;
    for r in folders {
        let path: String = r.get("path");
        let created: String = r.get("created_at");
        let parent = ids
            .get(&parent_of(&path))
            .cloned()
            .ok_or_else(|| Error::Internal(format!("legacy folder {path} has no parent")))?;
        let id = insert_node(&mut tx, &parent, name_of(&path), "folder", "migrated", &created).await?;
        ids.insert(path, id);
    }

    let mut file_nodes: HashMap<i64, String> = HashMap::new();
    let files = sqlx::query("SELECT id, path, current_version, deleted, created_at, updated_at FROM files ORDER BY id")
        .fetch_all(&mut *tx)
        .await?;
    for r in files {
        let path: String = r.get("path");
        let parent = ids
            .get(&parent_of(&path))
            .cloned()
            .ok_or_else(|| Error::Internal(format!("legacy file {path} has no parent folder")))?;
        let created: String = r.get("created_at");
        let id = insert_node(&mut tx, &parent, name_of(&path), "file", "migrated", &created).await?;
        sqlx::query("UPDATE nodes SET head_revision = ?, trashed = ?, modified_at = ? WHERE id = ?")
            .bind(r.get::<i64, _>("current_version"))
            .bind(r.get::<i64, _>("deleted"))
            .bind(r.get::<String, _>("updated_at"))
            .bind(&id)
            .execute(&mut *tx)
            .await?;
        file_nodes.insert(r.get("id"), id);
    }

    let versions = sqlx::query("SELECT file_id, version, hash, size, mime, author, comment, created_at FROM versions ORDER BY id")
        .fetch_all(&mut *tx)
        .await?;
    for r in versions {
        let node_id = file_nodes
            .get(&r.get::<i64, _>("file_id"))
            .ok_or_else(|| Error::Internal("legacy version without a file".into()))?;
        sqlx::query(
            "INSERT INTO revisions (node_id, revision, hash, size, mime, author, comment, properties, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, '{}', ?)",
        )
        .bind(node_id)
        .bind(r.get::<i64, _>("version"))
        .bind(r.get::<String, _>("hash"))
        .bind(r.get::<i64, _>("size"))
        .bind(r.get::<String, _>("mime"))
        .bind(r.get::<String, _>("author"))
        .bind(r.get::<String, _>("comment"))
        .bind(r.get::<String, _>("created_at"))
        .execute(&mut *tx)
        .await?;
    }

    for table in ["versions", "files", "folders"] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP TABLE {table}"))).execute(&mut *tx).await?;
    }
    tx.commit().await?;
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
        migrate_legacy(&setup).await?;
        setup.close().await;
        let pool = SqlitePool::connect_with(opts).await?;
        Ok(Self {
            pool,
            blobs: BlobStore::new(data_dir.join("blobs")),
            write_lock: Arc::new(Mutex::new(())),
        })
    }

    // ----- lookups -------------------------------------------------------------------------

    /// Id and kind of a child by name. `trashed` picks which side of the trash to look at.
    async fn child(&self, parent: &str, name: &str, trashed: bool) -> Result<Option<(String, String)>> {
        Ok(sqlx::query("SELECT id, kind FROM nodes WHERE parent_id = ? AND name = ? AND trashed = ?")
            .bind(parent)
            .bind(name)
            .bind(trashed as i64)
            .fetch_optional(&self.pool)
            .await?
            .map(|r| (r.get("id"), r.get("kind"))))
    }

    async fn path_of(&self, id: &str) -> Result<String> {
        let mut names = Vec::new();
        let mut cur = id.to_string();
        loop {
            let r = sqlx::query("SELECT parent_id, name FROM nodes WHERE id = ?")
                .bind(&cur)
                .fetch_optional(&self.pool)
                .await?
                .ok_or_else(|| Error::NotFound(format!("node {cur}")))?;
            match r.get::<Option<String>, _>("parent_id") {
                None => break,
                Some(p) => {
                    names.push(r.get::<String, _>("name"));
                    cur = p;
                }
            }
        }
        names.reverse();
        Ok(format!("/{}", names.join("/")))
    }

    /// A node by id, trashed or not.
    async fn node(&self, id: &str, path: Option<String>) -> Result<Node> {
        let r = sqlx::query(sqlx::AssertSqlSafe(format!("{NODE_SELECT} WHERE n.id = ?")))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| Error::NotFound(format!("node {id}")))?;
        let path = match path {
            Some(p) => p,
            None => self.path_of(id).await?,
        };
        Ok(node_from_row(&r, path))
    }

    /// A live node by path, or `None`.
    async fn lookup(&self, p: &str) -> Result<Option<Node>> {
        let norm = normalize(p)?;
        let mut id = ROOT_ID.to_string();
        for seg in segments(&norm) {
            match self.child(&id, seg, false).await? {
                Some((cid, _)) => id = cid,
                None => return Ok(None),
            }
        }
        Ok(Some(self.node(&id, Some(norm)).await?))
    }

    async fn resolve(&self, r: Ref<'_>) -> Result<Node> {
        match r {
            Ref::Id(id) => self.node(id, None).await,
            Ref::Path(p) => self
                .lookup(p)
                .await?
                .ok_or_else(|| Error::NotFound(format!("no live file or folder at {p}"))),
        }
    }

    /// Decide where a write to `norm` lands, walking live folders from the root.
    async fn slot(&self, norm: &str) -> Result<Slot> {
        let segs = segments(norm);
        let mut anchor = ROOT_ID.to_string();
        for (i, seg) in segs.iter().enumerate() {
            let last = i + 1 == segs.len();
            match self.child(&anchor, seg, false).await? {
                Some((cid, kind)) => {
                    if last {
                        return Ok(Slot::Live(cid));
                    }
                    if kind == "file" {
                        return Err(Error::Conflict(format!("/{} is a file", segs[..=i].join("/"))));
                    }
                    anchor = cid;
                }
                None => {
                    let trashed = if last { self.child(&anchor, seg, true).await? } else { None };
                    if let Some((tid, kind)) = trashed
                        && kind == "file"
                    {
                        return Ok(Slot::Revive(tid));
                    }
                    return Ok(Slot::Create { anchor, missing: segs[i..].iter().map(|s| s.to_string()).collect() });
                }
            }
        }
        Ok(Slot::Live(ROOT_ID.to_string()))
    }

    /// True when `ancestor` is `id` itself or one of its ancestors.
    async fn is_ancestor_or_self(&self, ancestor: &str, id: &str) -> Result<bool> {
        Ok(sqlx::query(
            "WITH RECURSIVE anc(id) AS (SELECT ? UNION ALL SELECT n.parent_id FROM nodes n JOIN anc ON n.id = anc.id \
             WHERE n.parent_id IS NOT NULL) SELECT 1 FROM anc WHERE id = ?",
        )
        .bind(id)
        .bind(ancestor)
        .fetch_optional(&self.pool)
        .await?
        .is_some())
    }

    /// Role check: grants on the node or any ancestor apply. A node with no grants anywhere is
    /// open, and its owner always passes. This is cooperative: callers name themselves.
    async fn authorize(&self, id: &str, actor: &str, need: u8) -> Result<()> {
        let rows = sqlx::query(
            "WITH RECURSIVE anc(id) AS (SELECT ? UNION ALL SELECT n.parent_id FROM nodes n JOIN anc ON n.id = anc.id \
             WHERE n.parent_id IS NOT NULL) \
             SELECT p.principal, p.role FROM permissions p JOIN anc ON p.node_id = anc.id",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        if rows.is_empty() {
            return Ok(());
        }
        let owner: String = sqlx::query_scalar("SELECT owner FROM nodes WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await?;
        if owner == actor {
            return Ok(());
        }
        let best = rows
            .iter()
            .filter(|r| {
                let p: String = r.get("principal");
                p == actor || p == "anyone"
            })
            .map(|r| rank(&r.get::<String, _>("role")))
            .max()
            .unwrap_or(0);
        if best >= need {
            return Ok(());
        }
        let needed = if need == OWNER { "owner" } else { "writer" };
        Err(Error::Forbidden(format!("{actor} needs {needed} access to this node")))
    }

    /// Result of SQLite's `PRAGMA quick_check`: `ok` for a healthy database.
    pub async fn integrity_check(&self) -> Result<String> {
        Ok(sqlx::query_scalar("PRAGMA quick_check").fetch_one(&self.pool).await?)
    }

    pub async fn stats(&self) -> Result<Stats> {
        let row = sqlx::query(
            "SELECT \
                (SELECT COUNT(*) FROM nodes WHERE kind = 'file' AND trashed = 0) AS files, \
                (SELECT COUNT(*) FROM nodes WHERE kind = 'folder' AND trashed = 0 AND id <> 'root') AS folders, \
                (SELECT COUNT(*) FROM nodes WHERE trashed = 1 AND parent_id IN (SELECT id FROM nodes WHERE trashed = 0)) AS trashed, \
                (SELECT COUNT(*) FROM revisions) AS revisions",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(Stats {
            files: row.get("files"),
            folders: row.get("folders"),
            trashed: row.get("trashed"),
            revisions: row.get("revisions"),
        })
    }

    /// Blob hashes that a revision points at but that are missing from disk.
    pub async fn missing_blobs(&self) -> Result<Vec<String>> {
        let rows = sqlx::query("SELECT DISTINCT hash FROM revisions ORDER BY hash").fetch_all(&self.pool).await?;
        let mut missing = Vec::new();
        for r in rows {
            let hash: String = r.get("hash");
            if !self.blobs.contains(&hash).await? {
                missing.push(hash);
            }
        }
        Ok(missing)
    }

    // ----- reads ---------------------------------------------------------------------------

    /// Live children of a folder, by name.
    pub async fn list(&self, path: &str) -> Result<Vec<Node>> {
        let folder = self
            .lookup(path)
            .await?
            .filter(|n| n.kind == "folder")
            .ok_or_else(|| Error::NotFound(format!("folder {path}")))?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!("{NODE_SELECT} WHERE n.parent_id = ? AND n.trashed = 0 ORDER BY n.name")))
            .bind(&folder.id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .map(|r| {
                let name: String = r.get("name");
                node_from_row(r, child_path(&folder.path, &name))
            })
            .collect())
    }

    pub async fn stat(&self, r: Ref<'_>) -> Result<Node> {
        self.resolve(r).await
    }

    /// One revision of a file (the head by default) and its content.
    pub async fn read(&self, r: Ref<'_>, revision: Option<i64>) -> Result<(Revision, Vec<u8>)> {
        let node = self.resolve(r).await?;
        let rev = self.revision_row(&node.id, revision).await?;
        let data = self.blobs.get(&rev.hash).await?;
        Ok((rev, data))
    }

    async fn revision_row(&self, node_id: &str, revision: Option<i64>) -> Result<Revision> {
        let node = self.node(node_id, None).await?;
        let want = match revision {
            Some(n) => n,
            None => node.head_revision.ok_or_else(|| Error::Invalid(format!("{} is a folder", node.path)))?,
        };
        let r = sqlx::query(sqlx::AssertSqlSafe(format!("{REVISION_SELECT} WHERE node_id = ? AND revision = ?")))
            .bind(node_id)
            .bind(want)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| Error::NotFound(format!("{} revision {want}", node.path)))?;
        Ok(revision_from_row(&r))
    }

    /// All revisions of a file, newest first.
    pub async fn revisions(&self, r: Ref<'_>) -> Result<Vec<Revision>> {
        let node = self.resolve(r).await?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!("{REVISION_SELECT} WHERE node_id = ? ORDER BY revision DESC")))
            .bind(&node.id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(revision_from_row).collect())
    }

    /// Top-most trashed nodes: what the trash shows. Their children come with them.
    pub async fn trash_list(&self) -> Result<Vec<Node>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{NODE_SELECT} WHERE n.trashed = 1 AND n.parent_id IN (SELECT id FROM nodes WHERE trashed = 0) ORDER BY n.name"
        )))
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::new();
        for r in rows {
            let id: String = r.get("id");
            out.push(node_from_row(&r, self.path_of(&id).await?));
        }
        Ok(out)
    }

    pub async fn starred(&self) -> Result<Vec<Node>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!("{NODE_SELECT} WHERE n.starred = 1 AND n.trashed = 0 ORDER BY n.name")))
            .fetch_all(&self.pool)
            .await?;
        let mut out = Vec::new();
        for r in rows {
            let id: String = r.get("id");
            out.push(node_from_row(&r, self.path_of(&id).await?));
        }
        Ok(out)
    }

    pub async fn permissions(&self, id: &str) -> Result<Vec<Permission>> {
        let rows = sqlx::query("SELECT principal, role FROM permissions WHERE node_id = ? ORDER BY principal")
            .bind(id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .map(|r| Permission { principal: r.get("principal"), role: r.get("role") })
            .collect())
    }

    /// Case-insensitive search over live file paths and the text of their head revisions.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Err(Error::Invalid("empty query".into()));
        }
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "{NODE_SELECT} WHERE n.kind = 'file' AND n.trashed = 0 AND n.head_revision IS NOT NULL"
        )))
        .fetch_all(&self.pool)
        .await?;
        let mut files = Vec::new();
        for r in &rows {
            let id: String = r.get("id");
            files.push(node_from_row(r, self.path_of(&id).await?));
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));

        let mut hits = Vec::new();
        for f in files {
            if hits.len() >= limit {
                break;
            }
            if f.path.to_lowercase().contains(&q) {
                hits.push(SearchHit { id: f.id, path: f.path, matched: "name", snippet: String::new() });
                continue;
            }
            let (Some(size), Some(mime), Some(hash)) = (f.size, f.mime.as_deref(), f.hash.as_deref()) else {
                continue;
            };
            if size > MAX_CONTENT_SCAN || !is_text(mime) {
                continue;
            }
            let data = self.blobs.get(hash).await?;
            let Ok(text) = std::str::from_utf8(&data) else { continue };
            if let Some(line) = text.lines().find(|l| l.to_lowercase().contains(&q)) {
                hits.push(SearchHit {
                    id: f.id,
                    path: f.path,
                    matched: "content",
                    snippet: line.chars().take(200).collect(),
                });
            }
        }
        Ok(hits)
    }

    // ----- writes --------------------------------------------------------------------------

    /// Create a folder and any missing parents. Existing folders are returned as they are.
    pub async fn mkdir(&self, path: &str, actor: &str) -> Result<Node> {
        let _guard = self.write_lock.lock().await;
        let norm = normalize(path)?;
        let mut id = ROOT_ID.to_string();
        let mut prefix = String::new();
        let mut checked = false;
        for seg in segments(&norm) {
            prefix = child_path(if prefix.is_empty() { "/" } else { &prefix }, seg);
            match self.child(&id, seg, false).await? {
                Some((cid, kind)) => {
                    if kind == "file" {
                        return Err(Error::Conflict(format!("{prefix} is a file")));
                    }
                    id = cid;
                }
                None => {
                    if !checked {
                        self.authorize(&id, actor, WRITER).await?;
                        checked = true;
                    }
                    let mut tx = self.pool.begin().await?;
                    id = insert_node(&mut tx, &id, seg, "folder", actor, &now()).await?;
                    tx.commit().await?;
                }
            }
        }
        self.node(&id, Some(norm)).await
    }

    /// Store `data` as the next revision of `path`, unless it is refused or nothing changed.
    pub async fn write(&self, path: &str, data: &[u8], opts: WriteOpts) -> Result<WriteOutcome> {
        let norm = normalize(path)?;
        if norm == "/" {
            return Err(Error::Invalid("cannot write to the root".into()));
        }
        validate_properties(&opts.properties)?;
        let hash = self.blobs.put(data).await?;
        let mime = guess_mime(&norm, data);
        self.commit(&norm, &hash, data.len() as i64, &mime, &opts.properties, &opts).await
    }

    /// Core write path, shared by `write`, `copy` and `rollback`. The blob must already exist.
    async fn commit(
        &self,
        norm: &str,
        hash: &str,
        size: i64,
        mime: &str,
        properties: &Properties,
        opts: &WriteOpts,
    ) -> Result<WriteOutcome> {
        let _guard = self.write_lock.lock().await;
        let slot = self.slot(norm).await?;

        // Who we check, and what the precondition sees as the live head.
        let (auth_id, live_head) = match &slot {
            Slot::Live(id) => {
                let n = self.node(id, None).await?;
                if n.kind == "folder" {
                    return Err(Error::Conflict(format!("{norm} is a folder")));
                }
                (id.clone(), n.head_revision)
            }
            Slot::Revive(id) => (id.clone(), None),
            Slot::Create { anchor, .. } => (anchor.clone(), None),
        };
        self.authorize(&auth_id, &opts.author, WRITER).await?;
        check_precondition(norm, live_head, opts.if_head_revision)?;

        let props_json = serde_json::to_string(properties).expect("string map serializes");
        if let Slot::Live(id) = &slot {
            let head = live_head.expect("a live file has a head revision");
            let same = sqlx::query("SELECT hash, properties FROM revisions WHERE node_id = ? AND revision = ?")
                .bind(id)
                .bind(head)
                .fetch_one(&self.pool)
                .await?;
            if same.get::<String, _>("hash") == hash && same.get::<String, _>("properties") == props_json {
                let node = self.node(id, Some(norm.to_string())).await?;
                return Ok(WriteOutcome { node, revision: head, new_revision: false });
            }
        }

        let ts = now();
        let mut tx = self.pool.begin().await?;
        let node_id = match &slot {
            Slot::Live(id) => id.clone(),
            Slot::Revive(id) => {
                sqlx::query("UPDATE nodes SET trashed = 0, modified_at = ? WHERE id = ?")
                    .bind(&ts)
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                id.clone()
            }
            Slot::Create { anchor, missing } => {
                let mut parent = anchor.clone();
                for dir in &missing[..missing.len() - 1] {
                    parent = insert_node(&mut tx, &parent, dir, "folder", &opts.author, &ts).await?;
                }
                insert_node(&mut tx, &parent, &missing[missing.len() - 1], "file", &opts.author, &ts).await?
            }
        };
        let next: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(revision), 0) + 1 FROM revisions WHERE node_id = ?")
            .bind(&node_id)
            .fetch_one(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO revisions (node_id, revision, hash, size, mime, author, comment, properties, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&node_id)
        .bind(next)
        .bind(hash)
        .bind(size)
        .bind(mime)
        .bind(&opts.author)
        .bind(&opts.comment)
        .bind(&props_json)
        .bind(&ts)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE nodes SET head_revision = ?, modified_at = ? WHERE id = ?")
            .bind(next)
            .bind(&ts)
            .bind(&node_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        let node = self.node(&node_id, Some(norm.to_string())).await?;
        Ok(WriteOutcome { node, revision: next, new_revision: true })
    }

    /// Copy one revision of a file to `to`, which may be the file's own path (a new revision
    /// with the same content). Content is shared by hash. The source properties carry over.
    pub async fn copy(
        &self,
        from: Ref<'_>,
        to: &str,
        source_revision: Option<i64>,
        opts: WriteOpts,
    ) -> Result<WriteOutcome> {
        let src = self.resolve(from).await?;
        if src.kind == "folder" {
            return Err(Error::Invalid("copying folders is not supported yet".into()));
        }
        let to = normalize(to)?;
        if to == "/" {
            return Err(Error::Invalid("cannot write to the root".into()));
        }
        let rev = self.revision_row(&src.id, source_revision).await?;
        self.commit(&to, &rev.hash, rev.size, &rev.mime, &rev.properties, &opts).await
    }

    /// Roll back by making an old revision the newest one. History is never rewritten.
    pub async fn rollback(&self, r: Ref<'_>, revision: i64, mut opts: WriteOpts) -> Result<WriteOutcome> {
        let node = self.resolve(r).await?;
        if node.trashed {
            return Err(Error::NotFound(format!("{} is in the trash", node.path)));
        }
        if opts.comment.is_empty() {
            opts.comment = format!("rollback to revision {revision}");
        }
        self.copy(Ref::Id(&node.id), &node.path, Some(revision), opts).await
    }

    /// Rename, move, star, trash or restore a node. Trashing a folder trashes everything under
    /// it, and restoring one restores everything under it.
    pub async fn update(&self, id: &str, u: NodeUpdate, actor: &str) -> Result<Node> {
        if id == ROOT_ID {
            return Err(Error::Invalid("the root cannot be changed".into()));
        }
        let _guard = self.write_lock.lock().await;
        let cur = self.node(id, None).await?;
        self.authorize(id, actor, WRITER).await?;
        if let Some(name) = &u.name {
            validate_name(name)?;
        }
        let new_parent = u
            .parent_id
            .clone()
            .or_else(|| cur.parent_id.clone())
            .expect("non-root nodes have a parent");
        if u.parent_id.is_some() {
            self.authorize(&new_parent, actor, WRITER).await?;
            let dest = self.node(&new_parent, None).await?;
            if dest.kind != "folder" || dest.trashed {
                return Err(Error::Invalid("destination must be a live folder".into()));
            }
            if self.is_ancestor_or_self(id, &new_parent).await? {
                return Err(Error::Invalid("cannot move a folder into itself".into()));
            }
        }
        let new_name = u.name.clone().unwrap_or_else(|| cur.name.clone());
        let new_trashed = u.trashed.unwrap_or(cur.trashed);
        if u.trashed == Some(false) && cur.trashed && self.node(&new_parent, None).await?.trashed {
            return Err(Error::Conflict("restore the parent folder first".into()));
        }
        if !new_trashed
            && let Some((clash, _)) = self.child(&new_parent, &new_name, false).await?
            && clash != id
        {
            return Err(Error::Conflict(format!("{new_name} already exists there")));
        }

        let ts = now();
        let mut tx = self.pool.begin().await?;
        if new_trashed != cur.trashed {
            sqlx::query(
                "UPDATE nodes SET trashed = ?, modified_at = ? WHERE id IN (\
                 WITH RECURSIVE sub(id) AS (SELECT ? UNION ALL SELECT n.id FROM nodes n JOIN sub ON n.parent_id = sub.id) \
                 SELECT id FROM sub)",
            )
            .bind(new_trashed as i64)
            .bind(&ts)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("UPDATE nodes SET parent_id = ?, name = ?, starred = ?, modified_at = ? WHERE id = ?")
            .bind(&new_parent)
            .bind(&new_name)
            .bind(u.starred.unwrap_or(cur.starred) as i64)
            .bind(&ts)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.node(id, None).await
    }

    /// Trash a file or folder, with an optional precondition on its head revision.
    pub async fn trash(&self, r: Ref<'_>, if_head_revision: Option<i64>, actor: &str) -> Result<Node> {
        let node = self.resolve(r).await?;
        check_precondition(&node.path, node.head_revision, if_head_revision)?;
        self.update(&node.id, NodeUpdate { trashed: Some(true), ..NodeUpdate::default() }, actor).await
    }

    /// Rename or move a live node to `to`, creating the destination's parent folders.
    pub async fn move_to(&self, from: Ref<'_>, to: &str, actor: &str) -> Result<Node> {
        let node = self.resolve(from).await?;
        let norm = normalize(to)?;
        if norm == "/" {
            return Err(Error::Invalid("cannot move to the root".into()));
        }
        let parent = self.mkdir(&parent_of(&norm), actor).await?;
        let name = name_of(&norm).to_string();
        self.update(
            &node.id,
            NodeUpdate { parent_id: Some(parent.id), name: Some(name), ..NodeUpdate::default() },
            actor,
        )
        .await
    }

    /// Permanently delete a trashed node and everything under it, with its revisions and grants.
    /// Blobs stay on disk until a garbage collector removes unreferenced ones.
    pub async fn purge(&self, id: &str, actor: &str) -> Result<()> {
        if id == ROOT_ID {
            return Err(Error::Invalid("the root cannot be deleted".into()));
        }
        let _guard = self.write_lock.lock().await;
        let node = self.node(id, None).await?;
        if !node.trashed {
            return Err(Error::Conflict(format!("move {} to the trash before deleting it forever", node.path)));
        }
        self.authorize(id, actor, OWNER).await?;
        let subtree = "(WITH RECURSIVE sub(id) AS (SELECT ? UNION ALL SELECT n.id FROM nodes n JOIN sub ON n.parent_id = sub.id) SELECT id FROM sub)";
        let mut tx = self.pool.begin().await?;
        for table in ["revisions", "permissions"] {
            sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {table} WHERE node_id IN {subtree}")))
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM nodes WHERE id IN {subtree}"))).bind(id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Replace the grant for `principal` on a node. Roles: reader, writer, owner.
    pub async fn share(&self, id: &str, principal: &str, role: &str, actor: &str) -> Result<Vec<Permission>> {
        validate_name(principal)?;
        if rank(role) == 0 {
            return Err(Error::Invalid(format!("role must be reader, writer or owner, not {role:?}")));
        }
        let _guard = self.write_lock.lock().await;
        self.authorize(id, actor, OWNER).await?;
        sqlx::query(
            "INSERT INTO permissions (node_id, principal, role) VALUES (?, ?, ?) \
             ON CONFLICT (node_id, principal) DO UPDATE SET role = excluded.role",
        )
        .bind(id)
        .bind(principal)
        .bind(role)
        .execute(&self.pool)
        .await?;
        self.permissions(id).await
    }

    pub async fn unshare(&self, id: &str, principal: &str, actor: &str) -> Result<Vec<Permission>> {
        let _guard = self.write_lock.lock().await;
        self.authorize(id, actor, OWNER).await?;
        let n = sqlx::query("DELETE FROM permissions WHERE node_id = ? AND principal = ?")
            .bind(id)
            .bind(principal)
            .execute(&self.pool)
            .await?
            .rows_affected();
        if n == 0 {
            return Err(Error::NotFound(format!("no grant for {principal} on this node")));
        }
        self.permissions(id).await
    }
}
