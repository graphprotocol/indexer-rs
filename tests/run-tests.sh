#!/bin/bash
set -e
echo "Starting TAP protocol tests..."

# Source environment variables from .env file
if [ -f /tests/.env ]; then
    echo "Sourcing environment variables from .env file"
    source /tests/.env
else
    echo "WARNING: .env file not found, using default values"
    RECEIVER_ADDRESS="0xf4EF6650E48d099a4972ea5B414daB86e1998Bd3"
    ACCOUNT0_ADDRESS="0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
fi

#_____________________________________________________________________________________________
echo "Checking network connectivity from test container..."

# List of all services you need
SERVICES=("postgres" "graph-node" "ipfs" "chain" "indexer-service" "tap-agent" "tap-aggregator" "block-oracle")

# Check DNS resolution for each service
for service in "${SERVICES[@]}"; do
    if getent hosts $service >/dev/null 2>&1; then
        IP=$(getent hosts $service | awk '{ print $1 }')
        echo "✅ DNS: $service resolves to $IP"
    else
        echo "❌ DNS: $service hostname resolution failed"
    fi
done

# Check connectivity for each service using common ports
echo ""
echo "Checking service connectivity..."
# Map service to port
declare -A SERVICE_PORTS
SERVICE_PORTS["postgres"]=5432
SERVICE_PORTS["graph-node"]=8000
SERVICE_PORTS["ipfs"]=5001
SERVICE_PORTS["chain"]=8545
SERVICE_PORTS["indexer-service"]=7601
# SERVICE_PORTS["tap-agent"]=7601
SERVICE_PORTS["tap-agent"]=7300 # tap-agent metrics
SERVICE_PORTS["tap-aggregator"]=7610
SERVICE_PORTS["block-oracle"]=9090

for service in "${SERVICES[@]}"; do
    PORT=${SERVICE_PORTS[$service]}
    if nc -z -w 1 $service $PORT >/dev/null 2>&1; then
        echo "✅ CONN: Can connect to $service:$PORT"
    else
        echo "❌ CONN: Cannot connect to $service:$PORT"
    fi
done

#____________________________________________________________________________________________

# Get network subgraph deployment ID
echo "Getting network subgraph deployment ID..."
RETRY_COUNT=0
MAX_RETRIES=10
NETWORK_DEPLOYMENT=""

while [ -z "$NETWORK_DEPLOYMENT" ] && [ $RETRY_COUNT -lt $MAX_RETRIES ]; do
    NETWORK_DEPLOYMENT=$(curl -s "http://graph-node:8000/subgraphs/name/graph-network" \
        -H 'content-type: application/json' \
        -d '{"query": "{ _meta { deployment } }"}' | jq -r '.data._meta.deployment' 2>/dev/null)

    if [ -z "$NETWORK_DEPLOYMENT" ] || [ "$NETWORK_DEPLOYMENT" = "null" ]; then
        RETRY_COUNT=$((RETRY_COUNT + 1))
        echo "Waiting for network subgraph to be available (attempt $RETRY_COUNT/$MAX_RETRIES)..."
        sleep 3
    fi
done

if [ -z "$NETWORK_DEPLOYMENT" ] || [ "$NETWORK_DEPLOYMENT" = "null" ]; then
    echo "❌ Failed to get network subgraph deployment ID after $MAX_RETRIES attempts"
    exit 1
fi

echo "✅ Network subgraph deployment ID: $NETWORK_DEPLOYMENT"

# Wait for indexer service to be fully ready
echo "Waiting for indexer service to be ready..."
RETRY_COUNT=0
while ! curl -s http://indexer-service:7601/ >/dev/null && [ $RETRY_COUNT -lt $MAX_RETRIES ]; do
    RETRY_COUNT=$((RETRY_COUNT + 1))
    echo "Waiting for indexer service (attempt $RETRY_COUNT/$MAX_RETRIES)..."
    sleep 2
done

if ! curl -s http://indexer-service:7601/ >/dev/null; then
    echo "❌ Indexer service not ready after $MAX_RETRIES attempts"
    exit 1
fi

echo "✅ Indexer service is ready"

# Check network connectivity from test container
echo "Checking network connectivity from test container..."
for service in postgres graph-node tap-aggregator indexer-service tap-agent; do
    if getent hosts $service >/dev/null 2>&1; then
        IP=$(getent hosts $service | awk '{ print $1 }')
        echo "✅ $service resolves to $IP"
    else
        echo "❌ $service hostname resolution failed"
    fi
done

# Create a signed receipt
# Format timestamp as Unix timestamp in seconds
TIMESTAMP=$(date +%s)
# Create the receipt JSON
RECEIPT='{
  "signature": "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef1c",
  "signer": "'$ACCOUNT0_ADDRESS'",
  "message": {
    "allocation_id": "'$NETWORK_DEPLOYMENT'",
    "timestamp_ns": '$((TIMESTAMP * 1000000000))',
    "value": 100,
    "nonce": 1
  }
}'

# Escape quotes for header transmission
RECEIPT_HEADER=$(echo $RECEIPT | jq -c '.' | sed 's/"/\\"/g')

echo "Running test suite..."

# ----- TEST CASE 1: Query without receipt should fail -----
echo "TEST CASE 1: Query without receipt should return payment required"
RESPONSE=$(curl -s -X POST "http://indexer-service:7601/subgraphs/id/$NETWORK_DEPLOYMENT" \
    -H 'content-type: application/json' \
    -d '{"query": "{ _meta { block { number } } }"}')
echo "Response: $RESPONSE"

# Check if the response contains payment required error
if echo "$RESPONSE" | grep -q "No Tap receipt was found"; then
    echo "✅ Test passed: Query without receipt correctly returned payment required"
else
    echo "❌ Test failed: Query without receipt should have returned payment required"
    exit 1
fi

# ----- TEST CASE 2: Query with receipt should succeed -----
echo "TEST CASE 2: Query with receipt should succeed"
RESPONSE=$(curl -s -X POST "http://indexer-service:7601/subgraphs/id/$NETWORK_DEPLOYMENT" \
    -H 'content-type: application/json' \
    -H "X-GraphOS-TAP-Receipt: $RECEIPT" \
    -d '{"query": "{ _meta { block { number } } }"}')
echo "Response: $RESPONSE"

# Check if the response contains block number
if echo "$RESPONSE" | grep -q "block"; then
    echo "✅ Test passed: Query with receipt succeeded"
else
    echo "❌ Test failed: Query with receipt should have succeeded"
    exit 1
fi

# ----- TEST CASE 3: Test multiple header formats -----
echo "TEST CASE 3: Testing different header formats"

echo "Testing with X-TAP-Receipt header..."
RESPONSE=$(curl -s -X POST "http://indexer-service:7601/subgraphs/id/$NETWORK_DEPLOYMENT" \
    -H 'content-type: application/json' \
    -H "X-TAP-Receipt: $RECEIPT" \
    -d '{"query": "{ _meta { block { number } } }"}')

if echo "$RESPONSE" | grep -q "block"; then
    echo "✅ X-TAP-Receipt header worked"
else
    echo "❌ X-TAP-Receipt header failed (this is expected if only X-GraphOS-TAP-Receipt is supported)"
fi

# ----- TEST CASE 4: Wait for receipt processing and check escrow accounts -----
echo "TEST CASE 4: Checking if receipt was processed and escrow accounts were updated"

# Wait for receipt processing
echo "Waiting for receipt processing..."
sleep 10

# Check escrow accounts
RETRY_COUNT=0
MAX_RETRIES=5
ESCROW_ACCOUNTS=""

while [ $RETRY_COUNT -lt $MAX_RETRIES ]; do
    echo "Checking escrow accounts (attempt $((RETRY_COUNT + 1))/$MAX_RETRIES)..."
    ESCROW_ACCOUNTS=$(curl -s "http://graph-node:8000/subgraphs/name/semiotic/tap" \
        -H 'content-type: application/json' \
        -d '{"query": "{ escrowAccounts { balance sender { id } receiver { id } } }"}')

    echo "Escrow accounts response: $ESCROW_ACCOUNTS"

    # Check if we have escrow accounts data
    if echo "$ESCROW_ACCOUNTS" | jq -e '.data.escrowAccounts' >/dev/null 2>&1; then
        if [ "$(echo "$ESCROW_ACCOUNTS" | jq '.data.escrowAccounts | length')" -gt 0 ]; then
            echo "✅ Found escrow accounts data"
            break
        fi
    fi

    RETRY_COUNT=$((RETRY_COUNT + 1))
    sleep 5
done

# Final validation
if echo "$ESCROW_ACCOUNTS" | jq -e '.data.escrowAccounts | length > 0' >/dev/null 2>&1; then
    echo "✅ Test passed: Found escrow accounts data"
else
    echo "❌ Test failed: Could not retrieve escrow accounts data after multiple attempts"
    exit 1
fi

# ----- TEST CASE 5: Check tap-agent functionality -----
echo "TEST CASE 5: Verifying tap-agent functionality"

# Check if tap-agent metrics endpoint is accessible
TAP_AGENT_METRICS=$(curl -s -f http://tap-agent:7300/metrics 2>/dev/null || echo "Connection failed")

if echo "$TAP_AGENT_METRICS" | grep -q "Connection failed"; then
    echo "⚠️ Could not connect to tap-agent metrics endpoint"
else
    echo "✅ TAP agent metrics endpoint is accessible"
fi
