#!/bin/bash
set -eu

# Source environment variables if available
if [ -f "/opt/.env" ]; then
    source /opt/.env
fi

cat /opt/.env

# Extract GraphTallyCollector address from horizon.json
stdbuf -oL echo "🔍 DEBUG: Extracting GraphTallyCollector address from horizon.json..."
GRAPH_TALLY_VERIFIER=$(jq -r '."1337".GraphTallyCollector.address' /opt/horizon.json)
stdbuf -oL echo "🔍 DEBUG: GraphTallyCollector address: $GRAPH_TALLY_VERIFIER"

# Override with test values taken from test-assets/src/lib.rs
ALLOCATION_ID="0xfa44c72b753a66591f241c7dc04e8178c30e13af" # ALLOCATION_ID_0

# Get network subgraph deployment ID
stdbuf -oL echo "🔍 DEBUG: Fetching network subgraph deployment ID..."
NETWORK_DEPLOYMENT=$(curl -s --max-time 10 "http://graph-node:8000/subgraphs/name/graph-network" \
    -H 'content-type: application/json' \
    -d '{"query": "{ _meta { deployment } }"}' | jq -r '.data._meta.deployment' 2>/dev/null)
stdbuf -oL echo "🔍 DEBUG: Network deployment result: $NETWORK_DEPLOYMENT"

# Get escrow subgraph deployment ID
stdbuf -oL echo "🔍 DEBUG: Fetching escrow subgraph deployment ID..."
ESCROW_DEPLOYMENT=$(curl -s --max-time 10 "http://graph-node:8000/subgraphs/name/semiotic/tap" \
    -H 'content-type: application/json' \
    -d '{"query": "{ _meta { deployment } }"}' | jq -r '.data._meta.deployment' 2>/dev/null)
stdbuf -oL echo "🔍 DEBUG: Escrow deployment result: $ESCROW_DEPLOYMENT"

# Run basic connectivity tests
stdbuf -oL echo "Testing graph-node endpoints..."
curl -s "http://graph-node:8000" >/dev/null && stdbuf -oL echo "Query endpoint OK" || stdbuf -oL echo "Query endpoint FAILED"
curl -s "http://graph-node:8030/graphql" >/dev/null && stdbuf -oL echo "Status endpoint OK" || stdbuf -oL echo "Status endpoint FAILED"

stdbuf -oL echo "Using GraphTallyCollector address: $GRAPH_TALLY_VERIFIER"
stdbuf -oL echo "Using test Indexer address: $RECEIVER_ADDRESS"
stdbuf -oL echo "Using test Account0 address: $ACCOUNT0_ADDRESS"

# Create config file inline (similar to the new run.sh approach)
cat >/opt/config.toml <<-EOF
[indexer]
indexer_address = "${RECEIVER_ADDRESS}"
operator_mnemonic = "${INDEXER_MNEMONIC}"

[database]
postgres_url = "postgresql://postgres@postgres:${POSTGRES}/indexer_components_1"

[graph_node]
query_url = "http://graph-node:8000"
status_url = "http://graph-node:8030/graphql"

[subgraphs.network]
query_url = "http://graph-node:8000/subgraphs/name/graph-network"$(if [ -n "$NETWORK_DEPLOYMENT" ] && [ "$NETWORK_DEPLOYMENT" != "null" ]; then echo "
deployment_id = \"$NETWORK_DEPLOYMENT\""; fi)
recently_closed_allocation_buffer_secs = 60
syncing_interval_secs = 30

[subgraphs.escrow]
query_url = "http://graph-node:8000/subgraphs/name/semiotic/tap"$(if [ -n "$ESCROW_DEPLOYMENT" ] && [ "$ESCROW_DEPLOYMENT" != "null" ]; then echo "
deployment_id = \"$ESCROW_DEPLOYMENT\""; fi)
syncing_interval_secs = 30

[blockchain]
chain_id = 1337
receipts_verifier_address = "${GRAPH_TALLY_VERIFIER}"

[service]
free_query_auth_token = "freestuff"
host_and_port = "0.0.0.0:7601"
url_prefix = "/"
serve_network_subgraph = false
serve_escrow_subgraph = false

[tap]
max_amount_willing_to_lose_grt = 1000

[tap.rav_request]
timestamp_buffer_secs = 1000

[tap.sender_aggregator_endpoints]
${ACCOUNT0_ADDRESS} = "http://tap-aggregator:8080"

[horizon]
# Enable Horizon migration support and detection
# When enabled: Check if Horizon contracts are active in the network
#   - If Horizon contracts detected: Hybrid migration mode (new V2 receipts only, process existing V1 receipts)
#   - If Horizon contracts not detected: Remain in legacy mode (V1 receipts only)
# When disabled: Pure legacy mode, no Horizon detection performed
enabled = true
EOF

stdbuf -oL echo "Starting indexer-service with config:"
cat /opt/config.toml

# Set profiling tool based on environment variable (keeping your existing profiling support)
PROFILER="${PROFILER:-none}"
stdbuf -oL echo "🔍 DEBUG: Profiling with: $PROFILER"

# Set environment variables for the service
export RUST_BACKTRACE=full
export RUST_LOG="${RUST_LOG:-trace}"

# Create output directory if it doesn't exist (for profiling)
mkdir -p /opt/profiling/indexer-service
chmod 777 /opt/profiling
chmod 777 /opt/profiling/indexer-service

case "$PROFILER" in
flamegraph)
    stdbuf -oL echo "🔥 Starting with profiler..."
    exec /usr/local/bin/indexer-service-rs --config /opt/config.toml
    ;;
strace)
    stdbuf -oL echo "🔍 Starting with strace..."
    exec strace -f -tt -T -e trace=all -s 256 -o /opt/profiling/indexer-service/strace.log /usr/local/bin/indexer-service-rs --config /opt/config.toml
    ;;
valgrind)
    stdbuf -oL echo "🔍 Starting with Valgrind profiling..."
    exec valgrind --tool=massif \
        --massif-out-file=/opt/profiling/indexer-service/massif.out \
        --time-unit=B \
        --detailed-freq=10 \
        --max-snapshots=100 \
        --threshold=0.5 \
        /usr/local/bin/indexer-service-rs --config /opt/config.toml
    ;;
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
