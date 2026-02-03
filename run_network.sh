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

# Save the root directory path for source mounts
INDEXER_RS_ROOT="$(pwd)"

# Set source roots for development if not already set
# This enables hot-reload development mode for the Rust services
if [[ -z "${INDEXER_SERVICE_SOURCE_ROOT:-}" ]]; then
    export INDEXER_SERVICE_SOURCE_ROOT="$INDEXER_RS_ROOT"
    echo "🔧 Setting INDEXER_SERVICE_SOURCE_ROOT to: $INDEXER_SERVICE_SOURCE_ROOT"
fi

if [[ -z "${TAP_AGENT_SOURCE_ROOT:-}" ]]; then
    export TAP_AGENT_SOURCE_ROOT="$INDEXER_RS_ROOT"
    echo "🔧 Setting TAP_AGENT_SOURCE_ROOT to: $TAP_AGENT_SOURCE_ROOT"
fi

# Optionally set INDEXER_AGENT_SOURCE_ROOT if you have the TypeScript indexer checked out
# export INDEXER_AGENT_SOURCE_ROOT="/path/to/graph-protocol/indexer"
echo $"🔧 INDEXER_SERVICE_SOURCE_ROOT is: ${INDEXER_SERVICE_SOURCE_ROOT:-<not set>}"
echo $"🔧 TAP_AGENT_SOURCE_ROOT is: ${TAP_AGENT_SOURCE_ROOT:-<not set>}"

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
    # git clone https://github.com/edgeandnode/local-network.git
    cd local-network
    # Checkout to the horizon branch
    git checkout semiotic/horizon
    cd ..
fi

# Start the required services from local-network
cd local-network

# Build the list of compose files to use
COMPOSE_BASE="-f docker-compose.yaml"

# Check for dev overrides
COMPOSE_DEV_FILES=""
if [[ -n "${INDEXER_SERVICE_SOURCE_ROOT:-}" ]]; then
    echo "📦 INDEXER_SERVICE_SOURCE_ROOT detected - will use dev override for indexer-service"
    COMPOSE_DEV_FILES="$COMPOSE_DEV_FILES -f overrides/indexer-service-dev/indexer-service-dev.yaml"
fi
if [[ -n "${TAP_AGENT_SOURCE_ROOT:-}" ]]; then
    echo "📦 TAP_AGENT_SOURCE_ROOT detected - will use dev override for tap-agent"
    COMPOSE_DEV_FILES="$COMPOSE_DEV_FILES -f overrides/tap-agent-dev/tap-agent-dev.yaml"
fi

echo "Starting services with overrides..."
# Show all compose files being used
echo "Using compose files:"
echo " - Base: $COMPOSE_BASE"
[ -n "$COMPOSE_DEV_FILES" ] && echo " - Dev: $COMPOSE_DEV_FILES"

# Build strategy (defaults favor clean rebuilds to avoid stale images)
FORCE_REBUILD=${FORCE_REBUILD:-true}
PULL_BASE=${PULL_BASE:-false}
REMOVE_ORPHANS=${REMOVE_ORPHANS:-true}

echo "Build options -> FORCE_REBUILD=${FORCE_REBUILD} PULL_BASE=${PULL_BASE} REMOVE_ORPHANS=${REMOVE_ORPHANS}"

# Optionally force a clean image rebuild before starting containers
if [[ "${FORCE_REBUILD}" == "true" ]]; then
    echo "🛠  Running docker compose build with --no-cache${PULL_BASE:+ and --pull}..."
    if [[ "${PULL_BASE}" == "true" ]]; then
        docker compose $COMPOSE_BASE $COMPOSE_DEV_FILES build --no-cache --pull
    else
        docker compose $COMPOSE_BASE $COMPOSE_DEV_FILES build --no-cache
    fi
fi

# Start all services (optionally remove any orphaned containers)
if [[ "${REMOVE_ORPHANS}" == "true" ]]; then
    docker compose $COMPOSE_BASE $COMPOSE_DEV_FILES up -d --remove-orphans
else
    docker compose $COMPOSE_BASE $COMPOSE_DEV_FILES up -d
fi

echo "=== DEV MODE SETUP COMPLETE ==="
echo "Services starting with your dev binaries mounted."
echo "To rebuild and restart: cargo build --release && docker restart indexer-service tap-agent"

# Ensure gateway is ready before testing
interruptible_wait 100 'curl -f http://localhost:7700/ > /dev/null 2>&1' "Waiting for gateway service"

cd ..

# Build and start indexer-cli for integration testing (last container)
echo "Building and starting indexer-cli container for integration testing..."
docker compose -f docker-compose.yml -f docker-compose.override.yml up --build -d indexer-cli
rm -f docker-compose.override.yml

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
echo "  Close allocation:  docker exec indexer-cli graph indexer allocations close 0x0a067bd57ad79716c2133ae414b8f6bb47aaa22d 0x0000000000000000000000000000000000000000000000000000000000000000 --network hardhat --force"
echo "  Close allocations (script): ./contrib/indexer-cli/close-allocations.sh 0x<allocation-id>"
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
