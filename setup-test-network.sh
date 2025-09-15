#!/bin/bash

# Interruptible timeout function
interruptible_wait() {
    local timeout_seconds=$1
    local condition_command="$2"
    local description="${3:-Waiting for condition}"

    echo "$description (timeout: ${timeout_seconds}s, press Ctrl+C to cancel)..."

    local elapsed=0
    local interval=5

    while [ $elapsed -lt $timeout_seconds ]; do
        if eval "$condition_command"; then
            return 0
        fi

        # Check for interrupt signal
        if ! sleep $interval; then
            echo "Interrupted by user"
            return 130 # Standard interrupt exit code
        fi

        elapsed=$((elapsed + interval))
        echo "Still waiting... (${elapsed}/${timeout_seconds}s elapsed)"
    done

    echo "Timeout after ${timeout_seconds}s waiting for: $description"
    return 1
}

# ==============================================================================
# SETUP LOCAL GRAPH NETWORK FOR TESTING (HORIZON VERSION)
# ==============================================================================
# This script sets up a local Graph network for testing with horizon upgrade.
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

get_docker_sizes() {
    local df_output=$(docker system df 2>/dev/null)

    # Extract sizes using awk (more reliable)
    local images_size=$(echo "$df_output" | awk '/Images/ {print $4}' | head -1)
    local containers_size=$(echo "$df_output" | awk '/Containers/ {print $4}' | head -1)
    local volumes_size=$(echo "$df_output" | awk '/Local Volumes/ {print $5}' | head -1)

    # If awk fails, try alternative method
    if [ -z "$images_size" ] || [ -z "$containers_size" ] || [ -z "$volumes_size" ]; then
        # Method 2: Use docker system df --format table and parse
        images_size=$(docker system df --format "table {{.Type}}\t{{.TotalCount}}\t{{.Size}}" 2>/dev/null | grep "Images" | awk '{print $4}' || echo "N/A")
        containers_size=$(docker system df --format "table {{.Type}}\t{{.TotalCount}}\t{{.Size}}" 2>/dev/null | grep "Containers" | awk '{print $4}' || echo "N/A")
        volumes_size=$(docker system df --format "table {{.Type}}\t{{.TotalCount}}\t{{.Size}}" 2>/dev/null | grep "Local Volumes" | awk '{print $5}' || echo "N/A")
    fi

    # Set defaults if still empty
    images_size=${images_size:-"N/A"}
    containers_size=${containers_size:-"N/A"}
    volumes_size=${volumes_size:-"N/A"}

    echo "$images_size $containers_size $volumes_size"
}

# Track build times
SCRIPT_START_TIME=$(date +%s)
# Save the starting disk usage
START_SPACE=$(df -h --output=used /var/lib/docker | tail -1)
START_SIZES=($(get_docker_sizes))
START_IMAGES_SIZE=${START_SIZES[0]}
START_CONTAINERS_SIZE=${START_SIZES[1]}
START_VOLUMES_SIZE=${START_SIZES[2]}

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

# Function to fund the escrow smart contract for horizon
# Uses L2GraphToken and TAPEscrow from the horizon structure
fund_escrow() {
    echo "Funding escrow for sender..."

    if [ -f "local-network/.env" ]; then
        source local-network/.env
    else
        echo "Error: local-network/.env file not found"
        return 1
    fi

    # Use L2GraphToken from horizon.json for horizon upgrade
    GRAPH_TOKEN=$(jq -r '."1337".L2GraphToken.address' local-network/horizon.json)
    TAP_ESCROW=$(jq -r '."1337".Escrow' local-network/tap-contracts.json)

    # Override with test values taken from test-assets/src/lib.rs
    ALLOCATION_ID="0xfa44c72b753a66591f241c7dc04e8178c30e13af" # ALLOCATION_ID_0

    if [ -z "$GRAPH_TOKEN" ] || [ -z "$TAP_ESCROW" ] || [ "$GRAPH_TOKEN" == "null" ] || [ "$TAP_ESCROW" == "null" ]; then
        echo "Error: Could not read contract addresses from horizon.json or tap-contracts.json"
        echo "GRAPH_TOKEN: $GRAPH_TOKEN"
        echo "TAP_ESCROW: $TAP_ESCROW"
        return 1
    fi

    # Use constants from .env
    SENDER_ADDRESS="$ACCOUNT0_ADDRESS"
    SENDER_KEY="$ACCOUNT0_SECRET"
    AMOUNT="10000000000000000000"

    echo "Using L2GraphToken at: $GRAPH_TOKEN"
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

if container_running "indexer-service" && container_running "tap-agent" && container_running "gateway" && container_running "indexer-cli"; then
    echo "====================================================================================="
    echo "All services are already running. To test changes to your indexer code, you can use:"
    echo "  just reload                   - To rebuild and restart just indexer-service tap-agent services"
    echo ""
    echo "If you need to start from scratch, first stop all services with:"
    echo "  just down"
    echo "  docker rm -f indexer-service tap-agent gateway indexer-cli"
    echo "====================================================================================="
    exit 0
fi

cd contrib/

# Clone local-network repo if it doesn't exist
if [ ! -d "local-network" ]; then
    git clone https://github.com/semiotic-ai/local-network.git
    cd local-network
    # Checkout to the horizon branch
    git checkout suchapalaver/test/horizon
    cd ..
fi

# Start the required services from local-network
cd local-network

echo "Starting core infrastructure services..."
docker compose up -d chain ipfs postgres graph-node
# Wait for graph-node to be healthy
echo "Waiting for graph-node to be healthy..."
# timeout 300 bash -c 'until docker ps | grep graph-node | grep -q healthy; do sleep 5; done'
interruptible_wait 300 'docker ps | grep graph-node | grep -q healthy' "Waiting for graph-node to be healthy"

echo "Deploying contract services..."
docker compose up -d graph-contracts
# Wait for contracts to be deployed
# timeout 300 bash -c 'until docker ps -a | grep graph-contracts | grep -q "Exited (0)"; do sleep 5; done'
interruptible_wait 300 'docker ps -a | grep graph-contracts | grep -q "Exited (0)"' "Waiting for contracts to be deployed"

# Verify the contracts have code using horizon structure
l2_graph_token_address=$(jq -r '."1337".L2GraphToken.address' horizon.json)
controller_address=$(jq -r '."1337".Controller.address' horizon.json)

echo "Checking L2GraphToken contract at $l2_graph_token_address"
code=$(docker exec chain cast code $l2_graph_token_address --rpc-url http://localhost:8545)
if [ -z "$code" ] || [ "$code" == "0x" ]; then
    echo "ERROR: L2GraphToken contract has no code!"
    exit 1
fi
echo "L2GraphToken contract verified."

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
# timeout 300 bash -c 'until docker ps | grep block-oracle | grep -q healthy; do sleep 5; done'
interruptible_wait 300 'docker ps | grep block-oracle | grep -q healthy' "Waiting for block-oracle to be healthy"

# export INDEXER_AGENT_SOURCE_ROOT=/home/neithanmo/Documents/Work/Semiotic/indexer-rs/indexer-src
# If INDEXER_AGENT_SOURCE_ROOT is set, use dev override; otherwise start only indexer-agent
if [[ -n "${INDEXER_AGENT_SOURCE_ROOT:-}" ]]; then
    echo "***********INDEXER_AGENT_SOURCE_ROOT set; using dev override for indexer-agent...************"
    G docker compose up -d indexer-agent

else
    echo "***** Starting indexer-agent from image... *****"
    docker compose up -d indexer-agent
fi

echo "Waiting for indexer-agent to be healthy..."
# timeout 300 bash -c 'until docker ps | grep indexer-agent | grep -q healthy; do sleep 5; done'
interruptible_wait 300 'docker ps | grep indexer-agent | grep -q healthy' "Waiting for indexer-agent to be healthy"

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
# timeout 90 bash -c 'until docker ps --filter "name=^tap-escrow-manager$" --format "{{.Names}}" | grep -q "^tap-escrow-manager$"; do echo "Waiting for tap-escrow-manager container to appear..."; sleep 5; done'
interruptible_wait 90 'docker ps --filter "name=^tap-escrow-manager$" --format "{{.Names}}" | grep -q "^tap-escrow-manager$"' "Waiting for tap-escrow-manager container to appear"

# Start redpanda if it's not already started (required for gateway)
if ! docker ps | grep -q redpanda; then
    echo "Starting redpanda..."
    docker compose up -d redpanda
    echo "Waiting for redpanda to be healthy..."
    # timeout 300 bash -c 'until docker ps | grep redpanda | grep -q healthy; do sleep 5; done'
    interruptible_wait 300 'docker ps | grep redpanda | grep -q healthy' "Waiting for redpanda to be healthy"
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

# Check to stop any previous instance of indexer-service, tap-agent, gateway, and indexer-cli
echo "Checking for existing conflicting services..."
if docker ps -a | grep -q "indexer-service\|tap-agent\|gateway\|indexer-cli"; then
    echo "Stopping existing indexer-service, tap-agent, gateway, or indexer-cli containers..."
    docker stop indexer-service tap-agent gateway indexer-cli 2>/dev/null || true
    docker rm indexer-service tap-agent gateway indexer-cli 2>/dev/null || true
fi

# Run the custom services using the override file
docker compose -f docker-compose.yml -f docker-compose.override.yml up --build -d
rm docker-compose.override.yml

# Wait for indexer-service and tap-agent to be healthy with better timeouts
echo "Waiting for indexer-service to be healthy..."
# timeout 120 bash -c 'until docker ps | grep indexer-service | grep -q healthy; do echo "Still waiting for indexer-service..."; sleep 5; done'
interruptible_wait 120 'docker ps | grep indexer-service | grep -q healthy' "Waiting for indexer-service to be healthy"

echo "Waiting for tap-agent to be healthy..."
# timeout 120 bash -c 'until docker ps | grep tap-agent | grep -q healthy; do echo "Still waiting for tap-agent..."; sleep 5; done'
interruptible_wait 120 'docker ps | grep tap-agent | grep -q healthy' "Waiting for tap-agent to be healthy"

# Additional check to ensure services are responding
echo "Verifying indexer-service is responding..."
# timeout 60 bash -c 'until curl -f http://localhost:7601/health > /dev/null 2>&1; do echo "Waiting for indexer-service health endpoint..."; sleep 3; done'
interruptible_wait 60 'curl -f http://localhost:7601/health > /dev/null 2>&1' "Verifying indexer-service is responding"

echo "Verifying tap-agent is responding..."
# timeout 60 bash -c 'until curl -f http://localhost:7300/metrics > /dev/null 2>&1; do echo "Waiting for tap-agent metrics endpoint..."; sleep 3; done'
interruptible_wait 60 'curl -f http://localhost:7300/metrics > /dev/null 2>&1' "Verifying tap-agent is responding"

# Wait for indexer to sync with chain before starting gateway
echo "Checking chain and indexer synchronization..."
sleep 10 # Give indexer time to process initial blocks

echo "Building gateway image..."
source local-network/.env
docker build -t local-gateway:latest ./local-network/gateway

echo "Running gateway container..."
# Verify required files exist before starting gateway
if [ ! -f "local-network/horizon.json" ]; then
    echo "ERROR: local-network/horizon.json not found!"
    exit 1
fi
if [ ! -f "local-network/tap-contracts.json" ]; then
    echo "ERROR: local-network/tap-contracts.json not found!"
    exit 1
fi
if [ ! -f "local-network/subgraph-service.json" ]; then
    echo "ERROR: local-network/subgraph-service.json not found!"
    exit 1
fi

# Updated to use the horizon file structure and include tap-contracts.json
# Gateway now generates config with increased max_lag_seconds in gateway/run.sh
# -v "$(pwd)/local-network/tap-contracts.json":/opt/tap-contracts.json:ro \
docker run -d --name gateway \
    --network local-network_default \
    -p 7700:7700 \
    -v "$(pwd)/local-network/horizon.json":/opt/horizon.json:ro \
    -v "$(pwd)/local-network/tap-contracts.json":/opt/contracts.json:ro \
    -v "$(pwd)/local-network/subgraph-service.json":/opt/subgraph-service.json:ro \
    -v "$(pwd)/local-network/.env":/opt/.env:ro \
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
# timeout 100 bash -c 'until curl -f http://localhost:7700/ > /dev/null 2>&1; do echo "Waiting for gateway service..."; sleep 5; done'
interruptible_wait 100 'curl -f http://localhost:7700/ > /dev/null 2>&1' "Waiting for gateway service"

# Build and start indexer-cli for integration testing (last container)
echo "Building and starting indexer-cli container for integration testing..."
docker compose -f docker-compose.yml -f docker-compose.override.yml up --build -d indexer-cli

# Wait for indexer-cli to be ready
echo "Waiting for indexer-cli to be ready..."
sleep 10 # Give time for the CLI to initialize

# Connect the CLI to the indexer-agent
echo "Connecting indexer-cli to indexer-agent..."
docker exec indexer-cli graph indexer connect http://indexer-agent:7600 || true

echo "============================================"
echo "Indexer CLI is ready for integration testing!"
echo "Example commands:"
echo "  List allocations:  docker exec indexer-cli graph indexer allocations get --network hardhat"
# FIXME: Provided by edge&node team, this does not work tho
echo "  Close allocation:  docker exec indexer-cli graph indexer allocations close 0x0a067bd57ad79716c2133ae414b8f6bb47aaa22d 0x0000000000000000000000000000000000000000000000000000000000000000 100 0x0000000000000000000000000000000000000000000000000000000000000000 --network hardhat --force"
echo "============================================"

# Calculate timing and final reports
SCRIPT_END_TIME=$(date +%s)
TOTAL_DURATION=$((SCRIPT_END_TIME - SCRIPT_START_TIME))
MINUTES=$((TOTAL_DURATION / 60))
SECONDS=$((TOTAL_DURATION % 60))

END_SPACE=$(df -h --output=used /var/lib/docker | tail -1)
END_SIZES=($(get_docker_sizes))
END_IMAGES_SIZE=${END_SIZES[0]}
END_CONTAINERS_SIZE=${END_SIZES[1]}
END_VOLUMES_SIZE=${END_SIZES[2]}

echo "============ SETUP COMPLETED ============"
echo "Total setup time: ${MINUTES}m ${SECONDS}s"
echo ""
echo "============ FINAL DISK USAGE ============"
echo "Docker directory usage: $END_SPACE"
echo "Images size: $END_IMAGES_SIZE"
echo "Containers size: $END_CONTAINERS_SIZE"
echo "Volumes size: $END_VOLUMES_SIZE"
echo "==========================================="
echo ""
echo "============ SERVICES RUNNING ============"
echo "✓ Indexer Service: http://localhost:7601"
echo "✓ TAP Agent: http://localhost:7300/metrics"
echo "✓ Gateway: http://localhost:7700"
echo "✓ Indexer CLI: Ready (container: indexer-cli)"
echo "   Use: docker exec indexer-cli graph-indexer indexer --help"
echo "=========================================="

# go back to root dir indexer-rs/
# and execute pg_admin.sh if requested
# this scripts deploys a docker container with pgAdmin which can be used to inspect/modify
# graphtally database tables like tap_horizon_ravs/tap_horizon_receipts and so on
cd ..

# Optional: Start pgAdmin for database inspection
if [ "$START_PGADMIN" = "true" ]; then
    echo "Starting pgAdmin for database inspection..."
    ./pg_admin.sh
fi
