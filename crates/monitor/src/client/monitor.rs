// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use graphql_client::GraphQLQuery;
use indexer_query::{
    deployment_status_query::{self, Health},
    DeploymentStatusQuery,
};
use indexer_watcher::new_watcher;
use reqwest::Url;
use serde::Deserialize;
use thegraph_core::DeploymentId;
use tokio::sync::watch::Receiver;

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
        ids: vec![deployment.to_string()],
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
    use reqwest::Url;
    use serde_json::json;
    use thegraph_core::deployment_id;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    use super::*;

    struct MonitorMock {
        mock_server: MockServer,
        status_url: Url,
        deployment: DeploymentId,
    }

    #[rstest::fixture]
    async fn monitor_mock() -> MonitorMock {
        let mock_server = MockServer::start().await;
        let status_url: Url = mock_server
            .uri()
            .parse::<Url>()
            .unwrap()
            .join("/status")
            .unwrap();
        let deployment = deployment_id!("QmAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        MonitorMock {
            mock_server,
            status_url,
            deployment,
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn test_parses_synced_and_healthy_response(#[future(awt)] monitor_mock: MonitorMock) {
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
            .mount(&monitor_mock.mock_server)
            .await;

        let status = monitor_deployment_status(monitor_mock.deployment, monitor_mock.status_url)
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

    #[rstest::rstest]
    #[tokio::test]
    async fn test_parses_not_synced_and_healthy_response(#[future(awt)] monitor_mock: MonitorMock) {
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
            .mount(&monitor_mock.mock_server)
            .await;

        let status = monitor_deployment_status(monitor_mock.deployment, monitor_mock.status_url)
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

    #[rstest::rstest]
    #[tokio::test]
    async fn test_parses_synced_and_unhealthy_response(#[future(awt)] monitor_mock: MonitorMock) {
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
            .mount(&monitor_mock.mock_server)
            .await;

        let status = monitor_deployment_status(monitor_mock.deployment, monitor_mock.status_url)
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

    #[rstest::rstest]
    #[tokio::test]
    async fn test_parses_synced_and_failed_response(#[future(awt)] monitor_mock: MonitorMock) {
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
            .mount(&monitor_mock.mock_server)
            .await;

        let status = monitor_deployment_status(monitor_mock.deployment, monitor_mock.status_url)
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
