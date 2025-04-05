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

# Wait for postgres to be ready
until pg_isready -h postgres -U postgres -d indexer_components_1; do
    stdbuf -oL echo "Waiting for postgres..."
    sleep 2
done

echo "Applying database migrations..."
# Get all the migration UP files and sort them by name
for migration_file in $(find /opt/migrations -name "*.up.sql" | sort); do
    echo "Applying migration: $(basename $migration_file)"
    psql -h postgres -U postgres postgres -f $migration_file
done
echo "Database migrations completed."

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

# Run service with verbose logging
export RUST_BACKTRACE=full
# export RUST_LOG=debug
export RUST_LOG=trace
exec /usr/local/bin/indexer-service-rs --config /opt/config.toml
