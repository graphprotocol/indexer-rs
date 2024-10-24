use std::process::ExitCode;

use local_service::bootstrap::start_server;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt::init();
    if let Err(e) = start_server().await {
        tracing::error!("Local Service error: {e}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
