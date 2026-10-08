# vTorrent daemon — multi-stage build
# Build:  docker build -t vtorrent/node -f docker/Dockerfile .
# Run:    docker run vtorrent/node --listen 0.0.0.0:22526 --rpc-addr 0.0.0.0:22525

# Base images are pinned by digest for reproducible builds (see
# docs/mainnet-readiness.md §4). Update the digest deliberately (security
# updates), re-run the reproducible-build check, and bump the release notes.
FROM rust:1.90-slim@sha256:7fa728f3678acf5980d5db70960cf8491aff9411976789086676bdf0c19db39e AS builder
WORKDIR /build

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock* ./
# All workspace members must be present for `cargo build` to resolve the
# workspace — even crates the daemon does not depend on (e.g. vtorrent-tauri).
COPY vtorrent-core ./vtorrent-core
COPY vtorrent-wallet ./vtorrent-wallet
COPY vtorrent-migrate ./vtorrent-migrate
COPY vtorrent-snapshot ./vtorrent-snapshot
COPY vtorrent-node ./vtorrent-node
COPY vtorrent-p2p ./vtorrent-p2p
COPY vtorrent-torrent ./vtorrent-torrent
COPY vtorrent-rpc ./vtorrent-rpc
COPY vtorrent-overlay ./vtorrent-overlay
COPY vtorrent-spv ./vtorrent-spv
COPY vtorrent-store ./vtorrent-store
COPY vtorrent-daemon ./vtorrent-daemon
COPY vtorrent-script ./vtorrent-script
COPY vtorrent-onion ./vtorrent-onion
COPY vtorrent-cli ./vtorrent-cli
COPY vtorrent-btc ./vtorrent-btc
COPY vtorrent-tauri ./vtorrent-tauri
COPY vtorrent-wallet-service ./vtorrent-wallet-service

RUN cargo build --release -p vtorrent-daemon

FROM debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl iptables \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -m -u 1000 vtorrent
USER vtorrent
WORKDIR /home/vtorrent
VOLUME /home/vtorrent/.vtorrent

COPY --from=builder /build/target/release/vtorrent-daemon /usr/local/bin/vtorrent-daemon

EXPOSE 22525 22526

ENTRYPOINT ["vtorrent-daemon"]
