FROM rust:1.86-bookworm as build

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
    && rm -rf /var/lib/apt/lists/*

# Create profiling directory with proper permissions
RUN mkdir -p /opt/profiling && chmod 777 /opt/profiling

# Copy our start script into the image
COPY contrib/tap-agent/start.sh /opt/config/start.sh
COPY contrib/tap-agent/start-perf.sh /usr/local/bin/start-perf.sh
COPY contrib/tap-agent/config.toml /opt/config/config.toml

RUN chmod +x /opt/config/start.sh 
RUN chmod +x /usr/local/bin/start-perf.sh

COPY --from=build /root/target/release/indexer-tap-agent /usr/local/bin/indexer-tap-agent

ENTRYPOINT [ "/usr/local/bin/indexer-tap-agent" ]
