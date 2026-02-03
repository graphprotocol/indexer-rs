#!/usr/bin/env bash
set -Eeuo pipefail

CONTAINER_NAME="${CONTAINER_NAME:-indexer-cli}"
NETWORK="${NETWORK:-hardhat}"
POI="${POI:-0x0000000000000000000000000000000000000000000000000000000000000000}"
BLOCK_NUMBER="${BLOCK_NUMBER:-}"
PUBLIC_POI="${PUBLIC_POI:-}"
FORCE_FLAG="${FORCE_FLAG:---force}"

usage() {
  cat <<USAGE
Usage:
  close-allocations.sh <allocation-id> [<allocation-id> ...]
  close-allocations.sh --all

Env:
  CONTAINER_NAME   indexer-cli container name (default: indexer-cli)
  NETWORK          network name (default: hardhat)
  POI              POI to submit (default: 0x00..00)
  BLOCK_NUMBER     Block number for Horizon close (optional, required if POI provided)
  PUBLIC_POI       Public POI for Horizon close (optional, required if POI provided)
  FORCE_FLAG       --force or empty (default: --force)
USAGE
}

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
  usage
  exit 0
fi

allocation_ids=()
if [ "${1:-}" = "--all" ]; then
  if ! command -v jq >/dev/null 2>&1; then
    echo "jq is required to close --all allocations. Install jq or pass allocation IDs explicitly." >&2
    exit 1
  fi

  allocations_json="$(docker exec "$CONTAINER_NAME" graph indexer allocations get --network "$NETWORK" --output json 2>/dev/null || true)"
  if [ -z "$allocations_json" ]; then
    allocations_json="$(docker exec "$CONTAINER_NAME" graph indexer allocations get --network "$NETWORK" --json 2>/dev/null || true)"
  fi
  if [ -z "$allocations_json" ]; then
    echo "Failed to fetch allocations JSON. Provide allocation IDs explicitly." >&2
    exit 1
  fi

  mapfile -t allocation_ids < <(printf '%s' "$allocations_json" | jq -r '..|.id? // empty' | sort -u)
else
  allocation_ids=("$@")
fi

if [ "${#allocation_ids[@]}" -eq 0 ]; then
  usage
  exit 1
fi

if [ -z "${BLOCK_NUMBER}" ]; then
  if command -v curl >/dev/null 2>&1 && command -v jq >/dev/null 2>&1; then
    block_json="$(curl -s http://localhost:8000/subgraphs/name/graph-network -H 'content-type: application/json' -d '{\"query\":\"{ _meta { block { number } } }\" }' || true)"
    BLOCK_NUMBER="$(printf '%s' "$block_json" | jq -r '.data._meta.block.number // empty' 2>/dev/null || true)"
    if [ -n "$BLOCK_NUMBER" ]; then
      echo "Detected network subgraph block number: $BLOCK_NUMBER"
    fi
  fi
fi

for allocation_id in "${allocation_ids[@]}"; do
  echo "Closing allocation $allocation_id (network=$NETWORK)"
  docker exec "$CONTAINER_NAME" graph indexer allocations close \
    "$allocation_id" \
    "$POI" \
    ${BLOCK_NUMBER:+$BLOCK_NUMBER} \
    ${PUBLIC_POI:+$PUBLIC_POI} \
    --network "$NETWORK" \
    $FORCE_FLAG
  echo ""
done
