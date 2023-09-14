use alloy_primitives::Address;
use alloy_sol_types::{eip712_domain, Eip712Domain};
use ethers::signers::{coins_bip39::English, LocalWallet, MnemonicBuilder, Signer};
use tap_core::receipt_aggregate_voucher::ReceiptAggregateVoucher;
use tap_core::tap_manager::SignedRAV;
use tap_core::tap_manager::SignedReceipt;
use tap_core::{eip_712_signed_message::EIP712SignedMessage, tap_receipt::Receipt};

/// Fixture to generate a wallet and address
pub fn keys() -> (LocalWallet, Address) {
    let wallet: LocalWallet = MnemonicBuilder::<English>::default()
        .phrase("abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about")
        .build()
        .unwrap();
    let address = wallet.address();

    (wallet, Address::from_slice(address.as_bytes()))
}

pub fn domain() -> Eip712Domain {
    eip712_domain! {
        name: "TAP",
        version: "1",
        chain_id: 1,
        verifying_contract: Address::from([0x11u8; 20]),
    }
}

/// Fixture to generate a signed receipt using the wallet from `keys()`
/// and the given `query_id` and `value`
pub async fn create_signed_receipt(
    allocation_id: Address,
    nonce: u64,
    timestamp_ns: u64,
    value: u128,
) -> SignedReceipt {
    let (wallet, _) = keys();

    EIP712SignedMessage::new(
        &domain(),
        Receipt {
            allocation_id,
            nonce,
            timestamp_ns,
            value,
        },
        &wallet,
    )
    .await
    .unwrap()
}

/// Fixture to generate a RAV using the wallet from `keys()`
pub async fn create_rav(
    allocation_id: Address,
    timestamp_ns: u64,
    value_aggregate: u128,
) -> SignedRAV {
    let (wallet, _) = keys();

    EIP712SignedMessage::new(
        &domain(),
        ReceiptAggregateVoucher {
            allocation_id,
            timestamp_ns,
            value_aggregate,
        },
        &wallet,
    )
    .await
    .unwrap()
}
