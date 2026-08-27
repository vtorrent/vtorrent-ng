# vTorrent daemon — multi-stage build
# Build:  docker build -t vtorrent/node -f docker/Dockerfile .
# Run:    docker run vtorrent/node --listen 0.0.0.0:22526 --rpc-addr 0.0.0.0:22525

FROM rust:1.90-slim AS builder
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

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -m -u 1000 vtorrent
USER vtorrent
WORKDIR /home/vtorrent
VOLUME /home/vtorrent/.vtorrent

COPY --from=builder /build/target/release/vtorrent-daemon /usr/local/bin/vtorrent-daemon

EXPOSE 22525 22526

ENTRYPOINT ["vtorrent-daemon"]
