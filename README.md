# indexer-service-rs

## Introduction

A Rust impl for The Graph [indexer service](https://github.com/graphprotocol/indexer/tree/main/packages/indexer-service) to provide data services as an Indexer, integrated with [TAP](https://github.com/semiotic-ai/timeline-aggregation-protocol) which is a fast, efficient, and trustless unidirectional micro-payments system.

## Features

- Receive paid or free query requests and route to graph node
- Route "meta" queries on indexing statuses and deployment health
- Serve indexer information such as health, indexer version, and operator address
- Monitor allocations, attestation signers, and manage receipts using TAP, store receipts in the indexer database
- Record performance and service metrics

## Quick start

```txt
$ cargo run -p indexer-service-rs -- --help

Usage: indexer-service-rs --config <FILE>

Options:
      --config <FILE>  Path to the configuration file.
                       See https://github.com/graphprotocol/indexer-rs/tree/main/service for examples.
  -h, --help           Print help
```

All the configuration is done through a TOML file. Please see up-to-date TOML configuration templates:

- [Minimal configuration template (recommended)](config/minimal-config-example.toml)
- [Maximal configuration template (not recommended, dangerous settings)](config/maximal-config-example.toml)

## Upgrading

We follow conventional semantics for package versioning. An indexer may set a minor version specification for automatic patch updates while preventing breaking changes. To safely upgrading the package, we recommend the following steps:

1. **Review Release Notes**: Before upgrading, check the release notes for the new version to understand what changes, fixes, or new features are included.
2. **Review Documentation**: Check the up-to-date documentation for an accurate reflection of the changes made during the upgrade.
3. **Backup Configuration**: Save your current configuration files and any local modifications you've made to the existing codebase.
4. **Deploy**: Replace the old executable or docker image with the new one and restart the service to apply the upgrade.
5. **Monitor and Validate**: After the upgrade, monitor system behavior and performance metrics to validate that the service is running as expected.

These steps should ensure a smooth transition to the latest version of `indexer-service-rs`, harnessing new capabilities while maintaining system integrity.

## Contributing

[Contributions guide](/contributing.md)

## Supported request and response format examples

```bash
curl http://localhost:7600/
```
```
Service is up and running
```

```bash
curl http://localhost:7600/version
```
```json
{ "version":"0.1.0", "dependencies": {..} }
```

```bash
curl http://localhost:7600/info
```
```json
{ "publicKey": "0xacb05407d78129b5717bb51712d3e23a78a10929" }
```

# Subgraph queries
## Checks for receipts and authorization
```bash
curl -X POST \
  -H 'Content-Type: application/json' \
  -H 'Authorization: Bearer token-for-graph-node-query-endpoint' \
  --data '{"query": "{_meta{block{number}}}"}' \
  http://localhost:7600/subgraphs/id/QmacQnSgia4iDPWHpeY6aWxesRFdb8o5DKZUx96zZqEWrB
```
```json
{
    "attestable": true,
    "graphQLResponse": "{\"data\":{\"_meta\":{\"block\":{\"number\":10666745}}}}"
}
```
## Takes hex representation for subgraphs deployment id aside from IPFS hash representation
```bash
curl -X POST \
  -H 'Content-Type: application/json' \
  -H 'Authorization: Bearer token-for-graph-node-query-endpoint' \
  --data '{"query": "{_meta{block{number}}}"}' \
  http://localhost:7600/subgraphs/id/0xb655ca6f49e73728a102219726ff678d61d8fb792874792e9f0d9887dc616600
```
```json
{
    "attestable": true,
    "graphQLResponse": "{\"data\":{\"_meta\":{\"block\":{\"number\":10666745}}}}"
}
```

## Free query auth token check failed
```bash
curl -X POST \
  -H 'Content-Type: application/json' \
  -H 'Authorization: blah' \
  --data '{"query": "{_meta{block{number}}}"}' \
  http://localhost:7600/subgraphs/id/0xb655ca6f49e73728a102219726ff678d61d8fb792874792e9f0d9887dc616600
```
```json
{
  "message":"No valid receipt or free query auth token provided"
}
```

# Network queries
## Checks for auth and configuration to serve-network-subgraph

```bash
curl -X POST \
  -H 'Content-Type: application/json' \
  -H 'Authorization: token-for-network-subgraph' \
  --data '{"query": "{_meta{block{number}}}"}' \
  http://localhost:7600/network
```
```json
{ 
  "message":"No valid receipt or free query auth token provided" 
}
```

## Indexing status resolver - Route supported root field queries to graph node status endpoint
```bash
curl -X POST \
  -H 'Content-Type: application/json' \
  --data '{"query": "{blockHashFromNumber(network:\"mainnet\", blockNumber: 21033)}"}' \
  http://localhost:7600/status
```
```json
{
  "data": {
    "blockHashFromNumber": "0x6d8daae97a562b1fff22162515452acdd817c3d3c5cde1497b7d9eb6666a957e"
  }
}
```

## Indexing status resolver
```bash
curl -X POST \
  -H 'Content-Type: application/json' \
  --data '{"query": "{indexingStatuses {subgraph health}}"}' \
  http://localhost:7600/status
```
```json
{
  "data": {
    "indexingStatuses": [
      {
        "subgraph": "QmVhiE4nax9i86UBnBmQCYDzvjWuwHShYh7aspGPQhU5Sj",
        "health": "healthy"
      },
      {
        "subgraph": "QmWVtsWk8Pqn3zY3czDjyoVreshRLmoz9jko3mQ4uvxQDj",
        "health": "healthy"
      },
      {
        "subgraph": "QmacQnSgia4iDPWHpeY6aWxesRFdb8o5DKZUx96zZqEWrB",
        "health": "healthy"
      }
    ]
  }
}
```

## Indexing status resolver - Filter out the unsupported queries
```bash
curl -X POST \
  -H 'Content-Type: application/json' \
  --data '{"query": "{_meta{block{number}}}"}' \
  http://localhost:7600/status
```
```json
{
  "errors": [
    {
      "locations": [
        {
          "line": 1,
          "column": 2
        }
      ],
      "message": "Type `Query` has no field `_meta`"
    }
  ]
}
```

## Cost server - read-only graphql query

```bash
curl -X GET \
  -H 'Content-Type: application/json' \
  --data '{"query": "{ costModels(deployments: [\"Qmb5Ysp5oCUXhLA8NmxmYKDAX2nCMnh7Vvb5uffb9n5vss\"]) { deployment model variables }} "}' \
  http://localhost:7300/cost
```
```json
{
  "data": {
    "costModels": [
      {
        "deployment": "0xbd499f7673ca32ef4a642207a8bebdd0fb03888cf2678b298438e3a1ae5206ea",
        "model": "default => 0.00025;",
        "variables": null
      }
    ]
  }
}
```



## Dependency choices

- switching from actix-web to `axum` for the service server
- App profiling should utilize `perf`, flamegraphs or cpu profilers, and benches to track and collect performance data. The typescript implementation uses `gcloud-profile`
- Consider replacing and adding parts from TAP manager
- `postgres` database connection required to indexer management server database, shared with the indexer agent
- No migration in indexer service as it might introduce conflicts to the database; indexer agent is solely responsible for database management.

### Indexer common components

Temporarily live inside the indexer-service package under `src/common`.

Simple indexer management client to track NetworkSubgraph and postgres connection.

- NetworkSubgraph instance track both remote API endpoint and local deployment query endpoint.
  - TODO: query indexing status of local deployment, only use remote API as fallback.
- Keeps cost model schema and resolvers with postgres and graphQL types: `costModel(deployment)` and `costModels(deployments)`. If deployments is empty, all cost models are returned.
  - Global cost model fallback used when specific deployments are queried
- No database migration in indexer service as it might introduce schema conflicts; indexer agent is solely responsible for database management.

### Indexer native dependency

Linked dependency could not be linked directly with git url "https://github.com/graphprotocol/indexer" and path "packages/indexer-native/native" at the same time, and could not access it on crates.io. So copid the folder to local repo with the version at https://github.com/graphprotocol/indexer/blob/972658b3ce8c512ad7b4dc575d29cd9d5377e3fe/packages/indexer-native/native.

Since indexer-service will be written in Rust and no need for typescript, indexer-native's neon build and util has been removed.

Component `NativeSignatureVerifier` renamed to `SignatureVerifier`.

Separate package in the workspace under 'native'.

### common-ts components

Temporarily live inside the indexer-service package under `src/types`

- Address
- readNumber

## Components checklist (basic, not extensive)

- [x] Server path routing
  - [x] basic structure
  - [x] CORS
  - [x] timeouts
  - [x] Rate limiting levels
  - [x] Logger stream
- [ ] Query processor
  - [x] graph node query endpoint at specific subgraph path
  - [x] wrap request to and response from graph node
  - [x] extract receipt header
  - [x] Free query
    - [x] Query struct
    - [x] Free query auth token check
    - [x] Query routes + responses
    - [x] set `graph-attestable` in response header to `true`
  - [x] Network subgraph query
    - [x] Query struct
    - [x] serve network subgraph boolean + auth token check
    - [x] Query routes + responses
    - [x] set `graph-attestable` in response header to `false`
  - [ ] Paid query
    - [ ] receipts graphQL schema
    - [ ] [TAP](https://github.com/semiotic-ai/timeline-aggregation-protocol/) manager to handle receipts logic
      - [ ] derive, cache, and look up attestation signers
        - [ ] contracts - connect by network chain id
          - [ ] network provider
      - [x] validate receipt format (need unit tests)
      - [x] parse receipt (need unit tests)
      - [x] validate signature (need unit tests)
      - [ ] store
    - [ ] extract graph-attestable from graph node response header
    - [ ] monitor eligible allocations
      - [ ] network subgraph
      - [ ] operator wallet -> indexer address
  - [ ] subgraph health check
  - [ ] query timing logs
- [x] Deployment health server
  - [x] query status endpoint and process result
- [ ] Status server
  - [x] indexing status resolver - to query indexingStatuses
  - [ ] Filter for unsupported queries
- [x] Cost server
  - [x] Simple indexer management client to track postgres connection and network subgraph endpoint.
  - [x] serve queries with defined graphQL schema and psql resolvers to database: `costModel(deployment)` and `costModels(deployments)`. If deployments is empty, all cost models are returned.
  - [x] Global cost model fallback used when specific deployments are queried
- [x] Constant service paths
  - [x] health
  - [x] ready to roll
  - [x] versions
  - [x] operator public key
    - [x] validate mnemonics to public key
- [x] Import indexer native
- [ ] Metrics
  - [x] Metrics setup
  - [x] serve basic indexer service metrics
  - [ ] Add cost model metrics
- [x] CLI args
- [ ] App profiling
  - [ ] No gcloud profiling, can use `perf` to collect performance data.