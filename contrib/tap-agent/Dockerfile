FROM rust:1.81-bookworm as build

WORKDIR /root
COPY . .

# Force SQLx to use the offline mode to statically check the database queries against
# the prepared files in the `.sqlx` directory.
ENV SQLX_OFFLINE=true
RUN apt-get update && apt-get install -y --no-install-recommends \
    protobuf-compiler && rm -rf /var/lib/apt/lists/*
RUN cargo build --release --bin indexer-tap-agent

########################################################################################

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    openssl ca-certificates postgresql-client curl jq iproute2 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /root/target/release/indexer-tap-agent /usr/local/bin/indexer-tap-agent

ENTRYPOINT [ "/usr/local/bin/indexer-tap-agent" ]
