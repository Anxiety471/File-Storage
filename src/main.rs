use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    drawer::cli::run().await
}
