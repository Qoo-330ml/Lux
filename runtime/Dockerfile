# Keep this digest fixed so application image rebuilds do not implicitly
# rebuild the operating-system dependency layer.
FROM debian:trixie-slim@sha256:3a39a0592364683e6bab97937b72cad5a8fa6dcbbee90edb3bb48c7f8e94f258

# Keep this value aligned with RUNTIME_IMAGE_TAG in the Docker Hub workflow;
# bump both when the runtime dependency set changes.
ARG LUX_RUNTIME_VERSION=trixie-jellyfin-ffmpeg7-v1
ARG JELLYFIN_FFMPEG_VERSION=7.1.4-3
ARG TARGETARCH

LABEL org.opencontainers.image.title="Lux runtime" \
      org.opencontainers.image.version="$LUX_RUNTIME_VERSION" \
      org.opencontainers.image.description="Pinned Debian Trixie runtime dependencies for Lux"

RUN set -eux; \
    case "$TARGETARCH" in \
      arm64) \
        package="jellyfin-ffmpeg7_${JELLYFIN_FFMPEG_VERSION}-trixie_arm64.deb"; \
        sha256="a8fa3ec7cf8fbaf06bb4fdb768d4dd7798277fe4c4b66884c777dc7d575877fb"; \
        ;; \
      amd64) \
        package="jellyfin-ffmpeg7_${JELLYFIN_FFMPEG_VERSION}-trixie_amd64.deb"; \
        sha256="8e30f9f7f3958c524bec8334540e4241145ad4300b328a08a55783c619187225"; \
        ;; \
      *) \
        echo "unsupported TARGETARCH: $TARGETARCH" >&2; \
        exit 1; \
        ;; \
    esac; \
    apt-get update; \
    apt-get install -y --no-install-recommends ca-certificates curl; \
    curl --fail --silent --show-error --location --retry 3 \
      --output "/tmp/$package" \
      "https://github.com/jellyfin/jellyfin-ffmpeg/releases/download/v${JELLYFIN_FFMPEG_VERSION}/$package"; \
    echo "$sha256  /tmp/$package" | sha256sum --check --strict; \
    apt-get install -y --no-install-recommends "/tmp/$package"; \
    rm -f "/tmp/$package"; \
    mkdir -p /config /media /usr/share/doc/lux; \
    rm -rf /var/lib/apt/lists/*

ENV PATH="/usr/lib/jellyfin-ffmpeg:${PATH}"

USER root
