// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use axum_extra::headers::{self, Header, HeaderName, HeaderValue};
use lazy_static::lazy_static;
use prometheus::{register_counter, Counter};
use serde::de;
use serde_json::Value;
use tap_core::receipt::SignedReceipt as SignedReceiptV1;
use tap_core_v2::receipt::SignedReceipt as SignedReceiptV2;

#[derive(Debug, PartialEq, Clone, serde::Serialize)]
#[serde(untagged)]
pub enum TapReceipt {
    V1(SignedReceiptV1),
    V2(SignedReceiptV2),
}

impl<'de> serde::Deserialize<'de> for TapReceipt {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let temp = Value::deserialize(deserializer)?;

        let is_v1 = temp
            .as_object()
            .ok_or(de::Error::custom("Didn't receive an object"))?
            .get("message")
            .ok_or(de::Error::custom("There's no message in the object"))?
            .as_object()
            .ok_or(de::Error::custom("Message is not an object"))?
            .contains_key("allocation_id");

        if is_v1 {
            // Try V1 first
            serde_json::from_value::<SignedReceiptV1>(temp)
                .map(TapReceipt::V1)
                .map_err(de::Error::custom)
        } else {
            // Fall back to V2
            serde_json::from_value::<SignedReceiptV2>(temp)
                .map(TapReceipt::V2)
                .map_err(de::Error::custom)
        }
    }
}

impl From<SignedReceiptV1> for TapReceipt {
    fn from(value: SignedReceiptV1) -> Self {
        Self::V1(value)
    }
}

impl From<SignedReceiptV2> for TapReceipt {
    fn from(value: SignedReceiptV2) -> Self {
        Self::V2(value)
    }
}

impl TryFrom<TapReceipt> for SignedReceiptV1 {
    type Error = anyhow::Error;

    fn try_from(value: TapReceipt) -> Result<Self, Self::Error> {
        match value {
            TapReceipt::V2(_) => Err(anyhow::anyhow!("TapReceipt is V2")),
            TapReceipt::V1(receipt) => Ok(receipt),
        }
    }
}

impl TryFrom<TapReceipt> for SignedReceiptV2 {
    type Error = anyhow::Error;

    fn try_from(value: TapReceipt) -> Result<Self, Self::Error> {
        match value {
            TapReceipt::V1(_) => Err(anyhow::anyhow!("TapReceipt is V1")),
            TapReceipt::V2(receipt) => Ok(receipt),
        }
    }
}

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
            let parsed_receipt: TapReceipt =
                serde_json::from_str(raw_receipt).map_err(|_| headers::Error::invalid())?;
            Ok(parsed_receipt)
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
    use axum::http::HeaderValue;
    use axum_extra::headers::Header;
    use test_assets::{
        create_signed_receipt, create_signed_receipt_v2, SignedReceiptRequest,
        SignedReceiptV2Request,
    };

    use super::TapReceipt;

    #[tokio::test]
    async fn test_decode_valid_tap_receipt_header() {
        let original_receipt = create_signed_receipt(SignedReceiptRequest::builder().build()).await;
        let serialized_receipt = serde_json::to_string(&original_receipt).unwrap();

        let original_receipt_v1: TapReceipt = original_receipt.clone().into();
        let serialized_receipt_v1 = serde_json::to_string(&original_receipt_v1).unwrap();

        assert_eq!(serialized_receipt, serialized_receipt_v1);

        println!("Was able to serialize properly: {serialized_receipt_v1:?}");
        let deserialized: TapReceipt = serde_json::from_str(&serialized_receipt_v1).unwrap();
        println!("Was able to deserialize properly: {deserialized:?}");
        let header_value = HeaderValue::from_str(&serialized_receipt_v1).unwrap();
        let header_values = vec![&header_value];
        let decoded_receipt = TapReceipt::decode(&mut header_values.into_iter())
            .expect("tap receipt header value should be valid");

        assert_eq!(decoded_receipt, original_receipt.into());
    }

    #[tokio::test]
    async fn test_decode_valid_tap_v2_receipt_header() {
        let original_receipt =
            create_signed_receipt_v2(SignedReceiptV2Request::builder().build()).await;
        let serialized_receipt = serde_json::to_string(&original_receipt).unwrap();

        let original_receipt_v1: TapReceipt = original_receipt.clone().into();
        let serialized_receipt_v1 = serde_json::to_string(&original_receipt_v1).unwrap();

        assert_eq!(serialized_receipt, serialized_receipt_v1);

        println!("Was able to serialize properly: {serialized_receipt_v1:?}");
        let deserialized: TapReceipt = serde_json::from_str(&serialized_receipt_v1).unwrap();
        println!("Was able to deserialize properly: {deserialized:?}");
        let header_value = HeaderValue::from_str(&serialized_receipt_v1).unwrap();
        let header_values = vec![&header_value];
        let decoded_receipt = TapReceipt::decode(&mut header_values.into_iter())
            .expect("tap receipt header value should be valid");

        assert_eq!(decoded_receipt, original_receipt.into());
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
