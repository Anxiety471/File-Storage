use std::collections::BTreeMap;

use drawer::{
    blob::BlobStore,
    error::Error,
    service::{Drawer, Metadata, WriteOpts},
};

async fn store() -> (Drawer, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    (Drawer::open(dir.path()).await.unwrap(), dir)
}

fn by(author: &str, comment: &str) -> WriteOpts {
    WriteOpts::new(author, comment)
}

fn guarded(expected: i64) -> WriteOpts {
    WriteOpts { if_generation_match: Some(expected), ..WriteOpts::new("t", "") }
}

fn meta(pairs: &[(&str, &str)]) -> Metadata {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<BTreeMap<_, _>>()
}

#[tokio::test]
async fn write_read_generations_and_rollback() {
    let (m, _d) = store().await;
    let v1 = m.write("/notes/a.md", b"one", by("alice", "first")).await.unwrap();
    assert!(v1.new_generation);
    assert_eq!(v1.info.generation, 1);
    let v2 = m.write("notes/a.md", b"two", by("bob", "second")).await.unwrap();
    assert_eq!(v2.info.generation, 2);

    // identical content does not create a generation
    let same = m.write("/notes/a.md", b"two", by("bob", "again")).await.unwrap();
    assert!(!same.new_generation);
    assert_eq!(same.info.generation, 2);

    assert_eq!(m.read("/notes/a.md", None).await.unwrap().1, b"two");
    assert_eq!(m.read("/notes/a.md", Some(1)).await.unwrap().1, b"one");

    let rb = m.rollback("/notes/a.md", 1, by("alice", "")).await.unwrap();
    assert_eq!(rb.info.generation, 3);
    assert_eq!(rb.info.comment, "rollback to generation 1");
    assert_eq!(m.read("/notes/a.md", None).await.unwrap().1, b"one");

    let gens = m.generations("/notes/a.md").await.unwrap();
    assert_eq!(gens.iter().map(|g| g.generation).collect::<Vec<_>>(), [3, 2, 1]);
    // history is untouched: generation 2 content is still readable
    assert_eq!(m.read("/notes/a.md", Some(2)).await.unwrap().1, b"two");
    // generations 1 and 3 share one blob
    assert_eq!(gens[0].hash, gens[2].hash);
}

#[tokio::test]
async fn folders_listing_and_conflicts() {
    let (m, _d) = store().await;
    m.write("/a/b/c.txt", b"x", by("t", "")).await.unwrap();
    m.mkdir("/a/empty").await.unwrap();

    let root = m.list("/").await.unwrap();
    assert_eq!(root.len(), 1);
    assert_eq!(root[0].path, "/a");
    let a: Vec<_> = m.list("/a").await.unwrap().into_iter().map(|e| (e.name, e.kind)).collect();
    assert_eq!(a, [("b".into(), "folder"), ("empty".into(), "folder")]);
    let b = m.list("/a/b").await.unwrap();
    assert_eq!(b[0].kind, "file");
    assert_eq!(b[0].size, Some(1));
    assert_eq!(b[0].generation, Some(1));

    assert!(matches!(m.mkdir("/a/b/c.txt").await, Err(Error::Conflict(_))));
    assert!(matches!(m.write("/a/b", b"x", by("t", "")).await, Err(Error::Conflict(_))));
    assert!(matches!(m.write("/a/b/c.txt/d", b"x", by("t", "")).await, Err(Error::Conflict(_))));
    assert!(matches!(m.rmdir("/a/b").await, Err(Error::Conflict(_))));
    m.rmdir("/a/empty").await.unwrap();
    assert!(matches!(m.list("/a/empty").await, Err(Error::NotFound(_))));
}

#[tokio::test]
async fn delete_keeps_history_and_rewrite_restores() {
    let (m, _d) = store().await;
    m.write("/f.txt", b"one", by("t", "")).await.unwrap();
    m.delete_file("/f.txt", None).await.unwrap();
    assert!(matches!(m.read("/f.txt", None).await, Err(Error::NotFound(_))));
    assert!(m.list("/").await.unwrap().is_empty());
    assert!(matches!(m.delete_file("/f.txt", None).await, Err(Error::NotFound(_))));

    let w = m.write("/f.txt", b"one", by("t", "back")).await.unwrap();
    assert!(w.new_generation);
    assert_eq!(w.info.generation, 2);
    assert_eq!(m.generations("/f.txt").await.unwrap().len(), 2);
}

#[tokio::test]
async fn create_only_precondition() {
    let (m, _d) = store().await;
    // 0 means "must not exist": first create passes
    m.write("/p.txt", b"one", guarded(0)).await.unwrap();
    // and a second create-only write is refused, leaving the file untouched
    let err = m.write("/p.txt", b"two", guarded(0)).await.unwrap_err();
    assert!(matches!(err, Error::PreconditionFailed(_)));
    assert_eq!(m.read("/p.txt", None).await.unwrap().1, b"one");
    assert_eq!(m.generations("/p.txt").await.unwrap().len(), 1);
}

#[tokio::test]
async fn stale_writer_is_refused() {
    let (m, _d) = store().await;
    m.write("/plan.md", b"v1", by("a", "")).await.unwrap();
    // agent A reads generation 1, agent B lands generation 2
    m.write("/plan.md", b"v2 by B", by("b", "")).await.unwrap();

    // A's write, guarded on the generation it read, must fail instead of clobbering B
    let err = m.write("/plan.md", b"v2 by A", guarded(1)).await.unwrap_err();
    assert!(matches!(err, Error::PreconditionFailed(ref msg) if msg.contains("generation 2")));
    assert_eq!(m.read("/plan.md", None).await.unwrap().1, b"v2 by B");

    // re-reading and guarding on the current generation goes through
    let ok = m.write("/plan.md", b"merged", guarded(2)).await.unwrap();
    assert_eq!(ok.info.generation, 3);
}

#[tokio::test]
async fn guarded_delete() {
    let (m, _d) = store().await;
    m.write("/d.txt", b"x", by("t", "")).await.unwrap();
    m.write("/d.txt", b"y", by("t", "")).await.unwrap();
    assert!(matches!(m.delete_file("/d.txt", Some(1)).await, Err(Error::PreconditionFailed(_))));
    assert!(m.read("/d.txt", None).await.is_ok());
    m.delete_file("/d.txt", Some(2)).await.unwrap();
    // a deleted file does not exist, so create-only applies again
    assert!(m.write("/d.txt", b"z", guarded(0)).await.is_ok());
}

#[tokio::test]
async fn metadata_is_stored_per_generation() {
    let (m, _d) = store().await;
    let mut opts = by("t", "");
    opts.metadata = meta(&[("source", "web"), ("topic", "rust")]);
    let w = m.write("/m.md", b"same", opts).await.unwrap();
    assert_eq!(w.info.metadata, meta(&[("source", "web"), ("topic", "rust")]));

    // same bytes, different metadata: that is a real change, so a new generation
    let mut opts = by("t", "");
    opts.metadata = meta(&[("source", "api")]);
    let w2 = m.write("/m.md", b"same", opts).await.unwrap();
    assert!(w2.new_generation);
    assert_eq!(m.stat("/m.md", Some(1)).await.unwrap().metadata["source"], "web");
    assert_eq!(m.stat("/m.md", None).await.unwrap().metadata, meta(&[("source", "api")]));

    // bad keys and too many entries are refused before anything is stored
    let mut bad = by("t", "");
    bad.metadata = meta(&[("has space", "x")]);
    assert!(matches!(m.write("/m.md", b"z", bad).await, Err(Error::Invalid(_))));
    assert_eq!(m.generations("/m.md").await.unwrap().len(), 2);
}

#[tokio::test]
async fn copy_shares_content_and_keeps_source() {
    let (m, _d) = store().await;
    let mut opts = by("t", "");
    opts.metadata = meta(&[("kind", "note")]);
    m.write("/src/a.md", b"alpha", opts).await.unwrap();
    m.write("/src/a.md", b"beta", by("t", "")).await.unwrap();

    // copy an older generation to a new path
    let c = m.copy("/src/a.md", "/dst/a.md", Some(1), by("t", "copied")).await.unwrap();
    assert!(c.new_generation);
    assert_eq!(c.info.generation, 1);
    assert_eq!(c.info.comment, "copied");
    assert_eq!(c.info.metadata, meta(&[("kind", "note")]));
    assert_eq!(m.read("/dst/a.md", None).await.unwrap().1, b"alpha");
    // source is untouched
    assert_eq!(m.read("/src/a.md", None).await.unwrap().1, b"beta");

    // copy onto a path that is already at that content is a no-op
    let again = m.copy("/src/a.md", "/dst/a.md", Some(1), by("t", "")).await.unwrap();
    assert!(!again.new_generation);

    // a guarded copy respects the destination's live generation
    assert!(matches!(
        m.copy("/src/a.md", "/dst/a.md", None, guarded(0)).await,
        Err(Error::PreconditionFailed(_))
    ));
}

#[tokio::test]
async fn search_names_and_contents() {
    let (m, _d) = store().await;
    m.write("/docs/plan.md", b"ship the Rust rewrite\nlater", by("t", "")).await.unwrap();
    m.write("/docs/other.md", b"nothing here", by("t", "")).await.unwrap();
    m.write("/blob.bin", &[0xff, 0xfe, 0x00], by("t", "")).await.unwrap();

    let by_name = m.search("PLAN", 10).await.unwrap();
    assert_eq!((by_name[0].path.as_str(), by_name[0].matched), ("/docs/plan.md", "name"));
    let by_content = m.search("rust rewrite", 10).await.unwrap();
    assert_eq!(by_content.len(), 1);
    assert_eq!(by_content[0].matched, "content");
    assert_eq!(by_content[0].snippet, "ship the Rust rewrite");
    assert!(m.search("zzz", 10).await.unwrap().is_empty());
    assert!(m.search("  ", 10).await.is_err());
}

#[tokio::test]
async fn path_normalization_rejects_traversal() {
    assert_eq!(drawer::service::normalize("a//b/./c/").unwrap(), "/a/b/c");
    assert_eq!(drawer::service::normalize("").unwrap(), "/");
    assert!(drawer::service::normalize("/a/../b").is_err());
}

#[tokio::test]
async fn reopen_persists() {
    let dir = tempfile::tempdir().unwrap();
    {
        let m = Drawer::open(dir.path()).await.unwrap();
        let mut opts = by("t", "");
        opts.metadata = meta(&[("k", "v")]);
        m.write("/p.txt", b"persist", opts).await.unwrap();
    }
    let m = Drawer::open(dir.path()).await.unwrap();
    let (info, data) = m.read("/p.txt", None).await.unwrap();
    assert_eq!(data, b"persist");
    assert_eq!(info.metadata, meta(&[("k", "v")]));
}

/// A store written by the pre-generation schema must open and keep its history.
#[tokio::test]
async fn legacy_store_is_migrated_in_place() {
    use sqlx::{SqlitePool, sqlite::SqliteConnectOptions};

    let dir = tempfile::tempdir().unwrap();
    let blobs = BlobStore::new(dir.path().join("blobs"));
    let hash = blobs.put(b"old body").await.unwrap();
    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new().filename(dir.path().join("drawer.db")).create_if_missing(true),
    )
    .await
    .unwrap();
    sqlx::raw_sql(
        "CREATE TABLE folders (path TEXT PRIMARY KEY, parent TEXT NOT NULL, created_at TEXT NOT NULL);
         CREATE TABLE files (id INTEGER PRIMARY KEY AUTOINCREMENT, path TEXT NOT NULL UNIQUE, parent TEXT NOT NULL,
             current_version INTEGER NOT NULL, deleted INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
         CREATE TABLE versions (id INTEGER PRIMARY KEY AUTOINCREMENT, file_id INTEGER NOT NULL REFERENCES files(id),
             version INTEGER NOT NULL, hash TEXT NOT NULL, size INTEGER NOT NULL, mime TEXT NOT NULL, author TEXT NOT NULL,
             comment TEXT NOT NULL, created_at TEXT NOT NULL, UNIQUE (file_id, version));
         INSERT INTO files (path, parent, current_version, created_at, updated_at) VALUES ('/old.md', '/', 1, 'x', 'x');
         INSERT INTO versions (file_id, version, hash, size, mime, author, comment, created_at)
             VALUES (1, 1, 'HASH', 8, 'text/markdown', 'legacy', 'imported', 'x');",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE versions SET hash = ?").bind(&hash).execute(&pool).await.unwrap();
    pool.close().await;

    let m = Drawer::open(dir.path()).await.unwrap();
    let (info, data) = m.read("/old.md", None).await.unwrap();
    assert_eq!(data, b"old body");
    assert_eq!(info.generation, 1);
    assert!(info.metadata.is_empty());
    // new-style preconditions work on migrated data
    assert!(matches!(m.write("/old.md", b"x", guarded(0)).await, Err(Error::PreconditionFailed(_))));
    let w = m.write("/old.md", b"new body", guarded(1)).await.unwrap();
    assert_eq!(w.info.generation, 2);
    // reopening an already-migrated store is a no-op
    drop(m);
    Drawer::open(dir.path()).await.unwrap();
}
