use actix_cors::Cors;
use actix_web::{
    body::{BoxBody, EitherBody},
    get, post,
    test::TestRequest,
    web::{self, Bytes, Data},
    App, HttpResponse, HttpServer, Responder, middleware,
};
use util::package_version;
use std::{error::Error, time::Duration, sync::Arc, env};
use actix_ratelimit::{RateLimiter, MemoryStore, MemoryStoreActor};

use crate::query_processor::{FreeQuery, QueryProcessor, SubgraphDeploymentID};
use server::{ServerOptions, index, subgraph_queries, network_queries};
use common::database::create_pg_pool;

mod query_processor;
mod query_fee;
mod server;
mod receipt_collector;
mod common;
mod util;

/// Create Indexer service App
///
/// Initialization for server and Query processor
///
/// Validate that graph-node instance is running for Query processor
/// Validate that server is running with a health check
///
/// Parse Requests received
///
/// Route the requests as a FreeQuery
///
/// Return response from Query Processor
#[actix_web::main]
async fn main() -> Result<(), std::io::Error> {
    env_logger::init();

    let database_url = env::var("DATABASE_URL").expect("Postgres URL required.");
    // Create config for user input args
    // DATABASE_URL required for using query_as macro
    let pg_pool = create_pg_pool(&database_url);
    let arc_pool = Arc::new(pg_pool);

    let connection = arc_pool.get().expect("Failed to get connection from pool");
    let release  = package_version().expect("Failed to resolve for release version");

    // Proper initiation of server, query processor
    // server health check, graph-node instance connection check
    let query_processor = QueryProcessor::new("http://127.0.0.1:8000", "https://api.thegraph.com/subgraphs/name/graphprotocol/graph-network-goerli");

    let service_options = ServerOptions::new(
        Some(8080),
        release,
        query_processor,
        Some("free_query_auth_token".to_string()),
        "graph_node_status_endpoint".to_string(),
        "operator_public_key".to_string(),
        Some("network_subgraph_auth_token".to_string()),
        true
    );
    
    // secure App, add logger, 
    HttpServer::new(move || {
        App::new()
            .wrap(middleware::Logger::default())
            .wrap(
                Cors::default()
                    .send_wildcard()
                    .allowed_methods(vec!["GET", "POST"])
                    .allowed_headers(vec![header::CONTENT_TYPE, header::ACCEPT])
                    .max_age(3600),
            )
            .app_data(Data::new(service_options.clone()))
            .service(index)
            .service(subgraph_queries)
            .service(network_queries)
            .default_service(web::to(|| HttpResponse::NotFound()))
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await
}

