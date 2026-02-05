FROM rust:1.90-bookworm as build
WORKDIR /root
# Copy from the root project directory (two levels up)
COPY ../../ .
RUN apt-get update && apt-get install -y --no-install-recommends \
    protobuf-compiler libsasl2-dev && rm -rf /var/lib/apt/lists/*
RUN cargo build --release --bin indexer-service-rs

########################################################################################
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    openssl ca-certificates protobuf-compiler postgresql-client curl \
    jq \
    && rm -rf /var/lib/apt/lists/*


COPY --from=build /root/target/release/indexer-service-rs /usr/local/bin/indexer-service-rs

# Copy our start script into the image
COPY contrib/local-network/.env /opt/.env
COPY contrib/indexer-service/start.sh /usr/local/bin/start.sh
# These config files are mounted at runtime by docker-compose.

RUN chmod +x /usr/local/bin/start.sh

ENTRYPOINT [ "/usr/local/bin/start.sh" ]
