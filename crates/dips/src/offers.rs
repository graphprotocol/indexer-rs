// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! On-chain offer lookup for DIPS agreements.
//!
//! Under the offer-based authorization path, the payer submits an RCA offer
//! on-chain via `RecurringCollector.offer()` before dipper dispatches the
//! gRPC proposal. The contract stores `rcaOffers[agreementId] = {offerHash,
//! data}` and emits an `OfferStored` event, which the
//! graphprotocol/indexing-payments-subgraph indexes as an `Offer` entity
//! keyed by the bytes16 agreement ID.
//!
//! This module defines the [`OfferLookup`] trait used by
//! `validate_and_create_rca` to verify that a stored offer exists on-chain
//! with the same hash as the locally-computed
//! `eip712_signing_hash(rca, domain)`. If the hashes match, the payer has
//! authorized the RCA terms. If no offer is found, the proposal is rejected
//! with [`crate::DipsError::OfferNotFound`]; if the hashes mismatch, with
//! [`crate::DipsError::OfferMismatch`].
//!
//! # Security model
//!
//! This replaces the old signature-based path ([`crate::signers`]). Instead
//! of recovering an EIP-712 signature and checking the signer against an
//! escrow-authorized list, we rely on the on-chain presence of the offer
//! (which the contract's `offer()` function only accepts when
//! `msg.sender == rca.payer`). The payer's on-chain transaction IS the
//! authorization.
//!
//! # Subgraph lag
//!
//! The indexing-payments-subgraph indexes events asynchronously. A query
//! immediately after the offer() tx confirms may still miss. Implementations
//! may retry or return `None` and let the caller apply a backoff.

use async_trait::async_trait;
use thegraph_core::alloy::primitives::B256;
use uuid::Uuid;

/// Look up an on-chain RCA offer hash by agreement ID.
///
/// Production impls query the indexing-payments-subgraph; the test mock
/// [`MockOfferLookup`] stores offers in-memory for unit tests.
#[async_trait]
pub trait OfferLookup: Send + Sync + std::fmt::Debug {
    /// Return the offer hash stored on-chain for the given agreement ID,
    /// or `None` if no offer has been indexed yet.
    async fn get_offer_hash(&self, agreement_id: Uuid) -> Result<Option<B256>, anyhow::Error>;
}

/// Production [`OfferLookup`] that queries the
/// `graphprotocol/indexing-payments-subgraph` over HTTP.
///
/// The subgraph's `Offer` entity is keyed by the bytes16 agreement ID
/// (hex-encoded). This impl formats the UUID as `0x` + 32 hex chars and
/// sends a plain POST to the GraphQL endpoint.
#[derive(Clone)]
pub struct HttpOfferLookup {
    http_client: reqwest::Client,
    query_url: reqwest::Url,
}

impl std::fmt::Debug for HttpOfferLookup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpOfferLookup")
            .field("query_url", &self.query_url)
            .finish()
    }
}

impl HttpOfferLookup {
    pub fn new(http_client: reqwest::Client, query_url: reqwest::Url) -> Self {
        Self {
            http_client,
            query_url,
        }
    }

    /// Format the agreement ID bytes as the 0x-prefixed hex string the
    /// subgraph expects (`Bytes!` field, lowercase).
    fn encode_agreement_id(agreement_id: Uuid) -> String {
        let mut s = String::with_capacity(34);
        s.push_str("0x");
        for b in agreement_id.as_bytes() {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }
}

#[async_trait]
impl OfferLookup for HttpOfferLookup {
    async fn get_offer_hash(&self, agreement_id: Uuid) -> Result<Option<B256>, anyhow::Error> {
        let id_hex = Self::encode_agreement_id(agreement_id);
        let body = serde_json::json!({
            "query": "query GetOffer($id: ID!) { offer(id: $id) { offerHash } }",
            "variables": { "id": id_hex },
        });

        let resp = self
            .http_client
            .post(self.query_url.clone())
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("indexing-payments-subgraph POST failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "indexing-payments-subgraph returned {}: {}",
                status,
                text
            ));
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("indexing-payments-subgraph JSON parse: {e}"))?;

        // GraphQL errors land in the top-level `errors` array.
        if let Some(errs) = json.get("errors").and_then(|v| v.as_array()) {
            if !errs.is_empty() {
                return Err(anyhow::anyhow!(
                    "indexing-payments-subgraph GraphQL error: {errs:?}"
                ));
            }
        }

        // `data.offer` is null when no offer is stored for this id.
        let offer_hash = json.get("data").and_then(|d| d.get("offer")).and_then(|o| {
            if o.is_null() {
                None
            } else {
                o.get("offerHash").and_then(|h| h.as_str())
            }
        });

        match offer_hash {
            None => Ok(None),
            Some(hex_str) => {
                use std::str::FromStr;
                let parsed = B256::from_str(hex_str)
                    .map_err(|e| anyhow::anyhow!("invalid offerHash hex '{hex_str}': {e}"))?;
                Ok(Some(parsed))
            }
        }
    }
}

/// In-memory [`OfferLookup`] for tests.
///
/// Preload the map with `(agreement_id, hash)` pairs to simulate on-chain
/// offers. Any lookup for an agreement ID not in the map returns `None`.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct MockOfferLookup {
    offers: std::sync::Mutex<std::collections::HashMap<Uuid, B256>>,
}

#[cfg(test)]
impl MockOfferLookup {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, agreement_id: Uuid, offer_hash: B256) {
        self.offers.lock().unwrap().insert(agreement_id, offer_hash);
    }
}

#[cfg(test)]
#[async_trait]
impl OfferLookup for MockOfferLookup {
    async fn get_offer_hash(&self, agreement_id: Uuid) -> Result<Option<B256>, anyhow::Error> {
        Ok(self.offers.lock().unwrap().get(&agreement_id).copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use thegraph_core::alloy::primitives::FixedBytes;

    #[tokio::test]
    async fn mock_returns_inserted_hash() {
        let lookup = MockOfferLookup::new();
        let id = Uuid::from_u128(0x0123);
        let hash = FixedBytes::from([0xaa; 32]);
        lookup.insert(id, hash);

        assert_eq!(lookup.get_offer_hash(id).await.unwrap(), Some(hash));
    }

    #[tokio::test]
    async fn mock_returns_none_for_missing() {
        let lookup = MockOfferLookup::new();
        let id = Uuid::from_u128(0x0123);
        assert_eq!(lookup.get_offer_hash(id).await.unwrap(), None);
    }
}
