// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use alloy::{dyn_abi::Eip712Domain, primitives::Address};
use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use indexer_monitor::EscrowAccounts;
use tap_core::receipt::SignedReceipt;
use tokio::sync::watch;

use crate::error::IndexerServiceError;

/// Stated used by sender middleware
#[derive(Clone)]
pub struct SenderState {
    /// Used to recover the signer address
    pub domain_separator: Eip712Domain,
    /// Used to get the sender address given the signer address
    pub escrow_accounts: watch::Receiver<EscrowAccounts>,
}

/// The current query Sender address
#[derive(Clone)]
pub struct Sender(pub Address);

impl From<Sender> for String {
    fn from(value: Sender) -> Self {
        value.0.to_string()
    }
}

/// Injects the sender found from the signer in the receipt
///
/// Requires Receipt extension
pub async fn sender_middleware(
    State(state): State<SenderState>,
    mut request: Request,
    next: Next,
) -> Result<Response, IndexerServiceError> {
    if let Some(receipt) = request.extensions().get::<SignedReceipt>() {
        let signer = receipt.recover_signer(&state.domain_separator)?;
        let sender = state
            .escrow_accounts
            .borrow()
            .get_sender_for_signer(&signer)?;
        request.extensions_mut().insert(Sender(sender));
    }

    Ok(next.run(request).await)
}

#[cfg(test)]
mod tests {
    use crate::middleware::inject_sender::SenderState;

    use super::{sender_middleware, Sender};
    use alloy::primitives::Address;
    use axum::{
        body::Body,
        http::{Extensions, Request},
        middleware::from_fn_with_state,
        routing::get,
        Router,
    };
    use indexer_monitor::EscrowAccounts;
    use reqwest::StatusCode;
    use test_assets::{
        create_signed_receipt, ESCROW_ACCOUNTS_BALANCES, ESCROW_ACCOUNTS_SENDERS_TO_SIGNERS,
    };
    use tokio::sync::watch;
    use tower::ServiceExt;

    #[tokio::test]
    async fn test_sender_middleware() {
        let escrow_accounts = watch::channel(EscrowAccounts::new(
            ESCROW_ACCOUNTS_BALANCES.to_owned(),
            ESCROW_ACCOUNTS_SENDERS_TO_SIGNERS.to_owned(),
        ))
        .1;
        let state = SenderState {
            domain_separator: test_assets::TAP_EIP712_DOMAIN.clone(),
            escrow_accounts,
        };

        let middleware = from_fn_with_state(state, sender_middleware);

        async fn handle(extensions: Extensions) -> Body {
            let sender = extensions.get::<Sender>().expect("Should contain sender");
            assert_eq!(sender.0, test_assets::TAP_SENDER.1);
            Body::empty()
        }

        let app = Router::new().route("/", get(handle)).layer(middleware);

        let receipt = create_signed_receipt(Address::ZERO, 1, 1, 1).await;

        let res = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .extension(receipt)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}
