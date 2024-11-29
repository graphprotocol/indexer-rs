# indexer-rs

[![Build Status](https://github.com/graphprotocol/indexer-rs/actions/workflows/containers.yml/badge.svg)](https://github.com/graphprotocol/indexer-rs/actions)
[![Coverage Status](https://coveralls.io/repos/github/graphprotocol/indexer-rs/badge.svg?branch=main)](https://coveralls.io/github/graphprotocol/indexer-rs?branch=main)
[![License](https://img.shields.io/github/license/graphprotocol/indexer-rs)](https://github.com/graphprotocol/indexer-rs/blob/main/LICENSE)
[![Contributing](https://img.shields.io/badge/contributions-welcome-brightgreen.svg)](./CONTRIBUTORS.md)
[![GitHub Release](https://img.shields.io/github/v/release/graphprotocol/indexer-rs?filter=indexer-service-rs-*)](https://github.com/graphprotocol/indexer-rs/releases?q=indexer-service-rs)
[![GitHub Release](https://img.shields.io/github/v/release/graphprotocol/indexer-rs?filter=indexer-tap-agent-*)](https://github.com/graphprotocol/indexer-rs/releases?q=indexer-tap-agent)

A Rust implementation for The Graph 
[indexer-service-ts](https://github.com/graphprotocol/indexer/tree/main/packages/indexer-service) 
to provide subgraph service as an Indexer, 
integrated with [TAP](https://github.com/semiotic-ai/timeline-aggregation-protocol) 
which is a fast, efficient, and trustless unidirectional micro-payments system.

--- 
## Getting Started

This section provides guidance for building, configuring, and running `indexer-service-rs` and `indexer-tap-agent`.

### Docker Images

Pre-built Docker images are available for both `indexer-service-rs` and `indexer-tap-agent`. You can pull these images using the following commands:

- **Indexer Service:**

```bash
docker pull ghcr.io/graphprotocol/indexer-service-rs:<tag>
```

- **TAP Agent:**

```bash
docker pull ghcr.io/graphprotocol/indexer-tap-agent:<tag>
```

The `<tag>` corresponds to the current release version, which can be found in the [Releases Page](https://github.com/graphprotocol/indexer-rs/releases).

#### Tag Examples for Version X.Y.Z

For version `X.Y.Z`, the available tags include:
- `latest`
- `vX.Y.Z`
- `X.Y.Z`
- `vX.Y`
- `X.Y`
- `vX`

Refer to the following badges for the latest release versions:
| indexer-service-rs | [![GitHub Release](https://img.shields.io/github/v/release/graphprotocol/indexer-rs?filter=indexer-service-rs-*)](https://github.com/graphprotocol/indexer-rs/releases?q=indexer-service-rs) |
|---------------------|---------------------------------------------------------------------------------------------------------------------------|
| indexer-tap-agent   | [![GitHub Release](https://img.shields.io/github/v/release/graphprotocol/indexer-rs?filter=indexer-tap-agent-*)](https://github.com/graphprotocol/indexer-rs/releases?q=indexer-tap-agent) |

---

### Building Locally

To build the services locally, ensure you have the latest version of Rust installed. No additional plugins are required.

#### Steps:

1. Clone the repository:

```bash
git clone https://github.com/graphprotocol/indexer-rs.git && cd indexer-rs
```

2. Build the binaries:
- **Indexer Service:**

  ```
  cargo build --release -p indexer-service-rs
  ```

- **TAP Agent:**

  ```
  cargo build --release -p indexer-tap-agent
  ```

3. The compiled binaries can be found in the `target/release/` directory:
- `target/release/indexer-service-rs`
- `target/release/indexer-tap-agent`

---

### Configuration

The services require a configuration file provided through the `--config` flag during startup. A minimal example configuration is available at:
- [Minimal Configuration Template](config/minimal-config-example.toml)

#### Steps:
1. **Edit the Configuration:**
Open the `minimal-config-example.toml` file and populate the required fields. Some fields must be configured with values from [this table](https://thegraph.com/docs/en/tap/#blockchain-addresses).

2. **Override with Environment Variables (Optional):**
You can override configuration fields using environment variables. Use the prefix `INDEXER_`, and for nested fields, use double underscores `__`. For example:

```bash
export INDEXER_SERVICE_SUBGRAPHS__NETWORK__QUERY_URL=<value>
```

3. **Start the Service:**
- **Indexer Service:**

  ```bash
  target/release/indexer-service-rs --config config/minimal-config-example.toml
  ```

- **TAP Agent:**

  ```bash
  target/release/indexer-tap-agent --config config/minimal-config-example.toml
  ```

## Configuration

All configuration is managed through a TOML file. Below are examples of configuration templates to help you get started:

- [Minimal Configuration Template (Recommended)](config/minimal-config-example.toml): A minimal setup with only the essential configuration options.
- [Maximal Configuration Template (Not Recommended)](config/maximal-config-example.toml): Includes all possible options, but some settings may be dangerous or unsuitable for production.

If you are migrating from an older stack, use the [Migration Configuration Guide](./docs/migration-config/README.md) for detailed instructions.

### Key Notes:

- Ensure your configuration is tailored to your deployment environment.
- Validate the configuration file syntax before starting the service.
- Use environment variables to override sensitive settings like database credentials or API tokens where applicable.


### Migrations

- `postgres_url` is required to be set to the same database as `indexer-agent`;
- No migrations are run in `indexer-rs` stack, since it could cause conflicts;
- `indexer-agent` is solely responsible for database management;
- `/migrations` folder is used **ONLY** for development purposes.


## Upgrading

We follow semantic versioning to ensure backward compatibility and minimize breaking changes. To upgrade safely, follow these steps:

1. **Review Release Notes:** Check the release notes for details about changes, fixes, and new features included in the updated version.
2. **Review Documentation:** Refer to the latest documentation to understand any modifications in behavior or configuration.
3. **Backup Configuration:** Save your existing configuration files and any local changes for easy recovery if needed.
4. **Deploy:** Replace the old executable or Docker image with the new version. Restart the service to apply the changes.
5. **Monitor and Validate:** After upgrading, monitor metrics and logs to ensure the service is running as expected and validate critical functionality.

By following these steps, you can take advantage of new features and improvements while maintaining the stability of your system.

## Contributing

We welcome contributions to `indexer-rs`! 

### How to Contribute
- Read the [Contributions Guide](./CONTRIBUTORS.md) for detailed guidelines.
- Follow coding standards and ensure your changes align with the project structure.
- Write tests to validate your contributions and maintain project integrity.
- Submit a pull request with a clear description of your changes and their purpose.

Contributions can include:
- Bug fixes
- New features
- Documentation improvements
- Performance optimizations

Feel free to suggest enhancements or report issues in the project repository. 


## Implementation Details

### indexer-service-rs

The Subgraph Service is an [axum](https://crates.io/crates/axum)-based web server that serves as a router to [graph-node](https://github.com/graphprotocol/graph-node). It is designed for extensibility, granularity, and testability.

#### Key Features
- **Middleware-Based Checks:** All validation and processing are performed through middleware, enabling modular and reusable components.
- **TAP Integration:** 
  - TAP receipts are processed by the `tap-middleware`, where they undergo various [checks](./crates/service/src/tap/checks/).
  - Valid receipts are stored in the database, later aggregated into RAVs (Redeemable Aggregate Values) by the [tap-agent](#indexer-tap-agent).
- **Router Implementation:**
  - Built using the builder pattern for compile-time validation of required components.
  - Routes are well-defined and documented. For a complete list, refer to the [Routes Documentation](./docs/Routes.md).
- **Query Examples:** Examples of queries that can be sent to `indexer-service-rs` are available in the [Queries Documentation](./docs/Queries.md).
- **Prometheus Metrics:**
  - The service is instrumented to expose metrics in Prometheus format.
  - Metrics are accessible via the `/metrics` endpoint on `<metric_port>` (default: `7300`).
  - A detailed list of metrics is available in the [Metrics Documentation](/docs/Metrics.md#indexer-service-metrics).

### indexer-tap-agent

The TAP Agent is an actor-based system powered by [ractor](https://crates.io/crates/ractor). It processes receipts into RAVs (Receipt Aggregate Vouchers) and prepares them for redemption by the `indexer-agent`.

#### Key Features
1. **Receipt Processing:**
   - Receipts are fetched from the `receipts` table in the database using `pglisten`.
   - The actor system validates and processes receipts before aggregating them into RAVs.
   - Debug logs can be activated using `RUST_LOG=debug` to provide additional insights during the receipt lifecycle.

2. **Actor System:**
   - The TAP Agent operates using three main actor groups:
     - **SenderAccountManager:** 
       - Monitors the indexer's escrow accounts.
       - Handles receipt routing to the appropriate `SenderAllocation`.
       - Kills the application if the database connection is lost.
     - **SenderAccount:**
       - Tracks receipts, pending RAVs, and invalid receipts across allocations.
       - Ensures the escrow account has sufficient funds to cover outstanding obligations.
       - Selects the next allocation for creating a RAV request and manages `SenderAllocation` actors.
     - **SenderAllocation:**
       - Represents a `(sender, allocation)` tuple.
       - Processes receipts and sends updated values to the `SenderAccount`.
       - Handles `TriggerRequest` messages to initiate RAV creation.

3. **RAV Workflow:**
   - Valid receipts are aggregated into RAVs, which replace older RAVs in the database.
   - Once an allocation is closed, the system creates a **Last RAV**, aggregating all receipts. This final RAV is marked as "Last" and prepared for redemption by the `indexer-agent`.

4. **Metrics and Monitoring:**
   - The TAP Agent exposes detailed metrics to monitor system behavior. Key metrics include:
     - **Unaggregated Receipts:** Ensure unaggregated receipts remain below the `max_willing_to_lose` value to prevent the sender from being denied.
     - **RAV Request Failures:** Monitor the rate of failed RAV requests, as repeated failures can disrupt aggregation.
   - Full details of all metrics can be found in the [Metrics Documentation](https://github.com/graphprotocol/indexer-rs/blob/gustavo/rebuild-documentation/docs/Metrics.md#tap-agent-metrics).
   - A Grafana dashboard is available to visualize system metrics and provide a clear overview of the TAP Agent's performance:
     - [Download Dashboard JSON](https://github.com/graphprotocol/indexer-rs/blob/gustavo/rebuild-documentation/docs/dashboard.json).

5. **Troubleshooting:**
   - If RAV requests fail or unaggregated receipts exceed the `max_willing_to_lose` limit:
     - Check the **RAV failure metrics**.
     - Review debug logs (`RUST_LOG=debug`) for additional context.
     - Use the Grafana dashboard to identify bottlenecks or failures in the actor system.


## Crates

| Crate Name               | Description                                                                                  |
|--------------------------|----------------------------------------------------------------------------------------------|
| `indexer-allocation`     | Shared structs and logic related to subgraph allocations in The Graph.                       |
| `indexer-attestation`    | Provides tools for generating and verifying attestations for requests and responses.         |
| `indexer-config`         | Parses shared configuration used by both `indexer-service-rs` and `indexer-tap-agent`.       |
| `indexer-dips`           | (WIP) Library for managing DIPS (Distributed Indexing Payment System).                       |
| `indexer-monitor`        | Monitors subgraphs through polling and updates shared state with reactive components.        |
| `indexer-query`          | Type-safe GraphQL queries, leveraging [graphql-client](https://github.com/graphql-rust/graphql-client). |
| `indexer-service-rs`     | Subgraph service application that handles payments, routes queries to graph-node, and creates attestations. |
| `indexer-tap-agent`      | Processes receipts into RAVs (Redeemable Aggregate Values) and marks them as ready for redemption. |
| `indexer-watcher`        | An alternative to [eventuals](https://github.com/edgeandnode/eventuals), built on `tokio::sync::watch`. |
| `test-assets`            | Provides assets for testing, such as generating allocations, wallets, and receipts.          |


## Additional Documentation

- [Routes Documentation](./docs/Routes.md): Comprehensive list of available routes and their descriptions.
- [Queries Documentation](./docs/Queries.md): Examples of queries you can perform with `indexer-service-rs`.
- [Metrics Documentation](./docs/Metrics.md): Detailed documentation of all Prometheus metrics exposed by the service.
