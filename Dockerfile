FROM rust:1.85-bookworm AS builder

WORKDIR /src
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libsqlite3-dev \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock rust-toolchain.toml build.rs ./
COPY src ./src
COPY migrations ./migrations
COPY web ./web

RUN cargo build --release --locked

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl ffmpeg \
    && groupadd --system --gid 10001 lux \
    && useradd --system --uid 10001 --gid lux --home-dir /data --no-create-home lux \
    && mkdir -p /data/config /media \
    && chown -R lux:lux /data \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/target/release/luxd /usr/local/bin/luxd

ENV LUX_HTTP_ADDR=0.0.0.0:8097 \
    LUX_CONFIG_DIR=/data/config \
    RUST_LOG=luxd=info,tower_http=info \
    TZ=UTC

VOLUME ["/data", "/media"]
EXPOSE 8097
USER lux

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl --fail --silent http://127.0.0.1:8097/health/live || exit 1

ENTRYPOINT ["/usr/local/bin/luxd"]
