// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use indexer_monitor::{EscrowAccounts, EscrowAccountsError};
use thegraph_core::alloy::{primitives::Address, sol_types::Eip712Domain};
use tokio::sync::watch;

use crate::{error::IndexerServiceError, tap::TapReceipt};

/// Stated used by sender middleware
#[derive(Clone)]
pub struct SenderState {
    /// Used to recover the signer address
    pub domain_separator: Eip712Domain,
    /// Used to get the sender address given the signer address if v1 receipt
    pub escrow_accounts_v1: Option<watch::Receiver<EscrowAccounts>>,
    /// Used to get the sender address given the signer address if v2 receipt
    pub escrow_accounts_v2: Option<watch::Receiver<EscrowAccounts>>,
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
/// A request won't always have a receipt because they might be
/// free queries.
/// That's why we don't fail with 400.
///
/// Requires Receipt extension
pub async fn sender_middleware(
    State(state): State<SenderState>,
    mut request: Request,
    next: Next,
) -> Result<Response, IndexerServiceError> {
    if let Some(receipt) = request.extensions().get::<TapReceipt>() {
        let signer = receipt.recover_signer(&state.domain_separator)?;
        let sender = match receipt {
            TapReceipt::V1(_) => {
                if let Some(ref escrow_accounts_v1) = state.escrow_accounts_v1 {
                    escrow_accounts_v1.borrow().get_sender_for_signer(&signer)?
                } else {
                    return Err(IndexerServiceError::EscrowAccount(
                        EscrowAccountsError::NoSenderFound { signer },
                    ));
                }
            }
            TapReceipt::V2(_) => {
                if let Some(ref escrow_accounts_v2) = state.escrow_accounts_v2 {
                    escrow_accounts_v2.borrow().get_sender_for_signer(&signer)?
                } else {
                    return Err(IndexerServiceError::EscrowAccount(
                        EscrowAccountsError::NoSenderFound { signer },
                    ));
                }
            }
        };
        request.extensions_mut().insert(Sender(sender));
    }

    Ok(next.run(request).await)
}

#[cfg(test)]
mod tests {
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
        create_signed_receipt, SignedReceiptRequest, ESCROW_ACCOUNTS_BALANCES,
        ESCROW_ACCOUNTS_SENDERS_TO_SIGNERS,
    };
    use tokio::sync::watch;
    use tower::ServiceExt;

    use super::{sender_middleware, Sender};
    use crate::{middleware::sender::SenderState, tap::TapReceipt};

    #[tokio::test]
    async fn test_sender_middleware() {
        let escrow_accounts_v1 = watch::channel(EscrowAccounts::new(
            ESCROW_ACCOUNTS_BALANCES.to_owned(),
            ESCROW_ACCOUNTS_SENDERS_TO_SIGNERS.to_owned(),
        ))
        .1;

        let escrow_accounts_v2 = watch::channel(EscrowAccounts::new(
            ESCROW_ACCOUNTS_BALANCES.to_owned(),
            ESCROW_ACCOUNTS_SENDERS_TO_SIGNERS.to_owned(),
        ))
        .1;

        let state = SenderState {
            domain_separator: test_assets::TAP_EIP712_DOMAIN.clone(),
            escrow_accounts_v1: Some(escrow_accounts_v1),
            escrow_accounts_v2: Some(escrow_accounts_v2),
        };

        let middleware = from_fn_with_state(state, sender_middleware);

        async fn handle(extensions: Extensions) -> Body {
            let sender = extensions.get::<Sender>().expect("Should contain sender");
            assert_eq!(sender.0, test_assets::TAP_SENDER.1);
            Body::empty()
        }

        let app = Router::new().route("/", get(handle)).layer(middleware);

        let receipt = create_signed_receipt(SignedReceiptRequest::builder().build()).await;

        let res = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .extension(TapReceipt::V1(receipt))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}
