//! Drawer: versioned filesystem storage for agents, over REST and MCP.
pub mod api;
pub mod blob;
pub mod error;
pub mod mcp;
pub mod service;

use std::{path::PathBuf, sync::Arc};

use axum::{
    Router,
    extract::Request,
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use tower_http::{cors::CorsLayer, services::{ServeDir, ServeFile}};

use service::Drawer;

/// Build the full HTTP app: REST at /drawer/v1, MCP at /mcp, and the UI if `ui_dir` exists.
pub fn app(svc: Drawer, token: Option<String>, ui_dir: Option<PathBuf>) -> Router {
    let mcp_svc = svc.clone();
    let mcp = StreamableHttpService::new(
        move || Ok(mcp::DrawerMcp::new(mcp_svc.clone())),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let mut app = Router::new()
        .nest("/drawer/v1", api::router().with_state(svc))
        .nest_service("/mcp", mcp);
    if let Some(token) = token {
        app = app.layer(middleware::from_fn(move |req: Request, next: Next| {
            let token = token.clone();
            async move { check_token(&token, req, next).await }
        }));
    }
    if let Some(dir) = ui_dir.filter(|d| d.join("index.html").exists()) {
        let index = dir.join("index.html");
        app = app.fallback_service(ServeDir::new(dir).fallback(ServeFile::new(index)));
    }
    app.layer(CorsLayer::permissive())
}

async fn check_token(token: &str, req: Request, next: Next) -> Response {
    let ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| t == token);
    if ok { next.run(req).await } else { StatusCode::UNAUTHORIZED.into_response() }
}
