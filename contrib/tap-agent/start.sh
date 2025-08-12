#!/bin/bash
set -eu

# Source environment variables from .env file
if [ -f /opt/.env ]; then
    stdbuf -oL echo "Sourcing environment variables from .env file"
    . /opt/.env
fi

cat /opt/.env

# Extract GraphTallyCollector address from horizon.json
stdbuf -oL echo "🔍 DEBUG: Extracting GraphTallyCollector address from horizon.json..."
GRAPH_TALLY_VERIFIER=$(jq -r '."1337".GraphTallyCollector.address' /opt/horizon.json)
stdbuf -oL echo "🔍 DEBUG: GraphTallyCollector address: $GRAPH_TALLY_VERIFIER"

# Override with test values taken from test-assets/src/lib.rs
ALLOCATION_ID="0xfa44c72b753a66591f241c7dc04e8178c30e13af" # ALLOCATION_ID_0

# Wait for postgres to be ready
stdbuf -oL echo "🔍 DEBUG: Waiting for postgres to be ready..."
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
stdbuf -oL echo "🔍 DEBUG: Getting network subgraph deployment ID..."
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

stdbuf -oL echo "🔍 DEBUG: Network subgraph deployment ID: $NETWORK_DEPLOYMENT"

# Get escrow subgraph deployment ID with retries
stdbuf -oL echo "🔍 DEBUG: Getting escrow subgraph deployment ID..."
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

stdbuf -oL echo "🔍 DEBUG: Escrow subgraph deployment ID: $ESCROW_DEPLOYMENT"

stdbuf -oL echo "🔍 DEBUG: Using GraphTallyCollector address: $GRAPH_TALLY_VERIFIER"
stdbuf -oL echo "🔍 DEBUG: Using Indexer address: $RECEIVER_ADDRESS"
stdbuf -oL echo "🔍 DEBUG: Using Account0 address: $ACCOUNT0_ADDRESS"

# Create endpoints.yaml file (matching the updated run.sh pattern)
cd /opt
cat >endpoints.yaml <<-EOF
${ACCOUNT0_ADDRESS}: "http://tap-aggregator:${TAP_AGGREGATOR}"
EOF

# Create config file inline (matching the updated run.sh pattern)
cat >config.toml <<-EOF
[indexer]
indexer_address = "${RECEIVER_ADDRESS}"
operator_mnemonic = "${INDEXER_MNEMONIC}"

[database]
postgres_url = "postgresql://postgres@postgres:${POSTGRES}/indexer_components_1"

[graph_node]
query_url = "http://graph-node:${GRAPH_NODE_GRAPHQL}"
status_url = "http://graph-node:${GRAPH_NODE_STATUS}/graphql"

[subgraphs.network]
query_url = "http://graph-node:${GRAPH_NODE_GRAPHQL}/subgraphs/name/graph-network"$(if [ -n "$NETWORK_DEPLOYMENT" ] && [ "$NETWORK_DEPLOYMENT" != "null" ]; then echo "
deployment_id = \"$NETWORK_DEPLOYMENT\""; fi)
recently_closed_allocation_buffer_secs = 60
syncing_interval_secs = 30

[subgraphs.escrow]
query_url = "http://graph-node:${GRAPH_NODE_GRAPHQL}/subgraphs/name/semiotic/tap"$(if [ -n "$ESCROW_DEPLOYMENT" ] && [ "$ESCROW_DEPLOYMENT" != "null" ]; then echo "
deployment_id = \"$ESCROW_DEPLOYMENT\""; fi)
syncing_interval_secs = 30

[blockchain]
chain_id = 1337
receipts_verifier_address = "${GRAPH_TALLY_VERIFIER}"

[service]
host_and_port = "0.0.0.0:${INDEXER_SERVICE}"
url_prefix = "/"
serve_network_subgraph = false
serve_escrow_subgraph = false

[tap]
max_amount_willing_to_lose_grt = 1000

[tap.rav_request]
timestamp_buffer_secs = 1000

[tap.sender_aggregator_endpoints]
${ACCOUNT0_ADDRESS} = "http://tap-aggregator:${TAP_AGGREGATOR}"

[horizon]
# Enable Horizon migration support and detection
# When enabled: Check if Horizon contracts are active in the network
#   - If Horizon contracts detected: Hybrid migration mode (new V2 receipts only, process existing V1 receipts)
#   - If Horizon contracts not detected: Remain in legacy mode (V1 receipts only)
# When disabled: Pure legacy mode, no Horizon detection performed
enabled = true
EOF

stdbuf -oL echo "Starting tap-agent with config:"
cat config.toml

# Set profiling tool based on environment variable
# Default is no profiling
PROFILER="${PROFILER:-none}"
stdbuf -oL echo "🔍 DEBUG: Profiling with: $PROFILER"

# Run agent with enhanced logging
stdbuf -oL echo "🔍 DEBUG: Starting tap-agent..."
export RUST_BACKTRACE=full
export RUST_LOG="${RUST_LOG:-debug}"

# Create output directory if it doesn't exist
mkdir -p /opt/profiling/tap-agent
chmod 777 /opt/profiling
chmod 777 /opt/profiling/tap-agent

stdbuf -oL echo "📁 DEBUG: Profiling output directory: $(ls -la /opt/profiling)"

case "$PROFILER" in
flamegraph)
    stdbuf -oL echo "🔥 Starting with profiler..."
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
# Use callgrind_annotate tap-agent.callgrind.out
# or KcacheGrind viewer
# for human friendly report
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
