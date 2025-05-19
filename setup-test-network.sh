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
#   just reload
#
# - The script checks for existing services and skips those already running
# ==============================================================================
#
# Save the starting disk usage
START_SPACE=$(df -h --output=used /var/lib/docker | tail -1)
START_IMAGES_SIZE=$(docker system df --format '{{.ImagesSize}}' 2>/dev/null || echo "N/A")
START_CONTAINERS_SIZE=$(docker system df --format '{{.ContainersSize}}' 2>/dev/null || echo "N/A")
START_VOLUMES_SIZE=$(docker system df --format '{{.VolumesSize}}' 2>/dev/null || echo "N/A")

echo "============ STARTING DISK USAGE ============"
echo "Docker directory usage: $START_SPACE"
echo "Images size: $START_IMAGES_SIZE"
echo "Containers size: $START_CONTAINERS_SIZE"
echo "Volumes size: $START_VOLUMES_SIZE"
echo "=============================================="

container_running() {
    docker ps --format '{{.Names}}' | grep -q "^$1$"
    return $?
}

# Function to fund the escrow smart contract
# 1. first read .env variables from local-network/.env
# 2. then read contract addresses from local-network/contracts.json
# 3. finally, use the cast command to approve and deposit GRT to the escrow
# this should be done just after deploying the gateway
# otherwise it does not move forward in its setup process
# causing false error during deployment of our local testnet
fund_escrow() {
    echo "Funding escrow for sender..."

    if [ -f "local-network/.env" ]; then
        source local-network/.env
    else
        echo "Error: local-network/.env file not found"
        return 1
    fi

    GRAPH_TOKEN=$(jq -r '."1337".GraphToken.address' local-network/contracts.json)
    TAP_ESCROW=$(jq -r '."1337".TAPEscrow.address' local-network/contracts.json)

    if [ -z "$GRAPH_TOKEN" ] || [ -z "$TAP_ESCROW" ]; then
        echo "Error: Could not read contract addresses from contracts.json"
        return 1
    fi

    # Use constants from .env
    SENDER_ADDRESS="$ACCOUNT0_ADDRESS"
    SENDER_KEY="$ACCOUNT0_SECRET"
    AMOUNT="10000000000000000000"

    echo "Using GraphToken at: $GRAPH_TOKEN"
    echo "Using TapEscrow at: $TAP_ESCROW"
    echo "Using sender address: $SENDER_ADDRESS"

    # Approve GRT for escrow
    echo "Approving GRT..."
    docker exec chain cast send \
        --rpc-url http://localhost:8545 \
        --private-key $SENDER_KEY \
        $GRAPH_TOKEN "approve(address,uint256)" $TAP_ESCROW $AMOUNT

    # Deposit to escrow
    echo "Depositing to escrow..."
    docker exec chain cast send \
        --rpc-url http://localhost:8545 \
        --private-key $SENDER_KEY \
        $TAP_ESCROW "deposit(address,uint256)" $SENDER_ADDRESS $AMOUNT

    # Verify deposit
    echo "Verifying deposit..."
    ESCROW_BALANCE=$(docker exec chain cast call \
        --rpc-url http://localhost:8545 \
        $TAP_ESCROW "getEscrowAmount(address,address)(uint256)" $SENDER_ADDRESS $SENDER_ADDRESS)
    echo "Escrow balance: $ESCROW_BALANCE"
    if [[ "$ESCROW_BALANCE" == "0" ]]; then
        echo "Error: Failed to fund escrow"
        return 1
    fi
    echo "Successfully funded escrow"
    return 0
}

if container_running "indexer-service" && container_running "tap-agent" && container_running "gateway"; then
    echo "====================================================================================="
    echo "All services are already running. To test changes to your indexer code, you can use:"
    echo "  just reload                   - To rebuild and restart just indexer-service tap-agent services"
    echo ""
    echo "If you need to start from scratch, first stop all services with:"
    echo "  just down"
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
    git checkout ad98716661b033dd5e4cb9f09cebb80dba954c2d
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

# Verify the contracts have code
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

echo "Starting subgraph deployment..."
docker compose up --build -d subgraph-deploy
sleep 10 # Give time for subgraphs to deploy

echo "Starting TAP services..."
echo "Starting tap-aggregator..."
docker compose up -d tap-aggregator
sleep 10

# tap-escrow-manager requires subgraph-deploy
echo "Starting tap-escrow-manager..."
docker compose up -d tap-escrow-manager
timeout 90 bash -c 'until docker ps --filter "name=^tap-escrow-manager$" --format "{{.Names}}" | grep -q "^tap-escrow-manager$"; do echo "Waiting for tap-escrow-manager container to appear..."; sleep 5; done'

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
# This is used for hot reloading
echo "Building base Docker image for development..."
docker build -t indexer-base:latest -f base/Dockerfile ..

# Check to stop any previous instance of indexer-service
# and tap-agent
echo "Checking for existing conflicting services..."
if docker ps -a | grep -q "indexer-service\|tap-agent\|gateway"; then
    echo "Stopping existing indexer-service or tap-agent containers..."
    docker stop indexer-service tap-agent gateway 2>/dev/null || true
    docker rm indexer-service tap-agent gateway 2>/dev/null || true
fi

# Run the custom services using the override file
docker compose -f docker-compose.yml -f docker-compose.override.yml up --build -d
rm docker-compose.override.yml

timeout 30 bash -c 'until docker ps | grep indexer | grep -q healthy; do sleep 5; done'
timeout 30 bash -c 'until docker ps | grep tap-agent | grep -q healthy; do sleep 5; done'

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

echo "Waiting for gateway to be available..."

# Try to fund escrow up to 3 times
for i in {1..3}; do
    echo "Attempt $i to fund escrow..."
    if fund_escrow; then
        break
    fi
    if [ $i -lt 3 ]; then
        echo "Waiting before retry..."
        sleep 10
    fi
done

# Ensure gateway is ready before testing
timeout 100 bash -c 'until curl -f http://localhost:7700/ > /dev/null 2>&1; do echo "Waiting for gateway service..."; sleep 5; done'

# After all services are running, measure the disk space used
END_SPACE=$(df -h --output=used /var/lib/docker | tail -1)
END_IMAGES_SIZE=$(docker system df --format '{{.ImagesSize}}' 2>/dev/null || echo "N/A")
END_CONTAINERS_SIZE=$(docker system df --format '{{.ContainersSize}}' 2>/dev/null || echo "N/A")
END_VOLUMES_SIZE=$(docker system df --format '{{.VolumesSize}}' 2>/dev/null || echo "N/A")

echo "All services are now running!"
echo "You can enjoy your new local network setup for testing."

echo "============ FINAL DISK USAGE ============"
echo "Docker directory usage: $END_SPACE"
echo "Images size: $END_IMAGES_SIZE"
echo "Containers size: $END_CONTAINERS_SIZE"
echo "Volumes size: $END_VOLUMES_SIZE"
echo "==========================================="
