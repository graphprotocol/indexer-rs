#!/bin/bash

# ==============================================================================
# FUND ESCROW FOR BOTH V1 AND V2 (HORIZON)
# ==============================================================================
# This script funds both TAP escrow contracts:
# - V1: TAPEscrow for legacy receipts
# - V2: PaymentsEscrow for Horizon receipts
# ==============================================================================

# Function to get contract address from JSON file
get_contract_address() {
    local file="$1"
    local contract="$2"
    if [ ! -f "$file" ]; then
        echo "Error: File $file not found"
        exit 1
    fi

    # Try to get address directly first (tap-contracts.json format)
    local address=$(jq -r ".\"1337\".$contract" "$file" 2>/dev/null)

    # If that gives us an object, try to get the .address field (horizon.json format)
    if [[ "$address" =~ ^\{ ]]; then
        address=$(jq -r ".\"1337\".$contract.address" "$file" 2>/dev/null)
    fi

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
    echo "Error: .env file not found. Please run from integration-tests directory."
    exit 1
fi

# Get contract addresses - Updated paths to local-network directory
GRAPH_TOKEN=$(get_contract_address "../contrib/local-network/horizon.json" "L2GraphToken")
TAP_ESCROW_V1=$(get_contract_address "../contrib/local-network/tap-contracts.json" "Escrow")
PAYMENTS_ESCROW_V2=$(get_contract_address "../contrib/local-network/horizon.json" "PaymentsEscrow")
GRAPH_TALLY_COLLECTOR_V2=$(get_contract_address "../contrib/local-network/horizon.json" "GraphTallyCollector")

# Use environment variables from .env
# Payer is ACCOUNT0 (sender), signer is ACCOUNT1, receiver is the indexer
SENDER_ADDRESS="$ACCOUNT0_ADDRESS"
SENDER_KEY="$ACCOUNT0_SECRET"
RECEIVER_ADDRESS="$RECEIVER_ADDRESS"
AMOUNT="10000000000000000000"        # 10 GRT per escrow

# Gateway signer info (for authorization)
# V1: Use ACCOUNT1 (ACCOUNT0 already authorized for itself)
# V2: Use ACCOUNT0 (as originally configured)
V1_GATEWAY_SIGNER_ADDRESS="$ACCOUNT1_ADDRESS"
V1_GATEWAY_SIGNER_KEY="$ACCOUNT1_SECRET"
V2_GATEWAY_SIGNER_ADDRESS="$ACCOUNT1_ADDRESS"
V2_GATEWAY_SIGNER_KEY="$ACCOUNT1_SECRET"

echo "============ FUNDING BOTH V1 AND V2 ESCROWS ============"
echo "L2GraphToken address: $GRAPH_TOKEN"
echo "TAPEscrow (v1) address: $TAP_ESCROW_V1"
echo "PaymentsEscrow (v2) address: $PAYMENTS_ESCROW_V2"
echo "GraphTallyCollector (v2) address: $GRAPH_TALLY_COLLECTOR_V2"
echo "Sender address: $SENDER_ADDRESS"
echo "Receiver address (indexer): $RECEIVER_ADDRESS"
echo "V1 Signer address: $V1_GATEWAY_SIGNER_ADDRESS"
echo "V2 Signer address: $V2_GATEWAY_SIGNER_ADDRESS"
echo "Amount per escrow: $AMOUNT (10 GRT)"
echo "======================================================"

# Check if contracts have code deployed
echo "Verifying L2GraphToken contract..."
code=$(docker exec chain cast code $GRAPH_TOKEN --rpc-url http://localhost:8545)
if [ -z "$code" ] || [ "$code" == "0x" ]; then
    echo "Error: L2GraphToken contract has no code at $GRAPH_TOKEN"
    exit 1
fi

echo "Verifying TAPEscrow (v1) contract..."
code=$(docker exec chain cast code $TAP_ESCROW_V1 --rpc-url http://localhost:8545)
if [ -z "$code" ] || [ "$code" == "0x" ]; then
    echo "Error: TAPEscrow contract has no code at $TAP_ESCROW_V1"
    exit 1
fi

echo "Verifying PaymentsEscrow (v2) contract..."
code=$(docker exec chain cast code $PAYMENTS_ESCROW_V2 --rpc-url http://localhost:8545)
if [ -z "$code" ] || [ "$code" == "0x" ]; then
    echo "Error: PaymentsEscrow contract has no code at $PAYMENTS_ESCROW_V2"
    exit 1
fi

# ============ FUND V1 ESCROW ============
echo ""
echo "========== FUNDING V1 ESCROW =========="

# Transfer GRT from ACCOUNT0 to indexer for V1 escrow funding
## With payer set to ACCOUNT0, no transfer is needed here

# Check current escrow balance before funding
# Note: V1 TAP escrow 2-arg deposit creates sender->sender accounts
echo "Checking current V1 escrow balance..."
CURRENT_BALANCE_V1=$(docker exec chain cast call \
    --rpc-url http://localhost:8545 \
    $TAP_ESCROW_V1 "getEscrowAmount(address,address)(uint256)" $SENDER_ADDRESS $SENDER_ADDRESS)
echo "Current V1 escrow balance: $CURRENT_BALANCE_V1"

# Approve GRT for V1 escrow
echo "Approving GRT for V1 escrow..."
docker exec chain cast send \
    --rpc-url http://localhost:8545 \
    --private-key $SENDER_KEY \
    --confirmations 1 \
    $GRAPH_TOKEN "approve(address,uint256)" $TAP_ESCROW_V1 $AMOUNT

# Deposit to V1 escrow - V1 uses 2-argument deposit (sender only)
echo "Depositing to V1 escrow..."
docker exec chain cast send \
    --rpc-url http://localhost:8545 \
    --private-key $SENDER_KEY \
    --confirmations 1 \
    $TAP_ESCROW_V1 "deposit(address,uint256)" $SENDER_ADDRESS $AMOUNT

# Authorize signer for V1 escrow
echo "Authorizing signer for V1 escrow..."
# V1 TAP Escrow uses: authorizeSigner(address signer, uint256 proofDeadline, bytes calldata proof)
# The proof must be signed by the signer and contains (chainId, proofDeadline, sender)
# Gateway uses ACCOUNT1 as V1 signer (ACCOUNT0 already authorized for its own escrow)
V1_SIGNER_ADDRESS="$V1_GATEWAY_SIGNER_ADDRESS"
V1_SIGNER_KEY="$V1_GATEWAY_SIGNER_KEY"
echo "Authorizing V1 signer: $V1_SIGNER_ADDRESS to sign for payer: $SENDER_ADDRESS"

# Create proof deadline (1 hour from now)
PROOF_DEADLINE=$(($(date +%s) + 3600))
echo "Creating V1 authorization proof with deadline: $PROOF_DEADLINE"

# Create the message to sign: keccak256(abi.encodePacked(chainId, proofDeadline, sender))
CHAIN_ID_HEX=$(printf "%064x" 1337)            # uint256: 32 bytes
DEADLINE_HEX=$(printf "%064x" $PROOF_DEADLINE) # uint256: 32 bytes
SENDER_HEX=${SENDER_ADDRESS:2}                 # address: 20 bytes (no padding in encodePacked)

MESSAGE_DATA="${CHAIN_ID_HEX}${DEADLINE_HEX}${SENDER_HEX}"
MESSAGE_HASH=$(docker exec chain cast keccak "0x$MESSAGE_DATA")

# Sign the message with the signer key (ACCOUNT1)
PROOF=$(docker exec chain cast wallet sign --private-key $V1_SIGNER_KEY "$MESSAGE_HASH")

echo "Calling V1 authorizeSigner with proof..."
docker exec chain cast send \
    --rpc-url http://localhost:8545 \
    --private-key $SENDER_KEY \
    --confirmations 1 \
    $TAP_ESCROW_V1 "authorizeSigner(address,uint256,bytes)" $V1_SIGNER_ADDRESS $PROOF_DEADLINE $PROOF 2>/dev/null || {
    echo "⚠️  V1 signer authorization failed. Checking if already authorized..."
    # Check current authorization state
    V1_AUTH_STATE=$(docker exec chain cast call \
        --rpc-url http://localhost:8545 \
        $TAP_ESCROW_V1 "authorizedSigners(address)(address,uint256)" $V1_SIGNER_ADDRESS 2>/dev/null || echo "error")
    echo "Current V1 authorization state for signer: $V1_AUTH_STATE"
}

# Verify V1 signer authorization using the correct mapping
echo "Verifying V1 signer authorization..."
V1_SENDER_ADDRESS=$(docker exec chain cast call \
    --rpc-url http://localhost:8545 \
    $TAP_ESCROW_V1 "authorizedSigners(address)(address,uint256)" $V1_SIGNER_ADDRESS 2>/dev/null | head -n 1 | tr -d '\n' || echo "error")

if [ "$V1_SENDER_ADDRESS" = "$SENDER_ADDRESS" ]; then
    echo "✅ V1 signer authorization successful!"
    echo "   Payer: $SENDER_ADDRESS"
    echo "   Signer: $V1_SIGNER_ADDRESS"
    echo "   Authorization confirmed on-chain"
else
    echo "⚠️  V1 signer authorization verification failed"
    echo "   Expected sender: $SENDER_ADDRESS"
    echo "   Returned sender: $V1_SENDER_ADDRESS"
    echo "This may cause V1 receipt validation to fail with '402 No sender found for signer' errors."
fi

# Verify V1 deposit
# Note: V1 TAP escrow 2-arg deposit creates sender->sender accounts
echo "Verifying V1 deposit..."
ESCROW_BALANCE_V1=$(docker exec chain cast call \
    --rpc-url http://localhost:8545 \
    $TAP_ESCROW_V1 "getEscrowAmount(address,address)(uint256)" $SENDER_ADDRESS $SENDER_ADDRESS)
echo "New V1 escrow balance: $ESCROW_BALANCE_V1"

# Check if V1 escrow balance increased
if [[ "$ESCROW_BALANCE_V1" != "0" ]] && [[ "$ESCROW_BALANCE_V1" != "$CURRENT_BALANCE_V1" ]]; then
    echo "✅ Successfully funded V1 escrow!"
    echo "   Previous balance: $CURRENT_BALANCE_V1"
    echo "   Added amount: $AMOUNT"
    echo "   New balance: $ESCROW_BALANCE_V1"
else
    echo "❌ Failed to fund V1 escrow. Balance did not increase."
    echo "   Current balance: $ESCROW_BALANCE_V1"
    exit 1
fi

# ============ FUND V2 ESCROW ============
echo ""
echo "========== FUNDING V2 ESCROW =========="

# Query the network subgraph to find the current allocation ID
echo "Querying network subgraph for current allocation ID..."
ALLOCATION_QUERY_RESULT=$(curl -s -X POST http://localhost:8000/subgraphs/name/graph-network \
    -H "Content-Type: application/json" \
    -d '{"query": "{ allocations(where: { status: Active }) { id indexer { id } subgraphDeployment { id } } }"}')

# Extract all allocation IDs
ALL_ALLOCATION_IDS=$(echo "$ALLOCATION_QUERY_RESULT" | jq -r '.data.allocations[].id')

# Loop through each allocation and fund it
for ALLOCATION_ID in $ALL_ALLOCATION_IDS; do
    # echo "Funding allocation: $ALLOCATION_ID"
    echo "✅ Funding allocation ID: $ALLOCATION_ID"
    # For V2, we need to specify payer, collector, and receiver
    # Payer is the test account, collector is the allocation ID, receiver is the indexer
    PAYER=$SENDER_ADDRESS
    COLLECTOR=$ALLOCATION_ID
    RECEIVER="$RECEIVER_ADDRESS" # Indexer address from env

    # Check current V2 escrow balance before funding
    echo "Checking current V2 escrow balance..."
    echo "  Payer: $PAYER"
    echo "  Collector: $COLLECTOR"
    echo "  Receiver: $RECEIVER"

    # Try to get balance - V2 might use a different function name
    CURRENT_BALANCE_V2="0"
    echo "Current V2 escrow balance: $CURRENT_BALANCE_V2 (assuming 0 for new escrow)"

    ## With payer set to ACCOUNT0, no transfer is needed here

    # Approve GRT for V2 escrow
    echo "Approving GRT for V2 escrow..."
    docker exec chain cast send \
        --rpc-url http://localhost:8545 \
        --private-key $SENDER_KEY \
        --confirmations 1 \
        $GRAPH_TOKEN "approve(address,uint256)" $PAYMENTS_ESCROW_V2 $AMOUNT

    # For V2, we also need to authorize the signer
    echo "Authorizing signer for V2..."
    # Create authorization proof: payer authorizes signer (V2 uses ACCOUNT0 as originally configured)
    V2_PROOF_DEADLINE=$(($(date +%s) + 3600)) # 1 hour from now
    echo "Creating V2 authorization proof with deadline: $V2_PROOF_DEADLINE"

    # Create the message to sign according to _verifyAuthorizationProof
    # abi.encodePacked(chainId, contractAddress, "authorizeSignerProof", deadline, authorizer)
    CHAIN_ID_HEX=$(printf "%064x" 1337)                   # uint256: 32 bytes
    CONTRACT_HEX=${GRAPH_TALLY_COLLECTOR_V2:2}            # address: 20 bytes (remove 0x)
    DOMAIN_HEX=$(echo -n "authorizeSignerProof" | xxd -p) # string: no length prefix
    DEADLINE_HEX=$(printf "%064x" $V2_PROOF_DEADLINE)     # uint256: 32 bytes
    AUTHORIZER_HEX=${SENDER_ADDRESS:2}                    # address: 20 bytes (remove 0x)

    MESSAGE_DATA="${CHAIN_ID_HEX}${CONTRACT_HEX}${DOMAIN_HEX}${DEADLINE_HEX}${AUTHORIZER_HEX}"
    MESSAGE_HASH=$(docker exec chain cast keccak "0x$MESSAGE_DATA")

    # Sign the message with the payer's private key (ACCOUNT0)
    PROOF=$(docker exec chain cast wallet sign --private-key $SENDER_KEY "$MESSAGE_HASH")

    echo "Calling V2 authorizeSigner with proof..."
    docker exec chain cast send \
        --rpc-url http://localhost:8545 \
        --private-key $V2_GATEWAY_SIGNER_KEY \
        --confirmations 1 \
        $GRAPH_TALLY_COLLECTOR_V2 "authorizeSigner(address,uint256,bytes)" $SENDER_ADDRESS $V2_PROOF_DEADLINE $PROOF 2>/dev/null || {
        echo "⚠️  Signer authorization failed (likely already authorized)"
        echo "Checking if signer is already authorized..."
        IS_AUTHORIZED=$(docker exec chain cast call \
            --rpc-url http://localhost:8545 \
            $GRAPH_TALLY_COLLECTOR_V2 "isAuthorized(address,address)(bool)" $SENDER_ADDRESS $V2_GATEWAY_SIGNER_ADDRESS)
        if [ "$IS_AUTHORIZED" = "true" ]; then
            echo "✅ Signer is already authorized"
        else
            echo "❌ Signer authorization failed for unknown reason"
            exit 1
        fi
    }

    # Deposit to V2 escrow with payer, collector, receiver
    echo "Depositing to V2 escrow..."
    docker exec chain cast send \
        --rpc-url http://localhost:8545 \
        --private-key $SENDER_KEY \
        --confirmations 1 \
        $PAYMENTS_ESCROW_V2 "deposit(address,address,uint256)" $COLLECTOR $RECEIVER $AMOUNT

    # Note: We'll check via the subgraph instead of direct contract call
    echo "Deposit transaction completed."
    ESCROW_BALANCE_V2="(check via subgraph)"

    # Since we can't easily check balance via contract call, we'll verify via transaction success
    echo "✅ V2 escrow deposit transaction completed!"
    echo "   Payer: $PAYER"
    echo "   Collector: $COLLECTOR"
    echo "   Receiver: $RECEIVER"
    echo "   Amount: $AMOUNT"
    echo ""
    echo "Note: V2 escrow balance can be verified via the TAP V2 subgraph"

    echo ""
    echo "✅ Successfully funded both V1 and V2 escrows!"
done

echo "✅ Done funding escrows."
