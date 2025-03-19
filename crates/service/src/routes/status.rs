// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::{collections::HashSet, sync::LazyLock};

use async_graphql_axum::GraphQLRequest;
use axum::{extract::State, response::IntoResponse, Json};
use graphql::graphql_parser::query as q;
use serde_json::{json, Map, Value};
use thegraph_graphql_http::{
    http::request::{IntoRequestParameters, RequestParameters},
    http_client::{ReqwestExt, ResponseError},
};

use crate::{error::SubgraphServiceError, service::GraphNodeState};

static SUPPORTED_ROOT_FIELDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "indexingStatuses",
        "chains",
        "latestBlock",
        "earliestBlock",
        "publicProofsOfIndexing",
        "entityChangesInBlock",
        "blockData",
        "blockHashFromNumber",
        "cachedEthereumCalls",
        "subgraphFeatures",
        "apiVersions",
        "version",
    ]
    .into_iter()
    .collect()
});

struct WrappedGraphQLRequest(async_graphql::Request);

impl IntoRequestParameters for WrappedGraphQLRequest {
    fn into_request_parameters(self) -> RequestParameters {
        RequestParameters {
            query: self.0.query.into(),
            operation_name: self.0.operation_name,
            variables: Map::from_iter(self.0.variables.iter().map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.clone().into_json().unwrap(),
                )
            })),
            extensions: Map::from_iter(
                self.0
                    .extensions
                    .0
                    .into_iter()
                    .map(|(name, value)| (name, value.into_json().unwrap())),
            ),
        }
    }
}

// Custom middleware function to process the request before reaching the main handler
pub async fn status(
    State(state): State<GraphNodeState>,
    request: GraphQLRequest,
) -> Result<impl IntoResponse, SubgraphServiceError> {
    let request = request.into_inner();

    let query: q::Document<String> = q::parse_query(request.query.as_str())
        .map_err(|e| SubgraphServiceError::InvalidStatusQuery(e.into()))?;

    let root_fields = query
        .definitions
        .iter()
        // This gives us all root selection sets
        .filter_map(|def| match def {
            q::Definition::Operation(op) => match op {
                q::OperationDefinition::Query(query) => Some(&query.selection_set),
                q::OperationDefinition::SelectionSet(selection_set) => Some(selection_set),
                _ => None,
            },
            q::Definition::Fragment(fragment) => Some(&fragment.selection_set),
        })
        // This gives us all field names of root selection sets (and potentially non-root fragments)
        .flat_map(|selection_set| {
            selection_set
                .items
                .iter()
                .filter_map(|item| match item {
                    q::Selection::Field(field) => Some(&field.name),
                    _ => None,
                })
                .collect::<HashSet<_>>()
        });

    let unsupported_root_fields: Vec<_> = root_fields
        .filter(|field| !SUPPORTED_ROOT_FIELDS.contains(field.as_str()))
        .map(ToString::to_string)
        .collect();

    if !unsupported_root_fields.is_empty() {
        return Err(SubgraphServiceError::UnsupportedStatusQueryFields(
            unsupported_root_fields,
        ));
    }

    let result = state
        .graph_node_client
        .post(state.graph_node_status_url.clone())
        .send_graphql::<Value>(WrappedGraphQLRequest(request))
        .await
        .map_err(|e| SubgraphServiceError::StatusQueryError(e.into()))?;

    result
        .map(|data| Json(json!({"data": data})))
        .or_else(|e| match e {
            ResponseError::Failure { errors } => Ok(Json(json!({
                "errors": errors,
            }))),
            ResponseError::Empty => todo!(),
        })
}
