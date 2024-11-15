//! injects the sender
//!
//! Needs Receipt Extension to be available

use alloy::dyn_abi::Eip712Domain;
use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use tap_core::receipt::SignedReceipt;
use tokio::sync::watch;

use crate::{error::IndexerError, escrow_accounts::EscrowAccounts};

#[derive(Clone)]
pub struct SenderState {
    pub domain_separator: Eip712Domain,
    pub escrow_accounts: watch::Receiver<EscrowAccounts>,
}

#[derive(Clone)]
pub struct Sender(String);

impl From<Sender> for String {
    fn from(value: Sender) -> Self {
        value.0
    }
}

pub async fn sender_middleware(
    State(state): State<SenderState>,
    mut request: Request,
    next: Next,
) -> Result<Response, IndexerError> {
    if let Some(receipt) = request.extensions().get::<SignedReceipt>() {
        let signer = receipt.recover_signer(&state.domain_separator)?;
        let sender = state
            .escrow_accounts
            .borrow()
            .get_sender_for_signer(&signer)?;
        request.extensions_mut().insert(Sender(sender.to_string()));
    }

    Ok(next.run(request).await)
}
