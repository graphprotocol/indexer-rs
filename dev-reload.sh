#!/bin/bash
set -e

# Define color codes for output
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Check if profiling is requested
PROFILER=${1:-""}
if [ -n "$PROFILER" ]; then
    echo "Rebuilding Rust binaries with profiling and restarting dev containers..."
    echo -e "${BLUE}Using profiler: ${GREEN}${PROFILER}${NC}"
    # Ensure profiling output directory exists
    mkdir -p contrib/profiling/indexer-service
    mkdir -p contrib/profiling/tap-agent
else
    echo "Rebuilding Rust binaries and restarting dev containers..."
fi

echo -e "${BLUE}Using development profile with host binary mounts${NC}"

# Always use docker-compose.dev.yml since this is dev-reload.sh
COMPOSE_FILE="-f docker-compose.dev.yml"
SERVICES="indexer-service tap-agent"

# 1. Compile the binaries locally with appropriate flags
if [ -n "$PROFILER" ]; then
    echo -e "${BLUE}Compiling Rust code with profiling support...${NC}"
    RUSTFLAGS='-C force-frame-pointers=yes' CARGO_PROFILE_RELEASE_DEBUG=true cargo build --release --features "profiling"
else
    echo -e "${BLUE}Compiling Rust code...${NC}"
    cargo build --release
fi

# 2. Check if compilation succeeded
if [ $? -ne 0 ]; then
    echo -e "${YELLOW}Compilation failed. Not restarting containers.${NC}"
    exit 1
fi

echo -e "${GREEN}Compilation successful!${NC}"

# 3. Stop and remove any conflicting containers
echo -e "${BLUE}Cleaning up conflicting containers...${NC}"
cd contrib
# Force stop and remove all indexer containers to avoid port conflicts
# Stop containers from both production and dev compose files
docker compose -f docker-compose.yml stop indexer-service tap-agent 2>/dev/null || true
docker compose -f docker-compose.dev.yml stop indexer-service tap-agent 2>/dev/null || true
docker rm indexer-service tap-agent 2>/dev/null || true

# 4. Start only the dev containers 
echo -e "${BLUE}Starting dev containers...${NC}"

# Set profiler environment variable if profiling
if [ -n "$PROFILER" ]; then
    PROFILER=$PROFILER docker compose $COMPOSE_FILE up -d --force-recreate $SERVICES
    echo -e "${GREEN}Done! Containers restarted with profiling.${NC}"
else
    docker compose $COMPOSE_FILE up -d --force-recreate $SERVICES
    echo -e "${GREEN}Done! Development containers restarted.${NC}"
fi

# 5. Verify the containers are running
echo -e "${BLUE}Checking container status...${NC}"
docker compose $COMPOSE_FILE ps

# Optional: Check logs for immediate errors
echo -e "${BLUE}Showing recent logs (Ctrl+C to exit):${NC}"
docker compose $COMPOSE_FILE logs --tail=20 -f $SERVICES
