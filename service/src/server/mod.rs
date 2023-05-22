use crate::{query_processor::{QueryProcessor, SubgraphDeploymentID, FreeQuery}, util::PackageVersion};

pub mod routes;
// mod status_server;


// // Endpoint for the public status API
// #[get("/status")]
// async fn status(
//     server: web::Data<ServerOptions>,
//     // graph_node_status_endpoint: Data<GraphStatusEndpoint>,
//     query: web::Bytes,
// ) -> impl Responder {
//     // Implementation for creating status server
//     // Replace `createStatusServer` with your logic
//     match response {
//         Ok(result) => HttpResponse::Ok().json(result),
//         Err(error) => HttpResponse::InternalServerError().json(error),
//     }
// }

// // Endpoint for subgraph health checks
// #[post("/subgraphs/health")]
// async fn subgraph_health(
//     graph_node_status_endpoint: Data<GraphStatusEndpoint>,
// ) -> impl Responder {
//     // Implementation for creating deployment health server
//     // Replace `createDeploymentHealthServer` with your logic
//     let response = createDeploymentHealthServer(graph_node_status_endpoint.get_ref()).await;
//     match response {
//         Ok(result) => HttpResponse::Ok().json(result),
//         Err(error) => HttpResponse::InternalServerError().json(error),
//     }
// }

// // Endpoint for the public cost API
// #[post("/cost")]
// async fn cost(
//     indexer_management_client: Data<IndexerManagementClient>,
//     metrics: Data<Metrics>,
//     payload: Json<CostPayload>,
// ) -> impl Responder {
//     // Implementation for creating cost server
//     // Replace `createCostServer` with your logic
//     let response = createCostServer(indexer_management_client.get_ref(), metrics.get_ref(), payload.into_inner()).await;
//     match response {
//         Ok(result) => HttpResponse::Ok().json(result),
//         Err(error) => HttpResponse::InternalServerError().json(error),
//     }
// }

// // Endpoint for operator information
// #[post("/operator")]
// async fn operator_info(
//     operator_public_key: Data<OperatorPublicKey>,
//     payload: Json<OperatorInfoPayload>,
// ) -> impl Responder {
//     // Implementation for creating operator server
//     // Replace `createOperatorServer` with your logic
//     let response = createOperatorServer(operator_public_key.get_ref(), payload.into_inner()).await;
//     match response {
//         Ok(result) => HttpResponse::Ok().json(result),
//         Err(error) => HttpResponse::InternalServerError().json(error),
//     }
// }

/// ADD: Endpoint for version, Endpoint for the public status API
/// public cost API, operator information
/// subgraph health checks, network subgraph queries



#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub port: Option<u32>,
    pub release: PackageVersion,
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
        release: PackageVersion,
        query_processor: QueryProcessor,
        free_query_auth_token: Option<String>,
        graph_node_status_endpoint: String,
        operator_public_key: String,
        network_subgraph_auth_token: Option<String>,
        serve_network_subgraph: bool,
    ) -> Self {
        ServerOptions { port, release, query_processor, free_query_auth_token, graph_node_status_endpoint, operator_public_key, network_subgraph_auth_token, serve_network_subgraph }
    }
}
