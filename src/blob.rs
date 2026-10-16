//! Content-addressed blob storage on disk: `<root>/ab/cdef...` named by sha256.
use std::path::{Path, PathBuf};
use tokio::io::AsyncReadExt;

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

    /// Stream a staged file into content-addressed storage without loading it into RAM.
    pub async fn put_file(&self, staged: &Path) -> Result<(String, i64)> {
        let mut file = tokio::fs::File::open(staged).await?;
        let mut sha = Sha256::new();
        let mut length = 0i64;
        let mut chunk = vec![0u8; 256 * 1024];
        loop {
            let n = file.read(&mut chunk).await?;
            if n == 0 { break; }
            sha.update(&chunk[..n]);
            length += n as i64;
        }
        let hash = hex::encode(sha.finalize());
        let path = self.path_for(&hash)?;
        if !tokio::fs::try_exists(&path).await? {
            tokio::fs::create_dir_all(path.parent().expect("blob directory")).await?;
            let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
            tokio::fs::copy(staged, &tmp).await?;
            tokio::fs::rename(&tmp, &path).await?;
        }
        Ok((hash, length))
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
