ARG LUX_RUNTIME_IMAGE=lux-runtime:trixie-jellyfin-ffmpeg7-v1

FROM node:22-bookworm-slim AS web-builder

WORKDIR /src/web
COPY web/package.json web/pnpm-lock.yaml ./
COPY web/pnpm-workspace.yaml ./
RUN corepack enable \
    && corepack prepare pnpm@11.9.0 --activate \
    && pnpm install --frozen-lockfile
COPY web ./
RUN pnpm build

FROM rust:1.94-bookworm AS builder

WORKDIR /src
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libsqlite3-dev \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock rust-toolchain.toml ./

# Keep dependency compilation in a layer that is independent of application
# source changes. The real sources are copied below and reuse this target dir.
RUN mkdir -p src/bin \
    && printf 'pub fn placeholder() {}\n' > src/lib.rs \
    && printf 'fn main() {}\n' > src/main.rs \
    && cargo build --release --locked --bin luxd

COPY build.rs ./build.rs
COPY src ./src
COPY assets ./assets
COPY migrations ./migrations
COPY migrations-postgres ./migrations-postgres
COPY logo.svg ./logo.svg
COPY web ./web
COPY --from=web-builder /src/web/dist ./web/dist

RUN cargo build --release --locked --bin luxd

# Local builds use the `lux-runtime` bake target here. Published builds replace
# this argument with the per-architecture runtime image pinned by digest, so
# application releases reuse the same dependency layers.
FROM ${LUX_RUNTIME_IMAGE}

ARG LUX_VERSION=dev
ARG LUX_REVISION=unknown
LABEL org.opencontainers.image.title="Lux" \
      org.opencontainers.image.version="$LUX_VERSION" \
      org.opencontainers.image.revision="$LUX_REVISION"

COPY --from=builder /src/target/release/luxd /usr/local/bin/luxd
COPY --from=builder /src/assets/fonts/SmileySans-LICENSE.txt /usr/share/doc/lux/SmileySans-LICENSE.txt
COPY --from=web-builder /src/web/dist /usr/local/share/lux/web
COPY --chmod=0755 docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh

ENV LUX_HTTP_ADDR=0.0.0.0:8097 \
    LUX_CONFIG_DIR=/config \
    LUX_WEB_DIR=/usr/local/share/lux/web \
    MALLOC_ARENA_MAX=2 \
    RUST_LOG=luxd=info,tower_http=info \
    TZ=UTC

# Keep the service as root so bind-mounted NAS directories work without a
# UID/GID handoff or a recursive ownership rewrite at startup.
USER root

VOLUME ["/config", "/media"]
EXPOSE 8097

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl --fail --silent http://127.0.0.1:8097/health/live || exit 1

ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
CMD ["/usr/local/bin/luxd"]
