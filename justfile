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


# Development workflow commands
# -----------------------------

# Full setup for testing 
# using a local network
setup:
    ./setup-test-network.sh

# Rebuild binaries and restart services after code changes
reload:
    ./dev-reload.sh

# Watch log output from services
logs:
    @cd contrib && docker compose -f docker-compose.dev.yml logs -f

# Stop all services
down:
    @cd contrib && docker compose -f docker-compose.dev.yml down
    @cd contrib/local-network && docker compose down
    docker rm -f indexer-service tap-agent gateway block-oracle indexer-agent graph-node redpanda tap-aggregator tap-escrow-manager 2>/dev/null || true


# Profiling commands
# -----------------------------

# Profile indexer-service with flamegraph
profile-flamegraph:
    @mkdir -p contrib/profiling/output
    ./prof-reload.sh flamegraph

# Profile indexer-service with valgrind
profile-valgrind:
    @mkdir -p contrib/profiling/output
    ./prof-reload.sh valgrind

# Profile indexer-service with strace
profile-strace:
    @mkdir -p contrib/profiling/output
    ./prof-reload.sh strace

profile-callgrind:
    @mkdir -p contrib/profiling/output
    ./prof-reload.sh callgrind

# Stop the running indexer-service (useful after profiling)
# This sends SIGTERM, allowing the trap in start-perf.sh to handle cleanup (e.g., generate flamegraph)
stop-profiling:  # <-- New Rule Added Here
    @echo "🛑 Stopping the indexer-service container (allowing profiling data generation)..."
    cd contrib && docker compose -f docker-compose.prof.yml stop indexer-service tap-agent
    @echo "✅ Service stop signal sent. Check profiling output directory."

# Restore normal service (without profiling)
profile-restore:
    @echo "🔄 Restoring normal service..."
    cd contrib && docker compose -f docker-compose.prof.yml up -d --force-recreate indexer-service tap-agent
    @echo "✅ Normal service restored"


test-local:
    @cd integration-tests && ./fund_escrow.sh
    @cd integration-tests && cargo run -- rav1

test-local-v2:
    @cd integration-tests && ./fund_escrow.sh
    @cd integration-tests && cargo run -- rav2
