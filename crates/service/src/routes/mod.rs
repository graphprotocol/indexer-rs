// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

//! # HTTP Route Handlers
//!
//! This module contains the HTTP route handlers for the indexer service API.
//!
//! ## Route Overview
//!
//! The route list lives in `docs/Routes.md`, and the registrations themselves in
//! `crate::service::router`. Keeping it in one place stops this list drifting from
//! what the service actually serves.
//!
//! ## Query Flow
//!
//! Paid queries (`/subgraphs/id/:id`) flow through the middleware stack defined
//! in the [`middleware`](crate::middleware) module before reaching the handler.
//! Free queries to `/status` bypass payment validation but are subject to field
//! allowlisting for security.
//!
//! ## Health Endpoints
//!
//! - [`health`]: Proxies to graph-node's indexing status for a specific deployment
//! - [`healthz`]: Checks connectivity to database and graph-node dependencies

pub mod cost;
pub mod dips_info;
mod health;
mod healthz;
mod request_handler;
mod static_subgraph;
mod status;

pub use dips_info::{dips_info, DipsInfoState};
pub use health::health;
pub use healthz::{healthz, HealthzState};
pub use request_handler::request_handler;
pub use static_subgraph::static_subgraph_request_handler;
pub use status::status;
