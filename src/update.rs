//! Self-update: replace this install with the latest GitHub release.
//!
//! This does what install.sh does, from inside the binary: fetch the release archive for this
//! platform and its SHA256SUMS, verify the checksum, then swap in the new binary and web UI.
//! The binary is staged beside the old one and renamed over it, so an interrupted update leaves
//! the previous binary in place. The store is never touched. curl and tar must be on PATH, as
//! they already are for install.sh. A running `drawer serve` keeps its old code until restarted.
//!
//! The CLI (`drawer update`) and the REST API (`/drawer/v1/update`) both call into here.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

const DEFAULT_REPO: &str = "Anxiety471/drawer";
const DEFAULT_API_URL: &str = "https://api.github.com";
const DEFAULT_GITHUB_URL: &str = "https://github.com";

/// One install at a time, so two requests cannot swap the same files.
static INSTALL_LOCK: Mutex<()> = Mutex::new(());

/// Where releases are read from. The environment overrides are for forks and for testing.
#[derive(Clone, Debug)]
pub struct Config {
    pub repo: String,
    pub api_url: String,
    pub github_url: String,
}

impl Config {
    /// GitHub, or the `DRAWER_REPO`, `DRAWER_API_URL` and `DRAWER_GITHUB_URL` overrides.
    pub fn from_env() -> Self {
        Self {
            repo: env_or("DRAWER_REPO", DEFAULT_REPO),
            api_url: env_or("DRAWER_API_URL", DEFAULT_API_URL),
            github_url: env_or("DRAWER_GITHUB_URL", DEFAULT_GITHUB_URL),
        }
    }
}

/// What the latest release looks like next to the running version.
#[derive(Debug, Serialize)]
pub struct Check {
    pub current: String,
    pub latest: String,
    pub update_available: bool,
}

/// Ask the release host for the latest release and compare it with this build.
pub fn check(cfg: &Config) -> Result<Check> {
    let current = env!("CARGO_PKG_VERSION");
    let work = Workspace::new()?;
    let release = work.path().join("release.json");
    let api = format!("{}/repos/{}/releases/latest", cfg.api_url, cfg.repo);
    if !fetch(&api, &release)? {
        bail!(
            "{} has no published release yet, so there is nothing to update to",
            cfg.repo
        );
    }
    let metadata: Value = serde_json::from_slice(&fs::read(&release)?)
        .context("GitHub's release metadata is not JSON")?;
    let latest = metadata["tag_name"]
        .as_str()
        .ok_or_else(|| anyhow!("the latest release has no tag_name"))?
        .to_string();
    let update_available = parse_version(&latest)? > parse_version(current)?;
    Ok(Check {
        current: current.to_string(),
        latest,
        update_available,
    })
}

/// Download release `tag`, verify it, and replace the binary at `exe` and the UI beside it.
pub fn install(cfg: &Config, tag: &str, exe: &Path) -> Result<()> {
    // The tag goes into a download URL, so it has to be a plain version.
    parse_version(tag)?;
    let _lock = INSTALL_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let target = target()?;
    let work = Workspace::new()?;
    let archive_name = format!("drawer-{target}.tar.gz");
    let base = format!("{}/{}/releases/download/{tag}", cfg.github_url, cfg.repo);
    let sums = work.path().join("SHA256SUMS");
    let archive = work.path().join(&archive_name);
    if !fetch(&format!("{base}/SHA256SUMS"), &sums)? {
        bail!("release {tag} has no SHA256SUMS");
    }
    if !fetch(&format!("{base}/{archive_name}"), &archive)? {
        bail!("release {tag} has no archive for {target}");
    }
    verify(&archive, &archive_name, &fs::read_to_string(&sums)?)?;

    let unpacked = work.path().join("unpacked");
    fs::create_dir(&unpacked)?;
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(&unpacked)
        .status()
        .context("tar is required for drawer update")?;
    if !status.success() {
        bail!("cannot unpack {archive_name}");
    }
    let bundle = unpacked.join(format!("drawer-{target}"));

    replace_binary(&bundle.join("drawer"), exe)?;
    // The UI sits beside the binary at ../share/drawer/ui, as install.sh lays it out. Only
    // refresh it where it is already installed, so a binary built by hand gets no new folders.
    let ui = exe
        .parent()
        .and_then(Path::parent)
        .map(|p| p.join("share/drawer/ui"));
    if let Some(ui) = ui.filter(|p| p.is_dir())
        && bundle.join("ui").is_dir()
    {
        replace_ui(&bundle.join("ui"), &ui)?;
    }
    Ok(())
}

/// `drawer update`: report whether a newer release exists (`check_only`), or install it.
pub fn run(check_only: bool) -> Result<()> {
    let cfg = Config::from_env();
    let status = check(&cfg)?;
    if !status.update_available {
        println!(
            "drawer {} is up to date (latest release {})",
            status.current, status.latest
        );
        return Ok(());
    }
    if check_only {
        println!(
            "drawer {} is installed; {} is available. Run `drawer update` to install it.",
            status.current, status.latest
        );
        return Ok(());
    }
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .context("cannot find the running drawer binary")?;
    install(&cfg, &status.latest, &exe)?;
    println!(
        "updated drawer {} to {} at {}",
        status.current,
        status.latest,
        exe.display()
    );
    println!("restart `drawer serve` to run the new version");
    Ok(())
}

/// The archive name suffix for this build, matching the targets in the release workflow.
pub fn target() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        (os, arch) => {
            bail!("no release is built for {os} {arch}; reinstall from source with install.sh")
        }
    }
}

/// Read `v1.2.3` or `1.2.3`. Pre-release suffixes are not used by the release workflow.
fn parse_version(s: &str) -> Result<(u64, u64, u64)> {
    let digits = s.strip_prefix('v').unwrap_or(s);
    let mut parts = digits.split('.').map(str::parse::<u64>);
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(Ok(major)), Some(Ok(minor)), Some(Ok(patch)), None) => Ok((major, minor, patch)),
        _ => bail!("cannot read {s:?} as a version like v1.2.3"),
    }
}

/// Download `url` to `dest`. Ok(false) when the server answers 404, so callers can say what is missing.
fn fetch(url: &str, dest: &Path) -> Result<bool> {
    let out = Command::new("curl")
        .args([
            "-sSL",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "User-Agent: drawer-update",
        ])
        .args(["-w", "%{http_code}", "-o"])
        .arg(dest)
        .arg(url)
        .output()
        .context("curl is required for drawer update")?;
    if !out.status.success() {
        bail!(
            "cannot reach {url}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    match String::from_utf8_lossy(&out.stdout).trim() {
        "200" => Ok(true),
        "404" => Ok(false),
        code => bail!("{url} answered HTTP {code}"),
    }
}

/// Check the archive against its line in SHA256SUMS, as `sha256sum -c` would.
fn verify(archive: &Path, name: &str, sums: &str) -> Result<()> {
    let expected = sums
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            let hash = fields.next()?;
            let file = fields.next()?.trim_start_matches('*');
            (file == name).then_some(hash)
        })
        .ok_or_else(|| anyhow!("{name} is not listed in SHA256SUMS"))?;
    let actual = hex::encode(Sha256::digest(fs::read(archive)?));
    if !actual.eq_ignore_ascii_case(expected) {
        bail!("checksum mismatch for {name}: expected {expected}, got {actual}");
    }
    Ok(())
}

/// Copy the new binary beside the old one, then rename it over the old one. The rename is
/// atomic, and `fs::copy` keeps the archive's executable bit.
fn replace_binary(new: &Path, exe: &Path) -> Result<()> {
    let dir = exe
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", exe.display()))?;
    let staged = dir.join(format!(".drawer-update-{}", uuid::Uuid::new_v4()));
    fs::copy(new, &staged).with_context(|| {
        format!(
            "cannot write to {}; run the update as a user who can write there",
            dir.display()
        )
    })?;
    if let Err(err) = fs::rename(&staged, exe) {
        let _ = fs::remove_file(&staged);
        return Err(err).with_context(|| format!("cannot replace {}", exe.display()));
    }
    Ok(())
}

/// Swap the UI folder the same way: stage the new copy, move the old one aside, then rename
/// the new one into place. If that last rename fails, the old UI goes back.
fn replace_ui(new: &Path, ui: &Path) -> Result<()> {
    let staged = ui.with_file_name("ui.new");
    let old = ui.with_file_name("ui.old");
    let _ = fs::remove_dir_all(&staged);
    let _ = fs::remove_dir_all(&old);
    if let Err(err) = copy_dir(new, &staged) {
        let _ = fs::remove_dir_all(&staged);
        return Err(err).context("cannot stage the new web UI");
    }
    fs::rename(ui, &old).context("cannot move the old web UI aside")?;
    if let Err(err) = fs::rename(&staged, ui) {
        let _ = fs::rename(&old, ui);
        let _ = fs::remove_dir_all(&staged);
        return Err(err).context("cannot install the new web UI");
    }
    let _ = fs::remove_dir_all(&old);
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &dest)?;
        } else {
            fs::copy(entry.path(), &dest)?;
        }
    }
    Ok(())
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
        .trim_end_matches('/')
        .to_string()
}

/// A scratch directory under the system temp dir, removed when dropped. Each one is unique,
/// so concurrent checks cannot clear each other's files.
struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!("drawer-update-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).with_context(|| format!("cannot create {}", path.display()))?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(parse_version("v1.2.3").unwrap(), (1, 2, 3));
        assert_eq!(parse_version("0.1.0").unwrap(), (0, 1, 0));
        assert!(parse_version("v0.10.0").unwrap() > parse_version("0.9.0").unwrap());
        assert!(parse_version("latest").is_err());
        assert!(parse_version("v1.2").is_err());
        assert!(parse_version("v1.2.3.4").is_err());
        // A tag like this must not reach a download URL.
        assert!(parse_version("v1.2.3/../x").is_err());
    }

    #[test]
    fn checksum_must_match_the_listed_hash() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("drawer-x.tar.gz");
        fs::write(&archive, b"hello").unwrap();
        let hello = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        let sums = format!(
            "{hello}  drawer-x.tar.gz\n{}  other.tar.gz\n",
            "0".repeat(64)
        );
        verify(&archive, "drawer-x.tar.gz", &sums).unwrap();
        // `sha256sum` marks binary-mode lines with a leading asterisk.
        verify(
            &archive,
            "drawer-x.tar.gz",
            &format!("{hello} *drawer-x.tar.gz\n"),
        )
        .unwrap();

        let err = verify(
            &archive,
            "drawer-x.tar.gz",
            &format!("{}  drawer-x.tar.gz\n", "0".repeat(64)),
        )
        .unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"), "{err}");
        let err = verify(&archive, "drawer-x.tar.gz", "").unwrap_err();
        assert!(err.to_string().contains("not listed"), "{err}");
    }

    #[test]
    fn binary_is_replaced_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("drawer");
        let new = dir.path().join("new-drawer");
        fs::write(&exe, b"old").unwrap();
        fs::write(&new, b"new").unwrap();
        replace_binary(&new, &exe).unwrap();
        assert_eq!(fs::read(&exe).unwrap(), b"new");
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers.len(), 2, "{leftovers:?}");
    }

    #[test]
    fn ui_folder_is_swapped_without_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        let ui = dir.path().join("ui");
        let new = dir.path().join("new-ui/assets");
        fs::create_dir_all(ui.join("assets")).unwrap();
        fs::write(ui.join("index.html"), b"old").unwrap();
        fs::create_dir_all(&new).unwrap();
        fs::write(dir.path().join("new-ui/index.html"), b"new").unwrap();
        fs::write(new.join("app.js"), b"js").unwrap();

        replace_ui(&dir.path().join("new-ui"), &ui).unwrap();
        assert_eq!(fs::read(ui.join("index.html")).unwrap(), b"new");
        assert_eq!(fs::read(ui.join("assets/app.js")).unwrap(), b"js");
        assert!(!dir.path().join("ui.old").exists());
        assert!(!dir.path().join("ui.new").exists());
    }
}
