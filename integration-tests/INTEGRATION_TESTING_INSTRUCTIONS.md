# Integration Testing Instructions from Scratch

This document provides step-by-step instructions for setting up and running integration tests for indexer-rs from a fresh repository checkout.

## Prerequisites

Before starting, ensure you have the following tools installed:

- **Docker**: `docker --version` (tested with 28.3.2)
- **Docker Compose**: `docker compose version` (tested with v2.38.2)
- **Rust/Cargo**: `cargo --version` (tested with 1.88.0)
- **Just**: `just --version` (tested with 1.40.0)

## Step 1: Initial Setup

1. **Navigate to repository root**:

   ```bash
   cd /path/to/indexer-rs
   ```

2. **Verify git status**:

   ```bash
   git status
   ```

   Expected: Clean working directory on main branch.

## Step 2: Clean Environment

**Clean any existing containers**:

```bash
just down
```

This removes all containers and networks from previous test runs.

## Step 3: Full Test Network Setup

**Start the complete test environment**:

```bash
just setup
```

This command:

- Clones the local-network repository (branch: `suchapalaver/test/horizon`)
- Starts blockchain, IPFS, PostgreSQL, and Graph Node
- Deploys Graph Protocol contracts (both Legacy and Horizon)
- Deploys TAP contracts
- Builds and starts indexer-service and tap-agent
- Deploys network subgraph with TAP v2 support
- Starts supporting services (gateway, TAP aggregator, etc.)

**Expected output**: All services should start successfully without errors.

## Step 4: Verify Services Are Running

**Check that all critical services are healthy**:

```bash
docker ps --format "table {{.Names}}\t{{.Status}}" | grep -E "(indexer-service|tap-agent|gateway|graph-node|chain)"
```

Expected output:

```
indexer-service      Up X minutes (healthy)
tap-agent            Up X minutes (healthy)
gateway              Up X minutes
graph-node           Up X minutes (healthy)
chain                Up X minutes (healthy)
```

## Step 5: Fund Escrow Accounts

**Fund both V1 and V2 escrow accounts**:

```bash
cd integration-tests && ./fund_escrow.sh
```

**Expected output**:

- V1 escrow successfully funded with 10 GRT
- V2 escrow successfully funded with 10 GRT
- Both transactions should show success confirmations

## Step 6: Verify Horizon Detection

**Check if indexer-service detects Horizon contracts**:

```bash
docker logs indexer-service 2>&1 | grep -i horizon | tail -3
```

**Expected output**:

```
Horizon contracts detected in network subgraph - enabling hybrid migration mode
Horizon contracts detected - using Horizon migration mode: V2 receipts only, but processing existing V1 receipts
```

## Step 7: Run V1 Integration Tests

**Test Legacy TAP (V1) functionality**:

```bash
just test-local
```

**Expected behavior**:

- Funds V1 escrow accounts
- Sends multiple batches of V1 receipts
- Waits for timestamp buffer period
- Sends trigger receipts until RAV generation is detected
- **Success criteria**: Either RAVs created increases OR unaggregated fees decrease significantly

## Step 8: Run V2 Integration Tests

**Test Horizon TAP (V2) functionality**:

```bash
just test-local-v2
```

**Expected behavior**:

- Funds both V1 and V2 escrow accounts
- Sends V2 receipts with collection IDs
- Monitors metrics for RAV generation
- **Success criteria**: V2 receipts are accepted (no "402 Payment Required" errors)

## Step 9: Run Load Tests

**Test system performance with high receipt volume**:

```bash
just load-test-v2 1000
```

This sends 1000 V2 receipts to test system performance.

## Step 10: Observe and Debug

### Check Network Subgraph

**Verify PaymentsEscrow accounts are indexed**:

```bash
curl -s -X POST http://localhost:8000/subgraphs/name/graph-network \
  -H "Content-Type: application/json" \
  -d '{"query": "{ paymentsEscrowAccounts(first: 5) { id payer receiver balance } }"}' | jq
```

**Expected output**:

```json
{
  "data": {
    "paymentsEscrowAccounts": [
      {
        "id": "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266f39fd6e51aad88f6f4ce6ab8827279cfffb92266f4ef6650e48d099a4972ea5b414dab86e1998bd3",
        "payer": "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266",
        "receiver": "0xf4EF6650E48d099a4972ea5B414daB86e1998Bd3",
        "balance": "10000000000000000000"
      }
    ]
  }
}
```

### Check Service Logs

**Monitor indexer-service logs**:

```bash
docker logs indexer-service -f
```

**Monitor tap-agent logs**:

```bash
docker logs tap-agent -f
```

### Check Metrics

**Check TAP agent metrics**:

```bash
curl -s http://localhost:7300/metrics | grep -E "(tap_ravs_created|tap_unaggregated_fees)"
```

## Step 11: Development Workflow

### Hot Reloading

**Rebuild and restart services after code changes**:

```bash
just reload
```

### View All Service Logs

**Monitor all service logs**:

```bash
just logs
```

### Stop All Services

**Clean shutdown**:

```bash
just down
```

## Troubleshooting

### Common Issues

1. **"402 Payment Required" errors**:
   - Verify escrow accounts are funded
   - Check that Horizon detection is working
   - Ensure network subgraph has PaymentsEscrow accounts

2. **Service startup failures**:
   - Run `just down` to clean state
   - Check Docker daemon is running
   - Verify port availability (7300, 7601, 7700, 8000, 8545)

3. **Network subgraph issues**:
   - Check graph-node logs: `docker logs graph-node`
   - Verify contracts are deployed: `docker logs graph-contracts`

4. **Test timeouts**:
   - Services may take time to start
   - RAV generation depends on receipt volume and timing
   - Check service health with `docker ps`

### Debug Commands

**Check contract deployment**:

```bash
# Check if contracts have code
docker exec chain cast code 0xE6E340D132b5f46d1e472DebcD681B2aBc16e57E --rpc-url http://localhost:8545 | wc -c
```

**Check blockchain state**:

```bash
# Get current block number
docker exec chain cast block-number --rpc-url http://localhost:8545
```

**Check database state**:

```bash
# Check for TAP receipts
docker exec postgres psql -U postgres -d indexer_components_1 -c "SELECT COUNT(*) FROM scalar_tap_receipts;"
```

## Test Success Criteria

### V1 Tests

- ✅ Receipts are accepted without errors
- ✅ RAVs are generated (metrics show increase)
- ✅ Unaggregated fees decrease after RAV generation

### V2 Tests

- ✅ V2 receipts are accepted without "402 Payment Required" errors
- ✅ System runs in Horizon hybrid migration mode
- ✅ Collection IDs are properly handled

### Load Tests

- ✅ System handles high receipt volume without errors
- ✅ Memory usage remains stable
- ✅ No significant performance degradation

## Key Configuration Files

- `contrib/indexer-service/config.toml`: Indexer service configuration
- `contrib/tap-agent/config.toml`: TAP agent configuration
- `contrib/local-network/.env`: Environment variables and addresses
- `contrib/local-network/horizon.json`: Horizon contract addresses
- `contrib/local-network/tap-contracts.json`: TAP contract addresses

## Next Steps

After successful testing, consider:

1. **Run comprehensive test suite**: `just ci` (includes format, lint, test, sqlx-prepare)
2. **Explore refactoring opportunities**: Review `INTEGRATION_TESTING_IMPROVEMENTS.md`
3. **Contribute improvements**: Follow the refactoring roadmap for better testing infrastructure

This testing infrastructure provides a solid foundation for developing and testing both V1 and V2 TAP functionality in indexer-rs.