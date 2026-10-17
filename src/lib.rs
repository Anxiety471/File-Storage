//! Drawer: versioned filesystem storage for agents, over REST and MCP.
pub mod api;
pub mod auth;
pub mod blob;
pub mod cli;
pub mod background;
pub mod error;
pub mod links;
pub mod transfers;
pub mod mcp;
pub mod service;
pub mod update;

use std::{path::PathBuf, sync::Arc};

use axum::{
    Router,
    extract::{DefaultBodyLimit, Request},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use tower_http::{cors::CorsLayer, services::{ServeDir, ServeFile}};

use service::Drawer;

/// Largest request body accepted, so uploads of PDFs, Office files and media fit.
pub const MAX_UPLOAD_BYTES: usize = 512 * 1024 * 1024;

/// Build the full HTTP app: REST at /drawer/v1, MCP at /mcp, and the UI if `ui_dir` exists.
pub fn app(svc: Drawer, token: Option<String>, ui_dir: Option<PathBuf>) -> Router {
    app_with_auth(svc, token, ui_dir, None, false)
}

/// Password-authenticated browser sessions. A configured API token remains available for MCP/CLI.
/// With password auth enabled, request-controlled author headers can no longer impersonate actors.
pub fn app_with_auth(
    svc: Drawer,
    token: Option<String>,
    ui_dir: Option<PathBuf>,
    admin_password: Option<String>,
    secure_cookie: bool,
) -> Router {
    let password_mode = admin_password.as_ref().is_some_and(|s| !s.is_empty());
    let mcp_svc = svc.clone();
    let mcp = StreamableHttpService::new(
        move || Ok(mcp::DrawerMcp::new(mcp_svc.clone()).with_verified_admin(password_mode)),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    // Top level, ahead of the UI fallback, so `/health` is never answered with index.html.
    let mut app = Router::new()
        .route("/health", axum::routing::get(api::health))
        .route("/s/{token}", axum::routing::get(api::download_shared).with_state(svc.clone()))
        .nest("/drawer/v1", api::router().with_state(svc).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)))
        .nest_service("/mcp", mcp)
        .layer(middleware::from_fn(session_delete_ok));
    if let Some(password) = admin_password.filter(|s| !s.is_empty()) {
        let gate = auth::AuthGate::new(password, token, secure_cookie);
        app = app.merge(
            Router::new()
                .route("/auth/login", axum::routing::post(auth::login))
                .route("/auth/logout", axum::routing::post(auth::logout))
                .route("/auth/me", axum::routing::get(auth::me))
                .with_state(gate.clone()),
        );
        app = app.layer(middleware::from_fn(move |mut req: Request, next: Next| {
            let gate = gate.clone();
            async move {
                let path = req.uri().path();
                let restricted = path.starts_with("/drawer/v1") || path.starts_with("/mcp");
                if restricted {
                    if !gate.authenticated(req.headers()).await {
                        return StatusCode::UNAUTHORIZED.into_response();
                    }
                    // Only a verified admin session or machine token can supply an author identity.
                    req.headers_mut().insert("x-drawer-author", "admin".parse().unwrap());
                    req.extensions_mut().insert(auth::VerifiedAdmin);
                }
                next.run(req).await
            }
        }));
    } else if let Some(token) = token {
        app = app.layer(middleware::from_fn(move |req: Request, next: Next| {
            let token = token.clone();
            async move { check_token(&token, req, next).await }
        }));
    }
    if let Some(dir) = ui_dir.filter(|d| d.join("index.html").exists()) {
        let index = dir.join("index.html");
        app = app.fallback_service(ServeDir::new(dir).fallback(ServeFile::new(index)));
    }
    // Cookie auth must never be combined with permissive cross-origin access.
    if password_mode { app } else { app.layer(CorsLayer::permissive()) }
}

/// rmcp answers a client's session-closing `DELETE /mcp` with 202, which the Python MCP SDK
/// (used by Hermes and many other harnesses) logs as a failed termination. The session is
/// closed either way, so report it as 200.
async fn session_delete_ok(req: Request, next: Next) -> Response {
    let closing = req.method() == axum::http::Method::DELETE && req.uri().path() == "/mcp";
    let mut resp = next.run(req).await;
    if closing && resp.status() == StatusCode::ACCEPTED {
        *resp.status_mut() = StatusCode::OK;
    }
    resp
}

async fn check_token(token: &str, req: Request, next: Next) -> Response {
    // Public shares bypass the instance-wide bearer token, but only this one GET route.
    let public_link = req.method() == axum::http::Method::GET
        && req.uri().path().strip_prefix("/s/").is_some_and(|token| {
            token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit())
        });
    if public_link {
        return next.run(req).await;
    }
    let ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| t == token);
    if ok { next.run(req).await } else { StatusCode::UNAUTHORIZED.into_response() }
}
