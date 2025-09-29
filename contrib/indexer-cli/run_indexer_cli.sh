#!/usr/bin/env bash
set -Eeuo pipefail

# Configuration
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IMAGE_TAG="${IMAGE_TAG:-indexer-cli:horizon}"
CONTAINER_NAME="${CONTAINER_NAME:-indexer-cli}"
DOCKER_NETWORK="${DOCKER_NETWORK:-local-network_default}"
INDEXER_AGENT_URL="${INDEXER_AGENT_URL:-http://indexer-agent:7600}"

echo "[indexer-cli] Building Docker image $IMAGE_TAG..."
echo "[indexer-cli] This will clone graphprotocol/indexer from GitHub and checkout horizon branch"
docker build -t "$IMAGE_TAG" "$SCRIPT_DIR"

echo "[indexer-cli] Stopping any existing container..."
docker rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true

echo "[indexer-cli] Starting container $CONTAINER_NAME on network $DOCKER_NETWORK..."
docker run -d \
  --name "$CONTAINER_NAME" \
  --network "$DOCKER_NETWORK" \
  "$IMAGE_TAG"

echo "[indexer-cli] Container is running. Waiting for initialization..."
sleep 3

# Connect the CLI to the indexer-agent
echo "[indexer-cli] Connecting CLI to indexer-agent at $INDEXER_AGENT_URL"
docker exec "$CONTAINER_NAME" graph indexer connect "$INDEXER_AGENT_URL"

echo ""
echo "========================================"
echo "Indexer CLI container is ready!"
echo "========================================"
echo ""
echo "Connected to: $INDEXER_AGENT_URL"
echo ""
echo "Usage examples:"
echo ""
echo "1. List allocations:"
echo "   docker exec $CONTAINER_NAME graph indexer allocations get --network hardhat"
echo ""
echo "2. Close an allocation (with POI):"
echo "   docker exec $CONTAINER_NAME graph indexer allocations close 0x<allocation-id> 0x<poi> --network hardhat --force"
echo ""
echo "3. Close allocation (zero POI for testing):"
echo "   docker exec $CONTAINER_NAME graph indexer allocations close 0x0a067bd57ad79716c2133ae414b8f6bb47aaa22d 0x0000000000000000000000000000000000000000000000000000000000000000 --network hardhat --force"
echo ""
echo "To stop the container:"
echo "   docker stop $CONTAINER_NAME && docker rm $CONTAINER_NAME"
echo ""
