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
  - [ ] subgraph health check
  - [ ] extract receipt header
  - [ ] Free query
    - [x] Query struct
    - [ ] Free query auth token check
    - [x] Query routes + responses
  - [ ] Paid query
    - [ ] receipts graphQL schema
    - [ ] Allocation receipt manager
      - [ ] derive, cache, and look up attestation signers
        - [ ] contracts - connect by network chain id
          - [ ] network provider
      - [x] validate receipt format (need unit tests)
      - [x] parse receipt (need unit tests)
      - [x] validate signature (need unit tests)
      - [ ] store
    - [ ] monitor eligible allocations
      - [ ] network subgraph
      - [ ] operator wallet -> indexer address
- [ ] Deployment health server / status server
  - [ ] indexing status resolver - to query indexingStatuses
- [ ] Cost server
  - [ ] Cost graphQL schema
  - [ ] query indexer management client for Cost model
- [x] Constant service paths
  - [x] health
  - [x] ready to roll
  - [x] versions
  - [x] operator public key
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


### Dependency choices

- switching from actix-web to axum for the service server


### Quick attempts

Running locally with command
```


```

After service start up, try with command 
```
curl -X POST \
	-H 'Content-Type: application/json' \
  --data '{"query": "{_meta{block{number}}}"}' \
	http://127.0.0.1:8080/subgraphs/id/QmVhiE4nax9i86UBnBmQCYDzvjWuwHShYh7aspGPQhU5Sj
```