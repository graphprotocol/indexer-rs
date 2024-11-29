# Metrics

## Indexer Service Metrics

### Main handler

| Metric Name                                 | Description                                                                                 | Labels                                      |
|---------------------------------------------|---------------------------------------------------------------------------------------------|---------------------------------------------|
| `indexer_query_handler_seconds_bucket`      | Histogram buckets for the duration of requests handled by the main query handler, in seconds. | deployment, allocation, sender, status_code |
| `indexer_query_handler_seconds_count`       | Total number of requests handled by the main query handler.                                  | deployment, allocation, sender, status_code |
| `indexer_query_handler_seconds_sum`         | Total duration of all requests handled by the main query handler, in seconds.               | deployment, allocation, sender, status_code |

### TAP related

| Metric Name                                 | Description                                                                                 | Labels                                      |
|---------------------------------------------|---------------------------------------------------------------------------------------------|---------------------------------------------|
| `indexer_receipt_failed_total`              | Total number of receipts that failed TAP validation.                                         | deployment, allocation, sender              |
| `indexer_tap_invalid_total`                 | Total number of malformed TAP receipts detected in headers.                                 | -                                           |

### Cost model

| Metric Name                                 | Description                                                                                 | Labels          |
|---------------------------------------------|---------------------------------------------------------------------------------------------|-----------------|
| `indexer_cost_model_seconds_bucket`         | Histogram buckets for the duration of individual `cost_model` queries, in seconds.          | deployment      |
| `indexer_cost_model_seconds_count`          | Total number of `cost_model` queries processed.                                             | deployment      |
| `indexer_cost_model_seconds_sum`            | Total duration of all `cost_model` queries processed, in seconds.                           | deployment      |
| `indexer_cost_model_failed_total`           | Total number of failed `cost_model` queries.                                                | deployment      |
| `indexer_cost_model_invalid_total`          | Total number of `cost_model` queries with invalid deployment IDs.                           | -               |
| `indexer_cost_model_batch_seconds_bucket`   | Histogram buckets for the duration of batch `cost_model` queries, in seconds.               | -               |
| `indexer_cost_model_batch_seconds_count`    | Total number of batch `cost_model` queries processed.                                       | -               |
| `indexer_cost_model_batch_seconds_sum`      | Total duration of all batch `cost_model` queries processed, in seconds.                     | -               |
| `indexer_cost_model_batch_size`             | Gauge of the size of deployment ID batches in `cost_model` queries.                         | -               |
| `indexer_cost_model_batch_failed_total`     | Total number of failed batch `cost_model` queries.                                          | -               |
| `indexer_cost_model_batch_invalid_total`    | Total number of batch `cost_model` queries with invalid deployment IDs.                     | -               |

---

## Tap Agent Metrics

### Metrics related to sender

| Metric Name                                 | Description                                                                                 | Labels          |
|---------------------------------------------|---------------------------------------------------------------------------------------------|-----------------|
| `tap_sender_denied`                         | Indicates whether a sender is denied (0: not denied, 1: denied).                            | sender          |
| `tap_sender_escrow_balance_grt_total`       | Total balance in GRT held in escrow for a sender.                                           | sender          |
| `tap_sender_fee_tracker_grt_total`          | Total pending fees in GRT for a sender (unaggregated receipts).                             | sender          |
| `tap_max_fee_per_sender_grt_total`          | Maximum amount in GRT a sender is willing to lose.                                          | sender          |
| `tap_rav_request_trigger_value`             | Trigger value calculated as the maximum amount willing to lose divided by a divisor.        | sender          |
| `tap_rav_response_time_seconds_bucket`      | Histogram buckets for the response time of RAV requests, in seconds.                        | sender          |
| `tap_rav_response_time_seconds_count`       | Total number of RAV requests processed.                                                    | sender          |
| `tap_rav_response_time_seconds_sum`         | Total response time for all RAV requests, in seconds.                                       | sender          |
| `tap_closed_sender_allocation_total`        | Total number of allocations closed for a sender.                                            | sender          |

### Metrics related to specific allocations for a sender

| Metric Name                                 | Description                                                                                 | Labels                 |
|---------------------------------------------|---------------------------------------------------------------------------------------------|------------------------|
| `tap_invalid_receipt_fees_grt_total`        | Total value of invalid receipt fees in GRT for each sender-allocation pair.                 | sender, allocation     |
| `tap_pending_rav_grt_total`                 | Total value of pending RAVs (not redeemed) in GRT for each sender-allocation pair.          | sender, allocation     |
| `tap_unaggregated_fees_grt_total`           | Total value of unaggregated fees in GRT for each sender-allocation pair.                    | sender, allocation     |
| `tap_ravs_created_total`                    | Total number of RAV requests created for each sender-allocation pair.                       | sender, allocation     |
| `tap_ravs_failed_total`                     | Total number of RAV requests that failed for each sender-allocation pair.                   | sender, allocation     |
| `tap_receipts_received_total`               | Total number of receipts received for each sender-allocation pair.                          | sender, allocation     |
