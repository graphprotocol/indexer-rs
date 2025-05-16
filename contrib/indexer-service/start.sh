#!/bin/bash
set -eu
# Source environment variables if available
if [ -f "/opt/.env" ]; then
    source /opt/.env
fi

cat /opt/.env

# Extract TAPVerifier address from contracts.json
VERIFIER_ADDRESS=$(jq -r '."1337".TAPVerifier.address' /opt/contracts.json)

# Override with test values taken from test-assets/src/lib.rs
ALLOCATION_ID="0xfa44c72b753a66591f241c7dc04e8178c30e13af" # ALLOCATION_ID_0

# Get network subgraph deployment ID
NETWORK_DEPLOYMENT=$(curl -s "http://graph-node:8000/subgraphs/name/graph-network" \
    -H 'content-type: application/json' \
    -d '{"query": "{ _meta { deployment } }"}' | jq -r '.data._meta.deployment' 2>/dev/null)
stdbuf -oL echo "Graph-network subgraph deployment ID: $NETWORK_DEPLOYMENT"

# Get escrow subgraph deployment ID
ESCROW_DEPLOYMENT=$(curl -s "http://graph-node:8000/subgraphs/name/semiotic/tap" \
    -H 'content-type: application/json' \
    -d '{"query": "{ _meta { deployment } }"}' | jq -r '.data._meta.deployment' 2>/dev/null)

stdbuf -oL echo "Escrow subgraph deployment ID: $ESCROW_DEPLOYMENT"
stdbuf -oL echo "Using test Network subgraph deployment ID: $NETWORK_DEPLOYMENT"
stdbuf -oL echo "Using test Verifier address: $VERIFIER_ADDRESS"
stdbuf -oL echo "Using test Indexer address: $RECEIVER_ADDRESS"
stdbuf -oL echo "Using TAPVerifier address from contracts.json: $VERIFIER_ADDRESS"
stdbuf -oL echo "Using test Account0 address: $ACCOUNT0_ADDRESS"

# Create/copy config file
cp /opt/config/config.toml /opt/config.toml

# Replace the placeholders with actual values
sed -i "s/NETWORK_DEPLOYMENT_PLACEHOLDER/$NETWORK_DEPLOYMENT/g" /opt/config.toml
sed -i "s/ESCROW_DEPLOYMENT_PLACEHOLDER/$ESCROW_DEPLOYMENT/g" /opt/config.toml
sed -i "s/VERIFIER_ADDRESS_PLACEHOLDER/$VERIFIER_ADDRESS/g" /opt/config.toml
sed -i "s/INDEXER_ADDRESS_PLACEHOLDER/$RECEIVER_ADDRESS/g" /opt/config.toml
sed -i "s/INDEXER_MNEMONIC_PLACEHOLDER/$INDEXER_MNEMONIC/g" /opt/config.toml
sed -i "s/ACCOUNT0_ADDRESS_PLACEHOLDER/$ACCOUNT0_ADDRESS/g" /opt/config.toml
sed -i "s/POSTGRES_PORT_PLACEHOLDER/$POSTGRES/g" /opt/config.toml

stdbuf -oL echo "Starting indexer-service with config:"
cat /opt/config.toml

# Run basic connectivity tests
stdbuf -oL echo "Testing graph-node endpoints..."
curl -s "http://graph-node:8000" >/dev/null && stdbuf -oL echo "Query endpoint OK" || stdbuf -oL echo "Query endpoint FAILED"
curl -s "http://graph-node:8030/graphql" >/dev/null && stdbuf -oL echo "Status endpoint OK" || stdbuf -oL echo "Status endpoint FAILED"

# Set profiling tool based on environment variable
# Default is no profiling
PROFILER="${PROFILER:-none}"
stdbuf -oL echo "🔍 DEBUG: Profiling with: $PROFILER"

# Set environment variables for the service
export RUST_BACKTRACE=full
export RUST_LOG="${RUST_LOG:-trace}"

# Create output directory if it doesn't exist
mkdir -p /opt/profiling/indexer-service
chmod 777 /opt/profiling
chmod 777 /opt/profiling/indexer-service

stdbuf -oL echo "📁 DEBUG: Profiling output directory: $(ls -la /opt/profiling)"

case "$PROFILER" in
flamegraph)
    stdbuf -oL echo "🔥 Starting with profiler..."

    # Start the service in the background with output redirection
    stdbuf -oL echo "🚀 Starting service..."
    exec /usr/local/bin/indexer-service-rs --config /opt/config.toml
    ;;
strace)
    stdbuf -oL echo "🔍 Starting with strace..."
    # -f: follow child processes
    # -tt: print timestamps with microsecond precision
    # -T: show time spent in each syscall
    # -e trace=all: trace all system calls
    # -s 256: show up to 256 characters per string
    # -o: output file
    exec strace -f -tt -T -e trace=all -s 256 -o /opt/profiling/indexer-service/strace.log /usr/local/bin/indexer-service-rs --config /opt/config.toml
    ;;
valgrind)
    stdbuf -oL echo "🔍 Starting with Valgrind profiling..."

    # Start with Massif memory profiler
    stdbuf -oL echo "🔄 Starting Valgrind Massif memory profiling..."
    exec valgrind --tool=massif \
        --massif-out-file=/opt/profiling/indexer-service/massif.out \
        --time-unit=B \
        --detailed-freq=10 \
        --max-snapshots=100 \
        --threshold=0.5 \
        /usr/local/bin/indexer-service-rs --config /opt/config.toml
    ;;
# Use callgrind_annotate indexer-service.callgrind.out
# for human-friendly report of callgrind output
# Ideally you should set:
# [profile.release.package."*"]
# debug = true
# force-frame-pointers = true
# in the Cargo.toml
callgrind)
    stdbuf -oL echo "🔍 Starting with Callgrind CPU profiling..."
    exec valgrind --tool=callgrind \
        --callgrind-out-file=/opt/profiling/indexer-service/callgrind.out \
        --cache-sim=yes \
        --branch-sim=yes \
        --collect-jumps=yes \
        --collect-systime=yes \
        --collect-bus=yes \
        --dump-instr=yes \
        --dump-line=yes \
        --compress-strings=no \
        /usr/local/bin/indexer-service-rs --config /opt/config.toml
    ;;
none)
    stdbuf -oL echo "🔍 Starting without profiling..."
    exec /usr/local/bin/indexer-service-rs --config /opt/config.toml
    ;;
esac
