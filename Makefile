.PHONY: setup setup-dev reload logs down fmt

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
	cd contrib && docker compose -f docker-compose.dev.yml down

# Cargo commands
fmt:
	cargo fmt

test-local:
	cd tests && ./run-test-local.sh
