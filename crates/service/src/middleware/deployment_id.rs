//! injects deployment id in extension
//! - check if allocation id already exists
//! - else, try to fetch allocation id from deployment_id and allocations watcher
//! - execute query
use axum::{
    extract::{Path, Request},
    middleware::Next,
    response::Response,
    RequestExt,
};
use thegraph_core::DeploymentId;

#[derive(Clone)]
pub struct Allocation(String);

impl From<Allocation> for String {
    fn from(value: Allocation) -> Self {
        value.0
    }
}

pub async fn deployment_middleware(mut request: Request, next: Next) -> Response {
    let deployment_id = request.extract_parts::<Path<DeploymentId>>().await.ok();
    if let Some(Path(deployment_id)) = deployment_id {
        request.extensions_mut().insert(deployment_id);
    }
    next.run(request).await
}
