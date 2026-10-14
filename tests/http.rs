use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use drawer::service::Drawer;
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
    let r = app.clone().oneshot(put("v1")).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(json_of(r).await["generation"], 1);
    app.clone().oneshot(put("v2")).await.unwrap();

    let r = app.clone().oneshot(Request::get("/drawer/v1/files?path=/x/y.txt&generation=1").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(r.headers()["x-drawer-generation"], "1");
    assert_eq!(r.into_body().collect().await.unwrap().to_bytes(), "v1");

    let r = app.clone().oneshot(Request::get("/drawer/v1/tree?path=/x").body(Body::empty()).unwrap()).await.unwrap();
    let tree = json_of(r).await;
    assert_eq!(tree["entries"][0]["name"], "y.txt");

    let r = app.clone().oneshot(
        Request::post("/drawer/v1/rollback")
            .header("content-type", "application/json")
            .body(Body::from(json!({"path": "/x/y.txt", "generation": 1}).to_string()))
            .unwrap(),
    ).await.unwrap();
    assert_eq!(json_of(r).await["generation"], 3);

    let r = app.clone().oneshot(Request::get("/drawer/v1/generations?path=/x/y.txt").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(json_of(r).await["generations"].as_array().unwrap().len(), 3);

    let r = app.clone().oneshot(Request::get("/drawer/v1/search?q=v1").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(json_of(r).await["hits"][0]["path"], "/x/y.txt");

    let r = app.clone().oneshot(Request::get("/drawer/v1/files?path=/nope").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    let r = app.oneshot(Request::get("/drawer/v1/files?path=/a/../b").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn bearer_token_is_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let app = drawer::app(Drawer::open(dir.path()).await.unwrap(), Some("s3cret".into()), None);
    let r = app.clone().oneshot(Request::get("/drawer/v1/health").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let r = app.oneshot(Request::get("/drawer/v1/health").header("authorization", "Bearer s3cret").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
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
    for n in ["drawer_list", "drawer_read", "drawer_write", "drawer_rollback", "drawer_copy", "drawer_search"] {
        assert!(names.iter().any(|x| x == n), "missing tool {n}: {names:?}");
    }

    let call = |name: &'static str, args: Value| {
        let client = &client;
        async move {
            let args = args.as_object().cloned().unwrap();
            client.call_tool(CallToolRequestParams::new(name).with_arguments(args)).await
        }
    };
    let text = |r: rmcp::model::CallToolResult| format!("{:?}", r.content);

    let r = call("drawer_write", json!({"path": "/mem/a.md", "content": "alpha", "comment": "c"})).await?;
    assert_ne!(r.is_error, Some(true));
    call("drawer_write", json!({"path": "/mem/a.md", "content": "beta"})).await?;
    let r = call("drawer_read", json!({"path": "/mem/a.md", "generation": 1})).await?;
    assert!(text(r).contains("alpha"));
    let r = call("drawer_rollback", json!({"path": "/mem/a.md", "generation": 1})).await?;
    assert_ne!(r.is_error, Some(true));
    let r = call("drawer_read", json!({"path": "/mem/a.md"})).await?;
    assert!(text(r).contains("alpha"));
    let r = call("drawer_read", json!({"path": "/missing"})).await?;
    assert_eq!(r.is_error, Some(true));

    client.cancel().await.ok();
    Ok(())
}

#[tokio::test]
async fn preconditions_metadata_and_copy_over_rest() {
    let dir = tempfile::tempdir().unwrap();
    let app = drawer::app(Drawer::open(dir.path()).await.unwrap(), None, None);

    // create-only: first PUT with if_generation_match=0 succeeds, the second is refused with 412
    let create_only = |body: &'static str| {
        Request::put("/drawer/v1/files?path=/g/a.txt&if_generation_match=0")
            .header("x-drawer-meta-source", "agent-1")
            .body(Body::from(body))
            .unwrap()
    };
    let r = app.clone().oneshot(create_only("one")).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let r = app.clone().oneshot(create_only("two")).await.unwrap();
    assert_eq!(r.status(), StatusCode::PRECONDITION_FAILED);

    // metadata comes back as x-drawer-meta-* headers on read
    let r = app.clone().oneshot(Request::get("/drawer/v1/files?path=/g/a.txt").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(r.headers()["x-drawer-meta-source"], "agent-1");

    // stale generation is refused on PUT and DELETE
    let r = app.clone().oneshot(
        Request::put("/drawer/v1/files?path=/g/a.txt&if_generation_match=7").body(Body::from("nope")).unwrap(),
    ).await.unwrap();
    assert_eq!(r.status(), StatusCode::PRECONDITION_FAILED);
    let r = app.clone().oneshot(Request::delete("/drawer/v1/files?path=/g/a.txt&if_generation_match=7").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(r.status(), StatusCode::PRECONDITION_FAILED);

    // copy to a new path keeps the content and metadata
    let r = app.clone().oneshot(
        Request::post("/drawer/v1/copy")
            .header("content-type", "application/json")
            .body(Body::from(json!({"from": "/g/a.txt", "to": "/g/b.txt"}).to_string()))
            .unwrap(),
    ).await.unwrap();
    let copied = json_of(r).await;
    assert_eq!(copied["new_generation"], true);
    assert_eq!(copied["metadata"]["source"], "agent-1");
    let r = app.oneshot(Request::get("/drawer/v1/files?path=/g/b.txt").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(r.into_body().collect().await.unwrap().to_bytes(), "one");
}
