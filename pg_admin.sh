#!/bin/bash

DATABASE_URL=postgresql://postgres@localhost:5432/indexer_components_1

echo "Starting pgAdmin4 in Docker..."
echo "Database URL: $DATABASE_URL"
echo ""

# Get the Docker network name for local-network-semiotic
NETWORK_NAME="local-network-semiotic_default"

# Check if the network exists
if ! docker network ls | grep -q "$NETWORK_NAME"; then
    echo "Warning: Docker network '$NETWORK_NAME' not found."
    echo "Make sure your local-network-semiotic compose stack is running first."
    echo "Run: cd local-network-semiotic && docker-compose up -d postgres"
    echo ""
fi

# Run pgAdmin in Docker, connected to the same network as PostgreSQL
docker run -p 8080:80 \
    --name pgadmin4 \
    -e PGADMIN_DEFAULT_EMAIL=admin@example.com \
    -e PGADMIN_DEFAULT_PASSWORD=admin \
    -d dpage/pgadmin4

echo ""
echo "pgAdmin4 is starting up..."
echo "Once ready, access it at: http://localhost:8080/login"
echo ""
echo "Login credentials:"
echo "  Email: admin@example.com"
echo "  Password: admin"
echo ""
echo "Database connection details:"
echo "  Host name/address: 172.17.0.1"
echo "  Port: 5432"
echo "  Database: postgres (or specific database name)"
echo "  Username: postgres"
echo "  Password: (leave empty - auth method is 'trust')"
echo ""
echo "Note: Use 'postgres' as the host since both containers are in the same Docker network"
echo ""
echo "To stop pgAdmin: docker stop pgadmin4 && docker rm pgadmin4"
