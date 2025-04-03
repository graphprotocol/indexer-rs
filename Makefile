.PHONY: setup setup-dev reload logs down fmt help rav_tests

# Default target shows help
help:
	@echo "Available commands:"
	@echo "  make setup         - Full setup for testing (builds and starts all services)"
	@echo "  make reload        - Rebuild binaries and restart services after code changes"
	@echo "  make logs          - Watch log output from services"
	@echo "  make down          - Stop all services and remove containers"
	@echo "  make fmt           - Run cargo fmt"
	@echo "  make rav_tests     - Run end to end tests"
	@echo ""
	@echo "Development workflow:"
	@echo "  1. Run 'make setup' to initialize everything"
	@echo "  2. After code changes, run 'make reload' to rebuild and restart"
	@echo "  3. Use 'make logs' to monitor service logs"
	@echo "  4. Run 'make down' to stop all services when done"

# Full setup for testing
# this includes building indexer-service and tap-agent
# containers and binaries
setup:
	./setup-test-network.sh

# Rebuild binaries and restart services after code changes
# This is useful for development as a complete container rebuild is not necessary
reload:
	./dev-reload.sh

# Watch log output from services
logs:
	cd contrib && docker compose -f docker-compose.dev.yml logs -f

# Stop all services
down:
	cd contrib && docker compose -f docker-compose.dev.yml down || true
	cd contrib/local-network && docker compose down || true
	docker rm -f indexer-service tap-agent gateway block-oracle indexer-agent graph-node redpanda tap-aggregator tap-escrow-manager 2>/dev/null || true


# Cargo commands
fmt:
	cargo fmt

rav_tests:
	# First fund the escrow account
	cd rav_e2e && ./fund_escrow.sh 
	cd rav_e2e && cargo run
