use std::net::SocketAddr;

use tap_aggregator::grpc::{tap_aggregator_server::{TapAggregator, TapAggregatorServer}, RavRequest, RavResponse};
use tokio::sync::oneshot;
use tonic::{transport::Server, Request, Response, Status};

#[derive(Default)]
pub struct MockTapAggregatorService;

#[tonic::async_trait]
impl TapAggregator for MockTapAggregatorService {
    async fn aggregate_receipts(
        &self,
        _request: Request<RavRequest>,
    ) -> Result<Response<RavResponse>, Status> {
        // Do nothing, just return an unimplemented response
        Err(Status::unimplemented("This is a mock service"))
    }
}


const DUMMY_ADDR: &str = "127.0.0.1:1234"; // Correct format for SocketAddr
/// Starts a mock TapAggregatorServer for testing purposes and returns a shutdown handle
pub async fn mock_tap_aggregator_server() -> Result<oneshot::Sender<()>, Box<dyn std::error::Error>> {
    let address: SocketAddr = DUMMY_ADDR
        .parse()
        .expect("Failed to parse DUMMY_ADDR as SocketAddr");

    println!("Starting TapAggregatorServer on {}", DUMMY_ADDR);

    // Create the blank mock service
    let service = MockTapAggregatorService::default();

    // Create a shutdown channel
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    // Spawn the server in a separate task
    tokio::spawn(async move {
        if let Err(e) = Server::builder()
            .add_service(TapAggregatorServer::new(service))
            .serve_with_shutdown(address, async {
                shutdown_rx.await.ok(); // Wait for the shutdown signal
            })
            .await
        {
            eprintln!("TapAggregatorServer failed: {}", e);
        }
    });

    Ok(shutdown_tx)
}


#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Start the mock server

    let shutdown_tx = mock_tap_aggregator_server().await?;

    println!("Mock  server running. Press Ctrl+C to stop.");

    // Wait until the user decides to terminate the process
    tokio::signal::ctrl_c().await.expect("Failed to listen for Ctrl+C");

    // Shut down the server gracefully
    shutdown_tx.send(()).ok();
    println!("Mock server shutting down.");

    Ok(())
}
