// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::Response,
    response::IntoResponse,
};
use indexer_common::middleware::attestation::AttestationInput;
use thegraph_core::DeploymentId;
use tracing::trace;

use crate::{error::SubgraphServiceError, service::SubgraphServiceState};

pub async fn request_handler(
    Path(deployment): Path<DeploymentId>,
    State(state): State<Arc<SubgraphServiceState>>,
    request: String,
) -> Result<impl IntoResponse, SubgraphServiceError> {
    trace!("Handling request for deployment `{deployment}`");

    let deployment_url = state
        .graph_node_query_base_url
        .join(&format!("subgraphs/id/{deployment}"))
        .map_err(|_| SubgraphServiceError::InvalidDeployment(deployment))?;
    let req = request.clone();

    let response = state
        .graph_node_client
        .post(deployment_url)
        .body(request)
        .send()
        .await
        .map_err(SubgraphServiceError::QueryForwardingError)?;

    let attestable = response
        .headers()
        .get("graph-attestable")
        .map_or(false, |value| {
            value.to_str().map(|value| value == "true").unwrap_or(false)
        });

    let body = response
        .text()
        .await
        .map_err(SubgraphServiceError::QueryForwardingError)?;

    let attestation_input = if attestable {
        AttestationInput::Attestable {
            req,
            res: body.clone(),
        }
    } else {
        AttestationInput::NotAttestable
    };

    let mut response = Response::new(body);
    response.extensions_mut().insert(attestation_input);
    Ok(response)
}
