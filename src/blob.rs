//! Content-addressed blob storage on disk: `<root>/ab/cdef...` named by sha256.
use std::path::PathBuf;

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

#[derive(Clone)]
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn hash(data: &[u8]) -> String {
        hex::encode(Sha256::digest(data))
    }

    pub(crate) fn path_for(&self, hash: &str) -> Result<PathBuf> {
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::Invalid("bad blob hash".into()));
        }
        Ok(self.root.join(&hash[..2]).join(&hash[2..]))
    }

    /// Store bytes, returning their hash. Identical content is stored once.
    pub async fn put(&self, data: &[u8]) -> Result<String> {
        let hash = Self::hash(data);
        let path = self.path_for(&hash)?;
        if tokio::fs::try_exists(&path).await? {
            return Ok(hash);
        }
        let dir = path.parent().expect("blob path has a parent");
        tokio::fs::create_dir_all(dir).await?;
        let tmp = dir.join(format!(".tmp-{}-{}", std::process::id(), &hash[..16]));
        tokio::fs::write(&tmp, data).await?;
        tokio::fs::rename(&tmp, &path).await?;
        Ok(hash)
    }

    /// Whether the blob for `hash` is on disk.
    pub async fn contains(&self, hash: &str) -> Result<bool> {
        Ok(tokio::fs::try_exists(self.path_for(hash)?).await?)
    }

    pub async fn get(&self, hash: &str) -> Result<Vec<u8>> {
        let path = self.path_for(hash)?;
        tokio::fs::read(&path)
            .await
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => Error::Internal(format!("blob {hash} missing")),
                _ => e.into(),
            })
    }
}
