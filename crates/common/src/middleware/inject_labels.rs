use std::sync::Arc;

use axum::{extract::Request, middleware::Next, response::Response};

use super::{
    allocation::Allocation,
    metrics::{MetricLabelProvider, MetricLabels},
    sender::Sender,
};

const NO_DEPLOYMENT_ID: &str = "no-deployment";
const NO_SENDER: &str = "no-sender";
const NO_ALLOCATION: &str = "no-allocation";

#[derive(Clone, Default)]
pub struct SenderAllocationDeploymentLabels {
    sender: Option<String>,
    allocation: Option<String>,
    deployment_id: Option<String>,
}

impl MetricLabelProvider for SenderAllocationDeploymentLabels {
    fn get_labels(&self) -> Vec<&str> {
        let mut list = vec![];
        if let Some(sender) = &self.sender {
            list.push(sender.as_str());
        } else {
            list.push(NO_SENDER);
        }
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
        .get::<Allocation>()
        .map(|s| s.clone().into());

    let labels: MetricLabels = Arc::new(SenderAllocationDeploymentLabels {
        sender,
        allocation,
        deployment_id,
    });
    request.extensions_mut().insert(labels);

    next.run(request).await
}
