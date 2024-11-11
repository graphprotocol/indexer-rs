//! injects the sender

use alloy::dyn_abi::Eip712Domain;
use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use axum_extra::TypedHeader;
use tokio::sync::watch;

use crate::escrow_accounts::EscrowAccounts;

use super::TapReceipt;

pub struct MyState {
    domain_separator: Eip712Domain,
    escrow_accounts: watch::Receiver<EscrowAccounts>,
}

#[derive(Clone)]
pub struct Sender(String);

impl From<Sender> for String {
    fn from(value: Sender) -> Self {
        value.0
    }
}

async fn sender_middleware(
    State(state): State<MyState>,
    TypedHeader(receipt): TypedHeader<TapReceipt>,
    mut request: Request,
    next: Next,
) -> Result<Response, anyhow::Error> {
    if let Some(receipt) = receipt.into_signed_receipt() {
        let signer = receipt.recover_signer(&state.domain_separator)?;
        let sender = state
            .escrow_accounts
            .borrow()
            .get_sender_for_signer(&signer)?;
        request.extensions_mut().insert(Sender(sender.to_string()));
    }

    Ok(next.run(request).await)
}
