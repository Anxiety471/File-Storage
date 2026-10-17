//! Expiring upload-only folder capabilities. No public read/list access is provided.
use axum::{Json, body::Body, extract::{Path, Query, State}, http::{HeaderMap, header}, response::{Html, IntoResponse, Response}};
use futures_lite::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value,json};
use sha2::{Digest,Sha256};
use tokio::io::AsyncWriteExt;
use crate::{error::{Error,Result}, service::{Drawer,OWNER,Ref,WriteOpts}};
#[derive(Deserialize)]
pub struct Policy { days:i64, max_files:i64, max_bytes:i64 }
#[derive(Serialize, sqlx::FromRow)]
pub struct UploadLink { id:String, expires_at:String, max_files:i64, max_bytes:i64, files_uploaded:i64, bytes_uploaded:i64 }
#[derive(Deserialize)]
pub struct Filename { name:String }
fn actor(headers:&HeaderMap)->&str { headers.get("x-drawer-author").and_then(|v|v.to_str().ok()).unwrap_or("api") }
fn digest(token:&str)->Result<String> { if token.len()!=64 || !token.bytes().all(|b|b.is_ascii_hexdigit()) { return Err(Error::NotFound("upload link".into())); } Ok(hex::encode(Sha256::digest(token.as_bytes()))) }
async fn folder(s:&Drawer,id:&str)->Result<crate::service::Node> {
    let node=s.stat(Ref::Id(id)).await?;
    if node.kind!="folder" || node.trashed || s.stat(Ref::Path(&node.path)).await?.id!=id { return Err(Error::NotFound("live upload folder".into())); }
    Ok(node)
}
pub async fn create(State(s):State<Drawer>,Path(id):Path<String>,headers:HeaderMap,Json(p):Json<Policy>)->Result<Json<Value>> {
    if !(1..=30).contains(&p.days) || !(1..=10000).contains(&p.max_files) || !(1..=1_099_511_627_776).contains(&p.max_bytes) { return Err(Error::Invalid("expiry 1–30 days, 1–10000 files, and 1 byte–1 TiB required".into())); }
    let _lock=s.write_lock.lock().await;
    folder(&s,&id).await?; s.authorize(&id,actor(&headers),OWNER).await?;
    let token=format!("{}{}",uuid::Uuid::new_v4().simple(),uuid::Uuid::new_v4().simple());
    let link=uuid::Uuid::new_v4().to_string();
    let expires=(chrono::Utc::now()+chrono::Duration::days(p.days)).to_rfc3339_opts(chrono::SecondsFormat::Millis,true);
    sqlx::query("INSERT INTO upload_links(id,node_id,token_hash,expires_at,max_files,max_bytes) VALUES(?,?,?,?,?,?)").bind(&link).bind(id).bind(digest(&token)?).bind(&expires).bind(p.max_files).bind(p.max_bytes).execute(&s.pool).await?;
    Ok(Json(json!({"id":link,"token":token,"expires_at":expires})))
}
pub async fn list(State(s):State<Drawer>,Path(id):Path<String>,headers:HeaderMap)->Result<Json<Vec<UploadLink>>> {
    s.authorize(&id,actor(&headers),OWNER).await?;
    Ok(Json(sqlx::query_as("SELECT id,expires_at,max_files,max_bytes,files_uploaded,bytes_uploaded FROM upload_links WHERE node_id=? ORDER BY expires_at DESC").bind(id).fetch_all(&s.pool).await?))
}
pub async fn revoke(State(s):State<Drawer>,Path((id,link)):Path<(String,String)>,headers:HeaderMap)->Result<Json<Value>> {
    let _lock=s.deposit_lock.lock().await;
    s.authorize(&id,actor(&headers),OWNER).await?;
    sqlx::query("DELETE FROM upload_links WHERE id=? AND node_id=?").bind(link).bind(id).execute(&s.pool).await?;
    Ok(Json(json!({"ok":true})))
}
async fn valid(s:&Drawer,token:&str)->Result<(String,String)> {
    let (id,node): (String,String)=sqlx::query_as("SELECT id,node_id FROM upload_links WHERE token_hash=? AND expires_at>strftime('%Y-%m-%dT%H:%M:%fZ','now')").bind(digest(token)?).fetch_optional(&s.pool).await?.ok_or_else(||Error::NotFound("upload link expired or revoked".into()))?;
    folder(s,&node).await?; Ok((id,node))
}
pub async fn landing(State(s):State<Drawer>,Path(token):Path<String>)->Result<Response> {
    valid(&s,&token).await?;
    Ok(([(header::CACHE_CONTROL,"no-store"),(header::REFERRER_POLICY,"no-referrer"),(header::X_CONTENT_TYPE_OPTIONS,"nosniff")],Html(include_str!("deposit.html"))).into_response())
}
struct Staged(std::path::PathBuf);
impl Drop for Staged { fn drop(&mut self) { let _=std::fs::remove_file(&self.0); } }
pub async fn upload(State(s):State<Drawer>,Path(token):Path<String>,Query(q):Query<Filename>,headers:HeaderMap,body:Body)->Result<Json<Value>> {
    if q.name.is_empty() || q.name.len()>200 || q.name=="." || q.name==".." || q.name.contains(['/', '\\']) || q.name.chars().any(char::is_control) { return Err(Error::Invalid("filename must be one component, at most 200 bytes".into())); }
    let size=headers.get(header::CONTENT_LENGTH).and_then(|v|v.to_str().ok()).and_then(|v|v.parse::<i64>().ok()).filter(|n|*n>=0 && *n<=crate::MAX_UPLOAD_BYTES as i64).ok_or_else(||Error::Invalid("Content-Length from 0 to 512 MiB is required".into()))?;
    let (id,node)=valid(&s,&token).await?;
    let reserved=sqlx::query("UPDATE upload_links SET files_uploaded=files_uploaded+1,bytes_uploaded=bytes_uploaded+? WHERE id=? AND files_uploaded<max_files AND bytes_uploaded+?<=max_bytes AND expires_at>strftime('%Y-%m-%dT%H:%M:%fZ','now')").bind(size).bind(&id).bind(size).execute(&s.pool).await?.rows_affected();
    if reserved==0 { return Err(Error::Conflict("upload link budget exhausted".into())); }
    let result=tokio::time::timeout(std::time::Duration::from_secs(900),async {
        tokio::fs::create_dir_all(&s.transfer_dir).await?;
        let staged=Staged(s.transfer_dir.join(format!("drop-{}.part",uuid::Uuid::new_v4())));
        let mut file=tokio::fs::OpenOptions::new().write(true).create_new(true).open(&staged.0).await?;
        let mut stream=body.into_data_stream(); let mut received=0i64;
        while let Some(chunk)=stream.next().await { let chunk=chunk.map_err(|_|Error::Invalid("upload stream failed".into()))?; received+=chunk.len() as i64; if received>size { return Err(Error::Invalid("upload exceeds declared length".into())); } file.write_all(&chunk).await?; }
        if received!=size { return Err(Error::Invalid("upload length mismatch".into())); }
        file.sync_data().await?; drop(file);
        let _lock=s.deposit_lock.lock().await;
        valid(&s,&token).await?;
        let parent=folder(&s,&node).await?;
        let destination=format!("{}/{}-{}",parent.path.trim_end_matches('/'),uuid::Uuid::new_v4().simple(),q.name);
        let mut opts=WriteOpts::new(parent.owner,"Upload-only deposit"); opts.if_head_revision=Some(0); opts.required_parent=Some(node);
        s.write_staged(&destination,&staged.0,opts,None).await?;
        Ok::<_,Error>(Json(json!({"uploaded":true,"bytes":size})))
    }).await.unwrap_or_else(|_|Err(Error::Invalid("upload timed out".into())));
    if result.is_err() { sqlx::query("UPDATE upload_links SET files_uploaded=files_uploaded-1,bytes_uploaded=bytes_uploaded-? WHERE id=?").bind(size).bind(id).execute(&s.pool).await?; }
    result
}
