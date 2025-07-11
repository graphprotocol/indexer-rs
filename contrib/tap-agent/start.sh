#!/bin/bash
set -eu

# Source environment variables from .env file
if [ -f /opt/.env ]; then
    echo "Sourcing environment variables from .env file"
    . /opt/.env
fi

# Extract TAPVerifier address from contracts.json
VERIFIER_ADDRESS=$(jq -r '."1337".TAPVerifier.address' /opt/contracts.json)
ALLOCATION_ID="0xfa44c72b753a66591f241c7dc04e8178c30e13af" # ALLOCATION_ID_0

# Wait for postgres to be ready with timeout
echo "Waiting for postgres to be ready..."
MAX_ATTEMPTS=30
ATTEMPT=0
until pg_isready -h postgres -U postgres -d indexer_components_1 || [ $ATTEMPT -eq $MAX_ATTEMPTS ]; do
    echo "Waiting for postgres... Attempt $((ATTEMPT + 1))/$MAX_ATTEMPTS"
    ATTEMPT=$((ATTEMPT + 1))
    sleep 2
done

if [ $ATTEMPT -eq $MAX_ATTEMPTS ]; then
    echo "ERROR: Failed to connect to postgres after $MAX_ATTEMPTS attempts"
    exit 1
fi

echo "Postgres is ready!"

# Wait for indexer-service to be ready with timeout
echo "Waiting for indexer-service to be ready..."
MAX_ATTEMPTS=30
ATTEMPT=0
until curl -s http://indexer-service:7601/ >/dev/null 2>&1 || [ $ATTEMPT -eq $MAX_ATTEMPTS ]; do
    echo "Waiting for indexer-service... Attempt $((ATTEMPT + 1))/$MAX_ATTEMPTS"
    ATTEMPT=$((ATTEMPT + 1))
    sleep 2
done

if [ $ATTEMPT -eq $MAX_ATTEMPTS ]; then
    echo "ERROR: Failed to connect to indexer-service after $MAX_ATTEMPTS attempts"
    exit 1
fi

echo "Indexer-service is ready!"

echo "Checking if required services are available..."
for service in postgres graph-node tap-aggregator; do
    if getent hosts $service >/dev/null 2>&1; then
        IP=$(getent hosts $service | awk '{ print $1 }')
        echo "✅ $service resolves to $IP"
    else
        echo "❌ Cannot resolve $service hostname"
    fi
done

# Get network subgraph deployment ID with retries
echo "Getting network subgraph deployment ID..."
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

echo "Network subgraph deployment ID: $NETWORK_DEPLOYMENT"

# Get escrow subgraph deployment ID with retries
echo "Getting escrow subgraph deployment ID..."
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
    echo "ERROR: Failed to get escrow subgraph deployment ID after $MAX_ATTEMPTS attempts"
    exit 1
fi

echo "Escrow subgraph deployment ID: $ESCROW_DEPLOYMENT"


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

echo "Starting tap-agent with config:"
cat /opt/config.toml

# Run agent with enhanced logging
echo "Starting tap-agent..."
export RUST_BACKTRACE=full
export RUST_LOG=debug
exec /usr/local/bin/indexer-tap-agent --config /opt/config.toml
