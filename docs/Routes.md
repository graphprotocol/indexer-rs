# Routes

This section lists the routes currently exposed by the Subgraph Service. Each route includes a brief description of its purpose and any requirements (e.g., tokens) for access.

Two listeners sit outside the HTTP routes below. Prometheus metrics are served on the `[metrics]` port (default 7300) at `/metrics`. When the `[dips]` section is configured, the service also starts a separate gRPC server on its own host and port (default `0.0.0.0:7601`), which is where the dipper, the service that offers indexing agreements, sends its proposals. That gRPC server only starts if DIPs initialisation succeeds; if it fails, the service carries on serving queries without DIPs.

`/dips/info` is how you tell which of those happened. It answers 200 with the pricing body when DIPs started, 503 with `{"status": "unavailable"}` when `[dips]` is configured but initialisation failed, and is not registered at all when `[dips]` is absent.

## Public Routes

| Route                   | Description                                                                                  |
|-------------------------|----------------------------------------------------------------------------------------------|
| `/`                     | Returns a simple greetings message.                                                         |
| `/info`                 | Displays the operator's public address.                                                     |
| `/healthz`              | Reports service dependency health (database and graph-node).                                |
| `/version`              | Provides the current version of `indexer-service-rs` and its dependencies.                  |
| `/dips/info`            | Reports the configured DIPs prices. See the note above on its 3 responses.                  |

## Token-Protected Routes

| Route                   | Description                                                                                  |
|-------------------------|----------------------------------------------------------------------------------------------|
| `/network`              | Routes queries to the network subgraph. Requires a valid token.                              |

## GraphQL API Routes

| Route                   | Description                                                                                  |
|-------------------------|----------------------------------------------------------------------------------------------|
| `/cost`                 | Provides access to the Cost Model GraphQL API.                                               |

## Subgraph Routes

| Route                                | Description                                                                                  |
|--------------------------------------|----------------------------------------------------------------------------------------------|
| `/subgraph/health/{deployment_id}`    | Retrieves the health state of a specified subgraph using its ID.                             |
| `/subgraphs/id/{id}`                  | Routes a query to a specific subgraph using its ID. Requires a receipt or valid token.       |

## Node Status Route

| Route                   | Description                                                                                  |
|-------------------------|----------------------------------------------------------------------------------------------|
| `/status`               | Routes requests to the graph-node status API.                                                |

---

## Note

You can always view the latest complete and up-to-date list of routes in the source code:  
[Service Router Implementation](../crates/service/src/service/router.rs)
