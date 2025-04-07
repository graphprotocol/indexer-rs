#!/bin/bash
set -e

# Print what we're doing
echo "Rebuilding Rust binaries and restarting containers..."

# Define color codes for output
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# 1. Compile the binaries locally
echo -e "${BLUE}Compiling Rust code...${NC}"
cargo build --release

# 2. Check if compilation succeeded
if [ $? -ne 0 ]; then
    echo -e "${YELLOW}Compilation failed. Not restarting containers.${NC}"
    exit 1
fi

echo -e "${GREEN}Compilation successful!${NC}"

# 3. Restart the containers with the newly compiled binaries
echo -e "${BLUE}Restarting containers...${NC}"
cd contrib
docker compose -f docker-compose.dev.yml restart indexer-service tap-agent

# 4. Verify the containers are running
echo -e "${BLUE}Checking container status...${NC}"
docker compose -f docker-compose.dev.yml ps

echo -e "${GREEN}Done! Containers restarted with new binaries.${NC}"

# Optional: Check logs for immediate errors
echo -e "${BLUE}Showing recent logs (Ctrl+C to exit):${NC}"
docker compose -f docker-compose.dev.yml logs --tail=20 -f
