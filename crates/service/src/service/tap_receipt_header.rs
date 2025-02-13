// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use axum_extra::headers::{self, Header, HeaderName, HeaderValue};
use base64::prelude::*;
use lazy_static::lazy_static;
use prometheus::{register_counter, Counter};
use prost::Message;
use tap_aggregator::grpc;
use tap_graph::SignedReceipt;

use crate::tap::TapReceipt;

#[derive(Debug, PartialEq)]
pub struct TapHeader(pub TapReceipt);

lazy_static! {
    static ref TAP_RECEIPT: HeaderName = HeaderName::from_static("tap-receipt");
    pub static ref TAP_RECEIPT_INVALID: Counter =
        register_counter!("indexer_tap_invalid_total", "Invalid tap receipt decode",).unwrap();
}

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
                    tracing::debug!("Decoded v2");
                    let receipt = grpc::v2::SignedReceipt::decode(raw_receipt.as_ref())?;
                    Ok(TapHeader(TapReceipt::V2(receipt.try_into()?)))
                }
                Err(_) => {
                    tracing::debug!("Could not decode v2, trying v1");
                    let parsed_receipt: SignedReceipt =
                        serde_json::from_slice(raw_receipt.as_ref())?;
                    Ok(TapHeader(TapReceipt::V1(parsed_receipt)))
                }
            }
        };
        execute()
            .map_err(|_| headers::Error::invalid())
            .inspect_err(|_| TAP_RECEIPT_INVALID.inc())
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
