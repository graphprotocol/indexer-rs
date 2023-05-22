// use actix_web::{web, App, HttpRequest, HttpResponse, HttpServer};
// use async_graphql::http::{playground_source, GraphQLPlaygroundConfig};
// use async_graphql_actix_web::{graphql, graphql_subscription, Request, Response};

// pub struct StatusServerOptions {
//     graph_node_status_endpoint: String,
// }

// pub async fn create_status_server(
//     options: StatusServerOptions,
// ) -> impl Fn(HttpRequest) -> Result<HttpResponse, actix_web::Error> {
//     let schema = {
//         let endpoint = options.graph_node_status_endpoint;
//         let response = reqwest::Client::new()
//             .post(&endpoint)
//             .header("Accept", "application/json")
//             .send()
//             .await
//             .expect("Failed to fetch schema");

//         response.text().await.expect("Failed to read schema")
//     };

//     let supported_root_fields = vec![
//         "indexingStatuses",
//         "publicProofsOfIndexing",
//         "entityChangesInBlock",
//         "blockData",
//         "cachedEthereumCalls",
//         "subgraphFeatures",
//         "apiVersions",
//     ];

//     let filtered_schema = schema_filter_root_fields(schema, supported_root_fields);

//     let endpoint = move |req: HttpRequest, payload: web::Payload| {
//         let endpoint = options.graph_node_status_endpoint.clone();
//         let filtered_schema = filtered_schema.clone();
//         let config = GraphQLPlaygroundConfig::new("/graphql").subscription_endpoint("/subscriptions");

//         if req.headers().get("content-type").map(|ct| ct == "text/html").unwrap_or(false) {
//             let html = playground_source(config);
//             Ok(HttpResponse::Ok()
//                 .content_type("text/html; charset=utf-8")
//                 .body(html))
//         } else {
//             let endpoint = endpoint.clone();
//             let schema = filtered_schema.clone();

//             let request = async_graphql::http::receive_request(req.head(), payload).await?;

//             let response = schema.execute(request.into_inner()).await;

//             let body = serde_json::to_string(&response)?;

//             Ok(HttpResponse::Ok()
//                 .content_type("application/json")
//                 .body(body))
//         }
//     };

//     endpoint
// }

// fn schema_filter_root_fields(
//     schema: String,
//     supported_root_fields: Vec<&str>,
// ) -> async_graphql::Schema<QueryRoot, async_graphql::EmptyMutation, async_graphql::EmptySubscription>
// {
//     let schema = async_graphql::Schema::build(QueryRoot, async_graphql::EmptyMutation, async_graphql::EmptySubscription)
//         .data(()) // Set the context data
//         .finish();

//     let filtered_schema = schema.filter_fields(|_, _, field| {
//         let field_name = field.name().to_string();
//         supported_root_fields.contains(&field_name.as_str())
//     });

//     filtered_schema
// }

// struct QueryRoot;

// #[async_graphql::Object]
// impl QueryRoot {
//     async fn indexing_statuses(&self) -> Vec<IndexingStatus> {
//         // Your implementation for the `indexingStatuses` field
//         unimplemented!()
//     }

//     async fn public_proofs_of_indexing(&self) -> Vec<PublicProofOfIndexing> {
//         // Your implementation for the `publicProofsOfIndexing` field
//         unimplemented!()
//     }

//     // Implement other fields...
// }
