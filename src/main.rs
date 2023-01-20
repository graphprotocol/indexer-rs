use actix_web::{
    body::{BoxBody, EitherBody},
    get, post,
    test::TestRequest,
    web::{self, Bytes, Data},
    App, HttpResponse, HttpServer, Responder, middleware,
};
use futures::StreamExt;
use reqwest::StatusCode;
use std::{error::Error, time::Duration};
use actix_ratelimit::{RateLimiter, MemoryStore, MemoryStoreActor};

use crate::query_processor::{FreeQuery, QueryProcessor, SubgraphDeploymentID};
use server::{ServerOptions, index, subgraph_queries, network_queries};
mod query_processor;
mod server;

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
    // let store = MemoryStore::new();
    // let ratelimiter = RateLimiter::new(
    //                         MemoryStoreActor::from(store.clone()).start())
    //                     .with_interval(Duration::from_secs(60))
    //                     .with_max_requests(100);
    

    // Proper initiation of server, query processor
    // server health check, graph-node instance connection check
    let query_processor = QueryProcessor::new("http://127.0.0.1:8000", "https://api.thegraph.com/subgraphs/name/graphprotocol/graph-network-goerli");

    let service_options = ServerOptions::new(
        Some(8080),
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

