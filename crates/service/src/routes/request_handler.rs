// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use crate::{
    error::SubgraphServiceError, middleware::AttestationInput, service::SubgraphServiceState,
};
use axum::{
    extract::{Path, State},
    http::{HeaderValue, Response},
    response::IntoResponse,
};
use reqwest::header::CONTENT_TYPE;
use thegraph_core::DeploymentId;
use tracing::trace;

const GRAPH_ATTESTABLE: &str = "graph-attestable";

pub async fn request_handler(
    Path(deployment): Path<DeploymentId>,
    State(state): State<SubgraphServiceState>,
    req: String,
) -> Result<impl IntoResponse, SubgraphServiceError> {
    trace!("Handling request for deployment `{deployment}`");

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
        .map_or(false, |value| {
            value.to_str().map(|value| value == "true").unwrap_or(false)
        });

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

    Ok(response)
}
