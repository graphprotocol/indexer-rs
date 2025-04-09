#!/bin/sh
set -x
set -eo pipefail

# Use Podman if Docker is unavailable
if command -v docker &> /dev/null; then
    CONTAINER_RUNTIME="docker"
else
    CONTAINER_RUNTIME="podman"
fi

$CONTAINER_RUNTIME run \
  --env POSTGRES_HOST_AUTH_METHOD=trust \
  --publish "127.0.0.1:5432":5432 \
  --detach \
  --name postgres_container \
  docker.io/library/postgres

# Check if .env file exists, if not create it with the DATABASE_URL
ENV_FILE=".env"
DATABASE_URL="postgres://postgres@127.0.0.1:5432/postgres"

if [ ! -f "$ENV_FILE" ]; then
  echo "Creating $ENV_FILE"
  echo "DATABASE_URL=$DATABASE_URL" > "$ENV_FILE"
else
  if grep -q "^DATABASE_URL=" "$ENV_FILE"; then
    echo "WARNING: Updating DATABASE_URL in $ENV_FILE"
    echo "Backing up .env to .env.bak ..."
    sed -i.bak "s|^DATABASE_URL=.*|DATABASE_URL=$DATABASE_URL|" "$ENV_FILE"
  else
    echo "Adding DATABASE_URL to $ENV_FILE"
    echo "DATABASE_URL=$DATABASE_URL" >> "$ENV_FILE"
  fi
fi

sleep 1
sqlx migrate run
