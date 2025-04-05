#!/bin/bash
set -e

# Constants taken from local-network/.env
GRAPH_TOKEN="0x3Aa5ebB10DC797CAC828524e59A333d0A371443c"
TAP_ESCROW="0x0355B7B8cb128fA5692729Ab3AAa199C1753f726"
# Sender address is also known as ACCOUNT0_ADDRESS
# in our services configuration along with local-network
SENDER_ADDRESS="0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
# Sender key is the private key use to sign receipts
# This is defined as ACCOUNT0_SECRET in local-network/.env
SENDER_KEY="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
AMOUNT="10000000000000000000"

echo "Funding escrow for sender: $SENDER_ADDRESS"

# Approve GRT for escrow
echo "Approving GRT..."
docker exec chain cast send \
    --rpc-url http://localhost:8545 \
    --private-key $SENDER_KEY \
    $GRAPH_TOKEN "approve(address,uint256)" $TAP_ESCROW $AMOUNT

# Deposit to escrow
echo "Depositing to escrow..."
docker exec chain cast send \
    --rpc-url http://localhost:8545 \
    --private-key $SENDER_KEY \
    $TAP_ESCROW "deposit(address,uint256)" $SENDER_ADDRESS $AMOUNT

# Verify deposit
echo "Verifying deposit..."
ESCROW_BALANCE=$(docker exec chain cast call \
    --rpc-url http://localhost:8545 \
    $TAP_ESCROW "getEscrowAmount(address,address)(uint256)" $SENDER_ADDRESS $SENDER_ADDRESS)
echo "Escrow balance: $ESCROW_BALANCE"
if [[ "$ESCROW_BALANCE" == "0" ]]; then
    echo "Error: Failed to fund escrow"
    exit 1
fi
echo "Successfully funded escrow"
