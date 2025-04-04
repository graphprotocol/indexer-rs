#!/bin/bash
set -e

# ==============================================================================
# SETUP LOCAL GRAPH NETWORK FOR TESTING
# ==============================================================================
# This script sets up a local Graph network for testing.
#
# NOTES:
# - If you encounter container conflicts, run: docker compose down
#   to stop all services before running this script again
#
# - To test changes to your indexer code without restarting everything:
#   make reload
#
# - The script checks for existing services and skips those already running
# ==============================================================================

container_running() {
    docker ps --format '{{.Names}}' | grep -q "^$1$"
    return $?
}

if container_running "indexer-service" && container_running "tap-agent" && container_running "gateway"; then
    echo "====================================================================================="
    echo "All services are already running. To test changes to your indexer code, you can use:"
    echo "  make reload                  - To rebuild and restart just indexer-service tap-agent services"
    echo ""
    echo "If you need to start from scratch, first stop all services with:"
    echo "  make down"
    echo "  docker rm -f indexer-service tap-agent gateway"
    echo "====================================================================================="
    exit 0
fi

cd contrib/
ls
pwd

# Clone local-network repo if it doesn't exist
if [ ! -d "local-network" ]; then
    git clone https://github.com/edgeandnode/local-network.git
    cd local-network
    # Checkout to a specific commit that is known to work
    git checkout 006e2511d4b8262ff14ff6cd5e1b75f0663dee98
    cd ..
fi

# Start the required services from local-network
cd local-network

echo "Starting core infrastructure services..."
docker compose up -d chain ipfs postgres graph-node
# Wait for graph-node to be healthy
echo "Waiting for graph-node to be healthy..."
timeout 300 bash -c 'until docker ps | grep graph-node | grep -q healthy; do sleep 5; done'

echo "Deploying contract services..."
docker compose up -d graph-contracts
# Wait for contracts to be deployed
timeout 300 bash -c 'until docker ps -a | grep graph-contracts | grep -q "Exited (0)"; do sleep 5; done'

# Verify the contracts have code, usually when starting from scratch
# there could be timing issues between contract deployment
# and when transaction gets approved and contract address gets available
# in the network.
# So bellow is a kind of double check
# Extract a few key contract addresses from the contracts.json file
graph_token_address=$(jq -r '."1337".GraphToken.address' contracts.json)
controller_address=$(jq -r '."1337".Controller.address' contracts.json)

echo "Checking GraphToken contract at $graph_token_address"
code=$(docker exec chain cast code $graph_token_address --rpc-url http://localhost:8545)
if [ -z "$code" ] || [ "$code" == "0x" ]; then
    echo "ERROR: GraphToken contract has no code!"
    exit 1
fi
echo "GraphToken contract verified."

echo "Checking Controller contract at $controller_address"
code=$(docker exec chain cast code $controller_address --rpc-url http://localhost:8545)
if [ -z "$code" ] || [ "$code" == "0x" ]; then
    echo "ERROR: Controller contract has no code!"
    exit 1
fi
echo "Controller contract verified."
echo "Contract deployment successful."

docker compose up -d tap-contracts

echo "Starting indexer services..."
docker compose up -d block-oracle
echo "Waiting for block-oracle to be healthy..."
timeout 300 bash -c 'until docker ps | grep block-oracle | grep -q healthy; do sleep 5; done'

docker compose up -d indexer-agent
echo "Waiting for indexer-agent to be healthy..."
timeout 300 bash -c 'until docker ps | grep indexer-agent | grep -q healthy; do sleep 5; done'

docker compose up -d indexer-service
echo "Waiting for indexer-service to be healthy..."
timeout 300 bash -c 'until docker ps | grep indexer-service | grep -q healthy; do sleep 5; done'

echo "Starting subgraph deployment..."
docker compose up --build -d subgraph-deploy
sleep 10 # Give time for subgraphs to deploy

echo "Starting TAP services..."
echo "Starting tap-aggregator..."
docker compose up -d tap-aggregator
sleep 10

echo "Starting tap-agent..."
docker compose up -d tap-agent
sleep 10

# tap-escrow-manager requires subgraph-deploy
echo "Starting tap-escrow-manager..."
docker compose up -d tap-escrow-manager
sleep 10

# Start redpanda if it's not already started (required for gateway)
if ! docker ps | grep -q redpanda; then
    echo "Starting redpanda..."
    docker compose up -d redpanda
    echo "Waiting for redpanda to be healthy..."
    timeout 300 bash -c 'until docker ps | grep redpanda | grep -q healthy; do sleep 5; done'
fi

# Get the network name used by local-network
NETWORK_NAME=$(docker inspect graph-node --format='{{range $net,$v := .NetworkSettings.Networks}}{{$net}}{{end}}')
echo "Local-network is using Docker network: $NETWORK_NAME"

# Output the network name for use in the next step
echo "NETWORK_NAME=$NETWORK_NAME" >>$GITHUB_ENV || echo "NETWORK_NAME=$NETWORK_NAME"

cd ..

# Get the network name from the environment or use a default
NETWORK_NAME=${NETWORK_NAME:-local-network_default}

# Create a temporary docker-compose override file to set the network
cat >docker-compose.override.yml <<EOF
version: '3'

networks:
  default:
    name: $NETWORK_NAME
    external: true
EOF

# Build the base image for development(base image:latest)
echo "Building base Docker image for development..."
docker build -t indexer-base:latest -f base/Dockerfile ..

# Check to stop any previous instance of indexer-service
# and tap-service
echo "Checking for existing conflicting services..."
if docker ps | grep -q "indexer-service\|tap-agent"; then
    echo "Stopping existing indexer-service or tap-agent containers..."
    docker stop indexer-service tap-agent 2>/dev/null || true
    docker rm indexer-service tap-agent 2>/dev/null || true
fi

# Run the custom services using the override file
docker compose -f docker-compose.yml -f docker-compose.override.yml up --build -d
rm docker-compose.override.yml

timeout 30 bash -c 'until docker ps | grep indexer | grep -q healthy; do sleep 5; done'
timeout 30 bash -c 'until docker ps | grep tap-agent | grep -q healthy; do sleep 5; done'

# GATEWAY DEPLOYMENT NOTE:
# We're deploying gateway directly with "docker run" instead of docker-compose
# because the local-network's compose file has a dependency on "indexer-service",
# which would conflict with our custom indexer-service container.
# This approach avoids container name conflicts while allowing full testing
# of the RAV flow through the gateway.
echo "Building gateway image..."
docker build -t local-gateway:latest ./local-network/gateway

echo "Running gateway container..."
docker run -d --name gateway \
    --network local-network_default \
    -p 7700:7700 \
    -v $(pwd)/local-network/.env:/opt/.env:ro \
    -v $(pwd)/local-network/contracts.json:/opt/contracts.json:ro \
    -e RUST_LOG=info,graph_gateway=trace \
    --restart on-failure:3 \
    local-gateway:latest

echo "Waiting for gateway to be healthy..."
if docker ps | grep -q "gateway"; then
    echo "Gateway is running"
else
    echo "Gateway failed to start. Check logs with: docker logs gateway"
    exit 1
fi

echo "All services are now running!"
echo "You can enjoy your new local network setup for testing."
