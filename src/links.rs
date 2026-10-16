//! Expiring, revocable, read-only file links. Link secrets are never stored in plaintext.
use chrono::{Duration, SecondsFormat, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::service::{Drawer, OWNER, Ref};

#[derive(Debug, Serialize)]
pub struct DownloadLink {
    pub id: String,
    pub created_at: String,
    pub expires_at: String,
}

#[derive(Debug, Serialize)]
pub struct NewDownloadLink {
    pub id: String,
    /// Returned only once; the database stores only its SHA-256 digest.
    pub token: String,
    pub created_at: String,
    pub expires_at: String,
}

impl Drawer {
    /// Only files can be linked. A link grants read-only access to the latest revision,
    /// and cannot be used to browse other files or invoke the authenticated API.
    pub async fn create_download_link(&self, id: &str, actor: &str, days: i64) -> Result<NewDownloadLink> {
        if !(1..=30).contains(&days) {
            return Err(Error::Invalid("link expiry must be between 1 and 30 days".into()));
        }
        let node = self.stat(Ref::Id(id)).await?;
        if node.kind != "file" || node.trashed || self.stat(Ref::Path(&node.path)).await?.id != id {
            return Err(Error::NotFound("live file".into()));
        }
        self.authorize(id, actor, OWNER).await?;
        // Two independent UUIDv4 values provide roughly 244 bits of unpredictable entropy.
        let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
        let link_id = Uuid::new_v4().to_string();
        let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let expires_at = (Utc::now() + Duration::days(days)).to_rfc3339_opts(SecondsFormat::Millis, true);
        sqlx::query("INSERT INTO download_links (id, node_id, token_hash, created_at, expires_at) VALUES (?, ?, ?, ?, ?)")
            .bind(&link_id)
            .bind(id)
            .bind(token_hash)
            .bind(&created_at)
            .bind(&expires_at)
            .execute(&self.pool)
            .await?;
        Ok(NewDownloadLink { id: link_id, token, created_at, expires_at })
    }

    pub async fn list_download_links(&self, id: &str, actor: &str) -> Result<Vec<DownloadLink>> {
        self.stat(Ref::Id(id)).await?;
        self.authorize(id, actor, OWNER).await?;
        let rows = sqlx::query(
            "SELECT id, created_at, expires_at FROM download_links WHERE node_id = ? ORDER BY created_at DESC",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| DownloadLink { id: r.get("id"), created_at: r.get("created_at"), expires_at: r.get("expires_at") })
            .collect())
    }

    pub async fn revoke_download_link(&self, id: &str, link_id: &str, actor: &str) -> Result<()> {
        self.stat(Ref::Id(id)).await?;
        self.authorize(id, actor, OWNER).await?;
        let changed = sqlx::query("DELETE FROM download_links WHERE id = ? AND node_id = ?")
            .bind(link_id)
            .bind(id)
            .execute(&self.pool)
            .await?
            .rows_affected();
        if changed == 0 {
            return Err(Error::NotFound("download link".into()));
        }
        Ok(())
    }

    /// No actor is trusted here. The only credential is the 64-character link secret.
    /// Trashing a file or any ancestor makes its links unusable until restored.
    pub async fn read_download_link(&self, token: &str) -> Result<(String, Vec<u8>)> {
        if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::NotFound("download link".into()));
        }
        let hash = hex::encode(Sha256::digest(token.as_bytes()));
        let row = sqlx::query("SELECT node_id FROM download_links WHERE token_hash = ? AND expires_at > ?")
            .bind(hash)
            .bind(Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true))
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| Error::NotFound("download link".into()))?;
        let id: String = row.get("node_id");
        let node = self.stat(Ref::Id(&id)).await?;
        if node.kind != "file" || node.trashed {
            return Err(Error::NotFound("download link".into()));
        }
        // Path resolution checks the full ancestor chain, not just the file's trashed flag.
        let live = self.stat(Ref::Path(&node.path)).await?;
        if live.id != id {
            return Err(Error::NotFound("download link".into()));
        }
        let (_, bytes) = self.read(Ref::Path(&node.path), None).await?;
        Ok((node.name, bytes))
    }
}
