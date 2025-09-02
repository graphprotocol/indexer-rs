// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use std::sync::LazyLock;

use axum_extra::headers::{self, Header, HeaderName, HeaderValue};
use base64::prelude::*;
use prometheus::{register_counter, Counter};
use prost::Message;
use tap_aggregator::grpc;
use tap_graph::SignedReceipt;

use crate::tap::TapReceipt;

#[derive(Debug, PartialEq)]
pub struct TapHeader(pub TapReceipt);

static TAP_RECEIPT: LazyLock<HeaderName> = LazyLock::new(|| HeaderName::from_static("tap-receipt"));
pub static TAP_RECEIPT_INVALID: LazyLock<Counter> = LazyLock::new(|| {
    register_counter!("indexer_tap_invalid_total", "Invalid tap receipt decode",).unwrap()
});

impl Header for TapHeader {
    fn name() -> &'static HeaderName {
        &TAP_RECEIPT
    }

    fn decode<'i, I>(values: &mut I) -> Result<Self, headers::Error>
    where
        I: Iterator<Item = &'i HeaderValue>,
    {
        let mut execute = || -> anyhow::Result<TapHeader> {
            let raw_receipt = values.next().ok_or(headers::Error::invalid())?;

            // we first try to decode a v2 receipt since it's cheaper and fail earlier than using
            // serde
            match BASE64_STANDARD.decode(raw_receipt) {
                Ok(raw_receipt) => {
                    tracing::debug!(
                        decoded_length = raw_receipt.len(),
                        "Successfully base64 decoded v2 receipt"
                    );
                    let receipt =
                        grpc::v2::SignedReceipt::decode(raw_receipt.as_ref()).map_err(|e| {
                            tracing::debug!(error = %e, "Failed to protobuf decode v2 receipt");
                            e
                        })?;
                    tracing::debug!("Successfully protobuf decoded v2 receipt");
                    let converted_receipt = receipt.try_into().map_err(|e: anyhow::Error| {
                        tracing::debug!(error = %e, "Failed to convert v2 receipt");
                        e
                    })?;
                    tracing::debug!("Successfully converted v2 receipt to TapReceipt::V2");
                    Ok(TapHeader(TapReceipt::V2(converted_receipt)))
                }
                Err(e) => {
                    tracing::debug!(error = %e, "Could not base64 decode v2 receipt, trying v1");
                    let parsed_receipt: SignedReceipt =
                        serde_json::from_slice(raw_receipt.as_ref()).map_err(|e| {
                            tracing::debug!(error = %e, "Failed to JSON decode v1 receipt");
                            e
                        })?;
                    tracing::debug!("Successfully decoded v1 receipt");
                    Ok(TapHeader(TapReceipt::V1(parsed_receipt)))
                }
            }
        };
        let result = execute();
        match &result {
            Ok(_) => {}
            Err(e) => {
                tracing::debug!(error = %e, "TAP receipt header parsing failed - detailed error before collapse");
                TAP_RECEIPT_INVALID.inc();
            }
        }
        result.map_err(|_| headers::Error::invalid())
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
    use axum::http::HeaderValue;
    use axum_extra::headers::Header;
    use base64::prelude::*;
    use prost::Message;
    use tap_aggregator::grpc::v2::SignedReceipt;
    use test_assets::{create_signed_receipt, create_signed_receipt_v2, SignedReceiptRequest};

    use super::TapHeader;
    use crate::tap::TapReceipt;

    #[tokio::test]
    async fn test_decode_valid_tap_v1_receipt_header() {
        let original_receipt = create_signed_receipt(SignedReceiptRequest::builder().build()).await;
        let serialized_receipt = serde_json::to_string(&original_receipt).unwrap();
        let header_value = HeaderValue::from_str(&serialized_receipt).unwrap();
        let header_values = vec![&header_value];
        let decoded_receipt = TapHeader::decode(&mut header_values.into_iter())
            .expect("tap receipt header value should be valid");

        assert_eq!(decoded_receipt, TapHeader(TapReceipt::V1(original_receipt)));
    }

    #[test_log::test(tokio::test)]
    async fn test_decode_valid_tap_v2_receipt_header() {
        let original_receipt = create_signed_receipt_v2().call().await;
        let protobuf_receipt = SignedReceipt::from(original_receipt.clone());
        let encoded = protobuf_receipt.encode_to_vec();
        let base64_encoded = BASE64_STANDARD.encode(encoded);
        let header_value = HeaderValue::from_str(&base64_encoded).unwrap();
        let header_values = vec![&header_value];
        let decoded_receipt = TapHeader::decode(&mut header_values.into_iter())
            .expect("tap receipt header value should be valid");

        assert_eq!(decoded_receipt, TapHeader(TapReceipt::V2(original_receipt)));
    }

    #[test]
    fn test_decode_non_string_tap_receipt_header() {
        let header_value = HeaderValue::from_static("123");
        let header_values = vec![&header_value];
        let result = TapHeader::decode(&mut header_values.into_iter());

        assert!(result.is_err());
    }

    #[test]
    fn test_decode_invalid_tap_receipt_header() {
        let header_value = HeaderValue::from_bytes(b"invalid").unwrap();
        let header_values = vec![&header_value];
        let result = TapHeader::decode(&mut header_values.into_iter());

        assert!(result.is_err());
    }
}
