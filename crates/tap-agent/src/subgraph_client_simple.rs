// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! Simple SubgraphClient Abstraction for Testing
//!
//! This provides a minimal abstraction to enable Layer 2 integration testing
//! without the complexity of async trait objects.

use crate::agent::sender_accounts_manager::AllocationId;
use anyhow::Result;
use indexer_monitor::SubgraphClient;
use std::sync::Arc;

/// Simple enum wrapper for different SubgraphClient implementations
/// This solves the dependency injection problem for testing
#[derive(Clone)]
pub enum SimpleSubgraphClient {
    /// Production implementation using real SubgraphClient
    Production(Arc<SubgraphClient>),
    /// Mock implementation for testing
    Mock(SimpleSubgraphMock),
}

impl SimpleSubgraphClient {
    /// Create a production client wrapper
    pub fn production(client: Arc<SubgraphClient>) -> Self {
        Self::Production(client)
    }

    /// Create a mock client for testing
    pub fn mock(mock: SimpleSubgraphMock) -> Self {
        Self::Mock(mock)
    }

    /// Validate allocation status for receipt processing
    /// This is the key operation that SenderAllocationTask needs
    pub async fn validate_allocation(&self, _allocation_id: &AllocationId) -> Result<bool> {
        match self {
            Self::Production(_client) => {
                // TODO: Implement real allocation validation logic
                // This would query the network subgraph to check allocation status
                Ok(true)
            }
            Self::Mock(mock) => Ok(mock.should_validate_allocation),
        }
    }

    /// Check if the subgraph client is healthy and ready
    pub async fn is_healthy(&self) -> bool {
        match self {
            Self::Production(_client) => {
                // TODO: Implement health check logic
                // This could query a simple health endpoint or check connectivity
                true
            }
            Self::Mock(mock) => mock.is_healthy,
        }
    }
}

/// Simple mock for testing SubgraphClient behavior
#[derive(Clone)]
pub struct SimpleSubgraphMock {
    /// Controls whether allocation validation succeeds
    pub should_validate_allocation: bool,
    /// Controls whether the client appears healthy
    pub is_healthy: bool,
}

impl SimpleSubgraphMock {
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

impl Default for SimpleSubgraphMock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::sender_accounts_manager::AllocationId;
    use thegraph_core::alloy::primitives::Address;

    #[tokio::test]
    async fn test_mock_allocation_validation_success() {
        let mock = SimpleSubgraphMock::new().with_allocation_validation(true);
        let client = SimpleSubgraphClient::mock(mock);

        let test_address = Address::from([0x42; 20]);
        let allocation_id = AllocationId::Legacy(test_address.into());
        let result = client.validate_allocation(&allocation_id).await.unwrap();

        assert!(result);
    }

    #[tokio::test]
    async fn test_mock_allocation_validation_failure() {
        let mock = SimpleSubgraphMock::new().with_allocation_validation(false);
        let client = SimpleSubgraphClient::mock(mock);

        let test_address = Address::from([0x42; 20]);
        let allocation_id = AllocationId::Legacy(test_address.into());
        let result = client.validate_allocation(&allocation_id).await.unwrap();

        assert!(!result);
    }

    #[tokio::test]
    async fn test_mock_health_check() {
        let healthy_mock = SimpleSubgraphMock::new().with_health_status(true);
        let healthy_client = SimpleSubgraphClient::mock(healthy_mock);

        let unhealthy_mock = SimpleSubgraphMock::new().with_health_status(false);
        let unhealthy_client = SimpleSubgraphClient::mock(unhealthy_mock);

        assert!(healthy_client.is_healthy().await);
        assert!(!unhealthy_client.is_healthy().await);
    }
}
