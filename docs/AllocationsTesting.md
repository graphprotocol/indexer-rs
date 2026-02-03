# Allocation Close Testing

This document explains how allocations are closed in the protocol and how to close them manually/programmatically in the local test network.

## How Allocations Are Closed (Normal Flow)

Allocations are closed by the indexer operator. In production, this is typically done by the indexer agent after it decides to close a specific allocation and includes a POI (proof of indexing) for the subgraph deployment.

Closing an allocation:
- Requires the allocation to be **open**.
- Requires **operator authority** for the allocation (the allocation ID is derived from the operator key).
- Typically includes a **POI** (proof of indexing). For local testing, a zero POI can be used with `--force`.

## Manual Close (Indexer CLI)

The indexer CLI (running in the `indexer-cli` container) can close allocations by calling the indexer-agent API:

```bash
# List allocations
 docker exec indexer-cli graph indexer allocations get --network hardhat

# Close allocation with zero POI (testing)
 docker exec indexer-cli graph indexer allocations close \
  0x<allocation-id> \
  0x0000000000000000000000000000000000000000000000000000000000000000 \
  --network hardhat \
  --force
```

## Programmatic Close for Tests

A helper script is provided for tests and automation:

```bash
# Close a specific allocation
 ./contrib/indexer-cli/close-allocations.sh 0x<allocation-id>

# Close all allocations (requires jq)
 ./contrib/indexer-cli/close-allocations.sh --all
```

Environment variables:
- `CONTAINER_NAME` (default: `indexer-cli`)
- `NETWORK` (default: `hardhat`)
- `NETWORK_SUBGRAPH_URL` (default: `http://localhost:8000/subgraphs/name/graph-network`)
- `POI` (default: zero POI)
- `BLOCK_NUMBER` (optional, Horizon/V2; auto-detected from network subgraph if unset)
- `PUBLIC_POI` (optional, Horizon/V2)
- `FORCE_FLAG` (default: `--force`)

## Multi-Mnemonic and Trusted Senders (Testing)

To simulate multiple operators or senders in local-network:

1) **Multiple operator mnemonics**

Set `INDEXER_MNEMONICS` to a comma-separated list of additional mnemonics. This will populate `operator_mnemonics` in the generated config for both `indexer-service` and `tap-agent`:

```bash
export INDEXER_MNEMONICS="mnemonic one..., mnemonic two..."
```

2) **Trusted senders**

Set `TRUSTED_SENDERS` to a comma-separated list of sender addresses. These senders can spend beyond escrow up to `max_amount_willing_to_lose_grt`:

```bash
export TRUSTED_SENDERS="0xabc...,0xdef..."
```

Note: Allocation creation is still driven by the indexer-agent and its operator key. Multiple mnemonics are used for attestation/signing and validation across allocations created with different operator keys.

## Constraints to Keep in Mind

- Allocations must be **open** before closing.
- The operator key used to create an allocation must be available for signing the close.
- Closing requires a POI; zero POI with `--force` is only suitable for testing.
- After closing, you may need to wait for the network subgraph to index the close before dependent flows (RAV redemption, etc.) proceed.
