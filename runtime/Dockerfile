# Keep this digest fixed so application image rebuilds do not implicitly
# rebuild the operating-system dependency layer.
FROM debian:trixie-slim@sha256:3a39a0592364683e6bab97937b72cad5a8fa6dcbbee90edb3bb48c7f8e94f258

ARG LUX_RUNTIME_VERSION=trixie-ffmpeg-v1

LABEL org.opencontainers.image.title="Lux runtime" \
      org.opencontainers.image.version="$LUX_RUNTIME_VERSION" \
      org.opencontainers.image.description="Pinned Debian Trixie runtime dependencies for Lux"

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
       ca-certificates \
       curl \
       ffmpeg \
       fonts-noto-cjk \
    && mkdir -p /config /media /usr/share/doc/lux \
    && rm -rf /var/lib/apt/lists/*

USER root
