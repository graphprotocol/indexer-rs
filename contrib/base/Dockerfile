FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    openssl ca-certificates protobuf-compiler postgresql-client curl \
    jq git linux-perf \
    strace valgrind procps \
    bpftrace linux-headers-generic \
    && rm -rf /var/lib/apt/lists/*


# Clone FlameGraph repository
RUN git clone https://github.com/brendangregg/FlameGraph.git /opt/FlameGraph && \
    chmod +x /opt/FlameGraph/*.pl

# Create profiling directory with proper permissions
RUN mkdir -p /opt/profiling && chmod 777 /opt/profiling
