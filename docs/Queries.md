# Query examples

This is a (non-exhaustive) list of possible queries 
and their responses for `indexer-service-rs`.

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

## Subgraph health check
```bash
curl http://localhost:7600/subgraphs/health/QmVhiE4nax9i86UBnBmQCYDzvjWuwHShYh7aspGPQhU5Sj
```

```json
{
    "health": "healthy"
}
```
## Unfound subgraph

```bash
curl http://localhost:7600/subgraphs/health/QmacQnSgia4iDPWHpeY6aWxesRFdb8o5DKZUx96zZqEWrB
```

```json
{
    "error": "Deployment not found"
}
```

## Failed Subgraph

```bash
curl http://localhost:7600/subgraphs/health/QmVGSJyvjEjkk5U9EdxyyB78NCXK3EAoFhrzm6LV7SxxAm
```

```json
{
    "fatalError": "transaction 21e77ed08fbc9df7be81101e9b03c2616494cee7cac2f6ad4f1ee387cf799e0c: error while executing at wasm backtrace:\t    0: 0x5972 - <unknown>!mappings/core/handleSwap: Mapping aborted at mappings/core.ts, line 73, column 16, with message: unexpected null in handler `handleSwap` at block #36654250 (5ab4d80c8e2cd628d5bf03abab4c302fd21d25d734e66afddff7a706b804fe13)",
    "health": "failed"
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
