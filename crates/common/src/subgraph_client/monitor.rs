// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use reqwest::Url;
use serde::Deserialize;
use serde_json::json;
use thegraph_core::DeploymentId;
use thegraph_graphql_http::{
    http::request::IntoRequestParameters,
    http_client::{ReqwestExt, ResponseResult},
};
use tokio::sync::watch::Receiver;

use crate::watcher::new_watcher;

use super::Query;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeploymentStatusResponse {
    indexing_statuses: Vec<DeploymentStatus>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct DeploymentStatus {
    pub synced: bool,
    pub health: String,
}

async fn query<T: for<'de> Deserialize<'de>>(
    url: Url,
    query: impl IntoRequestParameters + Send,
) -> Result<ResponseResult<T>, anyhow::Error> {
    Ok(reqwest::Client::new().post(url).send_graphql(query).await?)
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

async fn check_deployment_status(
    deployment: DeploymentId,
    status_url: Url,
) -> Result<DeploymentStatus, anyhow::Error> {
    let body = Query::new_with_variables(
        r#"
        query indexingStatuses($ids: [String!]!) {
            indexingStatuses(subgraphs: $ids) {
                synced
                health
            }
        }
    "#,
        [("ids", json!([deployment.to_string()]))],
    );

    let response = query::<DeploymentStatusResponse>(status_url, body).await?;

    match response {
        Ok(deployment_status) => deployment_status
            .indexing_statuses
            .first()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Deployment `{deployment}` not found")),
        Err(e) => Err(anyhow::anyhow!(
            "Failed to query status of deployment `{deployment}`: {e}"
        )),
    }
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
