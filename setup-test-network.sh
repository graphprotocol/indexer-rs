#!/bin/bash
set -e

cd contrib/
ls
pwd

# Clone local-network repo if it doesn't exist
if [ ! -d "local-network" ]; then
    git clone https://github.com/edgeandnode/local-network.git
    cd local-network
    git checkout 0af4bbcd851b365715e11ac68b29f263204353fb
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
sleep 20
docker compose up -d tap-contracts

echo "Starting indexer services..."
docker compose up -d block-oracle
echo "Waiting for block-oracle to be healthy..."
timeout 300 bash -c 'until docker ps | grep block-oracle | grep -q healthy; do sleep 5; done'

docker compose up -d indexer-agent
echo "Waiting for indexer-agent to be healthy..."
timeout 300 bash -c 'until docker ps | grep indexer-agent | grep -q healthy; do sleep 5; done'

# subgraph-deploy is not needed for our testing suite
# echo "Starting subgraph deployment..."
# docker compose up -d subgraph-deploy
# sleep 30 # Give time for subgraphs to deploy

echo "Starting TAP services..."
docker compose up -d tap-aggregator
# tap-scrow-manager requires subgraph-deploy
# docker compose up -d tap-escrow-manager
sleep 10
# Phase 4: Try a simple escrowAccounts query to see if the schema is accessible
echo "Testing escrowAccounts query..."
curl -s "http://localhost:8000/subgraphs/name/semiotic/tap" \
    -H 'content-type: application/json' \
    -d '{"query": "{ escrowAccounts { id } }"}'
# docker compose up -d chain ipfs postgres graph-node graph-contracts tap-contracts tap-escrow-manager tap-aggregator
# docker compose up -d chain ipfs postgres graph-node graph-contracts tap-contracts tap-escrow-manager tap-aggregator block-oracle indexer-agent

# # Wait for services to be ready
# echo "Waiting for graph-node to be healthy..."
# timeout 300 bash -c 'until docker ps | grep graph-node | grep -q healthy; do sleep 5; done'
#
# # Check if tap-contracts deployed the subgraph
# echo "Checking if TAP subgraph is deployed..."
# timeout 300 bash -c 'until curl -s "http://localhost:8000/subgraphs/name/semiotic/tap" -H "content-type: application/json" -d "{\"query\": \"{ _meta { block { number } } }\"}" | grep -q "data"; do sleep 5; done'

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

# Run the custom services using the override file
docker compose -f docker-compose.yml -f docker-compose.override.yml up --build -d
rm docker-compose.override.yml
