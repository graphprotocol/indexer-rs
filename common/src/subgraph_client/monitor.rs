// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use eventuals::{timer, Eventual, EventualExt};
use graphql::http::Response;
use log::warn;
use reqwest::{header, Url};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::time::sleep;
use toolshed::thegraph::DeploymentId;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeploymentStatusResponse {
    indexing_statuses: Option<Vec<DeploymentStatus>>,
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
pub struct DeploymentStatus {
    pub synced: bool,
    pub health: String,
}

async fn query<T: for<'de> Deserialize<'de>>(
    url: Url,
    body: &Value,
) -> Result<Response<T>, reqwest::Error> {
    reqwest::Client::new()
        .post(url)
        .json(body)
        .header(header::CONTENT_TYPE, "application/json")
        .send()
        .await
        .and_then(|response| response.error_for_status())?
        .json::<Response<T>>()
        .await
}

pub fn monitor_deployment_status(
    deployment: DeploymentId,
    status_url: Url,
) -> Eventual<DeploymentStatus> {
    timer(Duration::from_secs(30)).map_with_retry(
        move |_| {
            let status_url = status_url.clone();

            async move {
                let body = json!({
                    "query": r#"
                    query indexingStatuses($ids: [ID!]!) {
                        indexingStatuses(deployments: $ids) {
                            synced
                            health
                        }
                    }
                "#,
                    "variables": {
                        "ids": [deployment.to_string()]
                    }
                });

                let response = query::<DeploymentStatusResponse>(status_url, &body)
                    .await
                    .map_err(|e| {
                        format!("Failed to query status of deployment `{deployment}`: {e}")
                    })?;

                if let Some(errors) = response.errors {
                    warn!(
                        "Errors encountered querying the deployment status for `{}`: {}",
                        deployment,
                        errors
                            .into_iter()
                            .map(|e| e.message)
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }

                response
                    .data
                    .and_then(|data| data.indexing_statuses)
                    .and_then(|data| data.get(0).map(Clone::clone))
                    .ok_or_else(|| format!("Deployment `{}` not found", deployment))
            }
        },
        move |err: String| async move {
            warn!(
                "Error querying deployment status for `{}`: {}",
                deployment, err
            );
            sleep(Duration::from_secs(15)).await
        },
    )
}
