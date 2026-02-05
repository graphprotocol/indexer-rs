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

/// State required by [`sender_middleware`] for resolving TAP receipt signers to senders.
///
/// This middleware recovers the signer address from a TAP receipt using the appropriate
/// EIP-712 domain separator, then resolves the actual sender address via the escrow accounts
/// mapping. The sender is the payment source tracked by the escrow contract.
///
/// # Request Flow
/// 1. Receipt is extracted from request extensions (set by prior middleware)
/// 2. Signer is recovered using the appropriate domain separator (V1 or V2)
/// 3. Sender is resolved from escrow accounts (signer → sender mapping)
/// 4. [`Sender`] is inserted into request extensions for downstream handlers
///
/// # Initialization Order
/// Must be constructed after:
/// - EIP-712 domain separators are configured
/// - Escrow account watchers are started (from escrow subgraph monitoring)
///
/// # Version Handling
/// - V1 receipts use `domain_separator` and `escrow_accounts_v1` (legacy Staking contract)
/// - V2 receipts use `domain_separator_v2` and `escrow_accounts_v2` (Horizon protocol)
#[derive(Clone)]
pub struct SenderState {
    /// EIP-712 domain separator for V1 TAP receipts (legacy Staking).
    pub domain_separator: Eip712Domain,
    /// EIP-712 domain separator for V2 TAP receipts (Horizon).
    pub domain_separator_v2: Eip712Domain,
    /// Escrow accounts watcher for V1 receipts. Provides signer → sender mapping.
    pub escrow_accounts_v1: Option<watch::Receiver<EscrowAccounts>>,
    /// Escrow accounts watcher for V2 receipts. Provides signer → sender mapping.
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
        let sender = match receipt {
            TapReceipt::V1(_) => {
                let signer = receipt.recover_signer(&state.domain_separator)?;
                if let Some(ref escrow_accounts_v1) = state.escrow_accounts_v1 {
                    escrow_accounts_v1.borrow().get_sender_for_signer(&signer)?
                } else {
                    return Err(IndexerServiceError::EscrowAccount(
                        EscrowAccountsError::NoSenderFound { signer },
                    ));
                }
            }
            TapReceipt::V2(_) => {
                let signer = receipt.recover_signer(&state.domain_separator_v2)?;
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
            domain_separator_v2: test_assets::TAP_EIP712_DOMAIN_V2.clone(),
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
