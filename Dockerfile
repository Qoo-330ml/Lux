FROM node:22-bookworm-slim AS web-builder

WORKDIR /src/web
COPY web/package.json web/pnpm-lock.yaml ./
COPY web/pnpm-workspace.yaml ./
RUN corepack enable \
    && corepack prepare pnpm@11.9.0 --activate \
    && pnpm install --frozen-lockfile
COPY web ./
RUN pnpm build

FROM rust:1.85-bookworm AS builder

WORKDIR /src
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libsqlite3-dev \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock rust-toolchain.toml build.rs ./
COPY src ./src
COPY migrations ./migrations
COPY logo.svg ./logo.svg
COPY web ./web
COPY --from=web-builder /src/web/dist ./web/dist

RUN cargo build --release --locked

FROM debian:bookworm-slim

ARG LUX_VERSION=dev
ARG LUX_REVISION=unknown
LABEL org.opencontainers.image.title="Lux" \
      org.opencontainers.image.version="$LUX_VERSION" \
      org.opencontainers.image.revision="$LUX_REVISION"

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl ffmpeg util-linux \
    && groupadd --system --gid 10001 lux \
    && useradd --system --uid 10001 --gid lux --home-dir /config --no-create-home lux \
    && mkdir -p /config /media \
    && chown -R lux:lux /config /media \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/target/release/luxd /usr/local/bin/luxd
COPY --from=web-builder /src/web/dist /usr/local/share/lux/web
COPY --chmod=0755 docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh

ENV LUX_HTTP_ADDR=0.0.0.0:8097 \
    LUX_CONFIG_DIR=/config \
    LUX_WEB_DIR=/usr/local/share/lux/web \
    RUST_LOG=luxd=info,tower_http=info \
    TZ=UTC

VOLUME ["/config", "/media"]
EXPOSE 8097

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD runuser --user lux -- curl --fail --silent http://127.0.0.1:8097/health/live || exit 1

ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
CMD ["/usr/local/bin/luxd"]
