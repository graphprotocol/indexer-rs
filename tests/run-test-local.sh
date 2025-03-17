#!/bin/bash
set -e
echo "Starting TAP protocol tests (local version)..."

INDEXER_ADDRESS="0xd75c4dbcb215a6cf9097cfbcc70aab2596b96a9c"
RECEIVER_ADDRESS="0xd75c4dbcb215a6cf9097cfbcc70aab2596b96a9c"
ACCOUNT0_ADDRESS="0x9858EfFD232B4033E47d90003D41EC34EcaEda94" # TAP_SENDER
NETWORK_DEPLOYMENT="QmU7zqJyHSyUP3yFii8sBtHT8FaJn2WmUnRvwjAUTjwMBP"
ALLOCATION_ID="0xfa44c72b753a66591f241c7dc04e8178c30e13af" # ALLOCATION_ID_0
# Get a valid deployment ID
DEPLOYMENT_ID="QmVXR3Eju3wC6BewhFgh5GcC91ujnCTXmonPAsAeASJuJx"

# Create a simplified receipt
TIMESTAMP=$(date +%s%N)
RECEIPT=$(
    cat <<EOF
{
  "message": {
    "allocation_id": "$ALLOCATION_ID",
    "timestamp_ns": $TIMESTAMP,
    "nonce": 0,
    "value": 100
  },
  "signature": {
    "r": "0xce9410f213c4cbfe15e5047a43568e7979119d588a64f97b71023c6406e1dde6",
    "s": "0x1ee325251fac54ba440a296f11aa66091fd9344a1077370286b382b250b63ba9",
    "yParity": "0x0",
    "v": "0x0"
  }
}
EOF
)

# Properly escape the receipt JSON
ESCAPED_RECEIPT=$(echo "$RECEIPT" | jq -c -R .)
# Remove the outer quotes that jq adds
ESCAPED_RECEIPT=${ESCAPED_RECEIPT:1:-1}

# Format the query exactly like in the test
QUERY='{"query":"query","variables":null}'

echo "Escaped Receipt: $ESCAPED_RECEIPT"
echo "Query: $QUERY"

# Test 1: Basic query without receipt
echo "Test 1: Basic query without receipt"
curl -s -X POST "http://localhost:7601/subgraphs/id/$DEPLOYMENT_ID" \
    -H 'content-type: application/json' \
    -d "$QUERY" | jq .

# Test 2: Create a temporary file with the receipt header
echo "Test 2: Using temporary file for header"
HEADER_FILE=$(mktemp)
echo -n "tap-receipt: $RECEIPT" >"$HEADER_FILE"
cat "$HEADER_FILE"

curl -s -X POST "http://localhost:7601/subgraphs/id/$DEPLOYMENT_ID" \
    -H 'content-type: application/json' \
    -H @"$HEADER_FILE" \
    -d "$QUERY" | jq .
rm "$HEADER_FILE"

# Test 3: Try with different quoted formats
echo "Test 3: Using different quoting style"
curl -s -X POST "http://localhost:7601/subgraphs/id/$DEPLOYMENT_ID" \
    -H 'content-type: application/json' \
    -H 'tap-receipt: '"$RECEIPT" \
    -d "$QUERY" | jq .

# Test 4: Try with a completely simplified receipt
echo "Test 4: Using simplified receipt"
SIMPLE_RECEIPT='{
  "message": {
    "allocation_id": "0xfa44c72b753a66591f241c7dc04e8178c30e13af",
    "timestamp_ns": 1742503053451576522,
    "nonce": 0,
    "value": 100
  },
  "signature": {
    "r": "0xce9410f213c4cbfe15e5047a43568e7979119d588a64f97b71023c6406e1dde6",
    "s": "0x1ee325251fac54ba440a296f11aa66091fd9344a1077370286b382b250b63ba9",
    "yParity": "0x0",
    "v": "0x0"
  }
}'

curl -s -X POST "http://localhost:7601/subgraphs/id/$DEPLOYMENT_ID" \
    -H 'content-type: application/json' \
    -H "tap-receipt: $SIMPLE_RECEIPT" \
    -d "$QUERY" | jq .

echo "Tests completed."TAP_SIGNER="0x533661F0fb14d2E8B26223C86a610Dd7D2260892"
