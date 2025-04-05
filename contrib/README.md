# Developer Setup Guide

This guide provides the step to set up a development workflow for the indexer-service project testing.
Including how to build and deploy the required containers, and how to use hot-reload for faster dev iterations.

### Note

The current implementation leverages local-network for the common services and uses its testing configuration.

## Available Commands

We provide the following Make/Just commands to streamline your development workflow:

- `just setup` - Full setup of all services, dependencies and binary compilation
- `just reload` - Rebuild Rust binaries and restart services after code changes
- `just logs` - Watch log output from all services
- `just down` - Stop all services
- `just test-local` - Run integration tests against local services

## Initial Setup

To get started with development:

1. Clone the repository
2. Run the full setup to initialize all services:

```bash
just setup
```

This will:

- Clone the local-network repo if needed
- Start core infrastructure services
- Deploy contracts and required services
- Set up and start the indexer and tap-agent services

## Development Workflow

After the initial setup, you can use the fast development workflow:

1. Make changes to the Rust code in `crates/`
2. Run the reload command to rebuild and restart indexer and tap-agent services:

```bash
just reload
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
just logs
```

This will show a live stream of logs from all services, which is useful for debugging.

## Running Tests

To run integration tests against your local environment:

```bash
just local-test
```

This is currently a work in progress, and additional testing strategies are still being defined and implemented.

## Stopping Services

When you're done working:

```bash
just down
```

This will stop and remove all the containers defined in the docker-compose file.
