//! REST API under `/drawer/v1`.
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Query, State},
    http::{HeaderMap, HeaderName, HeaderValue, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::service::{Drawer, Metadata, WriteOpts};

/// Custom metadata travels as `x-drawer-meta-<key>` headers, like GCS's `x-goog-meta-*`.
const META_PREFIX: &str = "x-drawer-meta-";

#[derive(Deserialize)]
struct PathQ {
    path: Option<String>,
    generation: Option<i64>,
    if_generation_match: Option<i64>,
}

#[derive(Deserialize)]
struct PathBody {
    path: String,
}

#[derive(Deserialize)]
struct RollbackBody {
    path: String,
    generation: i64,
    author: Option<String>,
    comment: Option<String>,
    if_generation_match: Option<i64>,
}

#[derive(Deserialize)]
struct CopyBody {
    from: String,
    to: String,
    source_generation: Option<i64>,
    author: Option<String>,
    comment: Option<String>,
    if_generation_match: Option<i64>,
}

#[derive(Deserialize)]
struct SearchQ {
    q: String,
    limit: Option<usize>,
}

pub fn router() -> Router<Drawer> {
    Router::new()
        .route("/health", get(|| async { Json(json!({ "ok": true })) }))
        .route("/tree", get(tree))
        .route("/folders", post(mkdir).delete(rmdir))
        .route("/files", get(read_file).put(write_file).delete(delete_file))
        .route("/files/meta", get(meta))
        .route("/generations", get(generations))
        .route("/rollback", post(rollback))
        .route("/copy", post(copy))
        .route("/search", get(search))
}

fn required(p: &PathQ) -> Result<&str> {
    p.path
        .as_deref()
        .ok_or_else(|| Error::Invalid("missing ?path=".into()))
}

fn header_str(h: &HeaderMap, name: &str, default: &str) -> String {
    h.get(name).and_then(|v| v.to_str().ok()).unwrap_or(default).to_string()
}

/// Author, comment, `x-drawer-meta-*` headers and the precondition become write options.
fn write_opts(h: &HeaderMap, if_generation_match: Option<i64>) -> Result<WriteOpts> {
    let mut metadata = Metadata::new();
    for (name, value) in h.iter() {
        if let Some(key) = name.as_str().strip_prefix(META_PREFIX) {
            let value = value.to_str().map_err(|_| Error::Invalid(format!("{name} must be ASCII")))?;
            // Header names are case-insensitive, so keys are stored lowercase.
            metadata.insert(key.to_ascii_lowercase(), value.to_string());
        }
    }
    Ok(WriteOpts {
        author: header_str(h, "x-drawer-author", "api"),
        comment: header_str(h, "x-drawer-comment", ""),
        metadata,
        if_generation_match,
    })
}

async fn tree(State(s): State<Drawer>, Query(q): Query<PathQ>) -> Result<Json<Value>> {
    let path = q.path.as_deref().unwrap_or("/");
    Ok(Json(json!({ "path": crate::service::normalize(path)?, "entries": s.list(path).await? })))
}

async fn mkdir(State(s): State<Drawer>, Json(b): Json<PathBody>) -> Result<Json<Value>> {
    Ok(Json(json!({ "path": s.mkdir(&b.path).await? })))
}

async fn rmdir(State(s): State<Drawer>, Query(q): Query<PathQ>) -> Result<Json<Value>> {
    s.rmdir(required(&q)?).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn read_file(State(s): State<Drawer>, Query(q): Query<PathQ>) -> Result<Response> {
    let (info, data) = s.read(required(&q)?, q.generation).await?;
    let mut resp = data.into_response();
    let h = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&info.mime) {
        h.insert(header::CONTENT_TYPE, v);
    }
    h.insert("x-drawer-generation", HeaderValue::from(info.generation));
    h.insert("x-drawer-hash", HeaderValue::from_str(&info.hash).expect("hex is ascii"));
    for (k, v) in &info.metadata {
        let name = HeaderName::from_bytes(format!("{META_PREFIX}{k}").as_bytes());
        let value = HeaderValue::from_str(v);
        if let (Ok(name), Ok(value)) = (name, value) {
            h.insert(name, value);
        }
    }
    Ok(resp)
}

async fn write_file(
    State(s): State<Drawer>,
    Query(q): Query<PathQ>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>> {
    let opts = write_opts(&headers, q.if_generation_match)?;
    let out = s.write(required(&q)?, &body, opts).await?;
    Ok(Json(serde_json::to_value(out).expect("serializable")))
}

async fn delete_file(State(s): State<Drawer>, Query(q): Query<PathQ>) -> Result<Json<Value>> {
    s.delete_file(required(&q)?, q.if_generation_match).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn meta(State(s): State<Drawer>, Query(q): Query<PathQ>) -> Result<Json<Value>> {
    Ok(Json(serde_json::to_value(s.stat(required(&q)?, q.generation).await?).expect("serializable")))
}

async fn generations(State(s): State<Drawer>, Query(q): Query<PathQ>) -> Result<Json<Value>> {
    Ok(Json(json!({ "generations": s.generations(required(&q)?).await? })))
}

async fn rollback(State(s): State<Drawer>, Json(b): Json<RollbackBody>) -> Result<Json<Value>> {
    let mut opts = WriteOpts::new(
        b.author.unwrap_or_else(|| "api".into()),
        b.comment.unwrap_or_default(),
    );
    opts.if_generation_match = b.if_generation_match;
    let out = s.rollback(&b.path, b.generation, opts).await?;
    Ok(Json(serde_json::to_value(out).expect("serializable")))
}

async fn copy(State(s): State<Drawer>, Json(b): Json<CopyBody>) -> Result<Json<Value>> {
    let mut opts = WriteOpts::new(
        b.author.unwrap_or_else(|| "api".into()),
        b.comment.unwrap_or_default(),
    );
    opts.if_generation_match = b.if_generation_match;
    let out = s.copy(&b.from, &b.to, b.source_generation, opts).await?;
    Ok(Json(serde_json::to_value(out).expect("serializable")))
}

async fn search(State(s): State<Drawer>, Query(q): Query<SearchQ>) -> Result<Json<Value>> {
    Ok(Json(json!({ "hits": s.search(&q.q, q.limit.unwrap_or(50)).await? })))
}
