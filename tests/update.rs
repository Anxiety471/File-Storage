//! Self-update against a fake release host, so the tests need no network.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use axum::{
    Router,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use drawer::update::{self, Config};
use sha2::{Digest, Sha256};

type Files = Arc<HashMap<String, Vec<u8>>>;

/// Serve `files` by request path, the way the release host would.
async fn serve(files: HashMap<String, Vec<u8>>) -> SocketAddr {
    async fn handler(State(files): State<Files>, req: Request) -> Response {
        match files.get(req.uri().path()) {
            Some(body) => (StatusCode::OK, body.clone()).into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        }
    }
    let app = Router::new().fallback(handler).with_state(Arc::new(files));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

fn config(addr: SocketAddr) -> Config {
    Config {
        repo: "test/drawer".into(),
        api_url: format!("http://{addr}"),
        github_url: format!("http://{addr}"),
    }
}

/// The files a release would publish for this platform: the latest-release JSON, SHA256SUMS
/// and an archive holding `drawer` and `ui/index.html`. `sums_hash` overrides the checksum.
fn release(
    tag: &str,
    binary: &[u8],
    ui: &[u8],
    sums_hash: Option<&str>,
    work: &Path,
) -> HashMap<String, Vec<u8>> {
    let target = update::target().unwrap();
    let bundle = work.join(format!("drawer-{target}"));
    std::fs::create_dir_all(bundle.join("ui")).unwrap();
    std::fs::write(bundle.join("drawer"), binary).unwrap();
    std::fs::write(bundle.join("ui/index.html"), ui).unwrap();
    let archive_name = format!("drawer-{target}.tar.gz");
    let archive = work.join(&archive_name);
    let status = Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(work)
        .arg(format!("drawer-{target}"))
        .status()
        .unwrap();
    assert!(status.success());
    let bytes = std::fs::read(&archive).unwrap();
    let hash = sums_hash
        .map(str::to_string)
        .unwrap_or_else(|| hex::encode(Sha256::digest(&bytes)));

    let mut files = HashMap::new();
    files.insert(
        "/test/drawer/releases/download/".to_string() + tag + "/" + &archive_name,
        bytes,
    );
    files.insert(
        format!("/test/drawer/releases/download/{tag}/SHA256SUMS"),
        format!("{hash}  {archive_name}\n").into_bytes(),
    );
    files.insert(
        "/repos/test/drawer/releases/latest".to_string(),
        format!(r#"{{"tag_name":"{tag}"}}"#).into_bytes(),
    );
    files
}

/// An install laid out the way install.sh leaves it: bin/drawer and share/drawer/ui.
fn old_install(root: &Path) -> std::path::PathBuf {
    let exe = root.join("bin/drawer");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, b"old binary").unwrap();
    let ui = root.join("share/drawer/ui");
    std::fs::create_dir_all(&ui).unwrap();
    std::fs::write(ui.join("index.html"), b"old ui").unwrap();
    exe
}

#[tokio::test]
async fn check_finds_a_newer_release() {
    let work = tempfile::tempdir().unwrap();
    let addr = serve(release("v9.0.0", b"", b"", None, work.path())).await;
    let cfg = config(addr);
    let status = tokio::task::spawn_blocking(move || update::check(&cfg))
        .await
        .unwrap()
        .unwrap();
    assert!(status.update_available, "{status:?}");
    assert_eq!(status.latest, "v9.0.0");
    assert_eq!(status.current, env!("CARGO_PKG_VERSION"));
}

#[tokio::test]
async fn check_says_so_when_no_release_is_published() {
    let addr = serve(HashMap::new()).await;
    let cfg = config(addr);
    let err = tokio::task::spawn_blocking(move || update::check(&cfg))
        .await
        .unwrap()
        .unwrap_err();
    assert!(err.to_string().contains("no published release"), "{err}");
}

#[tokio::test]
async fn install_replaces_the_binary_and_the_ui() {
    let work = tempfile::tempdir().unwrap();
    let addr = serve(release(
        "v9.0.0",
        b"new binary",
        b"new ui",
        None,
        work.path(),
    ))
    .await;
    let cfg = config(addr);
    let install = tempfile::tempdir().unwrap();
    let exe = old_install(install.path());

    let exe_clone = exe.clone();
    tokio::task::spawn_blocking(move || update::install(&cfg, "v9.0.0", &exe_clone))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(std::fs::read(&exe).unwrap(), b"new binary");
    let ui = install.path().join("share/drawer/ui/index.html");
    assert_eq!(std::fs::read(ui).unwrap(), b"new ui");
    assert!(!install.path().join("share/drawer/ui.old").exists());
    assert!(!install.path().join("share/drawer/ui.new").exists());
}

#[tokio::test]
async fn install_keeps_the_old_install_when_the_checksum_does_not_match() {
    let work = tempfile::tempdir().unwrap();
    let wrong = "0".repeat(64);
    let addr = serve(release(
        "v9.0.0",
        b"new binary",
        b"new ui",
        Some(&wrong),
        work.path(),
    ))
    .await;
    let cfg = config(addr);
    let install = tempfile::tempdir().unwrap();
    let exe = old_install(install.path());

    let exe_clone = exe.clone();
    let err = tokio::task::spawn_blocking(move || update::install(&cfg, "v9.0.0", &exe_clone))
        .await
        .unwrap()
        .unwrap_err();
    assert!(err.to_string().contains("checksum mismatch"), "{err}");
    assert_eq!(std::fs::read(&exe).unwrap(), b"old binary");
    assert_eq!(
        std::fs::read(install.path().join("share/drawer/ui/index.html")).unwrap(),
        b"old ui"
    );
}

#[tokio::test]
async fn install_refuses_a_tag_that_is_not_a_version() {
    // Nothing is served, so a request would fail differently; this must fail before any request.
    let addr = serve(HashMap::new()).await;
    let cfg = config(addr);
    let install = tempfile::tempdir().unwrap();
    let exe = old_install(install.path());
    let err = tokio::task::spawn_blocking(move || update::install(&cfg, "v9.0.0/../../x", &exe))
        .await
        .unwrap()
        .unwrap_err();
    assert!(err.to_string().contains("cannot read"), "{err}");
}
