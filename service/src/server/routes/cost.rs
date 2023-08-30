// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use async_graphql::http::{playground_source, GraphQLPlaygroundConfig};
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::{
    extract::Extension,
    response::{Html, IntoResponse},
};

use std::sync::Arc;
use tracing::trace;

use crate::{model::schema::CostSchema, server::ServerOptions};

pub(crate) async fn graphql_handler(
    req: GraphQLRequest,
    Extension(schema): Extension<CostSchema>,
    Extension(server_options): Extension<ServerOptions>,
) -> GraphQLResponse {
    let response = async move { schema.execute(req.into_inner().data(server_options)).await }.await;
    response.into()
}
