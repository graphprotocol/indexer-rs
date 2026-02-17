//! Protocol buffer definitions for DIPS gRPC services.
//!
//! This module re-exports auto-generated protobuf types from prost-build.
//! The `.proto` files define two service interfaces:
//!
//! - **gateway** - Gateway-to-Dipper communication (not used by indexer-rs)
//! - **indexer** - Dipper-to-indexer communication ([`IndexerDipsService`])
//!
//! The indexer service implements `IndexerDipsService` to receive RCA proposals.

pub mod gateway;
pub mod indexer;
