FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    openssl ca-certificates protobuf-compiler postgresql-client curl \
    jq \
    && rm -rf /var/lib/apt/lists/*
