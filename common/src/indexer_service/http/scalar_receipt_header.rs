// Copyright 2023-, GraphOps and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::ops::Deref;

use headers::{Header, HeaderName, HeaderValue};
use lazy_static::lazy_static;
use tap_core::tap_manager::SignedReceipt;

#[derive(Debug, PartialEq)]
pub struct ScalarReceipt(Option<SignedReceipt>);

impl ScalarReceipt {
    pub fn into_signed_receipt(self) -> Option<SignedReceipt> {
        self.0
    }
}

impl Deref for ScalarReceipt {
    type Target = Option<SignedReceipt>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

lazy_static! {
    static ref SCALAR_RECEIPT: HeaderName = HeaderName::from_static("scalar-receipt");
}

impl Header for ScalarReceipt {
    fn name() -> &'static HeaderName {
        &SCALAR_RECEIPT
    }

    fn decode<'i, I>(values: &mut I) -> Result<Self, headers::Error>
    where
        I: Iterator<Item = &'i HeaderValue>,
    {
        let value = values.next();
        let raw_receipt = value
            .map(|value| value.to_str())
            .transpose()
            .map_err(|_| headers::Error::invalid())?;
        let parsed_receipt = raw_receipt
            .map(serde_json::from_str)
            .transpose()
            .map_err(|_| headers::Error::invalid())?;
        Ok(ScalarReceipt(parsed_receipt))
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

    use axum::{headers::Header, http::HeaderValue};
    use thegraph::types::Address;

    use crate::test_vectors::create_signed_receipt;

    use super::ScalarReceipt;

    #[tokio::test]
    async fn test_decode_valid_scalar_receipt_header() {
        let allocation = Address::from_str("0xdeadbeefcafebabedeadbeefcafebabedeadbeef").unwrap();
        let original_receipt =
            create_signed_receipt(allocation, u64::MAX, u64::MAX, u128::MAX).await;
        let serialized_receipt = serde_json::to_string(&original_receipt).unwrap();
        let header_value = HeaderValue::from_str(&serialized_receipt).unwrap();
        let header_values = vec![&header_value];
        let decoded_receipt = ScalarReceipt::decode(&mut header_values.into_iter())
            .expect("scalar receipt header value should be valid");

        assert_eq!(
            decoded_receipt,
            ScalarReceipt(Some(original_receipt.clone()))
        );
    }

    #[test]
    fn test_decode_non_string_scalar_receipt_header() {
        let header_value = HeaderValue::from_static("123");
        let header_values = vec![&header_value];
        let result = ScalarReceipt::decode(&mut header_values.into_iter());

        assert!(result.is_err());
    }

    #[test]
    fn test_decode_invalid_scalar_receipt_header() {
        let header_value = HeaderValue::from_bytes(b"invalid").unwrap();
        let header_values = vec![&header_value];
        let result = ScalarReceipt::decode(&mut header_values.into_iter());

        assert!(result.is_err());
    }
}
