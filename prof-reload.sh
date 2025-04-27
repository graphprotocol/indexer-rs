#!/bin/bash
set -e

# TODO: Might this file is redundant and we can use dev-reload
# script instead?

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

PROFILER=${1:-flamegraph}
echo -e "${BLUE}Using profiler: ${GREEN}${PROFILER}${NC}"

# Ensure profiling output directory exists
mkdir -p contrib/profiling/indexer-service
mkdir -p contrib/profiling/tap-agent

# 1. Compile the binaries locally
# and use profiling feature flag
# for flamegraph
echo -e "${BLUE}Compiling Rust code...${NC}"
RUSTFLAGS='-C force-frame-pointers=yes' CARGO_PROFILE_RELEASE_DEBUG=true cargo build --release --features "profiling"

# 2. Check if compilation succeeded
if [ $? -ne 0 ]; then
    echo -e "${YELLOW}Compilation failed. Not restarting containers.${NC}"
    exit 1
fi

echo -e "${GREEN}Compilation successful!${NC}"

echo -e "${BLUE}Restarting indexer-service with profiling...${NC}"
cd contrib

# Stop the existing service and remove container
# to avoid conflicts. probably not needed and a restart could
# be enough.
docker compose -f docker-compose.prof.yml stop indexer-service tap-agent
docker rm -f indexer-service tap-agent 2>/dev/null || true

export PROFILER=$PROFILER
docker compose -f docker-compose.prof.yml up -d indexer-service tap-agent

echo -e "${GREEN}Done! Containers restarted with profiling.${NC}"
