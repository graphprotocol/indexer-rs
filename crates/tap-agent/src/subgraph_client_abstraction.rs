// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! TAP SubgraphClient Abstraction for Testing
//!
//! This provides a minimal abstraction to enable Layer 2 integration testing
//! without the complexity of async trait objects.

use crate::agent::allocation_id::AllocationId;
use anyhow::Result;
use indexer_monitor::SubgraphClient;
use serde_json;
use std::sync::Arc;

/// TAP-specific enum wrapper for different SubgraphClient implementations
/// This solves the dependency injection problem for testing
#[derive(Clone)]
pub enum TapSubgraphClient {
    /// Production implementation using real SubgraphClient
    Production(Arc<SubgraphClient>),
    /// Mock implementation for testing
    Mock(TapSubgraphMock),
}

impl TapSubgraphClient {
    /// Create a production client wrapper
    pub fn production(client: Arc<SubgraphClient>) -> Self {
        Self::Production(client)
    }

    /// Create a mock client for testing
    pub fn mock(mock: TapSubgraphMock) -> Self {
        Self::Mock(mock)
    }

    /// Validate allocation status for receipt processing
    /// This is the key operation that SenderAllocationTask needs
    pub async fn validate_allocation(&self, allocation_id: &AllocationId) -> Result<bool> {
        match self {
            Self::Production(client) => {
                Self::validate_allocation_via_subgraph(client, allocation_id).await
            }
            Self::Mock(mock) => Ok(mock.should_validate_allocation),
        }
    }

    /// Query the network subgraph to validate allocation status
    async fn validate_allocation_via_subgraph(
        client: &SubgraphClient,
        allocation_id: &AllocationId,
    ) -> Result<bool> {
        // Convert AllocationId to string format for GraphQL query
        let allocation_id_str = match allocation_id {
            AllocationId::Legacy(id) => id.to_string(),
            AllocationId::Horizon(id) => id.to_string(),
        };

        // Simple GraphQL query to check if allocation exists and is active
        let query = format!(
            r#"{{
                allocation(id: "{}") {{
                    id
                    status
                    indexer {{
                        id
                    }}
                }}
            }}"#,
            allocation_id_str.to_lowercase()
        );

        tracing::debug!(
            allocation_id = %allocation_id_str,
            "Validating allocation status via network subgraph"
        );

        // Execute the GraphQL query
        let response = client
            .query_raw(query.into())
            .await
            .map_err(|e| anyhow::anyhow!("Failed to query allocation status: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Failed to read response body".to_string());
            tracing::warn!(
                allocation_id = %allocation_id_str,
                status = %status,
                body = %body,
                "Subgraph query failed"
            );
            return Ok(false);
        }

        let response_text = response
            .text()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read response body: {}", e))?;

        // Parse the JSON response to check allocation status
        let response_json: serde_json::Value = serde_json::from_str(&response_text)
            .map_err(|e| anyhow::anyhow!("Failed to parse JSON response: {}", e))?;

        // Check if allocation exists and is active
        let is_valid = response_json
            .get("data")
            .and_then(|data| data.get("allocation"))
            .map(|allocation| {
                // If allocation is null, it doesn't exist
                if allocation.is_null() {
                    tracing::debug!(
                        allocation_id = %allocation_id_str,
                        "Allocation not found in network subgraph"
                    );
                    false
                } else {
                    // Check if allocation status is "Active"
                    let status = allocation
                        .get("status")
                        .and_then(|s| s.as_str())
                        .unwrap_or("");

                    let is_active = status == "Active";

                    tracing::debug!(
                        allocation_id = %allocation_id_str,
                        status = %status,
                        is_active = %is_active,
                        "Allocation validation result"
                    );

                    is_active
                }
            })
            .unwrap_or(false);

        Ok(is_valid)
    }

    /// Check if the subgraph client is healthy and ready
    pub async fn is_healthy(&self) -> bool {
        match self {
            Self::Production(client) => Self::check_subgraph_health(client).await,
            Self::Mock(mock) => mock.is_healthy,
        }
    }

    /// Perform a health check against the subgraph endpoint
    async fn check_subgraph_health(client: &SubgraphClient) -> bool {
        // Use a simple _meta query to check connectivity and basic functionality
        let health_query = r#"{
            _meta {
                block {
                    number
                    hash
                }
            }
        }"#;

        tracing::debug!("Performing subgraph health check");

        match client.query_raw(health_query.to_string().into()).await {
            Ok(response) => {
                let is_healthy = response.status().is_success();

                if is_healthy {
                    tracing::debug!("Subgraph health check passed");
                } else {
                    tracing::warn!(
                        status = %response.status(),
                        "Subgraph health check failed - HTTP error"
                    );
                }

                is_healthy
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Subgraph health check failed - connection error"
                );
                false
            }
        }
    }
}

/// TAP-specific mock for testing SubgraphClient behavior
#[derive(Clone)]
pub struct TapSubgraphMock {
    /// Controls whether allocation validation succeeds
    pub should_validate_allocation: bool,
    /// Controls whether the client appears healthy
    pub is_healthy: bool,
}

impl TapSubgraphMock {
    /// Create a new mock with default settings
    pub fn new() -> Self {
        Self {
            should_validate_allocation: true,
            is_healthy: true,
        }
    }

    /// Configure the mock to simulate allocation validation failures
    pub fn with_allocation_validation(mut self, should_validate: bool) -> Self {
        self.should_validate_allocation = should_validate;
        self
    }

    /// Configure the mock to simulate health check results
    pub fn with_health_status(mut self, is_healthy: bool) -> Self {
        self.is_healthy = is_healthy;
        self
    }
}

impl Default for TapSubgraphMock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::allocation_id::AllocationId;
    use thegraph_core::alloy::primitives::Address;

    #[tokio::test]
    async fn test_mock_allocation_validation_success() {
        let mock = TapSubgraphMock::new().with_allocation_validation(true);
        let client = TapSubgraphClient::mock(mock);

        let test_address = Address::from([0x42; 20]);
        let allocation_id = AllocationId::Legacy(test_address.into());
        let result = client.validate_allocation(&allocation_id).await.unwrap();

        assert!(result);
    }

    #[tokio::test]
    async fn test_mock_allocation_validation_failure() {
        let mock = TapSubgraphMock::new().with_allocation_validation(false);
        let client = TapSubgraphClient::mock(mock);

        let test_address = Address::from([0x42; 20]);
        let allocation_id = AllocationId::Legacy(test_address.into());
        let result = client.validate_allocation(&allocation_id).await.unwrap();

        assert!(!result);
    }

    #[tokio::test]
    async fn test_mock_health_check() {
        let healthy_mock = TapSubgraphMock::new().with_health_status(true);
        let healthy_client = TapSubgraphClient::mock(healthy_mock);

        let unhealthy_mock = TapSubgraphMock::new().with_health_status(false);
        let unhealthy_client = TapSubgraphClient::mock(unhealthy_mock);

        assert!(healthy_client.is_healthy().await);
        assert!(!unhealthy_client.is_healthy().await);
    }
}
