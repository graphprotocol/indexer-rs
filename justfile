# Set DATABASE_URL:
# export DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432
#
#  # print the help
help:
    just -l
deps:
    cargo install sqlx-cli --no-default-features --features native-tls,postgres
url:
    @echo export DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432
clippy:
    cargo +nightly clippy --all-targets --all-features
#  run everything that is needed for ci to pass
ci:
  just fmt
  just clippy
  just test
  just sqlx-prepare
test:
    RUST_LOG=debug cargo nextest run
review:
    RUST_LOG=debug cargo insta review
fmt:
    cargo +nightly fmt
sqlx-prepare:
    cargo sqlx prepare --workspace  -- --all-targets --all-features
psql-up: 
    @docker run -d --name indexer-rs-psql -p 5432:5432 -e POSTGRES_PASSWORD=postgres postgres
    @sleep 5
    @just migrate
     
psql-down:
    docker stop indexer-rs-psql
    docker rm indexer-rs-psql
migrate:
     sqlx migrate run --database-url postgresql://postgres:postgres@127.0.0.1:5432

# Creates symlink to contrib/local-network/.env file
# Required for scripts that need to read contract addresses and network config
# Assumes the local-network submodule is present (run 'just setup' if missing)
setup-integration-env:
    @if [ ! -f integration-tests/.env ]; then \
        ln -sf ../contrib/local-network/.env integration-tests/.env; \
        echo "Symlink to local-network .env created"; \
    else \
        echo "Integration tests .env already exists"; \
    fi

# Development workflow commands
# -----------------------------
# Full setup for testing using a local network
# This deploys all containers, contracts, and funds escrow
setup:
    ./setup-test-network.sh

# Stop all services
down:
    cd contrib && docker compose -f docker-compose.yml down --remove-orphans
    cd contrib && docker compose -f docker-compose.dev.yml down --remove-orphans
    cd contrib/local-network && docker compose down --remove-orphans
    docker rm -f indexer-service tap-agent gateway block-oracle indexer-agent graph-node redpanda tap-aggregator tap-escrow-manager 2>/dev/null || true
    docker network prune -f


# Check status of all project services
services-status:
    @echo "🔍 Checking project services status..."
    @echo ""
    @echo "=== Project Containers ==="
    @docker ps --format 'table {{{{.Names}}}}\t{{{{.Status}}}}\t{{{{.Ports}}}}' | grep -E "(indexer-service|tap-agent|gateway|graph-node|chain|block-oracle|indexer-agent|redpanda|tap-aggregator|tap-escrow-manager)" || echo "No project containers running"
    @echo ""
    @echo "=== Docker Compose Services ==="
    @cd contrib && docker compose -f docker-compose.yml ps 2>/dev/null || echo "Production compose not running"
    @cd contrib && docker compose -f docker-compose.dev.yml ps 2>/dev/null || echo "Dev compose not running"
    @cd contrib/local-network && docker compose ps 2>/dev/null || echo "Local network compose not running"
    @echo ""
    @echo "=== Active Networks ==="
    @docker network ls | grep -E "(contrib|local-network)" || echo "No project networks found"

# Restart production services (uses Docker-built binaries)
# Assumes local network is already running (run 'just setup' if not)
reload: setup-integration-env
    ./reload.sh

# Rebuild binaries and restart development services (compiles and mounts host binaries)
# Assumes local network is already running (run 'just setup' if not)
reload-dev: setup-integration-env
    ./dev-reload.sh

# Watch log output from services (production mode)
# Assumes local network is already running (run 'just setup' if not)
logs:
    @cd contrib && docker compose -f docker-compose.yml logs -f

# Watch log output from services (development mode)
# Assumes local network is already running (run 'just setup' if not)
logs-dev:
    @cd contrib && docker compose -f docker-compose.dev.yml logs -f


# Profiling commands
# -----------------------------
# Profile indexer-service with flamegraph
# Assumes local network is already running (run 'just setup' if not)
profile-flamegraph: setup-integration-env
    @mkdir -p contrib/profiling/output
    ./dev-reload.sh flamegraph

# Profile indexer-service with valgrind
# Assumes local network is already running (run 'just setup' if not)
profile-valgrind: setup-integration-env
    @mkdir -p contrib/profiling/output
    ./dev-reload.sh valgrind

# Profile indexer-service with strace
# Assumes local network is already running (run 'just setup' if not)
profile-strace: setup-integration-env
    @mkdir -p contrib/profiling/output
    ./dev-reload.sh strace

# Profile indexer-service with callgrind
# Assumes local network is already running (run 'just setup' if not)  
profile-callgrind: setup-integration-env
    @mkdir -p contrib/profiling/output
    ./dev-reload.sh callgrind

# Stop the running indexer-service (useful after profiling)
# This sends SIGTERM, allowing the trap in start-perf.sh to handle cleanup (e.g., generate flamegraph)
stop-profiling:
    @echo "🛑 Stopping the indexer-service container (allowing profiling data generation)..."
    cd contrib && docker compose -f docker-compose.dev.yml stop indexer-service tap-agent
    @echo "✅ Service stop signal sent. Check profiling output directory."

# Restore normal service (without profiling)
profile-restore:
    @echo "🔄 Restoring normal service..."
    cd contrib && docker compose -f docker-compose.dev.yml up -d --force-recreate indexer-service tap-agent
    @echo "✅ Normal service restored"

# Integration test commands (assume local network is already running)
# For fresh setup, run 'just setup' first to deploy all infrastructure
# -----------------------------------------------------------------------------
fund-escrow: setup-integration-env
    @cd integration-tests && ./fund_escrow.sh

# Test RAV v1 receipts (legacy TAP) 
# Assumes local network is running - run 'just setup' if services are not available
test-local: setup-integration-env
    @echo "Running RAV v1 integration tests (assumes local network is running)..."
    @cd integration-tests && ./fund_escrow.sh
    @cd integration-tests && cargo run -- rav1

# Test RAV v2 receipts (Horizon TAP)
# Assumes local network is running - run 'just setup' if services are not available  
test-local-v2: setup-integration-env
    @echo "Running RAV v2 integration tests (assumes local network is running)..."
    @cd integration-tests && bash -x ./fund_escrow.sh && cargo run -- rav2

# Load test with v2 receipts
# Assumes local network is running - run 'just setup' if services are not available
load-test-v2 num_receipts="1000": setup-integration-env
    @echo "Running load test with {{num_receipts}} receipts (assumes local network is running)..."
    @cd integration-tests && ./fund_escrow.sh
    @cd integration-tests && cargo run -- load-v2 --num-receipts {{num_receipts}}


