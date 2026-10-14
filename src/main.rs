use std::path::PathBuf;

use clap::{Parser, Subcommand};
use drawer::{mcp::DrawerMcp, service::Drawer};
use rmcp::ServiceExt;

#[derive(Parser)]
#[command(name = "drawer", about = "Drawer: versioned filesystem storage for agents")]
struct Cli {
    /// Where the database and blobs live.
    #[arg(long, env = "DRAWER_DATA_DIR", default_value = "./data", global = true)]
    data_dir: PathBuf,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve REST (/drawer/v1), MCP (/mcp) and the UI over HTTP. Default.
    Serve {
        #[arg(long, env = "DRAWER_ADDR", default_value = "127.0.0.1:8787")]
        addr: String,
        /// Require `Authorization: Bearer <token>` on every request.
        #[arg(long, env = "DRAWER_TOKEN")]
        token: Option<String>,
        /// Built UI directory (ui/dist).
        #[arg(long, env = "DRAWER_UI_DIR", default_value = "ui/dist")]
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
    let svc = Drawer::open(&cli.data_dir).await?;
    match cli.cmd.unwrap_or(Cmd::Serve {
        addr: "127.0.0.1:8787".into(),
        token: std::env::var("DRAWER_TOKEN").ok(),
        ui_dir: "ui/dist".into(),
    }) {
        Cmd::Serve { addr, token, ui_dir } => {
            let listener = tokio::net::TcpListener::bind(&addr).await?;
            tracing::info!("drawer listening on http://{addr} (data: {})", cli.data_dir.display());
            axum::serve(listener, drawer::app(svc, token, Some(ui_dir))).await?;
        }
        Cmd::Stdio => {
            let running = DrawerMcp::new(svc).serve(rmcp::transport::stdio()).await?;
            running.waiting().await?;
        }
    }
    Ok(())
}
