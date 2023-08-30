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
- [x] Deployment health server
  - [x] query status endpoint and process result
- [ ] Status server 
  - [x] indexing status resolver - to query indexingStatuses
  - [ ] Filter for unsupported queries
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

Set up configurations. To run with toml configurations
```
cargo run -- config "template.toml"

```

To run with CLI args
```
cargo run -- --ethereum <eth-node-provider> \
  --mnemonic <operator-mnemonic> \
  --indexer-address  <indexer-address ></indexer-address> \
  --port 7300 \
  --metrics-port 7500 \
  --graph-node-query-endpoint http://localhost:8000 \
  --graph-node-status-endpoint http://localhost:8030 \ 
  --free-query-auth-token "free-query-auth" \
  --postgres-host "127.0.0.1" \
  --postgres-port 5432 \
  --postgres-database postgres  \
  --postgres-username <postgres-username> \
  --postgres-password <postgres-password> \
  --network-subgraph-endpoint "https://api.thegraph.com/subgraphs/name/graphprotocol/graph-network \
  --network-subgraph-auth-token "network-subgraph-auth" \
  --serve-network-subgraph true \
  --client-signer-address "0xe1EC4339019eC9628438F8755f847e3023e4ff9c" \

```

After service start up, try with command 
```
curl -X POST \
	-H 'Content-Type: application/json' \
  --data '{"query": "{_meta{block{number}}}"}' \
	http://127.0.0.1:7300/network
```


### Supported request and response format examples


```
✗ curl http://localhost:7300/
Ready to roll! 

✗ curl http://localhost:7300/health
{"healthy":true}

✗ curl http://localhost:7300/version
{"version":"0.1.0","dependencies":{}}

✗ curl http://localhost:7300/operator/info
{"publicKey":"0xacb05407d78129b5717bb51712d3e23a78a10929"}

# Subgraph queries
# Checks for receipts and authorization
✗ curl -X POST -H 'Content-Type: application/json' -H 'Authorization: Bearer token-for-graph-node-query-endpoint' --data '{"query": "{_meta{block{number}}}"}' http://localhost:7300/subgraphs/id/QmacQnSgia4iDPWHpeY6aWxesRFdb8o5DKZUx96zZqEWrB
"{\"data\":{\"_meta\":{\"block\":{\"number\":9425787}}}}"

# Takes hex representation for subgraphs deployment id aside from IPFS hash representation
✗ curl -X POST -H 'Content-Type: application/json' -H 'Authorization: Bearer token-for-graph-node-query-endpoint' --data '{"query": "{_meta{block{number}}}"}' http://localhost:7300/subgraphs/id/0xb655ca6f49e73728a102219726ff678d61d8fb792874792e9f0d9887dc616600
"{\"data\":{\"_meta\":{\"block\":{\"number\":9425787}}}}"

# Free query auth token check failed
✗ curl -X POST -H 'Content-Type: application/json' -H 'Authorization: blah' --data '{"query": "{_meta{block{number}}}"}' http://localhost:7300/subgraphs/id/0xb655ca6f49e73728a102219726ff678d61d8fb792874792e9f0d9887dc616600
"Invalid Scalar-Receipt header provided"%

# Subgraph health check
✗ curl http://localhost:7300/subgraphs/health/QmVhiE4nax9i86UBnBmQCYDzvjWuwHShYh7aspGPQhU5Sj
"Subgraph deployment is up to date"%                    
## Unfound subgraph
✗ curl http://localhost:7300/subgraphs/health/QmacQnSgia4iDPWHpeY6aWxesRFdb8o5DKZUx96zZqEWrB
"Invalid indexing status"%   

# Network queries
# Checks for auth and configuration to serve-network-subgraph
✗ curl -X POST -H 'Content-Type: application/json' -H 'Authorization: token-for-network-subgraph' --data '{"query": "{_meta{block{number}}}"}' http://localhost:7300/network 
"Not enabled or authorized query"

# Indexing status resolver - Route supported root field queries to graph node status endpoint
✗ curl -X POST -H 'Content-Type: application/json' --data '{"query": "{blockHashFromNumber(network:\"goerli\", blockNumber: 9069120)}"}' http://localhost:7300/status 
{"data":{"blockHashFromNumber":"e1e5472636db73ba5496aee098dc21310683c95eb30fc46f9ba6c36d8b28d58e"}}%                

# Indexing status resolver - 
✗ curl -X POST -H 'Content-Type: application/json' --data '{"query": "{indexingStatuses {subgraph health} }"}' http://localhost:7300/status 
{"data":{"indexingStatuses":[{"subgraph":"QmVhiE4nax9i86UBnBmQCYDzvjWuwHShYh7aspGPQhU5Sj","health":"healthy"},{"subgraph":"QmWVtsWk8Pqn3zY3czDjyoVreshRLmoz9jko3mQ4uvxQDj","health":"healthy"},{"subgraph":"QmacQnSgia4iDPWHpeY6aWxesRFdb8o5DKZUx96zZqEWrB","health":"healthy"}]}}

# Indexing status resolver - Filter out the unsupported queries
✗ curl -X POST -H 'Content-Type: application/json' --data '{"query": "{_meta{block{number}}}"}' http://localhost:7300/status 
{"errors":[{"locations":[{"line":1,"column":2}],"message":"Type `Query` has no field `_meta`"}]}%              

######## Cost server - read-only graphql query
curl -X GET -H 'Content-Type: application/json' --data '{"query": "{ costModel(deployment: \"Qmb5Ysp5oCUXhLA8NmxmYKDAX2nCMnh7Vvb5uffb9n5vss\") { deployment model variables }} "}' http://localhost:7300/cost

curl -X GET -H 'Content-Type: application/json' --data '{"query": "{ costModel(deployment: \"Qmb5Ysp5oCUXhLA8NmxmYKDAX2nCMnh7Vvb5uffb9n5vss\") { deployment model variables }} "}' http://localhost:7300/cost
{"data":{"costModel":{"deployment":"0xbd499f7673ca32ef4a642207a8bebdd0fb03888cf2678b298438e3a1ae5206ea","model":"default => 0.00025;","variables":null}}}%

curl -X GET -H 'Content-Type: application/json' --data '{"query": "{ costModel(deployment: \"Qmb5Ysp5oCUXhLA8NmxmYKDAX2nCMnh7Vvb5uffb9n5vas\") { deployment model variables }} "}' http://localhost:7300/cost
{"data":{"costModel":null}}%

curl -X GET -H 'Content-Type: application/json' --data '{"query": "{ costModel(deployment: \"Qmb5Ysp5oCUXhLA8NmxmYKDAX2nCMnh7Vvb5uffb9n5vss\") { deployment odel variables }} "}' http://localhost:7300/cost 
{"errors":[{"message":"Cannot query field \"odel\" on type \"CostModel\". Did you mean \"model\"?","locations":[{"line":1,"column":88}]}]}%     

curl -X GET -H 'Content-Type: application/json' --data '{"query": "{ costModels(deployments: [\"Qmb5Ysp5oCUXhLA8NmxmYKDAX2nCMnh7Vvb5uffb9n5vss\"]) { deployment model variables }} "}' http://localhost:7300/cost
{"data":{"costModels":[{"deployment":"0xbd499f7673ca32ef4a642207a8bebdd0fb03888cf2678b298438e3a1ae5206ea","model":"default => 0.00025;","variables":null}]}}%

```