// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

pub mod address;
pub mod allocations;
pub mod attestations;
pub mod cost_model;
pub mod escrow_accounts;
pub mod graphql;
pub mod middleware;
pub mod subgraph_client;
pub mod tap;
pub mod watcher;

#[cfg(test)]
mod test_vectors;



// monitors
//      - allocations
         // - new allocation
         // - close allocation
//      - attestations
//      - dispute_manager
//      - status_monitor
//          - allow subgraph clients to and have a single monitor
//      - escrow_accounts
         // - new sender
         // - update escrow balance
         // - new signer
         // - remove signer
// queries
//      - allocation
//      - escrow_accounts
//      - dispute_manager
// watcher
// middlewares
//      - free query middleware -> validates free query
//      - allocation middleware -> injects allocation id
//      - attestation middleware -> inject the attestation
//      - tap middleware -> validates tap query
// subgraph_client
// tap
//      - manager
//      - checks
//          - allocation eligible check
//          - allocation id not redeemed check
//          - deny list check
//          - timestamp check
//          - sender balance check
//          - receipt max value check
//


