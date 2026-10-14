use drawer::{error::Error, service::Drawer};

async fn store() -> (Drawer, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    (Drawer::open(dir.path()).await.unwrap(), dir)
}

#[tokio::test]
async fn write_read_versions_and_rollback() {
    let (m, _d) = store().await;
    let v1 = m.write("/notes/a.md", b"one", "alice", "first").await.unwrap();
    assert!(v1.created);
    assert_eq!(v1.info.version, 1);
    let v2 = m.write("notes/a.md", b"two", "bob", "second").await.unwrap();
    assert_eq!(v2.info.version, 2);

    // identical content does not create a version
    let same = m.write("/notes/a.md", b"two", "bob", "again").await.unwrap();
    assert!(!same.created);
    assert_eq!(same.info.version, 2);

    assert_eq!(m.read("/notes/a.md", None).await.unwrap().1, b"two");
    assert_eq!(m.read("/notes/a.md", Some(1)).await.unwrap().1, b"one");

    let rb = m.rollback("/notes/a.md", 1, "alice").await.unwrap();
    assert_eq!(rb.info.version, 3);
    assert_eq!(rb.info.comment, "rollback to v1");
    assert_eq!(m.read("/notes/a.md", None).await.unwrap().1, b"one");

    let versions = m.versions("/notes/a.md").await.unwrap();
    assert_eq!(versions.iter().map(|v| v.version).collect::<Vec<_>>(), [3, 2, 1]);
    // history is untouched: v2 content is still readable
    assert_eq!(m.read("/notes/a.md", Some(2)).await.unwrap().1, b"two");
    // v1 and v3 share one blob
    assert_eq!(versions[0].hash, versions[2].hash);
}

#[tokio::test]
async fn folders_listing_and_conflicts() {
    let (m, _d) = store().await;
    m.write("/a/b/c.txt", b"x", "t", "").await.unwrap();
    m.mkdir("/a/empty").await.unwrap();

    let root = m.list("/").await.unwrap();
    assert_eq!(root.len(), 1);
    assert_eq!(root[0].path, "/a");
    let a: Vec<_> = m.list("/a").await.unwrap().into_iter().map(|e| (e.name, e.kind)).collect();
    assert_eq!(a, [("b".into(), "folder"), ("empty".into(), "folder")]);
    let b = m.list("/a/b").await.unwrap();
    assert_eq!(b[0].kind, "file");
    assert_eq!(b[0].size, Some(1));

    assert!(matches!(m.mkdir("/a/b/c.txt").await, Err(Error::Conflict(_))));
    assert!(matches!(m.write("/a/b", b"x", "t", "").await, Err(Error::Conflict(_))));
    assert!(matches!(m.write("/a/b/c.txt/d", b"x", "t", "").await, Err(Error::Conflict(_))));
    assert!(matches!(m.rmdir("/a/b").await, Err(Error::Conflict(_))));
    m.rmdir("/a/empty").await.unwrap();
    assert!(matches!(m.list("/a/empty").await, Err(Error::NotFound(_))));
}

#[tokio::test]
async fn delete_keeps_history_and_rewrite_restores() {
    let (m, _d) = store().await;
    m.write("/f.txt", b"one", "t", "").await.unwrap();
    m.delete_file("/f.txt").await.unwrap();
    assert!(matches!(m.read("/f.txt", None).await, Err(Error::NotFound(_))));
    assert!(m.list("/").await.unwrap().is_empty());
    assert!(matches!(m.delete_file("/f.txt").await, Err(Error::NotFound(_))));

    let w = m.write("/f.txt", b"one", "t", "back").await.unwrap();
    assert!(w.created);
    assert_eq!(w.info.version, 2);
    assert_eq!(m.versions("/f.txt").await.unwrap().len(), 2);
}

#[tokio::test]
async fn search_names_and_contents() {
    let (m, _d) = store().await;
    m.write("/docs/plan.md", b"ship the Rust rewrite\nlater", "t", "").await.unwrap();
    m.write("/docs/other.md", b"nothing here", "t", "").await.unwrap();
    m.write("/blob.bin", &[0xff, 0xfe, 0x00], "t", "").await.unwrap();

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
        m.write("/p.txt", b"persist", "t", "").await.unwrap();
    }
    let m = Drawer::open(dir.path()).await.unwrap();
    assert_eq!(m.read("/p.txt", None).await.unwrap().1, b"persist");
}
