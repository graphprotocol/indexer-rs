# Direct Indexer Testing with Multiple Senders

This guide explains how to set up multiple test senders for direct indexer testing, bypassing the gateway to test various sender scenarios and edge cases.

## Overview

**Direct testing approach:**
- Create receipts with different sender private keys
- Send signed receipts directly to indexer endpoints
- Test trusted vs untrusted senders, balance limits, deny list behavior, etc.

**Why this works:**
- Gateway signature verification happens **off-chain** by the indexer service
- RAV signature verification happens **on-chain** by the TAPVerifier contract
- We can simulate different senders without multiple gateway instances

## Signature Scheme & Address Generation

**The system uses standard Ethereum cryptography:**

- **Signature Algorithm**: **ECDSA with secp256k1** curve (same as Ethereum)
- **Address Derivation**: **Keccak-256 hash** of public key (standard Ethereum addresses)
- **Receipt Signing**: **EIP-712 structured data signing** for TAP receipts
- **Key Format**: **32-byte private keys** (standard Ethereum private keys)

### Key Implementation Details:

```rust
// From gateway_palaver/src/main.rs:74-76
let receipt_signer = PrivateKeySigner::from_bytes(&conf.receipts.signer)
    .expect("failed to prepare receipt signer");
let signer_address = receipt_signer.address(); // Standard Ethereum address derivation
```

```rust
// From gateway_palaver/src/receipts.rs:170-176  
let v2_domain = Eip712Domain {
    name: Some("TAP".into()),
    version: Some("2".into()),
    chain_id: Some(chain_id),
    verifying_contract: Some(verifying_contract),
    salt: None,
};
```

**Address generation follows Ethereum standards:**
1. Private key (32 bytes) → Public key (secp256k1)
2. Public key → Keccak-256 hash → Take last 20 bytes → Ethereum address

**This means you can use any Ethereum wallet/tooling to generate test keys!**

### ⚠️ **Important: Independent Keys for Security**

**DO NOT** use mnemonic-derived keys for different senders! Even though they have different addresses, they share the same seed entropy, which is a security risk.

**Instead, use completely independent private keys:**

```rust
// Account 0 (existing gateway - from hardhat mnemonic)
pub const ACCOUNT0_SECRET: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
pub const ACCOUNT0_ADDRESS: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";

// Test Sender 1 (independent random key)
pub const TEST_SENDER_1_SECRET: &str = "7c852118294e51e653712a81e05800f419141751be58f605c371e15141b007a6";
pub const TEST_SENDER_1_ADDRESS: &str = "0x976EA74026E726554dB657fA54763abd0C3a0aa9";

// Test Sender 2 (independent random key)
pub const TEST_SENDER_2_SECRET: &str = "8b3a350cf5c34c9194ca85829a2df0ec3153be0318b5e2d3348e872092edffba";  
pub const TEST_SENDER_2_ADDRESS: &str = "0x14dC79964da2C08b23698B3D3cc7Ca32193d9955";

// Untrusted Sender (independent random key - not in trusted_senders)
pub const UNTRUSTED_SENDER_SECRET: &str = "92db14e403b83dfe3df233f83dfa3a0d7096f21ca9b0d6d6b8d88b2b4ec1564e";
pub const UNTRUSTED_SENDER_ADDRESS: &str = "0x23618e81E3f5cdF7f54C3d65f7FBc0aBf5B21E8f";
```

### Generate Independent Keys:

```bash
# Generate completely independent random keys for testing:
cast wallet new
# Output: 
# Address:     0x976EA74026E726554dB657fA54763abd0C3a0aa9
# Private key: 0x7c852118294e51e653712a81e05800f419141751be58f605c371e15141b007a6

cast wallet new  
# Output:
# Address:     0x14dC79964da2C08b23698B3D3cc7Ca32193d9955
# Private key: 0x8b3a350cf5c34c9194ca85829a2df0ec3153be0318b5e2d3348e872092edffba

# Verify address derivation:
cast wallet address --private-key 0x7c852118294e51e653712a81e05800f419141751be58f605c371e15141b007a6
# Output: 0x976EA74026E726554dB657fA54763abd0C3a0aa9 ✅
```

**Why independent keys matter:**
- Each key has **completely different entropy** 
- **No shared seed material** between test senders
- **Realistic testing** - real-world senders don't share keys
- **Better security practices** - follows principle of key isolation

## Requirements

### 1. On-Chain: Escrow Funding

Each test sender must have GRT deposited in the TAP escrow contract:

```solidity
// Escrow.sol maintains this mapping:
mapping(address sender => mapping(address receiver => EscrowAccount)) escrowAccounts;
```

**Required steps per sender:**
1. Transfer GRT tokens to sender address
2. Approve TAP escrow contract to spend GRT  
3. Deposit GRT to escrow for the indexer (receiver)

### 2. Off-Chain: Indexer Configuration

Update indexer configuration to recognize new senders:

```toml
[tap]
trusted_senders = [
    "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266",  # Original (Account 0)
    "0x70997970C51812dc3A010C7d01b50e0d17dc79C8",  # Test Sender 1 (Account 1)  
    "0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC"   # Test Sender 2 (Account 2)
]

[tap.sender_aggregator_endpoints]
"0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266" = "http://tap-aggregator:7610"
"0x70997970C51812dc3A010C7d01b50e0d17dc79C8" = "http://tap-aggregator:7610"
"0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC" = "http://tap-aggregator:7610"
```

## Setup Script

Create `setup_test_senders.sh`:

```bash
#!/bin/bash
# Load environment from local network
source ../contrib/local-network/.env

# Get contract addresses
GRAPH_TOKEN=$(jq -r '."1337".L2GraphToken.address' ../contrib/local-network/horizon.json)
TAP_ESCROW_V1=$(jq -r '."1337".TAPEscrow.address' ../contrib/local-network/tap-contracts.json)

# Test senders (independent random keys - NOT mnemonic-derived)
SENDERS=(
    "0x976EA74026E726554dB657fA54763abd0C3a0aa9:7c852118294e51e653712a81e05800f419141751be58f605c371e15141b007a6"
    "0x14dC79964da2C08b23698B3D3cc7Ca32193d9955:8b3a350cf5c34c9194ca85829a2df0ec3153be0318b5e2d3348e872092edffba"
)

AMOUNT="10000000000000000000" # 10 GRT per sender

echo "Setting up test senders for direct indexer testing..."

for sender_info in "${SENDERS[@]}"; do
    SENDER_ADDR="${sender_info%:*}"
    SENDER_KEY="${sender_info#*:}"
    
    echo "📝 Setting up sender: $SENDER_ADDR"
    
    # 1. Transfer GRT to sender
    echo "  💰 Transferring GRT..."
    docker exec chain cast send \
        --rpc-url http://localhost:8545 \
        --private-key $ACCOUNT0_SECRET \
        $GRAPH_TOKEN "transfer(address,uint256)" $SENDER_ADDR "20000000000000000000"
    
    # 2. Approve escrow contract
    echo "  ✅ Approving escrow..."
    docker exec chain cast send \
        --rpc-url http://localhost:8545 \
        --private-key $SENDER_KEY \
        $GRAPH_TOKEN "approve(address,uint256)" $TAP_ESCROW_V1 $AMOUNT
        
    # 3. Deposit to escrow for indexer
    echo "  🏦 Depositing to escrow..."
    docker exec chain cast send \
        --rpc-url http://localhost:8545 \
        --private-key $SENDER_KEY \
        $TAP_ESCROW_V1 "deposit(address,uint256)" $RECEIVER_ADDRESS $AMOUNT
        
    echo "  ✅ Completed setup for $SENDER_ADDR"
done

# 4. Update indexer config
echo "📝 Updating indexer configuration..."
docker exec -it indexer-service bash -c '
  sed -i "s/trusted_senders = \[\]/trusted_senders = [\"0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266\", \"0x976EA74026E726554dB657fA54763abd0C3a0aa9\", \"0x14dC79964da2C08b23698B3D3cc7Ca32193d9955\"]/g" /opt/config.toml
'

# 5. Restart services
echo "🔄 Restarting indexer services..."
docker restart indexer-service tap-agent

echo "✅ Multi-sender test setup complete!"
```

## Integration Test Usage

Add test sender constants to your `constants.rs`:

```rust
// constants.rs - Additional test senders for multi-sender testing

// Trusted Sender 1 (independent random key)
pub const TRUSTED_SENDER_1_KEY: &str = "7c852118294e51e653712a81e05800f419141751be58f605c371e15141b007a6";  
pub const TRUSTED_SENDER_1_ADDR: &str = "0x976EA74026E726554dB657fA54763abd0C3a0aa9";

// Trusted Sender 2 (independent random key)  
pub const TRUSTED_SENDER_2_KEY: &str = "8b3a350cf5c34c9194ca85829a2df0ec3153be0318b5e2d3348e872092edffba";
pub const TRUSTED_SENDER_2_ADDR: &str = "0x14dC79964da2C08b23698B3D3cc7Ca32193d9955";

// Untrusted Sender (independent random key - not in trusted_senders config)
pub const UNTRUSTED_SENDER_KEY: &str = "92db14e403b83dfe3df233f83dfa3a0d7096f21ca9b0d6d6b8d88b2b4ec1564e";
pub const UNTRUSTED_SENDER_ADDR: &str = "0x23618e81E3f5cdF7f54C3d65f7FBc0aBf5B21E8f";

// EIP-712 Domain for TAP receipts (matches gateway configuration)
pub const TAP_EIP712_DOMAIN_NAME: &str = "TAP";
pub const TAP_EIP712_DOMAIN_VERSION: &str = "2";
```

Example test:

```rust
#[tokio::test]
async fn test_trusted_vs_untrusted_senders() -> Result<()> {
    // Create signers
    let trusted_signer = PrivateKeySigner::from_str(TRUSTED_SENDER_1_KEY)?;
    let untrusted_signer = PrivateKeySigner::from_str(UNTRUSTED_SENDER_KEY)?;
    
    let allocation_id = find_allocation().await?;
    let large_fee = MAX_RECEIPT_VALUE * 10; // Above normal limits
    
    // Test 1: Trusted sender can exceed escrow balance
    let trusted_receipt = create_signed_receipt(&trusted_signer, allocation_id, large_fee)?;
    let response = send_receipt_directly(INDEXER_URL, trusted_receipt).await?;
    assert!(response.status().is_success(), "Trusted sender should be accepted");
    
    // Test 2: Untrusted sender is denied for large amounts  
    let untrusted_receipt = create_signed_receipt(&untrusted_signer, allocation_id, large_fee)?;
    let response = send_receipt_directly(INDEXER_URL, untrusted_receipt).await?;
    assert_eq!(response.status(), 403, "Untrusted sender should be denied");
    
    Ok(())
}

async fn send_receipt_directly(indexer_url: &str, receipt: TapReceipt) -> Result<Response> {
    Client::new()
        .post(format!("{}/subgraphs/id/{}", indexer_url, SUBGRAPH_ID))
        .header("tap-receipt", serde_json::to_string(&receipt)?)
        .header("content-type", "application/json")
        .json(&serde_json::json!({"query": "{ _meta { block { number } } }"}))
        .send()
        .await
        .map_err(Into::into)
}
```

## Test Scenarios You Can Cover

With multiple senders, test:

- ✅ **Trusted vs untrusted sender behavior**
- ✅ **Balance limit enforcement** 
- ✅ **Deny list functionality**
- ✅ **RAV generation with mixed senders**
- ✅ **Receipt signature verification**
- ✅ **Timestamp buffer edge cases**
- ✅ **Sender timeout behavior**

## Key Benefits

1. **Fast execution** - No container restarts between tests
2. **Precise control** - Test exact sender scenarios
3. **Easy isolation** - Each sender can use different allocations
4. **Comprehensive coverage** - Test all edge cases without gateway complexity

Run `./setup_test_senders.sh` once, then execute all your multi-sender integration tests! 🚀

## Summary: What You Need to Know

### ✅ **Signature Scheme**
- **Standard Ethereum**: ECDSA secp256k1 + Keccak-256 addresses  
- **EIP-712 structured signing** for TAP receipts
- **Use any Ethereum tooling** (cast, MetaMask, etc.) to generate keys

### ✅ **On-Chain Requirements** 
- **GRT tokens** must be transferred to each test sender
- **Escrow deposits** must be made for each sender → indexer pair
- **Smart contract** maintains `escrowAccounts[sender][receiver]` balances

### ✅ **Off-Chain Requirements**
- **Indexer config** must include senders in `trusted_senders` array
- **Aggregator endpoints** must be configured for each sender
- **Services restart** required to pick up new configuration

### ✅ **Testing Benefits**
- **No gateway complexity** - sign receipts directly with test keys
- **Precise control** over sender scenarios (trusted/untrusted/balance limits)  
- **Fast execution** - no container restarts between tests
- **Complete coverage** of edge cases and corner scenarios

### 🔑 **Key Insight**
The system uses **standard Ethereum cryptography** throughout. Any Ethereum private key can be used as a TAP sender - you just need to fund the escrow and update the indexer configuration.

**Direct indexer testing gives you full control over multi-sender scenarios without the overhead of multiple gateway instances!** 🎯