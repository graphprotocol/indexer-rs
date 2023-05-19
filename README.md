# indexer-service-rs

Experimental rust impl for The Graph [indexer service](https://github.com/graphprotocol/indexer/tree/main/packages/indexer-service)

> Don't know if receipt validation is actually correct, need testing

## Components checklist (basic, not extensive)

- [ ] Server path routing
  - [x] basic structure
  - [x] CORS
  - [ ] Rate limiting levels
  - [ ] Logger stream
- [ ] Query processor
  - [x] graph node query endpoint at specific subgraph path
  - [x] wrap request to and response from graph node
  - [ ] extract receipt header
  - [ ] Free query
    - [ ] Free query auth token check
  - [ ] Paid query
    - [ ] receipts graphQL schema
    - [ ] Allocation receipt manager
      - [ ] derive, cache, and look up attestation signers
        - [ ] contracts - connect by network chain id
          - [ ] network provider
      - [x] validate receipt format (need validating)
      - [x] parse receipt (need validating)
      - [x] validate signature (need validating)
      - [ ] store
    - [ ] monitor eligible allocations
      - [ ] network subgraph
      - [ ] operator wallet -> indexer address
- [ ] Deployment health server / status server
  - [ ] indexing status resolver - to query indexingStatuses
- [ ] Cost server
  - [ ] Cost graphQL schema
  - [ ] query indexer management client for Cost model
- [ ] Constant service paths
  - [ ] status check
  - [ ] versions
  - [ ] operator public key
- [x] Import indexer native
- [ ] Metrics

### Indexer common components

Temporarily live inside the indexer-service package under `src/common`.


### Indexer native dependency

Linked dependency could not be linked directly with git url "https://github.com/graphprotocol/indexer" and path "packages/indexer-native/native" at the same time, and could not access it on crates.io. So copid the folder to local repo with the version at https://github.com/graphprotocol/indexer/blob/972658b3ce8c512ad7b4dc575d29cd9d5377e3fe/packages/indexer-native/native.

Since indexer-service will be written in Rust, neon build and util has been removed. 

Use component `NativeSignatureVerifier` as `SignatureVerifier`

### common-ts components

Temporarily live inside the indexer-service package under `src/types`

- Address
- readNumber
