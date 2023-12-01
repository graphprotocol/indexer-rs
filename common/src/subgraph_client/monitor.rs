// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use eventuals::{timer, Eventual, EventualExt};
use graphql_http::http::response::ResponseBody;
use reqwest::{header, Url};
use serde::Deserialize;
use serde_json::{json, Value};
use thegraph::types::DeploymentId;
use tokio::time::sleep;
use tracing::warn;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeploymentStatusResponse {
    indexing_statuses: Option<Vec<DeploymentStatus>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct DeploymentStatus {
    pub synced: bool,
    pub health: String,
}

async fn query<T: for<'de> Deserialize<'de>>(
    url: Url,
    body: &Value,
) -> Result<ResponseBody<T>, reqwest::Error> {
    reqwest::Client::new()
        .post(url)
        .json(body)
        .header(header::CONTENT_TYPE, "application/json")
        .send()
        .await
        .and_then(|response| response.error_for_status())?
        .json::<ResponseBody<T>>()
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
                        indexingStatuses(subgraphs: $ids) {
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

                if !response.errors.is_empty() {
                    warn!(
                        "Errors encountered querying the deployment status for `{}`: {}",
                        deployment,
                        response
                            .errors
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

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    #[tokio::test]
    async fn test_parses_synced_and_healthy_response() {
        let mock_server = MockServer::start().await;
        let status_url: Url = mock_server
            .uri()
            .parse::<Url>()
            .unwrap()
            .join("/status")
            .unwrap();
        let deployment =
            DeploymentId::from_str("QmAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();

        Mock::given(method("POST"))
            .and(path("/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "indexingStatuses": [
                        {
                            "synced": true,
                            "health": "healthy"
                        }
                    ]
                }
            })))
            .mount(&mock_server)
            .await;

        let status = monitor_deployment_status(deployment, status_url);

        assert_eq!(
            status.value().await.unwrap(),
            DeploymentStatus {
                synced: true,
                health: "healthy".to_string()
            }
        );
    }

    #[tokio::test]
    async fn test_parses_not_synced_and_healthy_response() {
        let mock_server = MockServer::start().await;
        let status_url: Url = mock_server
            .uri()
            .parse::<Url>()
            .unwrap()
            .join("/status")
            .unwrap();
        let deployment =
            DeploymentId::from_str("QmAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();

        Mock::given(method("POST"))
            .and(path("/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "indexingStatuses": [
                        {
                            "synced": false,
                            "health": "healthy"
                        }
                    ]
                }
            })))
            .mount(&mock_server)
            .await;

        let status = monitor_deployment_status(deployment, status_url);

        assert_eq!(
            status.value().await.unwrap(),
            DeploymentStatus {
                synced: false,
                health: "healthy".to_string()
            }
        );
    }

    #[tokio::test]
    async fn test_parses_synced_and_unhealthy_response() {
        let mock_server = MockServer::start().await;
        let status_url: Url = mock_server
            .uri()
            .parse::<Url>()
            .unwrap()
            .join("/status")
            .unwrap();
        let deployment =
            DeploymentId::from_str("QmAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();

        Mock::given(method("POST"))
            .and(path("/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "indexingStatuses": [
                        {
                            "synced": true,
                            "health": "unhealthy"
                        }
                    ]
                }
            })))
            .mount(&mock_server)
            .await;

        let status = monitor_deployment_status(deployment, status_url);

        assert_eq!(
            status.value().await.unwrap(),
            DeploymentStatus {
                synced: true,
                health: "unhealthy".to_string()
            }
        );
    }

    #[tokio::test]
    async fn test_parses_synced_and_failed_response() {
        let mock_server = MockServer::start().await;
        let status_url: Url = mock_server
            .uri()
            .parse::<Url>()
            .unwrap()
            .join("/status")
            .unwrap();
        let deployment =
            DeploymentId::from_str("QmAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();

        Mock::given(method("POST"))
            .and(path("/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "indexingStatuses": [
                        {
                            "synced": true,
                            "health": "failed"
                        }
                    ]
                }
            })))
            .mount(&mock_server)
            .await;

        let status = monitor_deployment_status(deployment, status_url);

        assert_eq!(
            status.value().await.unwrap(),
            DeploymentStatus {
                synced: true,
                health: "failed".to_string()
            }
        );
    }
}
