FROM rust:1.86-bookworm as build
WORKDIR /root
# Copy from the root project directory (two levels up)
COPY ../../ .
# Force SQLx to use the offline mode to statically check the database queries against
# the prepared files in the `.sqlx` directory.
#
ENV SQLX_OFFLINE=true
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
COPY contrib/indexer-service/start.sh /usr/local/bin/start.sh
COPY contrib/indexer-service/config.toml /opt/config/config.toml
COPY contrib/indexer-service/start-perf.sh /usr/local/bin/start-perf.sh

RUN chmod +x /usr/local/bin/start.sh
RUN chmod +x /usr/local/bin/start-perf.sh

ENTRYPOINT [ "/usr/local/bin/start-perf.sh" ]
