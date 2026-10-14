use std::path::PathBuf;

use clap::{Parser, Subcommand};
use mnemo::{mcp::MnemoMcp, service::Mnemo};
use rmcp::ServiceExt;

#[derive(Parser)]
#[command(name = "mnemo", about = "Mnemosyne: versioned filesystem storage for agents")]
struct Cli {
    /// Where the database and blobs live.
    #[arg(long, env = "MNEMO_DATA_DIR", default_value = "./data", global = true)]
    data_dir: PathBuf,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve REST (/mnemo/v1), MCP (/mcp) and the UI over HTTP. Default.
    Serve {
        #[arg(long, env = "MNEMO_ADDR", default_value = "127.0.0.1:8787")]
        addr: String,
        /// Require `Authorization: Bearer <token>` on every request.
        #[arg(long, env = "MNEMO_TOKEN")]
        token: Option<String>,
        /// Built UI directory (ui/dist).
        #[arg(long, env = "MNEMO_UI_DIR", default_value = "ui/dist")]
        ui_dir: PathBuf,
    },
    /// Speak MCP over stdin/stdout, for clients that spawn the server.
    Stdio,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    // Logs go to stderr so stdio MCP stays clean.
    tracing_subscriber::fmt().with_writer(std::io::stderr).init();
    let svc = Mnemo::open(&cli.data_dir).await?;
    match cli.cmd.unwrap_or(Cmd::Serve {
        addr: "127.0.0.1:8787".into(),
        token: std::env::var("MNEMO_TOKEN").ok(),
        ui_dir: "ui/dist".into(),
    }) {
        Cmd::Serve { addr, token, ui_dir } => {
            let listener = tokio::net::TcpListener::bind(&addr).await?;
            tracing::info!("mnemo listening on http://{addr} (data: {})", cli.data_dir.display());
            axum::serve(listener, mnemo::app(svc, token, Some(ui_dir))).await?;
        }
        Cmd::Stdio => {
            let running = MnemoMcp::new(svc).serve(rmcp::transport::stdio()).await?;
            running.waiting().await?;
        }
    }
    Ok(())
}
