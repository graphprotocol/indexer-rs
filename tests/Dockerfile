FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    curl \
    jq \
    dnsutils \
    netcat-openbsd \
    iputils-ping \
    net-tools \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /tests
COPY . .
RUN chmod +x /tests/run-tests.sh

CMD ["/bin/bash", "-c", "/tests/run-tests.sh"]
