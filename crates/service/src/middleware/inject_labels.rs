// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Injects Metric Labels
//!
//! Require Sender, Allocation and Deployment extensions

use std::sync::Arc;

use axum::{extract::Request, middleware::Next, response::Response};
use thegraph_core::DeploymentId;

use super::{
    inject_allocation::Allocation,
    inject_sender::Sender,
    metrics::{MetricLabelProvider, MetricLabels},
};

const NO_DEPLOYMENT_ID: &str = "no-deployment";
const NO_ALLOCATION: &str = "no-allocation";
const NO_SENDER: &str = "no-sender";

#[derive(Clone, Default)]
pub struct SenderAllocationDeploymentLabels {
    sender: Option<String>,
    allocation: Option<String>,
    deployment_id: Option<String>,
}

impl MetricLabelProvider for SenderAllocationDeploymentLabels {
    fn get_labels(&self) -> Vec<&str> {
        let mut list = vec![];
        if let Some(deployment_id) = &self.deployment_id {
            list.push(deployment_id.as_str());
        } else {
            list.push(NO_DEPLOYMENT_ID);
        }
        if let Some(allocation) = &self.allocation {
            list.push(allocation.as_str());
        } else {
            list.push(NO_ALLOCATION);
        }
        if let Some(sender) = &self.sender {
            list.push(sender.as_str());
        } else {
            list.push(NO_SENDER);
        }
        list
    }
}

pub async fn labels_middleware(mut request: Request, next: Next) -> Response {
    let sender: Option<String> = request
        .extensions()
        .get::<Sender>()
        .map(|s| s.clone().into());

    let allocation: Option<String> = request
        .extensions()
        .get::<Allocation>()
        .map(|s| s.clone().into());

    let deployment_id: Option<String> = request
        .extensions()
        .get::<DeploymentId>()
        .map(|s| s.clone().to_string());

    let labels: MetricLabels = Arc::new(SenderAllocationDeploymentLabels {
        sender,
        allocation,
        deployment_id,
    });
    request.extensions_mut().insert(labels);

    next.run(request).await
}

#[cfg(test)]
mod tests {
    use crate::middleware::{
        inject_allocation::Allocation, inject_sender::Sender, metrics::MetricLabels,
    };

    use super::labels_middleware;

    use alloy::primitives::Address;
    use axum::{
        body::Body,
        http::{Extensions, Request},
        middleware::from_fn,
        routing::get,
        Router,
    };
    use reqwest::StatusCode;
    use test_assets::ESCROW_SUBGRAPH_DEPLOYMENT;
    use tower::ServiceExt;

    #[tokio::test]
    async fn test_receipt_middleware() {
        let middleware = from_fn(labels_middleware);

        let deployment = *ESCROW_SUBGRAPH_DEPLOYMENT;
        let sender = Address::ZERO;
        let allocation = Address::ZERO;

        let handle = move |extensions: Extensions| async move {
            let metrics = extensions
                .get::<MetricLabels>()
                .expect("Should decode tap receipt");
            assert_eq!(
                metrics.get_labels(),
                vec![
                    &deployment.to_string(),
                    &allocation.to_string(),
                    &sender.to_string(),
                ]
            );
            Body::empty()
        };

        let app = Router::new().route("/", get(handle)).layer(middleware);

        let res = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .extension(Sender(sender))
                    .extension(deployment)
                    .extension(Allocation(allocation))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}
