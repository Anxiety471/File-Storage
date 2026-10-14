//! REST API under `/drawer/v1`.
//!
//! Nodes can be named by `path` or by stable `id`. Mutations name their actor in the
//! `x-drawer-author` header (`api` by default).
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderName, HeaderValue, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::service::{Drawer, NodeUpdate, Properties, Ref, WriteOpts, normalize};

/// Custom properties travel as `x-drawer-prop-<key>` headers, like GCS's `x-goog-meta-*`.
const PROP_PREFIX: &str = "x-drawer-prop-";

#[derive(Deserialize)]
struct PathQ {
    path: Option<String>,
    id: Option<String>,
    revision: Option<i64>,
    if_head_revision: Option<i64>,
}

#[derive(Deserialize)]
struct PathBody {
    path: String,
}

#[derive(Deserialize)]
struct RollbackBody {
    path: Option<String>,
    id: Option<String>,
    revision: i64,
    author: Option<String>,
    comment: Option<String>,
    if_head_revision: Option<i64>,
}

#[derive(Deserialize)]
struct CopyBody {
    from: Option<String>,
    from_id: Option<String>,
    to: String,
    source_revision: Option<i64>,
    author: Option<String>,
    comment: Option<String>,
    if_head_revision: Option<i64>,
}

#[derive(Deserialize)]
struct MoveBody {
    from: Option<String>,
    from_id: Option<String>,
    to: String,
}

#[derive(Deserialize)]
struct UpdateBody {
    name: Option<String>,
    parent_id: Option<String>,
    starred: Option<bool>,
    trashed: Option<bool>,
}

#[derive(Deserialize)]
struct ShareBody {
    principal: String,
    role: String,
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
        .route("/folders", post(mkdir).delete(delete_file))
        .route("/files", get(read_file).put(write_file).delete(delete_file))
        .route("/files/meta", get(meta))
        .route("/revisions", get(revisions))
        .route("/rollback", post(rollback))
        .route("/copy", post(copy))
        .route("/move", post(move_to))
        .route("/nodes/{id}", axum::routing::patch(update_node).delete(purge_node))
        .route("/nodes/{id}/permissions", get(list_permissions).put(share))
        .route("/nodes/{id}/permissions/{principal}", axum::routing::delete(unshare))
        .route("/trash", get(trash))
        .route("/starred", get(starred))
        .route("/search", get(search))
}

/// Exactly one of `id` or `path` must name the node.
fn pick<'a>(id: &'a Option<String>, path: &'a Option<String>) -> Result<Ref<'a>> {
    match (id, path) {
        (Some(id), _) => Ok(Ref::Id(id)),
        (None, Some(p)) => Ok(Ref::Path(p)),
        _ => Err(Error::Invalid("name the node with `id` or `path`".into())),
    }
}

fn target(q: &PathQ) -> Result<Ref<'_>> {
    pick(&q.id, &q.path)
}

fn header_str(h: &HeaderMap, name: &str, default: &str) -> String {
    h.get(name).and_then(|v| v.to_str().ok()).unwrap_or(default).to_string()
}

fn actor(h: &HeaderMap) -> String {
    header_str(h, "x-drawer-author", "api")
}

fn out<T: Serialize>(v: T) -> Json<Value> {
    Json(serde_json::to_value(v).expect("serializable"))
}

/// Author, comment, `x-drawer-prop-*` headers and the precondition become write options.
fn write_opts(h: &HeaderMap, if_head_revision: Option<i64>) -> Result<WriteOpts> {
    let mut properties = Properties::new();
    for (name, value) in h.iter() {
        if let Some(key) = name.as_str().strip_prefix(PROP_PREFIX) {
            let value = value.to_str().map_err(|_| Error::Invalid(format!("{name} must be ASCII")))?;
            // Header names are case-insensitive, so keys are stored lowercase.
            properties.insert(key.to_ascii_lowercase(), value.to_string());
        }
    }
    Ok(WriteOpts {
        author: actor(h),
        comment: header_str(h, "x-drawer-comment", ""),
        properties,
        if_head_revision,
    })
}

async fn tree(State(s): State<Drawer>, Query(q): Query<PathQ>) -> Result<Json<Value>> {
    let path = q.path.as_deref().unwrap_or("/");
    Ok(Json(json!({ "path": normalize(path)?, "entries": s.list(path).await? })))
}

async fn mkdir(State(s): State<Drawer>, headers: HeaderMap, Json(b): Json<PathBody>) -> Result<Json<Value>> {
    Ok(out(s.mkdir(&b.path, &actor(&headers)).await?))
}

async fn read_file(State(s): State<Drawer>, Query(q): Query<PathQ>) -> Result<Response> {
    let (rev, data) = s.read(target(&q)?, q.revision).await?;
    let mut resp = data.into_response();
    let h = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&rev.mime) {
        h.insert(header::CONTENT_TYPE, v);
    }
    h.insert("x-drawer-revision", HeaderValue::from(rev.revision));
    h.insert("x-drawer-hash", HeaderValue::from_str(&rev.hash).expect("hex is ascii"));
    for (k, v) in &rev.properties {
        let name = HeaderName::from_bytes(format!("{PROP_PREFIX}{k}").as_bytes());
        let value = HeaderValue::from_str(v);
        if let (Ok(name), Ok(value)) = (name, value) {
            h.insert(name, value);
        }
    }
    Ok(resp)
}

async fn write_file(State(s): State<Drawer>, Query(q): Query<PathQ>, headers: HeaderMap, body: Bytes) -> Result<Json<Value>> {
    let path = q.path.as_deref().ok_or_else(|| Error::Invalid("PUT needs ?path=".into()))?;
    let opts = write_opts(&headers, q.if_head_revision)?;
    Ok(out(s.write(path, &body, opts).await?))
}

/// DELETE trashes a file or folder. Trash is reversible, so this is the everyday delete.
async fn delete_file(State(s): State<Drawer>, Query(q): Query<PathQ>, headers: HeaderMap) -> Result<Json<Value>> {
    Ok(out(s.trash(target(&q)?, q.if_head_revision, &actor(&headers)).await?))
}

async fn meta(State(s): State<Drawer>, Query(q): Query<PathQ>) -> Result<Json<Value>> {
    Ok(out(s.stat(target(&q)?).await?))
}

async fn revisions(State(s): State<Drawer>, Query(q): Query<PathQ>) -> Result<Json<Value>> {
    Ok(Json(json!({ "revisions": s.revisions(target(&q)?).await? })))
}

async fn rollback(State(s): State<Drawer>, Json(b): Json<RollbackBody>) -> Result<Json<Value>> {
    let mut opts = WriteOpts::new(b.author.unwrap_or_else(|| "api".into()), b.comment.unwrap_or_default());
    opts.if_head_revision = b.if_head_revision;
    Ok(out(s.rollback(pick(&b.id, &b.path)?, b.revision, opts).await?))
}

async fn copy(State(s): State<Drawer>, Json(b): Json<CopyBody>) -> Result<Json<Value>> {
    let mut opts = WriteOpts::new(b.author.unwrap_or_else(|| "api".into()), b.comment.unwrap_or_default());
    opts.if_head_revision = b.if_head_revision;
    Ok(out(s.copy(pick(&b.from_id, &b.from)?, &b.to, b.source_revision, opts).await?))
}

async fn move_to(State(s): State<Drawer>, headers: HeaderMap, Json(b): Json<MoveBody>) -> Result<Json<Value>> {
    Ok(out(s.move_to(pick(&b.from_id, &b.from)?, &b.to, &actor(&headers)).await?))
}

async fn update_node(
    State(s): State<Drawer>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(b): Json<UpdateBody>,
) -> Result<Json<Value>> {
    let u = NodeUpdate { name: b.name, parent_id: b.parent_id, starred: b.starred, trashed: b.trashed };
    Ok(out(s.update(&id, u, &actor(&headers)).await?))
}

/// DELETE on a node id permanently removes it. It must already be in the trash.
async fn purge_node(State(s): State<Drawer>, Path(id): Path<String>, headers: HeaderMap) -> Result<Json<Value>> {
    s.purge(&id, &actor(&headers)).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn list_permissions(State(s): State<Drawer>, Path(id): Path<String>) -> Result<Json<Value>> {
    Ok(Json(json!({ "permissions": s.permissions(&id).await? })))
}

async fn share(State(s): State<Drawer>, Path(id): Path<String>, headers: HeaderMap, Json(b): Json<ShareBody>) -> Result<Json<Value>> {
    Ok(Json(json!({ "permissions": s.share(&id, &b.principal, &b.role, &actor(&headers)).await? })))
}

async fn unshare(
    State(s): State<Drawer>,
    Path((id, principal)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>> {
    Ok(Json(json!({ "permissions": s.unshare(&id, &principal, &actor(&headers)).await? })))
}

async fn trash(State(s): State<Drawer>) -> Result<Json<Value>> {
    Ok(Json(json!({ "nodes": s.trash_list().await? })))
}

async fn starred(State(s): State<Drawer>) -> Result<Json<Value>> {
    Ok(Json(json!({ "nodes": s.starred().await? })))
}

async fn search(State(s): State<Drawer>, Query(q): Query<SearchQ>) -> Result<Json<Value>> {
    Ok(Json(json!({ "hits": s.search(&q.q, q.limit.unwrap_or(50)).await? })))
}
