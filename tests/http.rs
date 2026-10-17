use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use drawer::service::Drawer;
use http_body_util::BodyExt;
use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, ClientConfig},
    transport::{
        StreamableHttpClientTransport,
        streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde_json::{Value, json};
use tower::ServiceExt as _;

async fn json_of(resp: axum::response::Response) -> Value {
    serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

async fn send(app: &Router, req: Request<Body>) -> axum::response::Response {
    app.clone().oneshot(req).await.unwrap()
}

fn get(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

fn json_post(uri: &str, body: Value) -> Request<Body> {
    Request::post(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn rest_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let app = drawer::app(Drawer::open(dir.path()).await.unwrap(), None, None);

    let put = |body: &'static str| {
        Request::put("/drawer/v1/files?path=/x/y.txt")
            .header("x-drawer-author", "tester")
            .header("x-drawer-comment", "hi")
            .body(Body::from(body))
            .unwrap()
    };
    let r = send(&app, put("v1")).await;
    assert_eq!(r.status(), StatusCode::OK);
    let written = json_of(r).await;
    assert_eq!(written["revision"], 1);
    assert_eq!(written["new_revision"], true);
    let id = written["node"]["id"].as_str().unwrap().to_string();
    send(&app, put("v2")).await;

    let r = send(&app, get("/drawer/v1/files?path=/x/y.txt&revision=1")).await;
    assert_eq!(r.headers()["x-drawer-revision"], "1");
    assert_eq!(r.into_body().collect().await.unwrap().to_bytes(), "v1");

    // the same file is reachable by its id
    let r = send(&app, get(&format!("/drawer/v1/files?id={id}"))).await;
    assert_eq!(r.into_body().collect().await.unwrap().to_bytes(), "v2");

    let tree = json_of(send(&app, get("/drawer/v1/tree?path=/x")).await).await;
    assert_eq!(tree["entries"][0]["name"], "y.txt");

    let r = send(&app, json_post("/drawer/v1/rollback", json!({"path": "/x/y.txt", "revision": 1}))).await;
    assert_eq!(json_of(r).await["revision"], 3);

    let revs = json_of(send(&app, get("/drawer/v1/revisions?path=/x/y.txt")).await).await;
    assert_eq!(revs["revisions"].as_array().unwrap().len(), 3);

    let hits = json_of(send(&app, get("/drawer/v1/search?q=y.txt")).await).await;
    assert_eq!(hits["hits"][0]["path"], "/x/y.txt");

    assert_eq!(send(&app, get("/drawer/v1/files?path=/nope")).await.status(), StatusCode::NOT_FOUND);
    assert_eq!(send(&app, get("/drawer/v1/files?path=/a/../b")).await.status(), StatusCode::BAD_REQUEST);
    assert_eq!(send(&app, get("/drawer/v1/files")).await.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn preconditions_properties_copy_move_and_trash_over_rest() {
    let dir = tempfile::tempdir().unwrap();
    let app = drawer::app(Drawer::open(dir.path()).await.unwrap(), None, None);

    // create-only: first PUT with if_head_revision=0 succeeds, the second is refused with 412
    let create_only = |body: &'static str| {
        Request::put("/drawer/v1/files?path=/g/a.txt&if_head_revision=0")
            .header("x-drawer-prop-source", "agent-1")
            .body(Body::from(body))
            .unwrap()
    };
    assert_eq!(send(&app, create_only("one")).await.status(), StatusCode::OK);
    assert_eq!(send(&app, create_only("two")).await.status(), StatusCode::PRECONDITION_FAILED);

    // properties come back as x-drawer-prop-* headers on read
    let r = send(&app, get("/drawer/v1/files?path=/g/a.txt")).await;
    assert_eq!(r.headers()["x-drawer-prop-source"], "agent-1");

    // a stale head is refused on PUT and on trash
    let stale_put = Request::put("/drawer/v1/files?path=/g/a.txt&if_head_revision=7").body(Body::from("nope")).unwrap();
    assert_eq!(send(&app, stale_put).await.status(), StatusCode::PRECONDITION_FAILED);
    let stale_del = Request::delete("/drawer/v1/files?path=/g/a.txt&if_head_revision=7").body(Body::empty()).unwrap();
    assert_eq!(send(&app, stale_del).await.status(), StatusCode::PRECONDITION_FAILED);

    // copy to a new path keeps the content and properties
    let copied = json_of(
        send(&app, json_post("/drawer/v1/copy", json!({"from": "/g/a.txt", "to": "/g/b.txt"}))).await,
    )
    .await;
    assert_eq!(copied["new_revision"], true);
    assert_eq!(copied["node"]["properties"]["source"], "agent-1");
    let r = send(&app, get("/drawer/v1/files?path=/g/b.txt")).await;
    assert_eq!(r.into_body().collect().await.unwrap().to_bytes(), "one");

    // rename /g/b.txt to /g/c.txt by node id
    let id = copied["node"]["id"].as_str().unwrap().to_string();
    let patch = Request::patch(format!("/drawer/v1/nodes/{id}"))
        .header("content-type", "application/json")
        .body(Body::from(json!({"name": "c.txt"}).to_string()))
        .unwrap();
    assert_eq!(json_of(send(&app, patch).await).await["path"], "/g/c.txt");

    // purging a live node is refused; trash first
    let purge = || Request::delete(format!("/drawer/v1/nodes/{id}")).body(Body::empty()).unwrap();
    assert_eq!(send(&app, purge()).await.status(), StatusCode::CONFLICT);
    let trash = Request::delete(format!("/drawer/v1/files?id={id}")).body(Body::empty()).unwrap();
    assert_eq!(json_of(send(&app, trash).await).await["trashed"], true);
    let bin = json_of(send(&app, get("/drawer/v1/trash")).await).await;
    assert_eq!(bin["nodes"][0]["id"], id);
    assert_eq!(send(&app, purge()).await.status(), StatusCode::OK);
    assert_eq!(send(&app, get(&format!("/drawer/v1/files?id={id}"))).await.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn binary_files_round_trip_with_types_and_safe_headers() {
    let dir = tempfile::tempdir().unwrap();
    let app = drawer::app(Drawer::open(dir.path()).await.unwrap(), None, None);

    // larger than axum's 2 MiB default limit, and not UTF-8
    let big: Vec<u8> = (0..3 * 1024 * 1024).map(|i| (i % 251) as u8 | 0x80).collect();
    let cases = [
        ("/docs/report.pdf", "application/pdf"),
        ("/docs/plan.docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
        ("/docs/budget.xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
        ("/docs/old.xls", "application/vnd.ms-excel"),
        ("/docs/blob.bin", "application/octet-stream"),
    ];
    for (path, mime) in cases {
        let put = Request::put(format!("/drawer/v1/files?path={path}")).body(Body::from(big.clone())).unwrap();
        assert_eq!(send(&app, put).await.status(), StatusCode::OK, "{path}");
        let r = send(&app, get(&format!("/drawer/v1/files?path={path}"))).await;
        assert_eq!(r.headers()["content-type"], mime, "{path}");
        assert_eq!(r.headers()["x-content-type-options"], "nosniff");
        assert_eq!(r.into_body().collect().await.unwrap().to_bytes().len(), big.len());
    }

    // source code and Markdown are text; HTML opened directly runs sandboxed
    for (path, body, mime) in [
        ("/src/main.rs", "fn main() {}", "text/plain"),
        ("/src/app.ts", "export {}", "text/plain"),
        ("/notes/a.md", "# hi", "text/markdown"),
        ("/web/x.html", "<script>alert(1)</script>", "text/html"),
    ] {
        let put = Request::put(format!("/drawer/v1/files?path={path}")).body(Body::from(body)).unwrap();
        send(&app, put).await;
        let r = send(&app, get(&format!("/drawer/v1/files?path={path}"))).await;
        assert_eq!(r.headers()["content-type"], mime, "{path}");
        assert!(r.headers()["content-security-policy"].to_str().unwrap().starts_with("sandbox"));
    }

    // inline by default, attachment on request, with the file name
    let r = send(&app, get("/drawer/v1/files?path=/docs/report.pdf")).await;
    assert_eq!(r.headers()["content-disposition"], "inline; filename*=UTF-8''report.pdf");
    assert!(r.headers().get("content-security-policy").is_none());
    let r = send(&app, get("/drawer/v1/files?path=/docs/plan.docx&download=true")).await;
    assert_eq!(r.headers()["content-disposition"], "attachment; filename*=UTF-8''plan.docx");
}

#[tokio::test]
async fn permissions_are_enforced_over_rest() {
    let dir = tempfile::tempdir().unwrap();
    let app = drawer::app(Drawer::open(dir.path()).await.unwrap(), None, None);

    let put_as = |who: &'static str| {
        Request::put("/drawer/v1/files?path=/team/doc.md")
            .header("x-drawer-author", who)
            .body(Body::from(who))
            .unwrap()
    };
    send(&app, put_as("alice")).await;
    let team = json_of(send(&app, get("/drawer/v1/files/meta?path=/team")).await).await["id"].as_str().unwrap().to_string();

    let share = Request::put(format!("/drawer/v1/nodes/{team}/permissions"))
        .header("content-type", "application/json")
        .header("x-drawer-author", "alice")
        .body(Body::from(json!({"principal": "bob", "role": "writer"}).to_string()))
        .unwrap();
    assert_eq!(send(&app, share).await.status(), StatusCode::OK);

    assert_eq!(send(&app, put_as("bob")).await.status(), StatusCode::OK);
    assert_eq!(send(&app, put_as("carol")).await.status(), StatusCode::FORBIDDEN);

    let list = json_of(send(&app, get(&format!("/drawer/v1/nodes/{team}/permissions"))).await).await;
    assert_eq!(list["permissions"][0]["principal"], "bob");
}

#[tokio::test]
async fn top_level_health_is_json_not_the_ui_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let ui = tempfile::tempdir().unwrap();
    std::fs::write(ui.path().join("index.html"), "<!doctype html><title>ui</title>").unwrap();
    let app = drawer::app(Drawer::open(dir.path()).await.unwrap(), None, Some(ui.path().to_path_buf()));
    let resp = send(&app, get("/health")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json_of(resp).await, json!({"ok": true}));
    assert_eq!(json_of(send(&app, get("/drawer/v1/health")).await).await, json!({"ok": true}));
}

#[tokio::test]
async fn bearer_token_is_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let app = drawer::app(Drawer::open(dir.path()).await.unwrap(), Some("s3cret".into()), None);
    assert_eq!(send(&app, get("/drawer/v1/health")).await.status(), StatusCode::UNAUTHORIZED);
    let authed = Request::get("/drawer/v1/health").header("authorization", "Bearer s3cret").body(Body::empty()).unwrap();
    assert_eq!(send(&app, authed).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn mcp_tools_over_http() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let app = drawer::app(Drawer::open(dir.path()).await?, None, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(format!("http://{addr}/mcp")),
    );
    let client = ClientConfig::default().serve(transport).await?;

    let tools = client.list_all_tools().await?;
    let names: Vec<_> = tools.iter().map(|t| t.name.to_string()).collect();
    for n in [
        "drawer_list",
        "drawer_read",
        "drawer_write",
        "drawer_rollback",
        "drawer_copy",
        "drawer_move",
        "drawer_trash",
        "drawer_restore",
        "drawer_share",
        "drawer_search",
    ] {
        assert!(names.iter().any(|x| x == n), "missing tool {n}: {names:?}");
    }

    // identity and usage notes reach the harness
    let info = client.peer_info().expect("initialized");
    assert_eq!(info.server_info.as_ref().map(|i| i.name.as_str()), Some("drawer"));
    assert!(info.instructions.as_deref().unwrap_or("").contains("if_head_revision"));

    // schemas stay in the subset Gemini and other strict model APIs accept
    for t in &tools {
        let schema = serde_json::to_string(&t.input_schema).unwrap();
        for banned in ["\"$schema\"", "\"format\"", "\"additionalProperties\"", "\"$ref\"", "\"null\""] {
            assert!(!schema.contains(banned), "{} schema has {banned}: {schema}", t.name);
        }
        let ann = t.annotations.as_ref().expect("annotated");
        assert!(ann.read_only_hint.is_some(), "{} has no read-only hint", t.name);
    }
    let purge = tools.iter().find(|t| t.name == "drawer_purge").unwrap();
    assert_eq!(purge.annotations.as_ref().unwrap().destructive_hint, Some(true));

    let call = |name: &'static str, args: Value| {
        let client = &client;
        async move {
            let args = args.as_object().cloned().unwrap();
            client.call_tool(CallToolRequestParams::new(name).with_arguments(args)).await
        }
    };
    let text = |r: rmcp::model::CallToolResult| format!("{:?}", r.content);
    let text_of = |r: &rmcp::model::CallToolResult| -> Value {
        let content = serde_json::to_value(&r.content).unwrap();
        serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap()
    };

    let r = call("drawer_write", json!({"path": "/mem/a.md", "content": "alpha", "comment": "c"})).await?;
    assert_ne!(r.is_error, Some(true));
    call("drawer_write", json!({"path": "/mem/a.md", "content": "beta"})).await?;
    let r = call("drawer_read", json!({"path": "/mem/a.md", "revision": 1})).await?;
    assert!(text(r).contains("alpha"));
    let r = call("drawer_rollback", json!({"path": "/mem/a.md", "revision": 1})).await?;
    assert_ne!(r.is_error, Some(true));
    let r = call("drawer_read", json!({"path": "/mem/a.md"})).await?;
    assert!(text(r).contains("alpha"));

    // move, trash and restore by id
    let r = call("drawer_move", json!({"from": "/mem/a.md", "to": "/mem/archive/a.md"})).await?;
    assert_ne!(r.is_error, Some(true));
    let id = text_of(&call("drawer_stat", json!({"path": "/mem/archive/a.md"})).await?)["id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = call("drawer_delete", json!({"path": "/mem/archive/a.md"})).await?;
    assert_ne!(r.is_error, Some(true));
    assert!(text(call("drawer_trash", json!({})).await?).contains(&id));
    let r = call("drawer_restore", json!({"id": id})).await?;
    assert_ne!(r.is_error, Some(true));
    let r = call("drawer_read", json!({"path": "/mem/archive/a.md"})).await?;
    assert!(text(r).contains("alpha"));

    // binary files travel as base64
    let r = call("drawer_write", json!({"path": "/mem/logo.png", "content": "iVBORw0KGgo=", "encoding": "base64"})).await?;
    assert_ne!(r.is_error, Some(true));
    let r = call("drawer_write", json!({"path": "/mem/blob.bin", "content": "AAEC/w==", "encoding": "base64"})).await?;
    assert_ne!(r.is_error, Some(true));
    let r = call("drawer_read", json!({"path": "/mem/blob.bin"})).await?;
    assert_eq!(r.is_error, Some(true), "binary as text is refused");
    // images come back as image content for vision models
    let r = call("drawer_read", json!({"path": "/mem/logo.png"})).await?;
    let content = serde_json::to_value(&r.content)?;
    assert_eq!(content[1]["type"], "image");
    assert_eq!(content[1]["mimeType"], "image/png");
    let r = call("drawer_read", json!({"path": "/mem/logo.png", "encoding": "base64"})).await?;
    assert!(text(r).contains("iVBORw0KGgo="));

    let r = call("drawer_read", json!({"path": "/missing"})).await?;
    assert_eq!(r.is_error, Some(true));

    // edit, diff and recent; changes are attributed to the client's handshake name
    call("drawer_write", json!({"path": "/notes/n.md", "content": "one\ntwo\n"})).await?;
    let r = call("drawer_edit", json!({"path": "/notes/n.md", "old_string": "two", "new_string": "2"})).await?;
    assert_ne!(r.is_error, Some(true), "{r:?}");
    assert_eq!(text_of(&r)["revision"], 2);
    let r = call("drawer_edit", json!({"path": "/notes/n.md", "old_string": "missing", "new_string": "x"})).await?;
    assert_eq!(r.is_error, Some(true));
    let r = call("drawer_diff", json!({"path": "/notes/n.md"})).await?;
    assert!(text(r).contains("+2"));
    let recent = text_of(&call("drawer_recent", json!({"limit": 1})).await?);
    assert_eq!(recent[0]["path"], "/notes/n.md");
    let by = recent[0]["modified_by"].as_str().unwrap().to_string();
    assert!(!by.is_empty() && by != "agent", "client name should be the author, got {by}");
    let r = call("drawer_write", json!({"path": "/notes/m.md", "content": "x", "author": "named"})).await?;
    assert_ne!(r.is_error, Some(true));
    assert_eq!(text_of(&call("drawer_recent", json!({"limit": 1})).await?)[0]["modified_by"], "named");

    // reads report the revision, and page through long files
    let r = call("drawer_read", json!({"path": "/notes/n.md"})).await?;
    let content = serde_json::to_value(&r.content)?;
    assert_eq!(content[0]["text"], "one\n2\n");
    assert!(content[1]["text"].as_str().unwrap().contains("revision 2"));
    let long: String = (1..=3000).map(|i| format!("line {i}\n")).collect();
    call("drawer_write", json!({"path": "/notes/long.txt", "content": long})).await?;
    let content = serde_json::to_value(&call("drawer_read", json!({"path": "/notes/long.txt"})).await?.content)?;
    assert!(content[0]["text"].as_str().unwrap().ends_with("line 2000\n"));
    assert!(content[1]["text"].as_str().unwrap().contains("offset=2001"));
    let content = serde_json::to_value(&call("drawer_read", json!({"path": "/notes/long.txt", "offset": 2999, "limit": 5})).await?.content)?;
    assert_eq!(content[0]["text"], "line 2999\nline 3000\n");

    // a whole tree in one call
    let tree = text_of(&call("drawer_list", json!({"depth": 5})).await?);
    let paths: Vec<_> = tree.as_array().unwrap().iter().map(|e| e["path"].as_str().unwrap().to_string()).collect();
    assert!(paths.contains(&"/mem/archive/a.md".to_string()), "{paths:?}");
    let top = text_of(&call("drawer_list", json!({})).await?);
    assert!(top.as_array().unwrap().iter().all(|e| e["path"].as_str().unwrap().matches('/').count() == 1));

    client.cancel().await.ok();
    Ok(())
}

#[tokio::test]
async fn etags_recent_diff_and_edit_over_rest() {
    let dir = tempfile::tempdir().unwrap();
    let app = drawer::app(Drawer::open(dir.path()).await.unwrap(), None, None);
    let put = Request::put("/drawer/v1/files?path=/e.txt").body(Body::from("a\nb\n")).unwrap();
    send(&app, put).await;

    let r = send(&app, get("/drawer/v1/files?path=/e.txt")).await;
    let etag = r.headers()["etag"].to_str().unwrap().to_string();
    let cached = Request::get("/drawer/v1/files?path=/e.txt").header("if-none-match", &etag).body(Body::empty()).unwrap();
    assert_eq!(send(&app, cached).await.status(), StatusCode::NOT_MODIFIED);

    let r = send(&app, json_post("/drawer/v1/edit", json!({"path": "/e.txt", "old_string": "b", "new_string": "c"}))).await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(json_of(r).await["revision"], 2);
    let stale = Request::get("/drawer/v1/files?path=/e.txt").header("if-none-match", &etag).body(Body::empty()).unwrap();
    assert_eq!(send(&app, stale).await.status(), StatusCode::OK);

    let d = json_of(send(&app, get("/drawer/v1/diff?path=/e.txt")).await).await;
    assert_eq!((d["added"].as_i64(), d["removed"].as_i64()), (Some(1), Some(1)));
    let recent = json_of(send(&app, get("/drawer/v1/recent")).await).await;
    assert_eq!(recent["nodes"][0]["path"], "/e.txt");
    assert_eq!(recent["nodes"][0]["modified_by"], "api");
    let tree = json_of(send(&app, get("/drawer/v1/tree?path=/&depth=3")).await).await;
    assert_eq!(tree["truncated"], false);
}

#[tokio::test]
async fn public_download_links_are_expiring_revocable_and_download_only() {
    let dir = tempfile::tempdir().unwrap();
    let app = drawer::app(Drawer::open(dir.path()).await.unwrap(), Some("master-secret".into()), None);

    let upload = |body: &'static str| {
        Request::put("/drawer/v1/files?path=/docs/report.txt")
            .header("authorization", "Bearer master-secret")
            .body(Body::from(body))
            .unwrap()
    };
    let first = json_of(send(&app, upload("first revision")).await).await;
    let id = first["node"]["id"].as_str().unwrap();
    let folder_id = json_of(send(&app, Request::get("/drawer/v1/files/meta?path=/docs")
        .header("authorization", "Bearer master-secret").body(Body::empty()).unwrap()).await)
        .await["id"].as_str().unwrap().to_string();

    // No unauthenticated way to mint or revoke a link, or access other API routes.
    let create = |days: i64, authed: bool| {
        let mut req = Request::post(format!("/drawer/v1/nodes/{id}/links"))
            .header("content-type", "application/json");
        if authed { req = req.header("authorization", "Bearer master-secret"); }
        req.body(Body::from(json!({"expires_in_days": days}).to_string())).unwrap()
    };
    assert_eq!(send(&app, create(7, false)).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(send(&app, create(31, true)).await.status(), StatusCode::BAD_REQUEST);
    assert_eq!(send(&app, get("/drawer/v1/tree?path=/")).await.status(), StatusCode::UNAUTHORIZED);

    let made = json_of(send(&app, create(7, true)).await).await;
    let token = made["token"].as_str().unwrap().to_string();
    assert_eq!(token.len(), 64);
    let link_id = made["id"].as_str().unwrap().to_string();
    let public_path = format!("/s/{token}");
    let download = send(&app, get(&public_path)).await;
    assert_eq!(download.status(), StatusCode::OK);
    assert!(download.headers()["content-disposition"].to_str().unwrap().starts_with("attachment;"));
    assert_eq!(download.headers()["x-content-type-options"], "nosniff");
    assert_eq!(download.headers()["cache-control"], "no-store");
    assert_eq!(download.into_body().collect().await.unwrap().to_bytes(), "first revision");

    let fake = format!("/s/{}", "a".repeat(64));
    assert_eq!(send(&app, get(&fake)).await.status(), StatusCode::NOT_FOUND);
    assert_eq!(send(&app, Request::post(&public_path).body(Body::empty()).unwrap()).await.status(), StatusCode::UNAUTHORIZED);

    // A link follows the latest file version, while its opaque token never appears in listings.
    assert_eq!(send(&app, upload("updated revision")).await.status(), StatusCode::OK);
    assert_eq!(
        send(&app, get(&public_path)).await.into_body().collect().await.unwrap().to_bytes(),
        "updated revision"
    );
    let listed = json_of(send(&app, Request::get(format!("/drawer/v1/nodes/{id}/links"))
        .header("authorization", "Bearer master-secret").body(Body::empty()).unwrap()).await).await;
    assert_eq!(listed["links"].as_array().unwrap().len(), 1);
    assert!(listed["links"][0].get("token").is_none());

    // Trashing a parent folder disables the link; restoring it re-enables access.
    let folder_path = format!("/drawer/v1/nodes/{folder_id}");
    let change_folder = |trashed: bool| Request::patch(&folder_path)
        .header("authorization", "Bearer master-secret")
        .header("content-type", "application/json")
        .body(Body::from(json!({"trashed":trashed}).to_string())).unwrap();
    assert_eq!(send(&app, change_folder(true)).await.status(), StatusCode::OK);
    assert_eq!(send(&app, get(&public_path)).await.status(), StatusCode::NOT_FOUND);
    assert_eq!(send(&app, change_folder(false)).await.status(), StatusCode::OK);
    assert_eq!(send(&app, get(&public_path)).await.status(), StatusCode::OK);

    // Revocation takes effect immediately.
    let revoke = Request::delete(format!("/drawer/v1/nodes/{id}/links/{link_id}"))
        .header("authorization", "Bearer master-secret").body(Body::empty()).unwrap();
    assert_eq!(send(&app, revoke).await.status(), StatusCode::OK);
    assert_eq!(send(&app, get(&public_path)).await.status(), StatusCode::NOT_FOUND);

    // Expiration is enforced from the database on every request.
    let another = json_of(send(&app, create(1, true)).await).await;
    let second_token = another["token"].as_str().unwrap().to_string();
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new().filename(dir.path().join("drawer.db")),
    ).await.unwrap();
    sqlx::query("UPDATE download_links SET expires_at = '2000-01-01T00:00:00.000Z' WHERE id = ?")
        .bind(another["id"].as_str().unwrap()).execute(&pool).await.unwrap();
    assert_eq!(send(&app, get(&format!("/s/{second_token}"))).await.status(), StatusCode::NOT_FOUND);
    pool.close().await;
}

#[tokio::test]
async fn password_session_authenticates_browser_and_prevents_author_spoofing() {
    let dir = tempfile::tempdir().unwrap();
    let app = drawer::app_with_auth(
        Drawer::open(dir.path()).await.unwrap(),
        Some("machine-secret".into()),
        None, Some("strong-admin-password".into()), false,
    );
    assert_eq!(send(&app, get("/drawer/v1/tree?path=/")).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(send(&app, get("/auth/me")).await.status(), StatusCode::OK);
    let denied = send(&app, json_post("/auth/login", json!({"password":"wrong"}))).await;
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

    let login = send(&app, json_post("/auth/login", json!({"password":"strong-admin-password"}))).await;
    assert_eq!(login.status(), StatusCode::OK);
    let set_cookie = login.headers()["set-cookie"].to_str().unwrap().to_string();
    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("SameSite=Strict"));
    let cookie = set_cookie.split(';').next().unwrap().to_string();

    let put = Request::put("/drawer/v1/files?path=/private.txt")
        .header("cookie", &cookie)
        .header("x-drawer-author", "someone-else")
        .body(Body::from("secret")).unwrap();
    let saved = json_of(send(&app, put).await).await;
    assert_eq!(saved["node"]["owner"], "admin");
    assert_eq!(saved["node"]["modified_by"], "admin");
    assert_eq!(send(&app, get("/drawer/v1/files?path=/private.txt")).await.status(), StatusCode::UNAUTHORIZED);

    let authed = Request::get("/drawer/v1/files?path=/private.txt")
        .header("cookie", &cookie).body(Body::empty()).unwrap();
    assert_eq!(send(&app, authed).await.status(), StatusCode::OK);
    // JSON author fields must follow the verified identity just like headers do.
    for (endpoint, body) in [
        ("edit", json!({"path":"/private.txt", "old_string":"secret", "new_string":"edited", "author":"impostor"})),
        ("rollback", json!({"path":"/private.txt", "revision":1, "author":"impostor"})),
        ("copy", json!({"from":"/private.txt", "to":"/copied.txt", "author":"impostor"})),
    ] {
        let request = Request::post(format!("/drawer/v1/{endpoint}"))
            .header("cookie", &cookie).header("content-type", "application/json")
            .body(Body::from(body.to_string())).unwrap();
        let response = send(&app, request).await;
        assert_eq!(response.status(), StatusCode::OK, "{endpoint}");
        let saved = json_of(response).await;
        assert_eq!(saved["node"]["modified_by"], "admin", "{endpoint}");
    }
    let logout = Request::post("/auth/logout").header("cookie", &cookie).body(Body::empty()).unwrap();
    assert_eq!(send(&app, logout).await.status(), StatusCode::NO_CONTENT);
    let expired = Request::get("/drawer/v1/tree?path=/").header("cookie", cookie).body(Body::empty()).unwrap();
    assert_eq!(send(&app, expired).await.status(), StatusCode::UNAUTHORIZED);

    let token_user = Request::get("/drawer/v1/tree?path=/")
        .header("authorization", "Bearer machine-secret").body(Body::empty()).unwrap();
    assert_eq!(send(&app, token_user).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn resumable_upload_rejects_bad_offsets_and_assembles_file_without_losing_history() {
    let dir = tempfile::tempdir().unwrap();
    let app = drawer::app_with_auth(
        Drawer::open(dir.path()).await.unwrap(),
        Some("machine-secret".into()), None, Some("admin-password".into()), false,
    );
    let req = |method: &str, path: &str, body: Body| {
        Request::builder().method(method).uri(path)
            .header("authorization", "Bearer machine-secret")
            .header("content-type", "application/json")
            .body(body).unwrap()
    };
    let created = json_of(send(&app, req("POST", "/drawer/v1/transfers",
        Body::from(json!({"path":"/work/report.bin", "size":10}).to_string()))).await).await;
    let id = created["id"].as_str().unwrap();
    let path = format!("/drawer/v1/transfers/{id}");
    let chunk = |offset: i64, bytes: &'static str| {
        Request::put(&path).header("authorization", "Bearer machine-secret")
            .header("x-upload-offset", offset.to_string()).body(Body::from(bytes)).unwrap()
    };
    assert_eq!(send(&app, chunk(2, "hello")).await.status(), StatusCode::PRECONDITION_FAILED);
    let first = json_of(send(&app, chunk(0, "hello")).await).await;
    assert_eq!(first["received"], 5);
    // Simulate restart by building another app against the same on-disk store.
    let restarted = drawer::app_with_auth(
        Drawer::open(dir.path()).await.unwrap(),
        Some("machine-secret".into()), None, Some("admin-password".into()), false,
    );
    let resumed = json_of(send(&restarted, req("GET", &path, Body::empty())).await).await;
    assert_eq!(resumed["received"], 5);
    assert_eq!(send(&restarted, chunk(5, "world")).await.status(), StatusCode::OK);
    let saved = json_of(send(&restarted, req("POST", &format!("{path}/complete"), Body::empty())).await).await;
    assert_eq!(saved["revision"], 1);
    assert_eq!(saved["node"]["owner"], "admin");

    let download = send(&restarted, req("GET", "/drawer/v1/files?path=/work/report.bin", Body::empty())).await;
    assert_eq!(download.into_body().collect().await.unwrap().to_bytes(), "helloworld");
    let partial = Request::get("/drawer/v1/files?path=/work/report.bin")
        .header("authorization", "Bearer machine-secret")
        .header("range", "bytes=2-5").body(Body::empty()).unwrap();
    let range_result = send(&restarted, partial).await;
    assert_eq!(range_result.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(range_result.headers()["content-range"], "bytes 2-5/10");
    assert_eq!(range_result.into_body().collect().await.unwrap().to_bytes(), "llow");
    let invalid_range = Request::get("/drawer/v1/files?path=/work/report.bin")
        .header("authorization", "Bearer machine-secret")
        .header("range", "bytes=500-900").body(Body::empty()).unwrap();
    assert_eq!(send(&restarted, invalid_range).await.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    let second = json_of(send(&restarted, req("POST", "/drawer/v1/transfers",
        Body::from(json!({"path":"/work/report.bin","size":3,"sha256":"0".repeat(64)}).to_string()))).await).await;
    let second_path = format!("/drawer/v1/transfers/{}", second["id"].as_str().unwrap());
    let chunk2 = Request::put(&second_path).header("authorization", "Bearer machine-secret")
        .header("x-upload-offset", "0").body(Body::from("new")).unwrap();
    assert_eq!(send(&restarted, chunk2).await.status(), StatusCode::OK);
    assert_eq!(send(&restarted, req("POST", &format!("{second_path}/complete"), Body::empty())).await.status(), StatusCode::BAD_REQUEST);
    assert_eq!(send(&restarted, req("DELETE", &second_path, Body::empty())).await.status(), StatusCode::OK);
    assert_eq!(send(&restarted, req("GET", &second_path, Body::empty())).await.status(), StatusCode::NOT_FOUND);
    let history = json_of(send(&restarted, req("GET", "/drawer/v1/revisions?path=/work/report.bin", Body::empty())).await).await;
    assert_eq!(history["revisions"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn download_link_follows_identity_when_its_old_path_is_reused() {
    let dir = tempfile::tempdir().unwrap();
    let svc = Drawer::open(dir.path()).await.unwrap();
    let saved = svc.write("/shared.txt", b"shared", drawer::service::WriteOpts::new("owner", "")).await.unwrap();
    let link = svc.create_download_link(&saved.node.id, "owner", 7).await.unwrap();
    svc.move_to(drawer::service::Ref::Id(&saved.node.id), "/moved.txt", "owner").await.unwrap();
    svc.write("/shared.txt", b"private replacement", drawer::service::WriteOpts::new("owner", "")).await.unwrap();
    let (name, _, bytes) = svc.download_link_file(&link.token).await.unwrap();
    assert_eq!(name, "moved.txt");
    assert_eq!(tokio::fs::read(bytes).await.unwrap(), b"shared");
}

#[tokio::test]
async fn transfer_chunk_limit_is_enforced_before_buffering_the_body() {
    let dir = tempfile::tempdir().unwrap();
    let app = drawer::app(Drawer::open(dir.path()).await.unwrap(), None, None);
    let request = Request::put("/drawer/v1/transfers/00000000-0000-4000-8000-000000000000")
        .header("x-upload-offset", "0")
        .body(Body::from(vec![0; 8 * 1024 * 1024 + 1])).unwrap();
    assert_eq!(send(&app, request).await.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn password_mode_mcp_uses_verified_actor() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let svc = Drawer::open(dir.path()).await?;
    let app = drawer::app_with_auth(svc.clone(), Some("machine".into()), None, Some("password".into()), false);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(format!("http://{addr}/mcp")).auth_header("machine"),
    );
    let client = ClientConfig::default().serve(transport).await?;
    let result = client.call_tool(CallToolRequestParams::new("drawer_write").with_arguments(
        json!({"path":"/mcp.txt", "content":"hello", "author":"impostor"}).as_object().unwrap().clone(),
    )).await?;
    assert_ne!(result.is_error, Some(true));
    let node = svc.stat(drawer::service::Ref::Path("/mcp.txt")).await?;
    assert_eq!(node.owner, "admin");
    assert_eq!(node.modified_by.as_deref(), Some("admin"));
    client.cancel().await?;
    server.abort();
    Ok(())
}

#[tokio::test]
async fn updater_requires_password_authentication() {
    let dir = tempfile::tempdir().unwrap();
    let app = drawer::app_with_auth(Drawer::open(dir.path()).await.unwrap(), None, None, Some("password".into()), false);
    assert_eq!(send(&app, get("/drawer/v1/update")).await.status(), StatusCode::UNAUTHORIZED);
    let install = Request::post("/drawer/v1/update").body(Body::empty()).unwrap();
    assert_eq!(send(&app, install).await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn transfer_manager_tracks_pause_resume_completion_and_owner() {
    let dir = tempfile::tempdir().unwrap();
    let app = drawer::app(Drawer::open(dir.path()).await.unwrap(), None, None);
    let transfer = json_of(send(&app, json_post("/drawer/v1/transfers", json!({"path":"/tracked.txt","size":3}))).await).await;
    let path = format!("/drawer/v1/transfers/{}", transfer["id"].as_str().unwrap());
    let paused = Request::patch(&path).header("content-type", "application/json").body(Body::from(r#"{"status":"paused"}"#)).unwrap();
    assert_eq!(send(&app, paused).await.status(), StatusCode::OK);
    let chunk = || Request::put(&path).header("x-upload-offset","0").body(Body::from("abc")).unwrap();
    assert_eq!(send(&app, chunk()).await.status(), StatusCode::CONFLICT);
    let resumed = Request::patch(&path).header("content-type", "application/json").body(Body::from(r#"{"status":"active"}"#)).unwrap();
    assert_eq!(send(&app, resumed).await.status(), StatusCode::OK);
    assert_eq!(send(&app, chunk()).await.status(), StatusCode::OK);
    assert_eq!(send(&app, Request::post(format!("{path}/complete")).body(Body::empty()).unwrap()).await.status(), StatusCode::OK);
    let history = json_of(send(&app, get("/drawer/v1/transfers")).await).await;
    assert_eq!(history["transfers"][0]["status"], "completed");
    assert_eq!(history["transfers"][0]["received"], 3);
    let other = Request::get("/drawer/v1/transfers").header("x-drawer-author", "other").body(Body::empty()).unwrap();
    assert_eq!(json_of(send(&app, other).await).await["transfers"].as_array().unwrap().len(),0);
}

#[tokio::test]
async fn zip_selection_streams_nested_files_and_empty_folders() {
    let dir = tempfile::tempdir().unwrap();
    let s = Drawer::open(dir.path()).await.unwrap();
    let opts = drawer::service::WriteOpts::new("api", "");
    let file = s.write("/folder/nested/a.txt", b"zip content", opts).await.unwrap();
    s.mkdir("/folder/empty", "api").await.unwrap();
    let folder = s.stat(drawer::service::Ref::Path("/folder")).await.unwrap();
    let app = drawer::app(s, None, None);
    let response = send(&app, get(&format!("/drawer/v1/archive?ids={},{}", folder.id, file.node.id))).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "application/zip");
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let zip = async_zip::base::read::mem::ZipFileReader::new(bytes.to_vec()).await.unwrap();
    let names: Vec<_> = zip.file().entries().iter().map(|e| e.filename().as_str().unwrap().to_owned()).collect();
    assert!(names.contains(&"folder/empty/".to_owned()));
    assert_eq!(names.iter().filter(|n| *n == "folder/nested/a.txt").count(), 1);
    let index = names.iter().position(|n| n == "folder/nested/a.txt").unwrap();
    let mut entry = zip.reader_with_entry(index).await.unwrap();
    let mut content = Vec::new();
    use futures_lite::io::AsyncReadExt;
    entry.read_to_end(&mut content).await.unwrap();
    assert_eq!(content, b"zip content");
}

#[tokio::test]
async fn storage_distinguishes_deduplication_history_and_staging() {
    let dir = tempfile::tempdir().unwrap();
    let s = Drawer::open(dir.path()).await.unwrap();
    s.write("/a", b"same", drawer::service::WriteOpts::new("api", "")).await.unwrap();
    s.write("/b", b"same", drawer::service::WriteOpts::new("api", "")).await.unwrap();
    s.write("/a", b"new", drawer::service::WriteOpts::new("api", "")).await.unwrap();
    tokio::fs::create_dir_all(dir.path().join("transfers")).await.unwrap();
    tokio::fs::write(dir.path().join("transfers/staged.part"), b"12345").await.unwrap();
    let app = drawer::app(s, None, None);
    let stats = json_of(send(&app, get("/drawer/v1/storage")).await).await;
    assert_eq!(stats["live_files"], 2);
    assert_eq!(stats["live_bytes"], 7);
    assert_eq!(stats["revision_bytes"], 11);
    assert_eq!(stats["blob_bytes"], 7);
    assert_eq!(stats["staging_bytes"], 5);
    assert_eq!(stats["unreferenced_blob_bytes"], 0);
    assert!(stats["filesystem_available_bytes"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn host_mount_browse_import_and_containment() {
    let dir = tempfile::tempdir().unwrap();
    let host = tempfile::tempdir().unwrap();
    std::fs::create_dir(host.path().join("nested")).unwrap();
    std::fs::write(host.path().join("nested/example.txt"), b"host contents").unwrap();
    let s = Drawer::open(dir.path()).await.unwrap();
    let mount = s.register_mount("Documents", host.path()).await.unwrap();
    #[cfg(unix)] {
        std::os::unix::fs::symlink(dir.path(), host.path().join("escape")).unwrap();
    }
    let app = drawer::app(s, None, None);
    let mounts = json_of(send(&app, get("/drawer/v1/mounts")).await).await;
    assert_eq!(mounts[0]["name"], "Documents");
    assert!(mounts[0].get("root").is_none());
    let base = format!("/drawer/v1/mounts/{}", mount.id);
    let entries = json_of(send(&app, get(&format!("{base}/entries?path=nested"))).await).await;
    assert_eq!(entries[0]["name"], "example.txt");
    for path in ["..", "%2Fetc", "nested%2F..%2F.."] {
        assert_eq!(send(&app, get(&format!("{base}/entries?path={path}"))).await.status(), StatusCode::BAD_REQUEST);
    }
    #[cfg(unix)] assert_eq!(send(&app, get(&format!("{base}/entries?path=escape"))).await.status(), StatusCode::BAD_REQUEST);
    let import = || json_post(&format!("{base}/import"), json!({"path":"nested/example.txt","destination":"/imported.txt"}));
    assert_eq!(send(&app, import()).await.status(), StatusCode::OK);
    assert_eq!(send(&app, import()).await.status(), StatusCode::PRECONDITION_FAILED);
    let imported = send(&app, get("/drawer/v1/files?path=/imported.txt")).await.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(imported, b"host contents".as_slice());
    assert_eq!(std::fs::read(host.path().join("nested/example.txt")).unwrap(), b"host contents");
    let protected = drawer::app_with_auth(Drawer::open(dir.path()).await.unwrap(), None, None, Some("password".into()), false);
    assert_eq!(send(&protected, get("/drawer/v1/mounts")).await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn archive_rejects_names_that_windows_extractors_treat_as_paths() {
    let dir = tempfile::tempdir().unwrap();
    let s = Drawer::open(dir.path()).await.unwrap();
    for path in ["/..\\escape.txt", "/C:drive.txt", "/.. /escape.txt"] {
        let file = s.write(path, b"safe contents", drawer::service::WriteOpts::new("api", "")).await.unwrap();
        let app = drawer::app(s.clone(), None, None);
        let response = send(&app, get(&format!("/drawer/v1/archive?ids={}", file.node.id))).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
    }
}
