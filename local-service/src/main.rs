use tokio::net::TcpListener;

const HOST_PORT: &str = "0.0.0.0:8000";

#[tokio::main]
async fn main() {
    let listener = TcpListener::bind(HOST_PORT)
        .await
        .expect("Failed to bind to host and port");

    axum::serve(listener, axum::Router::new())
        .await
        .expect("Failed to start server");
}
