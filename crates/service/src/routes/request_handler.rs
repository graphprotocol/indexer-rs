// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use axum::{
    extract::{Path, State},
    http::{HeaderValue, Response},
    response::IntoResponse,
};
use reqwest::header::CONTENT_TYPE;
use thegraph_core::DeploymentId;

use crate::{error::SubgraphServiceError, middleware::AttestationInput, service::GraphNodeState};

const GRAPH_ATTESTABLE: &str = "graph-attestable";
const GRAPH_INDEXED: &str = "graph-indexed";

/// Handles paid query requests by forwarding them to graph-node and wrapping the response.
///
/// # Response Status Code Behavior
///
/// This handler intentionally returns HTTP 200 OK regardless of the graph-node's response
/// status code. This is a design decision for the TAP (Timeline Aggregation Protocol) flow:
///
/// - The gateway expects a consistent 200 OK response with the GraphQL response wrapped
///   in the `IndexerResponsePayload` format (via attestation middleware)
/// - GraphQL errors are conveyed in the response body, not via HTTP status codes
/// - The graph-node's status code is not propagated to maintain protocol compatibility
///
/// This differs from `static_subgraph_request_handler` which preserves status codes
/// for non-paid, direct subgraph queries.
pub async fn request_handler(
    Path(deployment): Path<DeploymentId>,
    State(state): State<GraphNodeState>,
    req: String,
) -> Result<impl IntoResponse, SubgraphServiceError> {
    tracing::trace!(deployment = %deployment, "Handling request for deployment");

    let deployment_url = state
        .graph_node_query_base_url
        .join(&format!("subgraphs/id/{deployment}"))
        .map_err(|_| SubgraphServiceError::InvalidDeployment(deployment))?;

    let response = state
        .graph_node_client
        .post(deployment_url)
        .body(req.clone())
        .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
        .send()
        .await
        .map_err(SubgraphServiceError::QueryForwardingError)?;

    let attestable = response
        .headers()
        .get(GRAPH_ATTESTABLE)
        .is_some_and(|value| value.to_str().map(|value| value == "true").unwrap_or(false));

    let graph_indexed = response.headers().get(GRAPH_INDEXED).cloned();
    let body = response
        .text()
        .await
        .map_err(SubgraphServiceError::QueryForwardingError)?;
    let attestation_input = if attestable {
        AttestationInput::Attestable { req }
    } else {
        AttestationInput::NotAttestable
    };

    let mut response = Response::new(body);
    response.extensions_mut().insert(attestation_input);

    if let Some(graph_indexed) = graph_indexed {
        response.headers_mut().append(GRAPH_INDEXED, graph_indexed);
    }

    Ok(response)
}
