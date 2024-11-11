//! verifies and stores tap receipt, or else reject

use anyhow::anyhow;
use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use axum_extra::TypedHeader;
use serde_json::value::RawValue;
use tap_core::{manager::Manager, receipt::Context};
use thegraph_core::{AllocationId, DeploymentId};

use crate::tap::{AgoraQuery, IndexerTapContext};

use super::{sender::Sender, TapReceipt};

#[derive(Debug, serde::Deserialize)]
pub struct QueryBody {
    pub query: String,
    pub variables: Option<Box<RawValue>>,
}

pub struct MyState {
    tap_manager: Manager<IndexerTapContext>,
    failed_receipt: prometheus::CounterVec,
}

const NO_SENDER: &str = "no-sender";
const NO_ALLOCATION: &str = "no-allocation";

async fn tap_middleware(
    manifest_id: DeploymentId,
    State(state): State<MyState>,
    TypedHeader(receipt): TypedHeader<TapReceipt>,
    request: Request,
    next: Next,
) -> Result<Response, anyhow::Error> {
    match receipt.into_signed_receipt() {
        Some(receipt) => {
            let sender = request.extensions().get::<Sender>();
            let allocation_id = request.extensions().get::<AllocationId>();
            let body = request.body().into();
            // let body = body::to_bytes(body, usize::MAX).await?;

            let request: QueryBody = serde_json::from_slice(&body)?;

            let variables = request
                .variables
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default();
            let mut ctx = Context::new();
            ctx.insert(AgoraQuery {
                deployment_id: manifest_id,
                query: request.query.clone(),
                variables,
            });

            // Verify the receipt and store it in the database
            state
                .tap_manager
                .verify_and_store_receipt(&ctx, receipt)
                .await
                .inspect_err(|_| {
                    state
                        .failed_receipt
                        .with_label_values(&[
                            &manifest_id.to_string(),
                            &allocation_id
                                .map(|a| a.to_string())
                                .unwrap_or(NO_ALLOCATION.to_string()),
                            &sender
                                .map(|sender| *sender.into())
                                .unwrap_or(NO_SENDER.to_string()),
                        ])
                        .inc()
                })?;
        }
        None => return Err(anyhow!("Could not verify receipt")),
    }

    Ok(next.run(request).await)
}
