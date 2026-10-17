//! Conservative two-way desktop sync: immutable downloads, guarded resumable uploads,
//! conflict copies and no automatic delete propagation.
use anyhow::{Context, Result, bail};
use cap_std::{ambient_authority, fs::Dir};
use clap::Args;
use reqwest::{Client, Method, RequestBuilder, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

#[derive(Args)]
pub struct SyncArgs {
    /// Local directory to synchronize. Symlinks and .drawer-sync are excluded.
    pub directory: PathBuf,
    /// Drawer origin, for example https://drawer.example.com.
    #[arg(long)]
    pub url: String,
    #[arg(long, default_value = "/Desktop")]
    pub remote: String,
    /// Retry offline failures and synchronize periodically until interrupted.
    #[arg(long)]
    pub watch: bool,
    /// Connect to a server with authentication disabled.
    #[arg(long)]
    pub anonymous: bool,
    #[arg(long, default_value_t = 30)]
    pub interval: u64,
}
#[derive(Default, Serialize, Deserialize)]
struct SyncState {
    version: u32,
    binding: String,
    entries: BTreeMap<String, Base>,
    pending: BTreeMap<String, Pending>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Base {
    hash: String,
    revision: i64,
    id: String,
}
#[derive(Clone, Serialize, Deserialize)]
struct Pending {
    id: String,
    hash: String,
    size: u64,
    head: i64,
}
#[derive(Clone)]
struct Local {
    hash: String,
    size: u64,
}
#[derive(Clone, Deserialize)]
struct RemoteNode {
    id: String,
    path: String,
    kind: String,
    hash: Option<String>,
    size: Option<i64>,
    head_revision: Option<i64>,
}
struct Remote {
    client: Client,
    base: String,
    bearer: Option<String>,
    cookie: Option<String>,
    identity: String,
}
#[derive(Default, Serialize)]
pub struct SyncReport {
    pub uploaded: usize,
    pub downloaded: usize,
    pub conflicts: usize,
    pub unchanged: usize,
}

fn safe(path: &str) -> Result<PathBuf> {
    if path.is_empty() || path.starts_with('/') || path.contains(['\\', ':', '\0']) {
        bail!("unsafe sync path");
    }
    for part in path.split('/') {
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.trim_end_matches(['.', ' ']) != part
            || part.chars().any(char::is_control)
        {
            bail!("unsafe sync path component");
        }
    }
    if path.split('/').next() == Some(".drawer-sync") {
        bail!("remote path uses reserved .drawer-sync directory");
    }
    Ok(PathBuf::from(path))
}
async fn checked(response: Response) -> Result<Response> {
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        bail!(
            "Drawer request failed ({status}): {}",
            text.chars().take(300).collect::<String>()
        );
    }
    Ok(response)
}
impl Remote {
    async fn connect(url: &str, anonymous: bool) -> Result<Self> {
        let parsed = reqwest::Url::parse(url)?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            bail!("use an HTTP(S) URL without credentials, query or fragment");
        }
        let client = Client::builder()
            .timeout(Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let mut remote = Self {
            client,
            base: url.trim_end_matches('/').to_owned(),
            bearer: if anonymous {
                None
            } else {
                std::env::var("DRAWER_TOKEN").ok().filter(|s| !s.is_empty())
            },
            cookie: None,
            identity: if anonymous {
                "anonymous".into()
            } else {
                "admin".into()
            },
        };
        if remote.bearer.is_none() && !anonymous {
            let username = std::env::var("DRAWER_SYNC_USERNAME").unwrap_or_else(|_| "admin".into());
            let password = std::env::var("DRAWER_SYNC_PASSWORD").context(
                "set DRAWER_TOKEN or DRAWER_SYNC_PASSWORD (and optionally DRAWER_SYNC_USERNAME)",
            )?;
            let response = checked(
                remote
                    .client
                    .post(format!("{}/auth/login", remote.base))
                    .json(&json!({"username":username,"password":password}))
                    .send()
                    .await?,
            )
            .await?;
            remote.cookie = Some(
                response
                    .headers()
                    .get("set-cookie")
                    .context("server did not issue a session")?
                    .to_str()?
                    .split(';')
                    .next()
                    .unwrap()
                    .to_owned(),
            );
            remote.identity = username;
        }
        Ok(remote)
    }
    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let mut request = self
            .client
            .request(method, format!("{}{}", self.base, path));
        if let Some(token) = &self.bearer {
            request = request.bearer_auth(token);
        }
        if let Some(cookie) = &self.cookie {
            request = request.header("cookie", cookie);
        }
        request
    }
    async fn json(&self, method: Method, path: &str, body: Value) -> Result<Value> {
        Ok(
            checked(self.request(method, path).json(&body).send().await?)
                .await?
                .json()
                .await?,
        )
    }
    async fn tree(&self, root: &str) -> Result<BTreeMap<String, RemoteNode>> {
        let mut todo = vec![root.to_owned()];
        let mut nodes = BTreeMap::new();
        while let Some(path) = todo.pop() {
            let response = checked(
                self.request(Method::GET, "/drawer/v1/tree")
                    .query(&[("path", &path)])
                    .send()
                    .await?,
            )
            .await?;
            let json: Value = response.json().await?;
            let entries: Vec<RemoteNode> = serde_json::from_value(json["entries"].clone())?;
            for node in entries {
                if nodes.len() >= 5000 {
                    bail!("sync exceeds 5000 nodes; use a smaller folder");
                }
                let prefix = if root == "/" {
                    "/".to_owned()
                } else {
                    format!("{root}/")
                };
                let relative = node
                    .path
                    .strip_prefix(&prefix)
                    .context("server returned a path outside sync root")?
                    .to_owned();
                safe(&relative)?;
                if node.kind == "folder" {
                    todo.push(node.path.clone());
                }
                nodes.insert(relative, node);
            }
        }
        Ok(nodes)
    }
}
async fn local_hash(dir: &Dir, path: &Path) -> Result<Local> {
    let mut file = tokio::fs::File::from_std(dir.open(path)?.into_std());
    let mut digest = Sha256::new();
    let mut size = 0;
    let mut chunk = vec![0; 256 * 1024];
    loop {
        let n = file.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        digest.update(&chunk[..n]);
        size += n as u64;
    }
    Ok(Local {
        hash: hex::encode(digest.finalize()),
        size,
    })
}
async fn scan(dir: &Dir) -> Result<(BTreeMap<String, Local>, BTreeSet<String>)> {
    let mut todo = vec![PathBuf::from(".")];
    let mut files = BTreeMap::new();
    let mut folders = BTreeSet::new();
    let mut count = 0;
    while let Some(path) = todo.pop() {
        for entry in dir.read_dir(&path)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("sync requires UTF-8 filenames"))?;
            if path == Path::new(".") && name == ".drawer-sync" {
                continue;
            }
            if kind.is_symlink() {
                continue;
            }
            let relative = if path == Path::new(".") {
                name
            } else {
                format!("{}/{}", path.to_string_lossy(), name)
            };
            let safe = safe(&relative)?;
            count += 1;
            if count > 5000 {
                bail!("local sync exceeds 5000 entries")
            }
            if kind.is_dir() {
                folders.insert(relative);
                todo.push(safe)
            } else if kind.is_file() {
                let info = local_hash(dir, &safe).await?;
                files.insert(relative, info);
            }
        }
    }
    Ok((files, folders))
}
fn save(dir: &Dir, state: &SyncState) -> Result<()> {
    let temp = format!(".drawer-sync/state-{}.json", uuid::Uuid::new_v4());
    dir.write(&temp, serde_json::to_vec_pretty(state)?)?;
    dir.rename(&temp, dir, ".drawer-sync/state.json")?;
    Ok(())
}
fn remote_path(root: &str, relative: &str) -> String {
    format!("{}/{}", root.trim_end_matches('/'), relative)
}
async fn upload(
    remote: &Remote,
    dir: &Dir,
    state: &mut SyncState,
    path: &str,
    local: &Local,
    head: i64,
    destination: &str,
) -> Result<Base> {
    if local.size > 4 * 1024 * 1024 * 1024 {
        bail!("file exceeds 4 GiB sync limit: {path}")
    }
    let mut pending = state
        .pending
        .get(path)
        .filter(|p| p.hash == local.hash && p.size == local.size && p.head == head)
        .cloned();
    if pending.is_none() {
        if let Some(old) = state.pending.remove(path) {
            let _ = remote
                .request(Method::DELETE, &format!("/drawer/v1/transfers/{}", old.id))
                .send()
                .await;
        }
        let info=remote.json(Method::POST,"/drawer/v1/transfers",json!({"path":destination,"size":local.size,"sha256":local.hash,"if_head_revision":head})).await?;
        let transfer = Pending {
            id: info["id"].as_str().context("missing upload id")?.to_owned(),
            hash: local.hash.clone(),
            size: local.size,
            head,
        };
        state.pending.insert(path.to_owned(), transfer.clone());
        save(dir, state)?;
        pending = Some(transfer);
    }
    let pending = pending.unwrap();
    let endpoint = format!("/drawer/v1/transfers/{}", pending.id);
    let response = remote.request(Method::GET, &endpoint).send().await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        state.pending.remove(path);
        save(dir, state)?;
        bail!("saved transfer expired; next sync will start a new upload")
    }
    let status: Value = checked(response).await?.json().await?;
    let mut offset = status["received"]
        .as_u64()
        .context("missing upload offset")?;
    if status["sha256"].as_str() != Some(&local.hash)
        || status["size"].as_u64() != Some(local.size)
        || offset > local.size
    {
        bail!("saved transfer does not match local file")
    }
    remote
        .json(Method::PATCH, &endpoint, json!({"status":"active"}))
        .await?;
    let mut file = tokio::fs::File::from_std(dir.open(safe(path)?)?.into_std());
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    while offset < local.size {
        let mut chunk = vec![0; ((local.size - offset).min(8 * 1024 * 1024)) as usize];
        file.read_exact(&mut chunk).await?;
        let expected_offset = offset + chunk.len() as u64;
        let response = checked(
            remote
                .request(Method::PUT, &endpoint)
                .header("x-upload-offset", offset)
                .body(chunk)
                .send()
                .await?,
        )
        .await?;
        let info: Value = response.json().await?;
        offset = info["received"]
            .as_u64()
            .context("missing committed offset")?;
        if offset != expected_offset {
            bail!("server returned an unexpected upload offset")
        }
    }
    let result = remote
        .json(Method::POST, &format!("{endpoint}/complete"), json!({}))
        .await?;
    state.pending.remove(path);
    Ok(Base {
        hash: result["node"]["hash"]
            .as_str()
            .context("missing committed hash")?
            .to_owned(),
        revision: result["revision"]
            .as_i64()
            .context("missing committed revision")?,
        id: result["node"]["id"]
            .as_str()
            .context("missing node identity")?
            .to_owned(),
    })
}
struct DownloadTemp {
    dir: Dir,
    path: String,
}
impl Drop for DownloadTemp {
    fn drop(&mut self) {
        let _ = self.dir.remove_file(&self.path);
    }
}

async fn download(
    remote: &Remote,
    dir: &Dir,
    path: &str,
    node: &RemoteNode,
    conflict: bool,
) -> Result<Base> {
    let relative = safe(path)?;
    if let Some(parent) = relative.parent()
        && !parent.as_os_str().is_empty()
    {
        dir.create_dir_all(parent)?;
    }
    let expected = node.hash.as_ref().context("remote file has no hash")?;
    let revision = node.head_revision.context("remote file has no revision")?;
    let mut response = checked(
        remote
            .request(Method::GET, "/drawer/v1/files")
            .query(&[("id", node.id.clone()), ("revision", revision.to_string())])
            .send()
            .await?,
    )
    .await?;
    let temp = format!(".drawer-sync/download-{}.part", uuid::Uuid::new_v4());
    let mut options = cap_std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = tokio::fs::File::from_std(dir.open_with(&temp, &options)?.into_std());
    let _cleanup = DownloadTemp {
        dir: dir.try_clone()?,
        path: temp.clone(),
    };
    let mut digest = Sha256::new();
    let mut bytes = 0i64;
    while let Some(chunk) = response.chunk().await? {
        bytes += chunk.len() as i64;
        if bytes > node.size.context("missing remote size")? {
            bail!("download exceeded expected size")
        };
        digest.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.sync_all().await?;
    drop(file);
    if bytes != node.size.unwrap() || hex::encode(digest.finalize()) != *expected {
        let _ = dir.remove_file(&temp);
        bail!("download checksum mismatch")
    }
    match dir.symlink_metadata(&relative) {
        Ok(meta) => {
            if !meta.is_file() || meta.file_type().is_symlink() {
                let _ = dir.remove_file(&temp);
                bail!("local destination is not a regular file")
            }
            // Preserve the prior inode, including writes from an editor with an open handle.
            let previous = if conflict {
                format!("{path}.conflict-{}", uuid::Uuid::new_v4().simple())
            } else {
                format!(".drawer-sync/previous/{}", uuid::Uuid::new_v4())
            };
            dir.rename(&relative, dir, &previous)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    dir.rename(&temp, dir, &relative)?;
    Ok(Base {
        hash: expected.clone(),
        revision,
        id: node.id.clone(),
    })
}
pub async fn cycle(args: &SyncArgs) -> Result<SyncReport> {
    let root = crate::service::normalize(&args.remote)?;
    std::fs::create_dir_all(&args.directory)?;
    let dir = Dir::open_ambient_dir(&args.directory, ambient_authority())?;
    if let Ok(meta) = dir.symlink_metadata(".drawer-sync")
        && (meta.file_type().is_symlink() || !meta.is_dir())
    {
        bail!(".drawer-sync must be a real directory")
    }
    dir.create_dir_all(".drawer-sync/previous")?;
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    let lease = dir.open_with(".drawer-sync/lock", &options)?.into_std();
    fs2::FileExt::try_lock_exclusive(&lease).context("another sync is using this local folder")?;
    let remote = Remote::connect(&args.url, args.anonymous).await?;
    let binding = format!("{}\n{}\n{}", remote.base, root, remote.identity);
    let mut state: SyncState = match dir.read(".drawer-sync/state.json") {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => SyncState {
            version: 1,
            binding: binding.clone(),
            ..SyncState::default()
        },
        Err(error) => return Err(error.into()),
    };
    if state.version != 1 || state.binding != binding {
        bail!("local sync state belongs to another server, workspace or remote folder")
    }
    let (local, folders) = scan(&dir).await?;
    remote
        .json(Method::POST, "/drawer/v1/folders", json!({"path":root}))
        .await?;
    let mut nodes = remote.tree(&root).await?;
    for folder in folders {
        remote
            .json(
                Method::POST,
                "/drawer/v1/folders",
                json!({"path":remote_path(&root,&folder)}),
            )
            .await?;
    }
    for (path, node) in &nodes {
        if node.kind == "folder" {
            dir.create_dir_all(safe(path)?)?;
        }
    }
    let paths: BTreeSet<_> = local
        .keys()
        .chain(
            nodes
                .iter()
                .filter(|(_, n)| n.kind == "file")
                .map(|(p, _)| p),
        )
        .cloned()
        .collect();
    let mut report = SyncReport::default();
    for path in paths {
        let local = local.get(&path);
        let node = nodes.remove(&path);
        let base = state.entries.get(&path);
        let entry = match (local, node.as_ref()) {
            (Some(local), Some(node)) => {
                let remote_hash = node
                    .hash
                    .as_ref()
                    .context("remote file has no content hash")?;
                if local.hash == *remote_hash {
                    if let Some(pending) = state.pending.remove(&path) {
                        let _ = remote
                            .request(
                                Method::DELETE,
                                &format!("/drawer/v1/transfers/{}", pending.id),
                            )
                            .send()
                            .await;
                    }
                    report.unchanged += 1;
                    Base {
                        hash: local.hash.clone(),
                        revision: node.head_revision.context("missing revision")?,
                        id: node.id.clone(),
                    }
                } else {
                    let local_changed = base.is_none_or(|b| b.hash != local.hash);
                    let remote_changed = base.is_none_or(|b| {
                        b.hash != *remote_hash
                            || b.id != node.id
                            || Some(b.revision) != node.head_revision
                    });
                    if local_changed && !remote_changed {
                        report.uploaded += 1;
                        upload(
                            &remote,
                            &dir,
                            &mut state,
                            &path,
                            local,
                            node.head_revision.unwrap(),
                            &node.path,
                        )
                        .await?
                    } else {
                        if let Some(pending) = state.pending.remove(&path) {
                            let _ = remote
                                .request(
                                    Method::DELETE,
                                    &format!("/drawer/v1/transfers/{}", pending.id),
                                )
                                .send()
                                .await;
                        }
                        let conflict = local_changed;
                        report.downloaded += 1;
                        if conflict {
                            report.conflicts += 1;
                        }
                        download(&remote, &dir, &path, node, conflict).await?
                    }
                }
            }
            (Some(local), None) => {
                report.uploaded += 1;
                upload(
                    &remote,
                    &dir,
                    &mut state,
                    &path,
                    local,
                    0,
                    &remote_path(&root, &path),
                )
                .await?
            }
            (None, Some(node)) => {
                report.downloaded += 1;
                download(&remote, &dir, &path, node, false).await?
            }
            (None, None) => continue,
        };
        state.entries.insert(path, entry);
        save(&dir, &state)?;
    }
    save(&dir, &state)?;
    Ok(report)
}
pub async fn run(args: &SyncArgs) -> Result<()> {
    if !(5..=3600).contains(&args.interval) {
        bail!("sync interval must be 5 to 3600 seconds")
    }
    loop {
        match cycle(args).await {
            Ok(report) => println!("{}", serde_json::to_string(&report)?),
            Err(error) if args.watch => eprintln!(
                "Sync paused: {error}. Retrying; local files and pending uploads are preserved."
            ),
            Err(error) => return Err(error),
        }
        if !args.watch {
            return Ok(());
        }
        tokio::select! {result=tokio::signal::ctrl_c()=>{result?;return Ok(())},_=tokio::time::sleep(Duration::from_secs(args.interval))=>{}}
    }
}
