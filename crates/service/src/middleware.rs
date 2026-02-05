// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! # Request Processing Middleware
//!
//! This module provides the middleware stack for processing incoming GraphQL queries
//! through the indexer service. The middleware is layered to handle different aspects
//! of request processing in a composable manner.
//!
//! ## Middleware Chain
//!
//! Requests flow through the middleware in the following order:
//!
//! ```text
//! Request
//!     │
//!     ├─► deployment_middleware    Extract deployment ID from URL path
//!     │
//!     ├─► receipt_middleware       Parse and validate TAP receipt from header
//!     │
//!     ├─► allocation_middleware    Map deployment to allocation for paid queries
//!     │
//!     ├─► sender_middleware        Validate sender from receipt signature
//!     │
//!     ├─► labels_middleware        Add tracing labels for observability
//!     │
//!     ├─► signer_middleware        Attach attestation signer for response signing
//!     │
//!     ├─► context_middleware       Store receipt via TAP context
//!     │
//!     ├─► Handler                  Execute GraphQL query against graph-node
//!     │
//!     └─► attestation_middleware   Generate attestation for response
//!             │
//!             ▼
//!         Response
//! ```
//!
//! ## Key Components
//!
//! - [`deployment_middleware`]: Extracts the deployment ID from the request path
//! - [`receipt_middleware`]: Parses TAP receipts from the `tap-receipt` header
//! - [`allocation_middleware`]: Maps deployments to allocations for billing
//! - [`sender_middleware`]: Recovers and validates the sender from receipt signatures
//! - [`signer_middleware`]: Provides attestation signing capability
//! - [`attestation_middleware`]: Generates cryptographic attestations for responses
//! - [`context_middleware`]: Handles receipt storage through the TAP context

mod allocation;
mod attestation;
mod attestation_signer;
pub mod auth;
mod deployment;
mod labels;
mod prometheus_metrics;
mod sender;
mod tap_context;
mod tap_receipt;

pub use allocation::{allocation_middleware, AllocationState};
pub use attestation::{attestation_middleware, AttestationInput};
pub use attestation_signer::{signer_middleware, AttestationState};
pub use deployment::deployment_middleware;
pub use labels::labels_middleware;
pub use prometheus_metrics::PrometheusMetricsMiddlewareLayer;
pub use sender::{sender_middleware, Sender, SenderState};
pub use tap_context::{context_middleware, QueryBody, TapContextState};
pub use tap_receipt::receipt_middleware;
