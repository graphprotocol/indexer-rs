use crate::query_processor::{QueryProcessor, SubgraphDeploymentID, FreeQuery};
use actix_web::{
    body::{BoxBody, EitherBody},
    get, post,
    test::TestRequest,
    web::{self, Bytes, Data},
    App, HttpResponse, HttpServer, Responder,
};
use reqwest::StatusCode;

/// Endpoint for health checks
#[get("/")]
pub async fn index() -> HttpResponse {
    let responder = "Ready to roll!".customize().with_status(StatusCode::OK);
    let request = TestRequest::default().to_http_request();
    let response = responder.respond_to(&request).map_into_boxed_body();
    response
}

/// ADD: Endpoint for version, Endpoint for the public status API
/// public cost API, operator information
/// subgraph health checks, network subgraph queries

// Endpoint for subgraph queries
#[post("/subgraphs/id/{id}")]
pub async fn subgraph_queries(
    server: web::Data<ServerOptions>,
    id: web::Path<String>,
    query: web::Bytes,
) -> impl Responder {
    let query_string = String::from_utf8(query.to_vec()).expect("Could not accept JSON body");

    // Initialize id into a subgraph deployment ID and make a freeQuery to graph node
    let subgraph_deployment_id = SubgraphDeploymentID::new(id.clone());
    let free_query = FreeQuery {
        subgraph_deployment_id,
        query: query_string,
    };

    let res = server.query_processor
        .execute_free_query(free_query)
        .await
        .expect("Failed to execute free query");
    // take response and send back as json
    match res.status {
        200 => HttpResponse::Ok()
            .content_type("application/json")
            .body(res.result.graphQLResponse),
        _ => HttpResponse::BadRequest()
            .content_type("application/json")
            .body("Bad subgraph query".to_string()),
    }
}

// Endpoint for subgraph queries
#[post("/network")]
pub async fn network_queries(
    server: web::Data<ServerOptions>,
    query: web::Bytes,
) -> impl Responder {
    println!("routing");
    let query_string = String::from_utf8(query.to_vec()).expect("Could not accept JSON body");

    // make query to network subgraph - should have endpoint as an input from the environmental variable but just use as a constant here for now
    let request = server.query_processor.execute_network_free_query(
        query_string,
    ).await.expect("Failed to execute free network subgraph query");
    
    // take response and send back as json
    match request.status {
        200 => HttpResponse::Ok()
            .content_type("application/json")
            .body(request.result.graphQLResponse),
        _ => HttpResponse::BadRequest()
            .content_type("application/json")
            .body("Bad subgraph query".to_string()),
    }
}

#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub port: Option<u32>,
    pub query_processor: QueryProcessor,
    pub free_query_auth_token: Option<String>,
    pub graph_node_status_endpoint: String,
    // pub indexer_management_client: IndexerManagementClient,
    pub operator_public_key: String,
    // pub network_subgraph: NetworkSubgraph,
    pub network_subgraph_auth_token: Option<String>,
    pub serve_network_subgraph: bool,
}

impl ServerOptions {
    pub fn new(
        port: Option<u32>,
        query_processor: QueryProcessor,
        free_query_auth_token: Option<String>,
        graph_node_status_endpoint: String,
        operator_public_key: String,
        network_subgraph_auth_token: Option<String>,
        serve_network_subgraph: bool,
    ) -> Self {
        ServerOptions { port, query_processor, free_query_auth_token, graph_node_status_endpoint, operator_public_key, network_subgraph_auth_token, serve_network_subgraph }
    }
}
