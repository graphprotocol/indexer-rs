// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use axum::{response::IntoResponse, Json};
use serde::{Deserialize, Serialize};

use crate::{allocations::Allocations, keys::Signer};

pub(crate) const NETWORK_SUBGRAPH_ROUTE: &str = "/network_subgraph";

pub(crate) async fn network_subgraph(indexer: Signer) -> impl IntoResponse {
    Json(GraphqlResponse::Allocations(Allocations::new(indexer)))
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum GraphqlResponse {
    Allocations(Allocations),
}
