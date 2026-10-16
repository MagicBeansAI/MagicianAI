# Build once per toolchain/architecture, then reuse for Linux artifact builds.
# No application source or runtime credentials enter this SDK image.
FROM node:20-bookworm-slim AS node
FROM rust:1.92.0-bookworm
COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN ln -s ../lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm && \
    ln -s ../lib/node_modules/npm/bin/npx-cli.js /usr/local/bin/npx && \
    apt-get update && apt-get install -y --no-install-recommends \
      build-essential clang cmake libclang-dev libssl-dev pkg-config \
      protobuf-compiler libprotobuf-dev perl git ca-certificates python3 && \
    rm -rf /var/lib/apt/lists/*
ENV CARGO_BUILD_JOBS=1 CMAKE_BUILD_PARALLEL_LEVEL=1 \
    CARGO_PROFILE_RELEASE_LTO=false NODE_OPTIONS=--max-old-space-size=3072
WORKDIR /src
CMD ["bash"]
