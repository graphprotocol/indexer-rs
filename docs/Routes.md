# Routes

This section lists the routes currently exposed by the Subgraph Service. Each route includes a brief description of its purpose and any requirements (e.g., tokens) for access.

## Public Routes

| Route                   | Description                                                                                  |
|-------------------------|----------------------------------------------------------------------------------------------|
| `/`                     | Returns a simple greetings message.                                                         |
| `/info`                 | Displays the operator's public address.                                                     |
| `/version`              | Provides the current version of `indexer-service-rs` and its dependencies.                  |

## Token-Protected Routes

| Route                   | Description                                                                                  |
|-------------------------|----------------------------------------------------------------------------------------------|
| `/escrow`               | Routes queries to the escrow subgraph. Requires a valid token.                               |
| `/network`              | Routes queries to the network subgraph. Requires a valid token.                              |

## GraphQL API Routes

| Route                   | Description                                                                                  |
|-------------------------|----------------------------------------------------------------------------------------------|
| `/dips`                 | Provides access to the Dips GraphQL API.                                                     |
| `/cost`                 | Provides access to the Cost Model GraphQL API.                                               |

## Subgraph Routes

| Route                                | Description                                                                                  |
|--------------------------------------|----------------------------------------------------------------------------------------------|
| `/subgraph/health/:id`               | Retrieves the health state of a specified subgraph using its ID.                             |
| `/subgraphs/id/:id`                  | Routes a query to a specific subgraph using its ID. Requires a receipt or valid token.       |

## Node Status Route

| Route                   | Description                                                                                  |
|-------------------------|----------------------------------------------------------------------------------------------|
| `/status`               | Routes requests to the graph-node status API.                                                |

---

## Note

You can always view the latest complete and up-to-date list of routes in the source code:  
[Service Router Implementation](./crates/service/src/service/router.rs)
