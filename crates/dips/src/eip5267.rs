// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! EIP-5267 domain discovery: derive the RCA verification domain from the
//! RecurringCollector's own `eip712Domain()` report so it tracks in-place
//! contract upgrades instead of relying on compile-time constants.

use std::time::Duration;

use anyhow::Context;
use thegraph_core::alloy::{
    network::TransactionBuilder,
    primitives::{Address, Bytes, U256},
    providers::{Provider, ProviderBuilder},
    rpc::types::TransactionRequest,
    sol,
    sol_types::{Eip712Domain, SolCall},
};

sol! {
    /// EIP-5267 introspection: a contract reports its own EIP-712 domain.
    function eip712Domain() external view returns (
        bytes1 fields,
        string name,
        string version,
        uint256 chainId,
        address verifyingContract,
        bytes32 salt,
        uint256[] extensions
    );
}

/// Attempts before giving up on the RPC fetch.
const FETCH_ATTEMPTS: u32 = 3;
/// Delay between fetch attempts.
const FETCH_RETRY_DELAY: Duration = Duration::from_secs(2);
/// Upper bound on the reported domain name/version. The real values are short
/// ("RecurringCollector", "1"); anything huge is a hostile or broken RPC.
const MAX_DOMAIN_FIELD_LEN: usize = 256;

/// Fetch the RCA EIP-712 domain from the RecurringCollector via `eip712Domain()`.
/// Only network errors are retried; a response that fails to decode or fails
/// validation (wrong chain id or contract) errors immediately, retries won't fix it.
pub async fn fetch_rca_eip712_domain(
    rpc_url: &str,
    recurring_collector: Address,
    expected_chain_id: u64,
) -> anyhow::Result<Eip712Domain> {
    let mut attempt = 1;
    let output = loop {
        match call_eip712_domain(rpc_url, recurring_collector).await {
            Ok(output) => break output,
            Err(err) if attempt < FETCH_ATTEMPTS => {
                tracing::warn!(
                    attempt,
                    error = format!("{err:#}"),
                    "eip712Domain() fetch failed, retrying"
                );
                attempt += 1;
                tokio::time::sleep(FETCH_RETRY_DELAY).await;
            }
            Err(err) => return Err(err),
        }
    };
    let report = eip712DomainCall::abi_decode_returns(&output)
        .context("decoding the eip712Domain() response")?;
    let domain = domain_from_report(report, recurring_collector, expected_chain_id)?;
    tracing::info!(
        name = domain.name.as_deref().unwrap_or_default(),
        version = domain.version.as_deref().unwrap_or_default(),
        chain_id = expected_chain_id,
        verifying_contract = %recurring_collector,
        "RCA EIP-712 domain fetched from RecurringCollector"
    );
    Ok(domain)
}

async fn call_eip712_domain(rpc_url: &str, recurring_collector: Address) -> anyhow::Result<Bytes> {
    let provider = ProviderBuilder::new()
        .connect(rpc_url)
        .await
        .context("connecting to dips.rpc_url")?;
    let request = TransactionRequest::default()
        .with_to(recurring_collector)
        .with_input(eip712DomainCall {}.abi_encode());
    provider
        .call(request)
        .await
        .context("calling eip712Domain() on the RecurringCollector")
}

/// Build the verification domain from the contract's report, rejecting
/// reports that don't match the configured chain id and collector address.
fn domain_from_report(
    report: eip712DomainReturn,
    recurring_collector: Address,
    expected_chain_id: u64,
) -> anyhow::Result<Eip712Domain> {
    anyhow::ensure!(
        report.fields.0 == [0x0f],
        "eip712Domain() fields bitmap {:#04x} unsupported: expected 0x0f \
         (name, version, chainId, verifyingContract)",
        report.fields.0[0]
    );
    anyhow::ensure!(
        report.extensions.is_empty(),
        "eip712Domain() reports {} extensions, which are not supported",
        report.extensions.len()
    );
    anyhow::ensure!(
        !report.name.is_empty() && report.name.len() <= MAX_DOMAIN_FIELD_LEN,
        "eip712Domain() reports a domain name of {} bytes; expected 1-{MAX_DOMAIN_FIELD_LEN}",
        report.name.len()
    );
    anyhow::ensure!(
        !report.version.is_empty() && report.version.len() <= MAX_DOMAIN_FIELD_LEN,
        "eip712Domain() reports a domain version of {} bytes; expected 1-{MAX_DOMAIN_FIELD_LEN}",
        report.version.len()
    );
    anyhow::ensure!(
        report.chainId == U256::from(expected_chain_id),
        "eip712Domain() reports chain id {} but blockchain.chain_id is {expected_chain_id}; \
         check that dips.rpc_url points at the right network",
        report.chainId
    );
    anyhow::ensure!(
        report.verifyingContract == recurring_collector,
        "eip712Domain() reports verifying contract {} but dips.recurring_collector \
         is {recurring_collector}",
        report.verifyingContract
    );
    Ok(Eip712Domain::new(
        Some(report.name.into()),
        Some(report.version.into()),
        Some(U256::from(expected_chain_id)),
        Some(recurring_collector),
        None,
    ))
}

#[cfg(test)]
mod tests {
    use thegraph_core::alloy::primitives::{hex, Address, FixedBytes, B256, U256};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    use super::*;
    use crate::rca_eip712_domain;

    const CHAIN_ID: u64 = 1337;

    fn collector() -> Address {
        Address::repeat_byte(0xCC)
    }

    fn report(
        fields: u8,
        name: &str,
        version: &str,
        chain_id: u64,
        verifying_contract: Address,
    ) -> eip712DomainReturn {
        eip712DomainReturn {
            fields: FixedBytes([fields]),
            name: name.to_string(),
            version: version.to_string(),
            chainId: U256::from(chain_id),
            verifyingContract: verifying_contract,
            salt: B256::ZERO,
            extensions: vec![],
        }
    }

    #[test]
    fn test_domain_from_report_matches_builtin_domain() {
        // Arrange
        let report = report(0x0f, "RecurringCollector", "1", CHAIN_ID, collector());

        // Act
        let domain = domain_from_report(report, collector(), CHAIN_ID).expect("valid report");

        // Assert
        assert_eq!(domain, rca_eip712_domain(CHAIN_ID, collector()));
    }

    #[test]
    fn test_domain_from_report_rejects_chain_id_mismatch() {
        // Arrange
        let report = report(0x0f, "RecurringCollector", "1", 42161, collector());

        // Act
        let err = domain_from_report(report, collector(), CHAIN_ID).unwrap_err();

        // Assert
        assert!(err.to_string().contains("chain id"), "got: {err}");
    }

    #[test]
    fn test_domain_from_report_rejects_contract_mismatch() {
        // Arrange
        let report = report(
            0x0f,
            "RecurringCollector",
            "1",
            CHAIN_ID,
            Address::repeat_byte(0xDD),
        );

        // Act
        let err = domain_from_report(report, collector(), CHAIN_ID).unwrap_err();

        // Assert
        assert!(err.to_string().contains("verifying contract"), "got: {err}");
    }

    #[test]
    fn test_domain_from_report_rejects_nonempty_extensions() {
        // Arrange
        let mut report = report(0x0f, "RecurringCollector", "1", CHAIN_ID, collector());
        report.extensions = vec![U256::ZERO];

        // Act
        let err = domain_from_report(report, collector(), CHAIN_ID).unwrap_err();

        // Assert
        assert!(err.to_string().contains("extensions"), "got: {err}");
    }

    #[test]
    fn test_domain_from_report_rejects_empty_name() {
        // Arrange
        let report = report(0x0f, "", "1", CHAIN_ID, collector());

        // Act
        let err = domain_from_report(report, collector(), CHAIN_ID).unwrap_err();

        // Assert
        assert!(err.to_string().contains("domain name"), "got: {err}");
    }

    #[test]
    fn test_domain_from_report_rejects_oversized_version() {
        // Arrange
        let oversized = "1".repeat(MAX_DOMAIN_FIELD_LEN + 1);
        let report = report(
            0x0f,
            "RecurringCollector",
            &oversized,
            CHAIN_ID,
            collector(),
        );

        // Act
        let err = domain_from_report(report, collector(), CHAIN_ID).unwrap_err();

        // Assert
        assert!(err.to_string().contains("domain version"), "got: {err}");
    }

    #[test]
    fn test_domain_from_report_rejects_unknown_fields_bitmap() {
        // Arrange: 0x1f would mean the domain also uses a salt, which the
        // built domain would silently omit, so it must be rejected.
        let report = report(0x1f, "RecurringCollector", "1", CHAIN_ID, collector());

        // Act
        let err = domain_from_report(report, collector(), CHAIN_ID).unwrap_err();

        // Assert
        assert!(err.to_string().contains("fields bitmap"), "got: {err}");
    }

    /// Replies to any JSON-RPC request with the given eth_call result,
    /// echoing the request id so the client accepts the response.
    struct EthCallResponder {
        result: Vec<u8>,
    }

    impl Respond for EthCallResponder {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": body["id"],
                "result": format!("0x{}", hex::encode(&self.result)),
            }))
        }
    }

    #[tokio::test]
    async fn test_fetch_rca_eip712_domain_round_trip() {
        // Arrange
        let encoded = eip712DomainCall::abi_encode_returns(&report(
            0x0f,
            "RecurringCollector",
            "1",
            CHAIN_ID,
            collector(),
        ));
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::method("POST"))
            .respond_with(EthCallResponder { result: encoded })
            .mount(&server)
            .await;

        // Act
        let domain = fetch_rca_eip712_domain(&server.uri(), collector(), CHAIN_ID)
            .await
            .expect("fetch should succeed");

        // Assert
        assert_eq!(domain, rca_eip712_domain(CHAIN_ID, collector()));
    }
}
