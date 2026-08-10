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
ARG LUX_PLUGIN_VERSION=1.0.0
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libsqlite3-dev \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock rust-toolchain.toml build.rs ./
COPY src ./src
COPY assets ./assets
COPY migrations ./migrations
COPY migrations-postgres ./migrations-postgres
COPY logo.svg ./logo.svg
COPY web ./web
COPY --from=web-builder /src/web/dist ./web/dist

RUN cargo build --release --locked \
        --bin luxd \
        --bin lux-plugin-tmdb \
        --bin lux-plugin-ip-hiofd \
        --bin lux-plugin-qoo-ip138 \
        --bin lux-plugin-pack \
    && plugin_arch="$(uname -m)" \
    && case "$plugin_arch" in \
         x86_64) plugin_arch="x86_64" ;; \
         aarch64) plugin_arch="aarch64" ;; \
         *) echo "unsupported plugin architecture: $plugin_arch" >&2; exit 1 ;; \
       esac \
    && mkdir -p /src/dist \
    && /src/target/release/lux-plugin-pack \
         --plugin tmdb \
         --binary /src/target/release/lux-plugin-tmdb \
         --output /src/dist/org.lux.tmdb.zip \
         --version "$LUX_PLUGIN_VERSION" \
         --platform linux \
         --arch "$plugin_arch" \
    && /src/target/release/lux-plugin-pack \
         --plugin ip-hiofd \
         --binary /src/target/release/lux-plugin-ip-hiofd \
         --output /src/dist/org.lux.ip-hiofd.zip \
         --version "$LUX_PLUGIN_VERSION" \
         --platform linux \
         --arch "$plugin_arch" \
    && /src/target/release/lux-plugin-pack \
         --plugin qoo-ip138 \
         --binary /src/target/release/lux-plugin-qoo-ip138 \
         --output /src/dist/org.lux.qoo-ip138.zip \
         --version "$LUX_PLUGIN_VERSION" \
         --platform linux \
         --arch "$plugin_arch"

FROM debian:bookworm-slim

ARG LUX_VERSION=dev
ARG LUX_REVISION=unknown
LABEL org.opencontainers.image.title="Lux" \
      org.opencontainers.image.version="$LUX_VERSION" \
      org.opencontainers.image.revision="$LUX_REVISION"

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl ffmpeg fonts-noto-cjk \
    && mkdir -p /config /media /usr/local/share/lux/plugins /usr/share/doc/lux \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/target/release/luxd /usr/local/bin/luxd
COPY --from=builder /src/assets/fonts/SmileySans-LICENSE.txt /usr/share/doc/lux/SmileySans-LICENSE.txt
COPY --from=builder /src/dist/org.lux.tmdb.zip /usr/local/share/lux/plugins/org.lux.tmdb.zip
COPY --from=builder /src/dist/org.lux.ip-hiofd.zip /usr/local/share/lux/plugins/org.lux.ip-hiofd.zip
COPY --from=builder /src/dist/org.lux.qoo-ip138.zip /usr/local/share/lux/plugins/org.lux.qoo-ip138.zip
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
