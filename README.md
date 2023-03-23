# indexer-service-rs

Experimental rust impl for The Graph [indexer service](https://github.com/graphprotocol/indexer/tree/main/packages/indexer-service)

## Components checklist (basic, not extensive)

- [ ] Server path routing
  - [x] basic structure
  - [ ] CORS
  - [ ] Rate limiting levels
  - [ ] Logger stream
- [ ] Query processor
  - [x] graph node query endpoint at specific subgraph path
  - [x] wrap request to and response from graph node
  - [ ] extract receipt header
  - [ ] Free query
    - [ ] Free query auth token check
  - [ ] Paid query
    - [ ] Allocation receipt manager
      - [ ] derive, cache, and look up attestation signers
        - [ ] contracts - connect by network chain id
          - [ ] network provider
      - [ ] validate receipt format, parse receipt, validate signature, store
    - [ ] monitor eligible allocations
      - [ ] network subgraph
      - [ ] operator wallet -> indexer address
- [ ] Deployment health server / status server
  - [ ] indexing status resolver - to query indexingStatuses
- [ ] Cost server
  - [ ] Cost graphQL schema
  - [ ] query indexer management client for Cost model
- [ ] Constant service paths: status check, versions, and operator public key
- [ ] Metrics
