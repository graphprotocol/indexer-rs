# Indexer CLI Docker Container

This directory contains the Docker setup for running the Graph Protocol Indexer CLI from the horizon branch for integration testing.

## Overview

The indexer-cli container provides command-line access to manage allocations in The Graph Protocol. It:
- Clones and builds from the `horizon` branch of https://github.com/graphprotocol/indexer.git
- Fixes ESM compatibility issues with the horizon branch
- Connects to the local indexer-agent for allocation management
- Runs on the `local-network_default` Docker network

## Files

- `Dockerfile` - Multi-stage build that clones, builds, and runs the indexer-cli
- `run_indexer_cli.sh` - Helper script to build and run the container

## Setup

The indexer-cli is automatically deployed when running the test network:

```bash
# From indexer-rs root directory
./setup-test-network.sh
```

Or run it standalone:

```bash
./contrib/indexer-cli/run_indexer_cli.sh
```

## Usage

### List Allocations

```bash
docker exec indexer-cli graph indexer allocations get --network hardhat
```

### Programmatic Close (Helper Script)

```bash
# Close one allocation
./contrib/indexer-cli/close-allocations.sh 0x<allocation-id>

# Close all allocations (requires jq)
./contrib/indexer-cli/close-allocations.sh --all
```

### Close an Allocation

With a valid POI:
```bash
docker exec indexer-cli graph indexer allocations close <allocation-id> <poi> --network hardhat --force
```

For testing with zero POI:
```bash
docker exec indexer-cli graph indexer allocations close 0x0a067bd57ad79716c2133ae414b8f6bb47aaa22d 0x0000000000000000000000000000000000000000000000000000000000000000 --network hardhat --force
```

### Other Commands

```bash
# Get help
docker exec indexer-cli graph indexer --help

# Check status
docker exec indexer-cli graph indexer status

# View rules
docker exec indexer-cli graph indexer rules get all
```

## Integration with Docker Compose

The indexer-cli service is included in both:
- `contrib/docker-compose.yml` - Production build
- `contrib/docker-compose.dev.yml` - Development build

It automatically joins the `local-network_default` network to communicate with other services.

## Troubleshooting

### wrap-ansi ESM Error
The Dockerfile includes a patch for the wrap-ansi ESM compatibility issue in the horizon branch. This is applied automatically during the build.

### Connection Issues
If you see connection errors, ensure:
1. The indexer-agent is running: `docker ps | grep indexer-agent`
2. Both containers are on the same network: `docker network inspect local-network_default`

### Rebuild After Changes
To rebuild the container after changes:
```bash
docker stop indexer-cli && docker rm indexer-cli
docker rmi indexer-cli:horizon
./contrib/indexer-cli/run_indexer_cli.sh
```

## Environment Variables

- `CONTAINER_NAME` - Container name (default: `indexer-cli`)
- `IMAGE_TAG` - Docker image tag (default: `indexer-cli:horizon`)
- `DOCKER_NETWORK` - Docker network (default: `local-network_default`)
- `INDEXER_AGENT_URL` - Agent endpoint (default: `http://indexer-agent:7600`)
