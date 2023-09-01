// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::extract::Extension;

use crate::{common::indexer_management_client::schema::CostSchema, server::ServerOptions};

pub(crate) async fn graphql_handler(
    req: GraphQLRequest,
    Extension(schema): Extension<CostSchema>,
    Extension(server_options): Extension<ServerOptions>,
) -> GraphQLResponse {
    schema.execute(req.into_inner().data(server_options)).await.into()
}
