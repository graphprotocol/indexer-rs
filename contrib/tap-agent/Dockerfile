FROM rust:1.87-bookworm as build
WORKDIR /root
COPY . .
# Force SQLx to use the offline mode to statically check the database queries against
# the prepared files in the `.sqlx` directory.
ENV SQLX_OFFLINE=true
RUN apt-get update && apt-get install -y --no-install-recommends \
    protobuf-compiler libsasl2-dev && rm -rf /var/lib/apt/lists/*
RUN cargo build --release --bin indexer-tap-agent

########################################################################################

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    openssl ca-certificates postgresql-client curl jq iproute2 \
    valgrind strace \
    && rm -rf /var/lib/apt/lists/*

# Create profiling directory with proper permissions
RUN mkdir -p /opt/profiling && chmod 777 /opt/profiling

# Copy our start script into the image
COPY contrib/tap-agent/start.sh /usr/local/bin/start.sh

# Copy the horizon.json and .env files
COPY contrib/local-network/horizon.json /opt/horizon.json
COPY contrib/local-network/tap-contracts.json /opt/contracts.json
COPY contrib/local-network/.env /opt/.env
RUN chmod +x /usr/local/bin/start.sh

COPY --from=build /root/target/release/indexer-tap-agent /usr/local/bin/indexer-tap-agent

ENTRYPOINT [ "/usr/local/bin/start.sh" ]
