# Developer Setup Guide

This guide provides the step to set up a development workflow for the indexer-service project testing.
Including how to build and deploy the required containers, and how to use hot-reload for faster dev iterations.

### Note

The current implementation leverages local-network for the common services and uses its testing configuration. We're specifically using an older [commit](https://github.com/edgeandnode/local-network/commit/ae72f1b8d57f18e7055a7fa29b873fe5bb8d9879) due to permissions issues with more recent versions, which fail when cloning the repository because of private git dependencies.

## Available Commands

We provide the following Make/Just commands to streamline your development workflow:

- `make setup` - Full setup of all services, dependencies and binary compilation
- `make reload` - Rebuild Rust binaries and restart services after code changes
- `make logs` - Watch log output from all services
- `make down` - Stop all services
- `make test-local` - Run integration tests against local services(Work in progress)

## Initial Setup

To get started with development:

1. Clone the repository
2. Run the full setup to initialize all services:

```bash
make setup
```

This will:

- Clone the local-network repo if needed
- Start core infrastructure services
- Deploy contracts and required services
- Set up and start the indexer services

## Development Workflow

After the initial setup, you can use the fast development workflow:

1. Make changes to the Rust code in `crates/`
2. Run the reload command to rebuild and restart indexer and tap-agent services:

```bash
make reload
```

This workflow is much faster because:

- It only recompiles the Rust code
- It doesn't rebuild Docker containers
- It restarts only the necessary services

The `reload` command will automatically show the logs after restarting, so you can see if your changes are working properly.

## How It Works

The development workflow uses volume mounts to avoid rebuilding containers:

1. A base Docker image with all dependencies is created once
2. Your code is compiled locally on your machine
3. The compiled binaries are mounted into the containers
4. Services are restarted with the new binaries

This approach avoids the time-consuming container rebuild process while still maintaining a consistent containerized environment.

## Viewing Logs

To monitor the services during development:

```bash
make logs
```

This will show a live stream of logs from all services, which is useful for debugging.

## Running Tests

To run integration tests against your local environment:

```bash
make test-local
```

This is currently a work in progress, and additional testing strategies are still being defined and implemented.

## Stopping Services

When you're done working:

```bash
make down
```

This will stop and remove all the containers defined in the docker-compose file.

## Testing Improvements

The current testing setup has some limitations that could be addressed in the future:

### Current Limitations

- Bash script for testing (`run-test-local.sh`) are not optimal
- Limited test coverage of complex edge cases
- Manual verification required for some functionality

## Complete End-to-End Environment

Note that the current test environment is a simplified version intended to emulate real-world operation. For a complete end-to-end testing environment, additional services are required but not currently deployed:

- `tap-escrow-manager` - Manages escrow accounts for payment channels
- `subgraph-deploy` - Handles deployment of subgraphs for testing

These services are commented out in the setup script for simplicity but would be needed in a full production environment or complete integration testing setup.

### Note

It would be worth to keep updating local-network dependency and try to conciliate future permissions errors
