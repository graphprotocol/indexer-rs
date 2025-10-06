########################################################################
# Build image
FROM node:20.19-bookworm-slim as build

ENV NODE_ENV production

RUN apt-get update && apt-get install -y python3 build-essential git curl

WORKDIR /opt

# Clone the indexer repository and checkout horizon branch
RUN git clone https://github.com/graphprotocol/indexer.git && \
    cd indexer && \
    git checkout horizon

WORKDIR /opt/indexer

# Install dependencies; include dev dependencies for build
RUN yarn --frozen-lockfile --non-interactive --production=false

########################################################################
# Runtime image
FROM node:20.19-bookworm-slim

ENV NODE_ENV production

RUN apt-get update && apt-get install -y python3 build-essential git curl

# Install Rust (may be needed for dependencies)
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs > /tmp/rustup.sh && sh /tmp/rustup.sh -y
ENV PATH="/root/.cargo/bin:$PATH"

WORKDIR /opt/indexer

# Copy root files from build stage
COPY --from=build /opt/indexer/package.json .
COPY --from=build /opt/indexer/yarn.lock .
COPY --from=build /opt/indexer/tsconfig.json .
COPY --from=build /opt/indexer/lerna.json .

# Copy build output
COPY --from=build /opt/indexer/packages/indexer-common/package.json /opt/indexer/packages/indexer-common/package.json
COPY --from=build /opt/indexer/packages/indexer-common/dist /opt/indexer/packages/indexer-common/dist
COPY --from=build /opt/indexer/packages/indexer-cli/package.json /opt/indexer/packages/indexer-cli/package.json
COPY --from=build /opt/indexer/packages/indexer-cli/dist /opt/indexer/packages/indexer-cli/dist
COPY --from=build /opt/indexer/packages/indexer-cli/bin /opt/indexer/packages/indexer-cli/bin

# Install dependencies; exclude dev dependencies
RUN yarn --frozen-lockfile --non-interactive --production=true
RUN ln -s /opt/indexer/packages/indexer-cli/bin/graph-indexer /usr/bin/graph

# Fix the wrap-ansi ESM issue in horizon branch
RUN sed -i 's/const wrap_ansi_1 = __importDefault(require("wrap-ansi"));/const wrap_ansi_1 = { default: (str) => str };/g' \
    /opt/indexer/packages/indexer-cli/dist/command-helpers.js

# Keep container running for docker exec
ENTRYPOINT ["tail", "-f", "/dev/null"]
