# TAP Receipt Aggregate Voucher (RAV) Testing Plan

## Overview

This plan outlines how to test the complete Timeline Aggregation Protocol (TAP) flow, focusing specifically on verifying that Receipt Aggregate Vouchers (RAVs) are properly generated when multiple receipts are collected.

## Components Involved

- Gateway - Sends receipts with queries
- Indexer Service - Processes queries and collects receipts
- TAP Agent - Manages receipts and requests RAVs
- Gateway Receipt Aggregator(tap-aggregator) - Aggregates receipts into RAVs
- Indexer Agent - Handles redemption of RAVs(Not used directly in this test)

## Prerequisites

- All containers are running (indexer-service, tap-agent, database, graph-node, tap-aggregator, indexer-agent, gateway, graph-node and database)
- Contracts deployed (graph-contracts and tap contracts)
- Gateway funded with GRT and aware of indexer address(use fund_escrow.sh script)
- An active allocation exists (or a mock allocation is used)

## Understanding RAV Trigger Mechanism

RAV requests are triggered based on a value threshold, not just the number of receipts. The key formula is:

```
trigger_value = max_amount_willing_to_lose_grt / trigger_value_divisor
```

With the following typical configuration values:

- `max_amount_willing_to_lose_grt` = 20 GRT
- `trigger_value_divisor` = 10
- Resulting `trigger_value` = 2 GRT

When the total accumulated receipt value outside the buffer period exceeds this `trigger_value` (2 GRT in this example), a RAV request is triggered. This is defined in the [condition](https://github.com/graphprotocol/indexer-rs/blob/main/crates/tap-agent/src/agent/sender_account.rs#L1114-L1123):

```rust
if !state.backoff_info.in_backoff() && total_fee_outside_buffer >= state.config.trigger_value {
    // Trigger RAV request
}
```

### Critical Role of Timestamp Buffer

The `timestamp_buffer_secs` configuration is critically important to RAV generation:

- Only receipts that are older than `now + timestamp_buffer_secs` are counted towards the `total_fee_outside_buffer`
- This means if `timestamp_buffer_secs = 1000`, receipts must be at least 1000 seconds (16.7 minutes) old before they'll contribute to triggering a RAV
- If you wait less than `timestamp_buffer_secs` between sending receipts and checking for RAVs, the test will fail even if you've sent enough value

Common issues caused by `timestamp_buffer_secs`:

- A high value (e.g., 1000 seconds) makes testing impractical without very long delays
- Sending many receipts doesn't help if they're all within the buffer period
- Metrics may show receipts are received (in `unaggregated_fees`) but not counted toward triggering (in `total_fee_outside_buffer`)

## Test Plan

### Step 1: Check and Configure Timestamp Buffer

1. First, check the current `timestamp_buffer_secs` setting:

   ```bash
   # Find the timestamp_buffer_secs setting
   docker exec tap-agent grep -r "timestamp_buffer_secs" /opt/config.toml
   ```

2. For testing purposes, consider temporarily updating this value to something more practical (15-60 seconds)

   ```bash
   # Example command to modify config (adjust based on your setup)
   docker exec -it tap-agent sed -i 's/timestamp_buffer_secs = 1000/timestamp_buffer_secs = 60/' /opt/config.toml
   docker restart tap-agent
   ```

3. Verify the change took effect:

   ```bash
   docker exec tap-agent grep -r "timestamp_buffer_secs" /opt/config.toml
   ```

### Step 2: Send Multiple Queries with Signed Receipts

1. Send enough queries to exceed the trigger value threshold
2. With max_receipt_value_grt of 0.001 GRT and a trigger_value of 0.002 GRT, this would mean sending at least 3 receipts
3. Each query should include a properly signed TAP receipt
4. Verify each query returns a successful response

**IMPORTANT**: Wait longer than `timestamp_buffer_secs` after sending receipts before checking for RAV generation. For example, if `timestamp_buffer_secs = 60`, wait at least 65 seconds.

```rust
// Example code to send multiple queries with receipts
async fn send_multiple_queries(http_client: &Client, gateway_url: &str, subgraph_id: &str,
                              gateway_api_key: &str, receipt_generator: &ReceiptGenerator,
                              num_queries: usize) -> Result<()> {
    // Use the maximum allowed receipt value
    let value_per_query = 1_000_000_000_000_000u128; // 0.001 GRT (maximum allowed)

    // Send receipts in batches with pauses to exceed the timestamp buffer
    let batch_size = 50;
    let num_batches = (num_queries + batch_size - 1) / batch_size; // Ceiling division

    let mut total_value = 0u128;

    // Get the timestamp buffer from configuration or use a default
    let timestamp_buffer_secs = 60; // ADJUST THIS to match your actual configuration!
    let wait_time = timestamp_buffer_secs + 5; // Wait longer than the timestanp buffer

    for batch in 0..num_batches {
        let start_idx = batch * batch_size;
        let end_idx = std::cmp::min((batch + 1) * batch_size, num_queries);

        for i in start_idx..end_idx {
            let receipt = receipt_generator.generate_receipt(value_per_query)?;
            let receipt_json = serde_json::to_string(&receipt)?;


            let query_response = http_client
                .post(format!("{}/api/subgraphs/id/{}", gateway_url, subgraph_id))
                .header("Content-Type", "application/json")
                .header("Authorization", format!("Bearer {}", gateway_api_key))
                .header("Tap-Receipt", receipt_json)
                .json(&json!({
                    "query": "{ _meta { block { number } } }"
                }))
                .send()
                .await?;

            println!("Query {} response: {}", i+1, query_response.status());

            total_value += value_per_query;

            // Small delay to prevent flooding
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }

        println!("Batch {} complete, total value sent: {} GRT",
                 batch+1,
                 total_value as f64 / 1_000_000_000_000_000f64);

        // Wait longer than timestamp_buffer_secs to ensure receipts are outside buffer
        println!("Waiting {} seconds to exceed the timestamp buffer...", wait_time);
        tokio::time::sleep(tokio::time::Duration::from_secs(wait_time)).await;
    }

    Ok(())
}
```

### Step 3: Monitor TAP Agent Logs for Receipt Collection

1. Check the tap-agent logs to verify receipts are being stored
2. Look for log entries that indicate the receipt has passed validation
3. Look for log entries showing the calculated `total_fee_outside_buffer` value

```bash
# Command to monitor tap-agent logs
docker logs -f tap-agent | grep -i "receipt\|rav\|trigger"
```

### Step 4: Monitor tap-aggregator for RAV Generation

1. Check the tap-aggregator logs for evidence of RAV creation
2. Look for log entries that mention "RAV" or "aggregation"

```bash
# Command to monitor tap-aggregator logs
docker logs -f tap-aggregator | grep -i "rav\|aggregat"
```

### Step 5: Monitor Metrics Endpoints (What we do here)

Both the TAP Agent and Gateway Receipt Aggregator expose Prometheus metrics that can help verify the flow:

1. Check the TAP Agent metrics for receipt and RAV counters
2. Check the Gateway Receipt Aggregator metrics for RAV generation counters

```bash
# Example commands to check metrics
curl http://localhost:7300/metrics | grep -i "tap_unaggregated_fees\|tap_rav"
curl http://localhost:7700/metrics | grep -i "rav"
```

Pay particular attention to metrics showing the actual accumulated value outside the buffer:

- `tap_unaggregated_fees_grt_total` - Shows total unaggregated fees (including those inside buffer)
- `tap_rav_request_trigger_value` - Shows the configured trigger value threshold

## Configuration Tips

### TAP Agent Configuration

The TAP Agent configuration determines when RAVs are requested. The most critical parameters are:

```toml
[service.tap]
# Maximum value allowed in a single receipt (in GRT)
max_receipt_value_grt = "0.001"

[tap]
# Maximum amount willing to lose (in GRT)
max_amount_willing_to_lose_grt = 1000  # Can be 20 or 1000 depending on config

[tap.rav_request]
# Determines the trigger threshold: max_amount_willing_to_lose_grt / trigger_value_divisor
# With max_amount_willing_to_lose_grt=1000, gives 0.002 GRT trigger
trigger_value_divisor = 500_000

# ⚠️ CRITICAL: Buffer period for receipts (in seconds)
# Should NOT be 1000 for testing!
# The higher the value, the higher the number
# of receipts and the longer the wait time
timestamp_buffer_secs = 60

# Timeout for RAV requests (in seconds)
request_timeout_secs = 5

# Maximum receipts to include in a single RAV request
max_receipts_per_request = 10000
```

### Modifying Configuration for Testing

For practical testing purposes, adjust these values:

1. **Ensure the timestamp buffer is reasonable**:

   - Set `timestamp_buffer_secs` to 60 or less
   - The higher this value, the longer you must wait between sending receipts and checking for RAVs

2. **Adjust the trigger threshold** by ensuring:
   - `max_amount_willing_to_lose_grt` is set to a reasonable value (20-1000)
   - `trigger_value_divisor` is set high enough to create a small trigger value
   - Example: with `max_amount_willing_to_lose_grt = 1000` and `trigger_value_divisor = 500_000`, the trigger is 0.002 GRT

Example test-friendly configuration:

```toml
[tap]
max_amount_willing_to_lose_grt = 1000

[tap.rav_request]
trigger_value_divisor = 500_000  # Makes trigger_value = 0.002 GRT
timestamp_buffer_secs = 60       # Receipts must be 60 seconds old to count
```

## Troubleshooting

### Timestamp Buffer Issues

- **Most common issue**: Waiting less time than `timestamp_buffer_secs` after sending receipts
- Check for log entries mentioning `total_fee_outside_buffer` to see what value is actually being compared against the trigger
- Verify your test waits at least `timestamp_buffer_secs + 5` seconds after sending receipts
- Consider temporarily reducing `timestamp_buffer_secs` if your test environment has it set too high (e.g., 1000 seconds)

### Value-Based Trigger Issues

- Calculate your actual trigger threshold using `max_amount_willing_to_lose_grt / trigger_value_divisor`
- Check metrics with `curl http://localhost:7300/metrics | grep tap_rav_request_trigger_value` to confirm
- Ensure you're sending enough total value to exceed this threshold
- Remember that each receipt value is limited by `max_receipt_value_grt` (typically 0.001 GRT)

### Other Common Issues

- Verify the allocation ID is valid and active
- Check for backoff conditions in the logs that might prevent RAV requests
- Verify the Gateway is properly funded with enough GRT
- Check if the Indexer or Gateway are experiencing any errors in the logs

### Monitoring Key Metrics

Monitor these metrics during testing:

- `tap_rav_request_trigger_value` - The calculated trigger threshold
- `tap_unaggregated_fees_grt_total` - Total unaggregated fees (including those in buffer)
- `tap_rav_request_total` - Total number of RAV requests made
