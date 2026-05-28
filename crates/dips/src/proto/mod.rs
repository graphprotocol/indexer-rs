//! Protocol buffer definitions for DIPS gRPC services.
//!
//! This module re-exports auto-generated protobuf types from prost-build.
//! Only one service interface remains: `IndexerDipsService`, the
//! Dipper-to-indexer RPC for delivering RCA proposals.

pub mod indexer;
