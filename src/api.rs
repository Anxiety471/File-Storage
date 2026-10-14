//! REST API under `/mnemo/v1`.
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::Result;
use crate::service::Mnemo;

#[derive(Deserialize)]
struct PathQ {
    path: Option<String>,
    version: Option<i64>,
}

#[derive(Deserialize)]
struct PathBody {
    path: String,
}

#[derive(Deserialize)]
struct RollbackBody {
    path: String,
    version: i64,
    author: Option<String>,
}

#[derive(Deserialize)]
struct SearchQ {
    q: String,
    limit: Option<usize>,
}

pub fn router() -> Router<Mnemo> {
    Router::new()
        .route("/health", get(|| async { Json(json!({ "ok": true })) }))
        .route("/tree", get(tree))
        .route("/folders", post(mkdir).delete(rmdir))
        .route("/files", get(read_file).put(write_file).delete(delete_file))
        .route("/files/meta", get(meta))
        .route("/versions", get(versions))
        .route("/rollback", post(rollback))
        .route("/search", get(search))
}

fn required(p: &PathQ) -> Result<&str> {
    p.path
        .as_deref()
        .ok_or_else(|| crate::error::Error::Invalid("missing ?path=".into()))
}

fn header_str(h: &HeaderMap, name: &str, default: &str) -> String {
    h.get(name).and_then(|v| v.to_str().ok()).unwrap_or(default).to_string()
}

async fn tree(State(s): State<Mnemo>, Query(q): Query<PathQ>) -> Result<Json<Value>> {
    let path = q.path.as_deref().unwrap_or("/");
    Ok(Json(json!({ "path": crate::service::normalize(path)?, "entries": s.list(path).await? })))
}

async fn mkdir(State(s): State<Mnemo>, Json(b): Json<PathBody>) -> Result<Json<Value>> {
    Ok(Json(json!({ "path": s.mkdir(&b.path).await? })))
}

async fn rmdir(State(s): State<Mnemo>, Query(q): Query<PathQ>) -> Result<Json<Value>> {
    s.rmdir(required(&q)?).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn read_file(State(s): State<Mnemo>, Query(q): Query<PathQ>) -> Result<Response> {
    let (info, data) = s.read(required(&q)?, q.version).await?;
    let mut resp = data.into_response();
    let h = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&info.mime) {
        h.insert(header::CONTENT_TYPE, v);
    }
    h.insert("x-mnemo-version", HeaderValue::from(info.version));
    h.insert("x-mnemo-hash", HeaderValue::from_str(&info.hash).expect("hex is ascii"));
    Ok(resp)
}

async fn write_file(
    State(s): State<Mnemo>,
    Query(q): Query<PathQ>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>> {
    let author = header_str(&headers, "x-mnemo-author", "api");
    let comment = header_str(&headers, "x-mnemo-comment", "");
    let out = s.write(required(&q)?, &body, &author, &comment).await?;
    Ok(Json(serde_json::to_value(out).expect("serializable")))
}

async fn delete_file(State(s): State<Mnemo>, Query(q): Query<PathQ>) -> Result<Json<Value>> {
    s.delete_file(required(&q)?).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn meta(State(s): State<Mnemo>, Query(q): Query<PathQ>) -> Result<Json<Value>> {
    Ok(Json(serde_json::to_value(s.stat(required(&q)?, q.version).await?).expect("serializable")))
}

async fn versions(State(s): State<Mnemo>, Query(q): Query<PathQ>) -> Result<Json<Value>> {
    Ok(Json(json!({ "versions": s.versions(required(&q)?).await? })))
}

async fn rollback(State(s): State<Mnemo>, Json(b): Json<RollbackBody>) -> Result<Json<Value>> {
    let author = b.author.unwrap_or_else(|| "api".into());
    let out = s.rollback(&b.path, b.version, &author).await?;
    Ok(Json(serde_json::to_value(out).expect("serializable")))
}

async fn search(State(s): State<Mnemo>, Query(q): Query<SearchQ>) -> Result<Json<Value>> {
    Ok(Json(json!({ "hits": s.search(&q.q, q.limit.unwrap_or(50)).await? })))
}
