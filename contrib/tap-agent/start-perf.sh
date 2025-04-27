#!/bin/bash
set -eu

# Source environment variables from .env file
if [ -f /opt/.env ]; then
    stdbuf -oL echo "Sourcing environment variables from .env file"
    . /opt/.env
fi

# Extract TAPVerifier address from contracts.json
VERIFIER_ADDRESS=$(jq -r '."1337".TAPVerifier.address' /opt/contracts.json)
ALLOCATION_ID="0xfa44c72b753a66591f241c7dc04e8178c30e13af" # ALLOCATION_ID_0

# Wait for postgres to be ready
until pg_isready -h postgres -U postgres -d indexer_components_1; do
    stdbuf -oL echo "Waiting for postgres..."
    sleep 2
done

stdbuf -oL echo "Checking if required services are available..."
for service in postgres graph-node tap-aggregator; do
    if getent hosts $service >/dev/null 2>&1; then
        IP=$(getent hosts $service | awk '{ print $1 }')
        stdbuf -oL echo "✅ $service resolves to $IP"
    else
        stdbuf -oL echo "❌ Cannot resolve $service hostname"
    fi
done

# Get network subgraph deployment ID with retries
stdbuf -oL echo "Getting network subgraph deployment ID..."
MAX_ATTEMPTS=30
ATTEMPT=0
NETWORK_DEPLOYMENT=""

while [ -z "$NETWORK_DEPLOYMENT" ] || [ "$NETWORK_DEPLOYMENT" = "null" ] && [ $ATTEMPT -lt $MAX_ATTEMPTS ]; do
    NETWORK_DEPLOYMENT=$(curl -s "http://graph-node:8000/subgraphs/name/graph-network" \
        -H 'content-type: application/json' \
        -d '{"query": "{ _meta { deployment } }"}' | jq -r '.data._meta.deployment' 2>/dev/null)

    if [ -z "$NETWORK_DEPLOYMENT" ] || [ "$NETWORK_DEPLOYMENT" = "null" ]; then
        ATTEMPT=$((ATTEMPT + 1))
        echo "Waiting for network subgraph to be deployed... Attempt $ATTEMPT/$MAX_ATTEMPTS"
        sleep 5
    fi
done

if [ -z "$NETWORK_DEPLOYMENT" ] || [ "$NETWORK_DEPLOYMENT" = "null" ]; then
    echo "ERROR: Failed to get network subgraph deployment ID after $MAX_ATTEMPTS attempts"
    exit 1
fi

stdbuf -oL echo "Network subgraph deployment ID: $NETWORK_DEPLOYMENT"

# Get escrow subgraph deployment ID with retries
stdbuf -oL echo "Getting escrow subgraph deployment ID..."
MAX_ATTEMPTS=30
ATTEMPT=0
ESCROW_DEPLOYMENT=""

while [ -z "$ESCROW_DEPLOYMENT" ] || [ "$ESCROW_DEPLOYMENT" = "null" ] && [ $ATTEMPT -lt $MAX_ATTEMPTS ]; do
    ESCROW_DEPLOYMENT=$(curl -s "http://graph-node:8000/subgraphs/name/semiotic/tap" \
        -H 'content-type: application/json' \
        -d '{"query": "{ _meta { deployment } }"}' | jq -r '.data._meta.deployment' 2>/dev/null)

    if [ -z "$ESCROW_DEPLOYMENT" ] || [ "$ESCROW_DEPLOYMENT" = "null" ]; then
        ATTEMPT=$((ATTEMPT + 1))
        echo "Waiting for escrow subgraph to be deployed... Attempt $ATTEMPT/$MAX_ATTEMPTS"
        sleep 5
    fi
done

if [ -z "$ESCROW_DEPLOYMENT" ] || [ "$ESCROW_DEPLOYMENT" = "null" ]; then
    stdbuf -oL echo "ERROR: Failed to get escrow subgraph deployment ID after $MAX_ATTEMPTS attempts"
    exit 1
fi

stdbuf -oL echo "Escrow subgraph deployment ID: $ESCROW_DEPLOYMENT"

# Copy the config template
cp /opt/config/config.toml /opt/config.toml

# Replace the placeholders with actual values
sed -i "s/NETWORK_DEPLOYMENT_PLACEHOLDER/$NETWORK_DEPLOYMENT/g" /opt/config.toml
sed -i "s/ESCROW_DEPLOYMENT_PLACEHOLDER/$ESCROW_DEPLOYMENT/g" /opt/config.toml
sed -i "s/VERIFIER_ADDRESS_PLACEHOLDER/$VERIFIER_ADDRESS/g" /opt/config.toml
sed -i "s/INDEXER_ADDRESS_PLACEHOLDER/$RECEIVER_ADDRESS/g" /opt/config.toml
sed -i "s/INDEXER_MNEMONIC_PLACEHOLDER/$INDEXER_MNEMONIC/g" /opt/config.toml
sed -i "s/ACCOUNT0_ADDRESS_PLACEHOLDER/$ACCOUNT0_ADDRESS/g" /opt/config.toml
sed -i "s/TAP_AGGREGATOR_PORT_PLACEHOLDER/$TAP_AGGREGATOR/g" /opt/config.toml
sed -i "s/POSTGRES_PORT_PLACEHOLDER/$POSTGRES/g" /opt/config.toml
sed -i "s/GRAPH_NODE_GRAPHQL_PORT_PLACEHOLDER/$GRAPH_NODE_GRAPHQL/g" /opt/config.toml
sed -i "s/GRAPH_NODE_STATUS_PORT_PLACEHOLDER/$GRAPH_NODE_STATUS/g" /opt/config.toml
sed -i "s/INDEXER_SERVICE_PORT_PLACEHOLDER/$INDEXER_SERVICE/g" /opt/config.toml

stdbuf -oL echo "Starting tap-agent with config:"
cat /opt/config.toml

# Set profiling tool based on environment variable
# Default is no profiling
PROFILER="${PROFILER:-none}"
stdbuf -oL echo "🔍 DEBUG: Profiling with: $PROFILER"

# Run agent with enhanced logging
stdbuf -oL echo "Starting tap-agent..."
export RUST_BACKTRACE=full
export RUST_LOG=debug

# Create output directory if it doesn't exist
mkdir -p /opt/profiling/tap-agent
chmod 777 /opt/profiling
chmod 777 /opt/profiling/tap-agent

case "$PROFILER" in
flamegraph)
    stdbuf -oL echo "🔥 Starting with profiler..."

    # Start the service in the background with output redirection
    stdbuf -oL echo "🚀 Starting service..."
    exec /usr/local/bin/indexer-tap-agent --config /opt/config.toml
    ;;
strace)
    stdbuf -oL echo "🔍 Starting with strace..."
    # -f: follow child processes
    # -tt: print timestamps with microsecond precision
    # -T: show time spent in each syscall
    # -e trace=all: trace all system calls
    # -s 256: show up to 256 characters per string
    # -o: output file
    exec strace -f -tt -T -e trace=all -s 256 -o /opt/profiling/tap-agent/strace.log /usr/local/bin/indexer-tap-agent --config /opt/config.toml
    ;;
valgrind)
    stdbuf -oL echo "🔍 Starting with Valgrind profiling..."

    # Start with Massif memory profiler
    stdbuf -oL echo "🔄 Starting Valgrind Massif memory profiling..."
    exec valgrind --tool=massif \
        --massif-out-file=/opt/profiling/tap-agent/massif.out \
        --time-unit=B \
        --detailed-freq=10 \
        --max-snapshots=100 \
        --threshold=0.5 \
        /usr/local/bin/indexer-tap-agent --config /opt/config.toml
    ;;
# Use callgrind_annotate indexer-service.callgrind.out
# or KcacheGrind viewer
# for humand friendly report
# Ideally you should set:
# [profile.release.package."*"]
# debug = true
# force-frame-pointers = true
# in the Cargo.toml
callgrind)
    stdbuf -oL echo "🔍 Starting with Callgrind CPU profiling..."
    exec valgrind --tool=callgrind \
        --callgrind-out-file=/opt/profiling/tap-agent/callgrind.out \
        --cache-sim=yes \
        --branch-sim=yes \
        --collect-jumps=yes \
        --collect-systime=yes \
        --collect-bus=yes \
        --separate-threads=yes \
        --dump-instr=yes \
        --dump-line=yes \
        --compress-strings=no \
        /usr/local/bin/indexer-tap-agent --config /opt/config.toml
    ;;
none)
    stdbuf -oL echo "🔍 Starting without profiling..."
    exec /usr/local/bin/indexer-tap-agent --config /opt/config.toml
    ;;
esac
