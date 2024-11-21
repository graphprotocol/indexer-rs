// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use axum_extra::headers::{self, Header, HeaderName, HeaderValue};
use lazy_static::lazy_static;
use prometheus::{register_counter, Counter};
use tap_core::receipt::SignedReceipt;

#[derive(Debug, PartialEq)]
pub struct TapReceipt(pub SignedReceipt);

lazy_static! {
    static ref TAP_RECEIPT: HeaderName = HeaderName::from_static("tap-receipt");
    pub static ref TAP_RECEIPT_INVALID: Counter =
        register_counter!("indexer_tap_invalid_total", "Invalid tap receipt decode",).unwrap();
}

impl Header for TapReceipt {
    fn name() -> &'static HeaderName {
        &TAP_RECEIPT
    }

    fn decode<'i, I>(values: &mut I) -> Result<Self, headers::Error>
    where
        I: Iterator<Item = &'i HeaderValue>,
    {
        let mut execute = || {
            let value = values.next();
            let raw_receipt = value.ok_or(headers::Error::invalid())?;
            let raw_receipt = raw_receipt
                .to_str()
                .map_err(|_| headers::Error::invalid())?;
            let parsed_receipt =
                serde_json::from_str(raw_receipt).map_err(|_| headers::Error::invalid())?;
            Ok(TapReceipt(parsed_receipt))
        };
        execute().inspect_err(|_| TAP_RECEIPT_INVALID.inc())
    }

    fn encode<E>(&self, _values: &mut E)
    where
        E: Extend<HeaderValue>,
    {
        unimplemented!()
    }
}

#[cfg(test)]
mod test {
    use std::str::FromStr;

    use alloy::primitives::Address;
    use axum::http::HeaderValue;
    use axum_extra::headers::Header;

    use test_assets::create_signed_receipt;

    use super::TapReceipt;

    #[tokio::test]
    async fn test_decode_valid_tap_receipt_header() {
        let allocation = Address::from_str("0xdeadbeefcafebabedeadbeefcafebabedeadbeef").unwrap();
        let original_receipt =
            create_signed_receipt(allocation, u64::MAX, u64::MAX, u128::MAX).await;
        let serialized_receipt = serde_json::to_string(&original_receipt).unwrap();
        let header_value = HeaderValue::from_str(&serialized_receipt).unwrap();
        let header_values = vec![&header_value];
        let decoded_receipt = TapReceipt::decode(&mut header_values.into_iter())
            .expect("tap receipt header value should be valid");

        assert_eq!(decoded_receipt, TapReceipt(original_receipt));
    }

    #[test]
    fn test_decode_non_string_tap_receipt_header() {
        let header_value = HeaderValue::from_static("123");
        let header_values = vec![&header_value];
        let result = TapReceipt::decode(&mut header_values.into_iter());

        assert!(result.is_err());
    }

    #[test]
    fn test_decode_invalid_tap_receipt_header() {
        let header_value = HeaderValue::from_bytes(b"invalid").unwrap();
        let header_values = vec![&header_value];
        let result = TapReceipt::decode(&mut header_values.into_iter());

        assert!(result.is_err());
    }
}
