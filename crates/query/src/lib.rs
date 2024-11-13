// Copyright 2023-, Edge & Node, GraphOps, and Semiotic Labs.
// SPDX-License-Identifier: Apache-2.0

use graphql_client::GraphQLQuery;

pub mod dispute_manager {
    use alloy::primitives::Address;
    use graphql_client::GraphQLQuery;
    type Bytes = Address;

    #[derive(GraphQLQuery)]
    #[graphql(
        schema_path = "graphql/network.schema.graphql",
        query_path = "graphql/dispute.query.graphql",
        response_derives = "Debug",
        variables_derives = "Clone"
    )]
    pub struct DisputeManager;

    pub use dispute_manager::Variables;
}

pub mod escrow_account {
    use graphql_client::GraphQLQuery;
    type BigInt = String;

    #[derive(GraphQLQuery)]
    #[graphql(
        schema_path = "graphql/tap.schema.graphql",
        query_path = "graphql/escrow_account.query.graphql",
        response_derives = "Debug",
        variables_derives = "Clone"
    )]
    pub struct EscrowAccountQuery;

    pub use escrow_account_query::Variables;
}

pub mod allocations_query {
    use alloy::primitives::{B256, U256};
    use graphql_client::GraphQLQuery;
    type BigInt = U256;
    type Bytes = B256;

    #[derive(GraphQLQuery)]
    #[graphql(
        schema_path = "graphql/network.schema.graphql",
        query_path = "graphql/allocations.query.graphql",
        response_derives = "Debug",
        variables_derives = "Clone"
    )]
    pub struct AllocationsQuery;

    pub use allocations_query::*;
}

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/indexing_status.schema.graphql",
    query_path = "graphql/subgraph_health.query.graphql",
    response_derives = "Debug",
    variables_derives = "Clone"
)]
pub struct HealthQuery;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/network.schema.graphql",
    query_path = "graphql/epoch.query.graphql",
    response_derives = "Debug",
    variables_derives = "Clone"
)]
pub struct CurrentEpoch;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/test.schema.graphql",
    query_path = "graphql/user.query.graphql",
    response_derives = "Debug",
    variables_derives = "Clone"
)]
pub struct UserQuery;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/indexing_status.schema.graphql",
    query_path = "graphql/subgraph_deployment_status.graphql",
    response_derives = "Debug",
    variables_derives = "Clone"
)]
pub struct DeploymentStatusQuery;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/tap.schema.graphql",
    query_path = "graphql/unfinalized_tx.query.graphql",
    response_derives = "Debug",
    variables_derives = "Clone"
)]
pub struct UnfinalizedTransactions;

pub mod closed_allocations {
    use graphql_client::GraphQLQuery;

    type Bytes = String;

    #[derive(GraphQLQuery)]
    #[graphql(
        schema_path = "graphql/network.schema.graphql",
        query_path = "graphql/closed_allocations.query.graphql",
        response_derives = "Debug",
        variables_derives = "Clone"
    )]
    pub struct ClosedAllocations;
    pub use closed_allocations::*;
}

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/tap.schema.graphql",
    query_path = "graphql/transactions.query.graphql",
    response_derives = "Debug",
    variables_derives = "Clone"
)]
pub struct TapTransactions;
