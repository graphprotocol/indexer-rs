// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use deployment_status_query::Health;
use graphql_client::GraphQLQuery;
use reqwest::Url;
use serde::Deserialize;
use thegraph_core::DeploymentId;
use tokio::sync::watch::Receiver;

use crate::watcher::new_watcher;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "../graphql/indexing_status.schema.graphql",
    query_path = "../graphql/subgraph_deployment_status.graphql",
    response_derives = "Debug",
    variables_derives = "Clone"
)]
pub struct DeploymentStatusQuery;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct DeploymentStatus {
    pub synced: bool,
    pub health: String,
}

pub async fn monitor_deployment_status(
    deployment: DeploymentId,
    status_url: Url,
) -> anyhow::Result<Receiver<DeploymentStatus>> {
    new_watcher(Duration::from_secs(30), move || {
        check_deployment_status(deployment, status_url.clone())
    })
    .await
}

pub async fn check_deployment_status(
    deployment: DeploymentId,
    status_url: Url,
) -> Result<DeploymentStatus, anyhow::Error> {
    let req_body = DeploymentStatusQuery::build_query(deployment_status_query::Variables {
        ids: Some(vec![deployment.to_string()]),
    });
    let client = reqwest::Client::new();
    let response = client.post(status_url).json(&req_body).send().await?;
    let graphql_response: graphql_client::Response<deployment_status_query::ResponseData> =
        response.json().await?;
    match graphql_response.data {
        Some(data) => data
            .indexing_statuses
            .first()
            .map(|status| DeploymentStatus {
                synced: status.synced,
                health: match status.health {
                    Health::healthy => "healthy".to_owned(),
                    Health::unhealthy => "unhealthy".to_owned(),
                    _ => "failed".to_owned(),
                },
            })
            .ok_or_else(|| anyhow::anyhow!("Deployment `{deployment}` not found")),
        None => Err(anyhow::anyhow!(
            "Failed to query status of deployment `{deployment}`"
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use serde_json::json;
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

        let status = monitor_deployment_status(deployment, status_url)
            .await
            .unwrap();

        assert_eq!(
            status.borrow().clone(),
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

        let status = monitor_deployment_status(deployment, status_url)
            .await
            .unwrap();

        assert_eq!(
            status.borrow().clone(),
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

        let status = monitor_deployment_status(deployment, status_url)
            .await
            .unwrap();

        assert_eq!(
            status.borrow().clone(),
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

        let status = monitor_deployment_status(deployment, status_url)
            .await
            .unwrap();

        assert_eq!(
            status.borrow().clone(),
            DeploymentStatus {
                synced: true,
                health: "failed".to_string()
            }
        );
    }
}
