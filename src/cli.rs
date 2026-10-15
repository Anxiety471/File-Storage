//! The `drawer` command line: the server commands, plus file operations on a store.
//!
//! File commands open the store directly, so no server has to be running. A store is a
//! directory holding `drawer.db` and `blobs/`. Writes are serialized inside one process only,
//! so don't run CLI writes against a store while a server is taking writes on it.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::json;

use crate::error::Error;
use crate::service::{Drawer, Node, NodeUpdate, Properties, Ref, WriteOpts, WriteOutcome};

const DEFAULT_ADDR: &str = "127.0.0.1:8787";
const DB_FILE: &str = "drawer.db";

#[derive(Parser)]
#[command(
    name = "drawer",
    version,
    about = "Drawer: versioned filesystem storage for agents",
    long_about = "Drawer keeps files as versioned nodes in a store directory.\n\n\
        `drawer serve` runs the web UI, REST API and MCP endpoint. `drawer stdio` speaks MCP over \
        a pipe. The file commands (ls, put, cat, ...) work on the store directly, so they need no \
        server. Paths look like /notes/todo.md. Use `drawer mcp-config` to wire up Hermes or another client."
)]
pub struct Cli {
    /// Store directory, holding drawer.db and blobs/.
    #[arg(long, env = "DRAWER_DATA_DIR", default_value = "./data", global = true, value_name = "DIR")]
    data_dir: PathBuf,
    /// Name recorded as the author of changes. Defaults to $USER.
    #[arg(long, env = "DRAWER_AUTHOR", global = true, value_name = "NAME")]
    author: Option<String>,
    /// Print JSON instead of text, for the commands that return data.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Serve the web UI, REST API (/drawer/v1) and MCP (/mcp) over HTTP. The default.
    Serve(ServeArgs),
    /// Speak MCP over stdin and stdout, for clients that start drawer as a subprocess.
    Stdio,
    /// List a folder. Folders are shown with a trailing slash.
    Ls {
        #[arg(default_value = "/")]
        path: String,
        /// Show kind, revision, size and modified time.
        #[arg(short, long)]
        long: bool,
    },
    /// Create a folder and any missing parents.
    Mkdir { path: String },
    /// Upload a file as a new revision, creating it if needed. Use - to read stdin.
    Put(PutArgs),
    /// Print a file to stdout.
    Cat {
        path: String,
        /// Print this revision instead of the current one.
        #[arg(long, value_name = "N")]
        revision: Option<i64>,
    },
    /// Download a file. Writes to the file name in the current directory unless OUT is given.
    Get(GetArgs),
    /// Show a file or folder: its id, revision, size, type, grants and properties.
    Stat { path: String },
    /// Move a file or folder to the trash. Its id and history are kept.
    Rm { path: String },
    /// List the trash, with the ids that restore and purge take.
    Trash,
    /// Bring a trashed file or folder back, by id.
    Restore { id: String },
    /// Delete a trashed file or folder for good, by id. Needs --yes.
    Purge {
        id: String,
        #[arg(long)]
        yes: bool,
    },
    /// Rename or move a file or folder. Moving onto a folder puts it inside. Keeps the id.
    Mv { from: String, to: String },
    /// Copy a file to a new path. Shares the stored bytes, so nothing is uploaded again.
    Cp {
        from: String,
        to: String,
        /// Copy this revision instead of the current one.
        #[arg(long, value_name = "N")]
        revision: Option<i64>,
    },
    /// List a file's revisions, newest first.
    History { path: String },
    /// Make an older revision the current one. Adds a revision, so nothing is lost.
    Rollback {
        path: String,
        revision: i64,
        #[arg(short, long, default_value = "")]
        comment: String,
    },
    /// Search live file names and the text of file contents.
    Search {
        query: String,
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
    },
    /// Star a file or folder.
    Star { path: String },
    /// Remove a star.
    Unstar { path: String },
    /// List starred files and folders.
    Starred,
    /// List who has been granted access to a file or folder. No grants means open.
    Perms { path: String },
    /// Grant a principal a role (reader, writer or owner) on a node and everything below it.
    Share {
        path: String,
        principal: String,
        role: String,
    },
    /// Remove a principal's grant on a node.
    Unshare { path: String, principal: String },
    /// Check the store: database integrity, counts, blob files and the web UI.
    Doctor,
    /// Print an MCP config entry that starts this drawer on this store.
    McpConfig {
        #[arg(long, value_enum, default_value_t = McpFormat::Hermes)]
        format: McpFormat,
    },
}

#[derive(Args)]
pub struct ServeArgs {
    /// Address to listen on. Use 0.0.0.0:PORT to accept connections from other machines.
    #[arg(long, env = "DRAWER_ADDR", default_value = DEFAULT_ADDR)]
    addr: String,
    /// Require `Authorization: Bearer <token>` on every request. An empty value counts as unset.
    #[arg(long, env = "DRAWER_TOKEN")]
    token: Option<String>,
    /// Built UI directory. Defaults to ./ui/dist, then ../share/drawer/ui beside the binary.
    #[arg(long, env = "DRAWER_UI_DIR")]
    ui_dir: Option<PathBuf>,
}

impl ServeArgs {
    /// What `drawer` with no subcommand runs: serve, with the same settings as the flags.
    fn from_env() -> Self {
        Self {
            addr: std::env::var("DRAWER_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.into()),
            token: std::env::var("DRAWER_TOKEN").ok(),
            ui_dir: std::env::var_os("DRAWER_UI_DIR").map(PathBuf::from),
        }
    }
}

#[derive(Args)]
pub struct PutArgs {
    /// Local file to upload, or - for stdin.
    file: PathBuf,
    /// Destination in Drawer. A folder, or a path ending in /, takes the file name inside it.
    /// Defaults to the file name at the root, which stdin cannot use.
    dest: Option<String>,
    /// Write only while the file is at this revision. 0 means create only, never overwrite.
    #[arg(long, value_name = "N")]
    if_revision: Option<i64>,
    /// Note stored with the revision.
    #[arg(short, long, default_value = "")]
    comment: String,
    /// Property stored with the revision, as key=value. Repeatable.
    #[arg(long = "prop", value_name = "KEY=VALUE", value_parser = parse_prop)]
    props: Vec<(String, String)>,
}

#[derive(Args)]
pub struct GetArgs {
    /// Drawer path to download.
    path: String,
    /// Local file to write. Defaults to the file name. Use - for stdout.
    out: Option<PathBuf>,
    /// Download this revision instead of the current one.
    #[arg(long, value_name = "N")]
    revision: Option<i64>,
    /// Overwrite the local file if it exists.
    #[arg(long)]
    force: bool,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum McpFormat {
    /// YAML for ~/.hermes/config.yaml.
    Hermes,
    /// JSON with an mcpServers object, for Claude Desktop, Cursor and most other clients.
    Json,
}

fn parse_prop(s: &str) -> std::result::Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .ok_or_else(|| format!("expected key=value, got {s:?}"))
}

/// Entry point for the `drawer` binary. Prints errors to stderr and exits 1.
pub async fn run() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("drawer: {err:#}");
            ExitCode::FAILURE
        }
    }
}

async fn dispatch(cli: Cli) -> Result<()> {
    let cmd = cli.cmd.unwrap_or_else(|| Cmd::Serve(ServeArgs::from_env()));
    if let Cmd::McpConfig { format } = &cmd {
        print!("{}", mcp_config(*format, &cli.data_dir)?);
        return Ok(());
    }
    if matches!(cmd, Cmd::Serve(_) | Cmd::Stdio) {
        // Logs go to stderr so stdio MCP keeps stdout clean.
        tracing_subscriber::fmt().with_writer(std::io::stderr).init();
    }
    // Only these start a store. Everything else needs one already there, so a typo in
    // --data-dir fails loudly instead of quietly creating an empty store.
    let creates = matches!(cmd, Cmd::Serve(_) | Cmd::Stdio | Cmd::Put(_) | Cmd::Mkdir { .. });
    if !creates && !cli.data_dir.join(DB_FILE).exists() {
        bail!(
            "no store at {}; set --data-dir or DRAWER_DATA_DIR, or start one with `drawer serve` or `drawer put`",
            absolute(&cli.data_dir).display()
        );
    }

    let svc = Drawer::open(&cli.data_dir).await?;
    let actor = cli.author.unwrap_or_else(default_author);
    let json = cli.json;
    match cmd {
        Cmd::Serve(args) => serve(svc, &cli.data_dir, args).await,
        Cmd::Stdio => {
            use rmcp::ServiceExt;
            let running = crate::mcp::DrawerMcp::new(svc).serve(rmcp::transport::stdio()).await?;
            running.waiting().await?;
            Ok(())
        }
        Cmd::Ls { path, long } => {
            let nodes = svc.list(&path).await?;
            if json {
                return emit_json(&nodes);
            }
            for n in &nodes {
                if long {
                    println!(
                        "{:<7} {:>4} {:>9}  {}  {}",
                        n.kind,
                        revision_label(n),
                        size_label(n),
                        short_time(&n.modified_at),
                        display_name(n)
                    );
                } else {
                    println!("{}", display_name(n));
                }
            }
            Ok(())
        }
        Cmd::Mkdir { path } => {
            let node = svc.mkdir(&path, &actor).await?;
            if json { emit_json(&node) } else { println!("{}", node.path); Ok(()) }
        }
        Cmd::Put(args) => {
            let (dest, data) = if args.file == Path::new("-") {
                // Stdin has no file name, so the destination must name the file itself.
                let dest = args
                    .dest
                    .clone()
                    .filter(|d| !d.ends_with('/'))
                    .ok_or_else(|| anyhow!("uploading from stdin needs a destination file path, like /notes/todo.md"))?;
                let mut buf = Vec::new();
                std::io::stdin().read_to_end(&mut buf).context("reading stdin")?;
                (dest, buf)
            } else {
                let name = args
                    .file
                    .file_name()
                    .and_then(|n| n.to_str())
                    .ok_or_else(|| anyhow!("{} has no usable file name", args.file.display()))?;
                let dest = target_path(&svc, args.dest.as_deref().unwrap_or("/"), name).await?;
                let data = tokio::fs::read(&args.file).await.with_context(|| format!("reading {}", args.file.display()))?;
                (dest, data)
            };
            let opts = write_opts(&actor, &args.comment, args.props, args.if_revision);
            let out = svc.write(&dest, &data, opts).await?;
            report_write(&out, json)
        }
        Cmd::Cat { path, revision } => {
            let (_, data) = svc.read(Ref::Path(&path), revision).await?;
            std::io::stdout().write_all(&data)?;
            Ok(())
        }
        Cmd::Get(args) => {
            let (rev, data) = svc.read(Ref::Path(&args.path), args.revision).await?;
            match args.out.as_deref() {
                Some(out) if out == Path::new("-") => {
                    std::io::stdout().write_all(&data)?;
                }
                other => {
                    let out = other.map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from(leaf(&args.path)));
                    if out.exists() && !args.force {
                        bail!("{} already exists; pass --force to overwrite it", out.display());
                    }
                    tokio::fs::write(&out, &data).await.with_context(|| format!("writing {}", out.display()))?;
                    println!("saved {} (revision {}, {}) to {}", args.path, rev.revision, human_size(data.len() as i64), out.display());
                }
            }
            Ok(())
        }
        Cmd::Stat { path } => {
            let node = svc.stat(Ref::Path(&path)).await?;
            if json {
                return emit_json(&node);
            }
            print_stat(&node);
            Ok(())
        }
        Cmd::Rm { path } => {
            let node = svc.trash(Ref::Path(&path), None, &actor).await?;
            if json { emit_json(&node) } else { println!("trashed {} (id {})", node.path, node.id); Ok(()) }
        }
        Cmd::Trash => {
            let nodes = svc.trash_list().await?;
            if json {
                return emit_json(&nodes);
            }
            if nodes.is_empty() {
                println!("trash is empty");
            }
            for n in &nodes {
                println!("{}  {:<7} {}", n.id, n.kind, n.path);
            }
            Ok(())
        }
        Cmd::Restore { id } => {
            let node = svc.update(&id, NodeUpdate { trashed: Some(false), ..NodeUpdate::default() }, &actor).await?;
            if json { emit_json(&node) } else { println!("restored {}", node.path); Ok(()) }
        }
        Cmd::Purge { id, yes } => {
            if !yes {
                bail!("purge deletes {id} for good; pass --yes to confirm");
            }
            let node = svc.stat(Ref::Id(&id)).await?;
            svc.purge(&id, &actor).await?;
            println!("purged {} (id {})", node.path, id);
            Ok(())
        }
        Cmd::Mv { from, to } => {
            let dest = target_path(&svc, &to, leaf(&from)).await?;
            let node = svc.move_to(Ref::Path(&from), &dest, &actor).await?;
            if json { emit_json(&node) } else { println!("{from} -> {}", node.path); Ok(()) }
        }
        Cmd::Cp { from, to, revision } => {
            let dest = target_path(&svc, &to, leaf(&from)).await?;
            let out = svc.copy(Ref::Path(&from), &dest, revision, write_opts(&actor, "", Vec::new(), None)).await?;
            report_write(&out, json)
        }
        Cmd::History { path } => {
            let revs = svc.revisions(Ref::Path(&path)).await?;
            if json {
                return emit_json(&revs);
            }
            println!("{:>4}  {:<16}  {:<12} {:>9}  COMMENT", "REV", "WHEN", "AUTHOR", "SIZE");
            for r in &revs {
                println!(
                    "{:>4}  {:<16}  {:<12} {:>9}  {}",
                    r.revision,
                    short_time(&r.created_at),
                    r.author,
                    human_size(r.size),
                    r.comment
                );
            }
            Ok(())
        }
        Cmd::Rollback { path, revision, comment } => {
            let out = svc.rollback(Ref::Path(&path), revision, write_opts(&actor, &comment, Vec::new(), None)).await?;
            report_write(&out, json)
        }
        Cmd::Search { query, limit } => {
            let hits = svc.search(&query, limit).await?;
            if json {
                return emit_json(&hits);
            }
            if hits.is_empty() {
                println!("no matches for {query:?}");
            }
            for h in &hits {
                match h.matched {
                    "name" => println!("{}  (name)", h.path),
                    _ => println!("{}: {}", h.path, h.snippet),
                }
            }
            Ok(())
        }
        Cmd::Star { path } => set_starred(&svc, &path, true, &actor, json).await,
        Cmd::Unstar { path } => set_starred(&svc, &path, false, &actor, json).await,
        Cmd::Starred => {
            let nodes = svc.starred().await?;
            if json {
                return emit_json(&nodes);
            }
            if nodes.is_empty() {
                println!("nothing starred");
            }
            for n in &nodes {
                println!("{}", n.path);
            }
            Ok(())
        }
        Cmd::Perms { path } => {
            let node = svc.stat(Ref::Path(&path)).await?;
            let grants = svc.permissions(&node.id).await?;
            if json {
                return emit_json(&grants);
            }
            if grants.is_empty() {
                println!("no grants on {path}: open to anyone with store access");
            }
            for g in &grants {
                println!("{:<24} {}", g.principal, g.role);
            }
            Ok(())
        }
        Cmd::Share { path, principal, role } => {
            let node = svc.stat(Ref::Path(&path)).await?;
            let grants = svc.share(&node.id, &principal, &role, &actor).await?;
            if json { emit_json(&grants) } else { println!("{principal} is now {role} on {}", node.path); Ok(()) }
        }
        Cmd::Unshare { path, principal } => {
            let node = svc.stat(Ref::Path(&path)).await?;
            let grants = svc.unshare(&node.id, &principal, &actor).await?;
            if json { emit_json(&grants) } else { println!("removed {principal} from {}", node.path); Ok(()) }
        }
        Cmd::Doctor => doctor(&svc, &cli.data_dir, json).await,
        Cmd::McpConfig { .. } => unreachable!("handled before the store is opened"),
    }
}

async fn serve(svc: Drawer, data_dir: &Path, args: ServeArgs) -> Result<()> {
    let token = args.token.filter(|t| !t.is_empty());
    let ui_dir = match args.ui_dir {
        Some(dir) => {
            if !dir.join("index.html").exists() {
                bail!("--ui-dir {} has no index.html; build the UI with `npm run build` in ui/", dir.display());
            }
            Some(dir)
        }
        None => default_ui_dir(),
    };
    let listener = tokio::net::TcpListener::bind(&args.addr)
        .await
        .with_context(|| format!("cannot listen on {}", args.addr))?;
    let bound = listener.local_addr()?;
    let url = format!("http://{bound}");
    eprintln!("drawer {} serving {url}", env!("CARGO_PKG_VERSION"));
    eprintln!("  store  {}", absolute(data_dir).display());
    eprintln!("  mcp    {url}/mcp");
    match &ui_dir {
        Some(dir) => eprintln!("  ui     {}", dir.display()),
        None => eprintln!("  ui     not built; `cd ui && npm run build` to add it"),
    }
    if token.is_some() {
        eprintln!("  auth   bearer token required");
    } else if bound.ip().is_loopback() {
        eprintln!("  auth   none (loopback only)");
    } else {
        eprintln!("  warning: no token set and listening beyond loopback; anyone who can reach this address can read and write");
    }
    axum::serve(listener, crate::app(svc, token, ui_dir)).await?;
    Ok(())
}

/// The UI is optional: ./ui/dist from a checkout, else the share directory the installer uses.
fn default_ui_dir() -> Option<PathBuf> {
    let local = PathBuf::from("ui/dist");
    if local.join("index.html").exists() {
        return Some(local);
    }
    let exe = std::env::current_exe().ok()?;
    let shared = exe.parent()?.join("../share/drawer/ui");
    shared.join("index.html").exists().then_some(shared)
}

fn mcp_config(format: McpFormat, data_dir: &Path) -> Result<String> {
    let exe = std::env::current_exe().context("finding the drawer binary")?;
    let exe = exe.display().to_string();
    let data = absolute(data_dir).display().to_string();
    Ok(match format {
        McpFormat::Hermes => format!(
            "# Add under mcp_servers in ~/.hermes/config.yaml\n\
             mcp_servers:\n  drawer:\n    command: {}\n    args: [\"--data-dir\", {}, \"stdio\"]\n",
            json!(exe),
            json!(data)
        ),
        McpFormat::Json => {
            let cfg = json!({ "mcpServers": { "drawer": { "command": exe, "args": ["--data-dir", data, "stdio"] } } });
            format!("{}\n", serde_json::to_string_pretty(&cfg)?)
        }
    })
}

async fn doctor(svc: &Drawer, data_dir: &Path, json: bool) -> Result<()> {
    let integrity = svc.integrity_check().await?;
    let stats = svc.stats().await?;
    let missing = svc.missing_blobs().await?;
    let ui = default_ui_dir();
    let healthy = integrity == "ok" && missing.is_empty();
    if json {
        emit_json(&json!({
            "healthy": healthy,
            "store": absolute(data_dir),
            "integrity": integrity,
            "stats": stats,
            "missing_blobs": missing,
            "ui": ui,
        }))?;
    } else {
        println!("store      {}", absolute(data_dir).display());
        println!("database   {integrity}");
        println!(
            "contents   {} files, {} folders, {} trashed, {} revisions",
            stats.files, stats.folders, stats.trashed, stats.revisions
        );
        if missing.is_empty() {
            println!("blobs      all revision blobs present");
        } else {
            println!("blobs      {} missing, e.g. {}", missing.len(), missing[0]);
        }
        match &ui {
            Some(dir) => println!("web UI     {}", dir.display()),
            None => println!("web UI     not built (serve still works without it)"),
        }
        println!("{}", if healthy { "healthy" } else { "problems found" });
    }
    if healthy { Ok(()) } else { Err(anyhow!("store is not healthy")) }
}

async fn set_starred(svc: &Drawer, path: &str, starred: bool, actor: &str, json: bool) -> Result<()> {
    let node = svc.stat(Ref::Path(path)).await?;
    let node = svc.update(&node.id, NodeUpdate { starred: Some(starred), ..NodeUpdate::default() }, actor).await?;
    if json {
        emit_json(&node)
    } else {
        println!("{} {}", if starred { "starred" } else { "unstarred" }, node.path);
        Ok(())
    }
}

/// Where an upload or copy lands. A destination that ends in `/`, or names an existing folder,
/// takes the item's own name inside it. Anything else is the full destination path.
async fn target_path(svc: &Drawer, dest: &str, name: &str) -> Result<String> {
    if dest.ends_with('/') {
        return Ok(format!("{dest}{name}"));
    }
    match svc.stat(Ref::Path(dest)).await {
        Ok(node) if node.kind == "folder" => Ok(format!("{}/{name}", node.path.trim_end_matches('/'))),
        Ok(_) | Err(Error::NotFound(_)) => Ok(dest.to_string()),
        Err(e) => Err(e.into()),
    }
}

fn write_opts(actor: &str, comment: &str, props: Vec<(String, String)>, if_head_revision: Option<i64>) -> WriteOpts {
    let properties: Properties = props.into_iter().collect();
    WriteOpts { author: actor.into(), comment: comment.into(), properties, if_head_revision }
}

fn report_write(out: &WriteOutcome, json: bool) -> Result<()> {
    if json {
        return emit_json(out);
    }
    if out.new_revision {
        println!("{} saved as revision {}", out.node.path, out.revision);
    } else {
        println!("{} unchanged, still revision {}", out.node.path, out.revision);
    }
    Ok(())
}

fn print_stat(n: &Node) {
    let row = |label: &str, value: String| println!("  {label:<10}{value}");
    println!("{}", n.path);
    row("id", n.id.clone());
    row("kind", n.kind.to_string());
    if let Some(rev) = n.head_revision {
        row("revision", rev.to_string());
    }
    if let Some(size) = n.size {
        row("size", human_size(size));
    }
    if let Some(mime) = &n.mime {
        row("type", mime.clone());
    }
    row("owner", n.owner.clone());
    row("modified", short_time(&n.modified_at));
    row("created", short_time(&n.created_at));
    row("starred", yes_no(n.starred));
    row("trashed", yes_no(n.trashed));
    for (k, v) in &n.properties {
        row("prop", format!("{k}={v}"));
    }
}

fn emit_json<T: Serialize>(v: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn yes_no(b: bool) -> String {
    if b { "yes" } else { "no" }.to_string()
}

fn display_name(n: &Node) -> String {
    if n.kind == "folder" { format!("{}/", n.name) } else { n.name.clone() }
}

fn revision_label(n: &Node) -> String {
    n.head_revision.map_or("-".to_string(), |r| r.to_string())
}

fn size_label(n: &Node) -> String {
    n.size.map_or("-".to_string(), human_size)
}

/// `2026-10-15T01:40:00.000Z` as `2026-10-15 01:40`.
fn short_time(ts: &str) -> String {
    ts.replace('T', " ").chars().take(16).collect()
}

fn human_size(bytes: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1} {}", UNITS[unit])
}

/// The last segment of a Drawer path, ignoring a trailing slash.
fn leaf(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or(path)
}

fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

fn default_author() -> String {
    std::env::var("USER").ok().filter(|u| !u.is_empty()).unwrap_or_else(|| "cli".into())
}
