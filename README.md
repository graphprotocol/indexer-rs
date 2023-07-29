# indexer-service-rs

Experimental rust impl for The Graph [indexer service](https://github.com/graphprotocol/indexer/tree/main/packages/indexer-service)

## Dependency choices

- switching from actix-web to `axum` for the service server
- App profiling should utilize `perf`, flamegraphs or cpu profilers, and benches to track and collect performance data. The typescript implementation uses `gcloud-profile`
- Consider replacing and adding parts from TAP manager

> Don't know if receipt validation is actually correct, need testing

## Components checklist (basic, not extensive)

- [ ] Server path routing
  - [x] basic structure
  - [x] CORS
  - [x] timeouts
  - [ ] Rate limiting levels
  - [ ] Logger stream
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
    - [x] validate mnemonics to public key
- [x] Import indexer native
- [ ] Metrics
  - [x] Metrics setup
  - [x] serve basic indexer service metrics
  - [ ] Add cost model metrics 
- [x] CLI args
- [ ] App profiling
  - [ ] No gcloud profiling, can use `perf` to collect performance data.

### Indexer common components

Temporarily live inside the indexer-service package under `src/common`.


### Indexer native dependency

Linked dependency could not be linked directly with git url "https://github.com/graphprotocol/indexer" and path "packages/indexer-native/native" at the same time, and could not access it on crates.io. So copid the folder to local repo with the version at https://github.com/graphprotocol/indexer/blob/972658b3ce8c512ad7b4dc575d29cd9d5377e3fe/packages/indexer-native/native.

Since indexer-service will be written in Rust and no need for typescript, indexer-native's neon build and util has been removed. 

Component `NativeSignatureVerifier` renamed to `SignatureVerifier`.

Separate package in the workspace under 'native'.

### common-ts components

Temporarily live inside the indexer-service package under `src/types`

- Address
- readNumber

### Quick attempts

Configure required start-up args, check description by 
```
cargo run -p service -- --help
```
Required
```
  --ethereum 
  --ethereum-polling-interval 
  --mnemonic 
  --indexer-address 

  --port 
  --metrics-port 
  --graph-node-query-endpoint 
  --graph-node-status-endpoint 
  --log-level 
  --gcloud-profiling
  --free-query-auth-token 
  
  --postgres-host 
  --postgres-port 
  --postgres-database 
  --postgres-username 
  --postgres-password 
  
  --network-subgraph-deployment 
  --network-subgraph-endpoint 
  --network-subgraph-auth-token 
  --serve-network-subgraph
  --allocation-syncing-interval 
  --client-signer-address 
```

Afterwards run
```
cargo run -p service

```

After service start up, try with command 
```
curl -X POST \
	-H 'Content-Type: application/json' \
  --data '{"query": "{_meta{block{number}}}"}' \
	http://127.0.0.1:8080/subgraphs/id/QmVhiE4nax9i86UBnBmQCYDzvjWuwHShYh7aspGPQhU5Sj
```


### Checks


```
✗ curl http://localhost:7300/             
Ready to roll! 
✗ curl http://localhost:7300/health
{"healthy":true}
✗ curl http://localhost:7300/version
{"version":"0.1.0","dependencies":{}}
✗ curl http://localhost:7300/operator/info
{"publicKey":"0xacb05407d78129b5717bb51712d3e23a78a10929"}






✗ curl -X POST -H 'Content-Type: application/json' -H 'Authorization: token-for-graph-node-query-endpoint' --data '{"query": "{_meta{block{number}}}"}' http://localhost:7300/subgraphs/id/QmVhiE4nax9i86UBnBmQCYDzvjWuwHShYh7aspGPQhU5Sj
"{\"data\":{\"_meta\":{\"block\":{\"number\":9425787}}}}"

// Checks for auth and configuration to serve-network-subgraph
✗ curl -X POST -H 'Content-Type: application/json' -H 'Authorization: token-for-network-subgraph' --data '{"query": "{_meta{block{number}}}"}' http://localhost:7300/network 
"Not enabled or authorized query"

// Indexing status resolver
✗ curl -X POST -H 'Content-Type: application/json' --data '{"query": "{blockHashFromNumber(network:\"goerli\", blockNumber: 9069120)}"}' http://localhost:7300/status 
{"data":{"blockHashFromNumber":"e1e5472636db73ba5496aee098dc21310683c95eb30fc46f9ba6c36d8b28d58e"}}%                

// Indexing status resolver - filter unsupported root field queries
✗ curl -X POST -H 'Content-Type: application/json' --data '{"query": "{blockHashFromNumber(network:\"goerli\", blockNumber: 9069120)}"}' http://localhost:7300/status 
{"data":{"blockHashFromNumber":"e1e5472636db73ba5496aee098dc21310683c95eb30fc46f9ba6c36d8b28d58e"}}%                
