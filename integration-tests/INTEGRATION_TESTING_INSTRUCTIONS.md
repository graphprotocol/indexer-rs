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

## Step 10: Run Enhanced Integration Tests (New Infrastructure)

**Test the new structured testing infrastructure**:

```bash
cd integration-tests
cargo run -- test-with-context
```

**What this tests**:
- V2 receipt processing with detailed error diagnostics
- Insufficient escrow scenario handling
- Batch processing with real-time monitoring
- Automatic test isolation and cleanup

**Expected behavior**:
- Each test runs with a unique ID for isolation
- Detailed progress logging with structured error messages
- Automatic resource cleanup even if tests fail
- Better diagnostics for debugging issues

**Benefits over existing tests**:
- Structured error types provide specific failure contexts
- Test isolation prevents interference between tests
- Reusable utilities reduce code duplication
- Enhanced observability into test execution

See `TESTING_INFRASTRUCTURE_IMPROVEMENT.md` for detailed documentation on the new testing capabilities.

## Step 11: Observe and Debug

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

## Step 12: Development Workflow

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

5. **TAP Agent panic on receipt processing** ✅ **FULLY RESOLVED**:
   - **Symptom**: Receipts are stored but no unaggregated fees accumulate, system crashes
   - **Root Cause**: Collection ID parsing errors with `.expect()` causing panics in notification handler + database padding from `character(64)` fields
   - **Fix Applied**:
     - Added error handling to prevent panics on malformed collection IDs
     - Added `.trim()` to handle padded collection IDs from fixed-length database fields
     - Added debug logging for notification processing
   - **Status**: ✅ **COMPLETELY FIXED** - system processes receipts and accumulates fees correctly
   - **Verification**:
     - ✅ Logs show successful notification processing: `Successfully handled notification`
     - ✅ Metrics show accumulated fees: `tap_unaggregated_fees_grt_total` > 0
     - ✅ Load tests process 50+ receipts without errors
     - ✅ No panics or crashes during continuous operation

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
docker exec postgres psql -U postgres -d indexer_components_1 -c "SELECT COUNT(*) FROM tap_horizon_receipts;"
```

**Service restart after issues**:

If you encounter notification processing issues or panics, restart the services:

```bash
# Restart both indexer-service and tap-agent
cd contrib && docker compose -f docker-compose.dev.yml restart indexer-service tap-agent

# Or rebuild and restart (if code changes were made)
just reload
```

**Verify notification listeners**:

```bash
# Check that both notification channels are being listened to
docker logs tap-agent 2>&1 | grep "LISTEN"
# Should show:
# LISTEN "scalar_tap_receipt_notification"      (V1/Legacy)
# LISTEN "tap_horizon_receipt_notification"     (V2/Horizon)
```

**Check receipt processing is working**:

```bash
# Verify receipts are being processed into unaggregated fees (should show non-zero values)
curl -s http://localhost:7300/metrics | grep -E "tap_unaggregated_fees|tap_ravs_created"

# Check database for stored receipts
docker exec postgres psql -U postgres -d indexer_components_1 -c "SELECT COUNT(*), SUM(value) FROM tap_horizon_receipts;"

# Verify successful notification processing (should show "Successfully handled notification")
docker logs tap-agent --tail 20 | grep -E "(Successfully handled notification|Received notification)"

# Verify no recent panics in logs (should report "No recent panics")
docker logs tap-agent --since="5m" 2>&1 | grep -i panic || echo "No recent panics - system healthy"

# Quick load test to verify processing (should show 50 successful receipts)
cd integration-tests && cargo run -- load --num-receipts 50
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
- ✅ Unaggregated fees accumulate correctly (receipts are processed)
- ⚠️ RAV generation may require additional configuration (trigger thresholds, timing)

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
2. **Use the new testing infrastructure**: Try `cargo run -- test-with-context` for enhanced testing capabilities
3. **Explore refactoring opportunities**: Review `TESTING_INFRASTRUCTURE_IMPROVEMENT.md`
4. **Contribute improvements**: Follow the refactoring roadmap for better testing infrastructure

This testing infrastructure provides a solid foundation for developing and testing both V1 and V2 TAP functionality in indexer-rs.

## TAP Receipt Processing Investigation Guide

If you encounter issues where receipts are stored but not processed (unaggregated fees remain 0), follow this systematic debugging approach:

### Step 1: Check System Health

```bash
# Look for panics that crash the notification system
docker logs tap-agent 2>&1 | grep -E "(panic|PANIC)" | tail -5

# Verify receipt watchers are running
docker logs tap-agent | grep "receipts watcher started" | tail -2
```

### Step 2: Test Notification System

```bash
# Send test notification to verify the system receives them
docker exec postgres psql -U postgres -d indexer_components_1 -c "NOTIFY tap_horizon_receipt_notification, '{\"id\": 1, \"collection_id\": \"5819cd0eb33a614e8426cf3bceaced9d47e178c8\", \"signer_address\": \"0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266\", \"timestamp_ns\": 1000000000, \"value\": 100}';"

# Check if notification was processed (should see logs)
docker logs tap-agent --tail 20
```

### Step 3: Verify Database Triggers

```bash
# Check trigger function exists
docker exec postgres psql -U postgres -d indexer_components_1 -c "SELECT pg_get_functiondef(oid) FROM pg_proc WHERE proname = 'tap_horizon_receipt_notify';"

# Verify trigger is attached
docker exec postgres psql -U postgres -d indexer_components_1 -c "SELECT tgname, relname FROM pg_trigger JOIN pg_class ON tgrelid = pg_class.oid WHERE tgname LIKE '%notify%';"
```

### Step 4: Clear Problematic Data (if needed)

```bash
# If malformed data causes issues, clear and restart
docker exec postgres psql -U postgres -d indexer_components_1 -c "TRUNCATE tap_horizon_receipts CASCADE;"
docker exec postgres psql -U postgres -d indexer_components_1 -c "TRUNCATE scalar_tap_receipts CASCADE;"
just reload
```

### Expected Behavior

- ✅ Receipt watchers start without panics
- ✅ Test notifications are received and logged
- ✅ Database triggers send properly formatted notifications
- ✅ Receipts are processed into unaggregated fees
- ✅ System runs continuously without crashes

### Key Files for TAP Processing

- `crates/tap-agent/src/agent/sender_accounts_manager.rs`: Notification handling
- Database triggers: `tap_horizon_receipt_notify()` and `scalar_tap_receipt_notify()`
- Metrics endpoint: <http://localhost:7300/metrics> The new enhanced testing infrastructure adds structured error handling, test isolation, and better observability for more reliable and debuggable integration tests.