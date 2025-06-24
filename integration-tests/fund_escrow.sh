#!/bin/bash
set -e

# ==============================================================================
# FUND ESCROW FOR HORIZON UPGRADE
# ==============================================================================
# This script funds the TAP escrow contract with GRT tokens for horizon upgrade.
# It reads the correct contract addresses from the horizon file structure.
# ==============================================================================

# Function to get contract address from JSON file
get_contract_address() {
    local file="$1"
    local contract="$2"
    if [ ! -f "$file" ]; then
        echo "Error: File $file not found"
        exit 1
    fi
    local address=$(jq -r ".\"1337\".$contract.address" "$file")
    if [ "$address" == "null" ] || [ -z "$address" ]; then
        echo "Error: Could not find $contract address in $file"
        exit 1
    fi
    echo "$address"
}

# Load environment variables
if [ -f ".env" ]; then
    source .env
else
    echo "Error: .env file not found. Please run from local-network directory."
    exit 1
fi

# Get contract addresses from horizon file structure
GRAPH_TOKEN=$(get_contract_address "horizon.json" "L2GraphToken")
TAP_ESCROW=$(get_contract_address "tap-contracts.json" "TAPEscrow")

# Use environment variables from .env
SENDER_ADDRESS="$ACCOUNT0_ADDRESS"
SENDER_KEY="$ACCOUNT0_SECRET"
AMOUNT="10000000000000000000"  # 10 GRT

echo "============ FUNDING ESCROW FOR HORIZON ============"
echo "L2GraphToken address: $GRAPH_TOKEN"
echo "TAPEscrow address: $TAP_ESCROW"
echo "Sender address: $SENDER_ADDRESS"
echo "Amount: $AMOUNT (10 GRT)"
echo "=================================================="

# Check if contracts have code deployed
echo "Verifying L2GraphToken contract..."
code=$(docker exec chain cast code $GRAPH_TOKEN --rpc-url http://localhost:8545)
if [ -z "$code" ] || [ "$code" == "0x" ]; then
    echo "Error: L2GraphToken contract has no code at $GRAPH_TOKEN"
    exit 1
fi

echo "Verifying TAPEscrow contract..."
code=$(docker exec chain cast code $TAP_ESCROW --rpc-url http://localhost:8545)
if [ -z "$code" ] || [ "$code" == "0x" ]; then
    echo "Error: TAPEscrow contract has no code at $TAP_ESCROW"
    exit 1
fi

# Check current escrow balance before funding
echo "Checking current escrow balance..."
CURRENT_BALANCE=$(docker exec chain cast call \
    --rpc-url http://localhost:8545 \
    $TAP_ESCROW "getEscrowAmount(address,address)(uint256)" $SENDER_ADDRESS $SENDER_ADDRESS)
echo "Current escrow balance: $CURRENT_BALANCE"

# Approve GRT for escrow
echo "Approving GRT for escrow..."
docker exec chain cast send \
    --rpc-url http://localhost:8545 \
    --private-key $SENDER_KEY \
    --confirmations 1 \
    $GRAPH_TOKEN "approve(address,uint256)" $TAP_ESCROW $AMOUNT

# Deposit to escrow
echo "Depositing to escrow..."
docker exec chain cast send \
    --rpc-url http://localhost:8545 \
    --private-key $SENDER_KEY \
    --confirmations 1 \
    $TAP_ESCROW "deposit(address,uint256)" $SENDER_ADDRESS $AMOUNT

# Verify deposit
echo "Verifying deposit..."
ESCROW_BALANCE=$(docker exec chain cast call \
    --rpc-url http://localhost:8545 \
    $TAP_ESCROW "getEscrowAmount(address,address)(uint256)" $SENDER_ADDRESS $SENDER_ADDRESS)
echo "New escrow balance: $ESCROW_BALANCE"

# Check if escrow balance increased (use string comparison for large numbers)
if [[ "$ESCROW_BALANCE" != "0" ]] && [[ "$ESCROW_BALANCE" != "$CURRENT_BALANCE" ]]; then
    echo "✅ Successfully funded escrow!"
    echo "   Previous balance: $CURRENT_BALANCE"
    echo "   Added amount: $AMOUNT"
    echo "   New balance: $ESCROW_BALANCE"
else
    echo "❌ Error: Escrow funding failed"
    echo "   Previous balance: $CURRENT_BALANCE"
    echo "   Actual balance: $ESCROW_BALANCE"
    exit 1
fi
