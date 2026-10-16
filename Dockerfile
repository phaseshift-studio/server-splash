# syntax=docker/dockerfile:1

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------
FROM rust:1-slim-bookworm AS build

WORKDIR /src

# The lockfile is copied and honoured so an image build is reproducible.
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src

RUN cargo build --release --locked \
 && strip target/release/splash

# ---------------------------------------------------------------------------
# Runtime
# ---------------------------------------------------------------------------
FROM debian:bookworm-slim

# `ss` (iproute2) is how listening sockets are discovered; `lsblk` (util-linux) gives
# the storage line. Both are absent from the slim base.
RUN apt-get update \
 && apt-get install -y --no-install-recommends iproute2 util-linux ca-certificates \
 && rm -rf /var/lib/apt/lists/*

COPY --from=build /src/target/release/splash /usr/local/bin/splash

# The page is generated and served from here, so the container needs no bind mount
# unless you want the HTML somewhere else.
ENV SPLASH_OUTPUT_DIR=/data \
    SPLASH_PORT=6420 \
    SPLASH_INTERVAL=900
VOLUME ["/data"]

EXPOSE 6420

# `--healthcheck` is the binary testing its own port, so the image needs no curl/wget.
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
  CMD ["/usr/local/bin/splash", "--healthcheck"]

ENTRYPOINT ["/usr/local/bin/splash"]
CMD ["--serve"]
