# Polaris — single-binary blog engine.
#
# Multi-stage build (builds natively per platform — the multi-arch manifest
# is produced by .github/workflows/docker.yml, not by QEMU emulation):
#   1. chef    — dependency recipe for layer caching
#   2. builder — static musl release binary (migrations are embedded)
#   3. runtime — minimal Alpine image, non-root user
#
# Build:  docker build -t polaris .
# Run:    docker run -p 3000:3000 -v polaris-data:/app/data polaris
#
# Mount your own polaris.toml over /app/polaris.toml for real deployments.

FROM rust:1-alpine3.21 AS chef
RUN apk add --no-cache musl-dev \
    && cargo install cargo-chef --locked --version 0.1.71
WORKDIR /app

FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
# Build dependencies first so source edits rebuild only the app.
RUN cargo chef cook --release --recipe-path recipe.json
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
RUN cargo build --locked --release --bin polaris

FROM alpine:3.21 AS runtime
LABEL org.opencontainers.image.title="Polaris" \
      org.opencontainers.image.description="Fast, lightweight, secure and extensible blog engine — a single static binary." \
      org.opencontainers.image.source="https://github.com/polaris-blog/blog" \
      org.opencontainers.image.licenses="MIT OR Apache-2.0"
RUN addgroup -S polaris && adduser -S polaris -G polaris \
    && mkdir -p /app/data && chown -R polaris:polaris /app
WORKDIR /app
COPY --from=builder /app/target/release/polaris /usr/local/bin/polaris
# Usable out-of-the-box defaults; override with volume mounts.
COPY --chown=polaris:polaris themes /app/themes
COPY --chown=polaris:polaris plugins /app/plugins
COPY --chown=polaris:polaris polaris.toml.example /app/polaris.toml
USER polaris
VOLUME ["/app/data"]
EXPOSE 3000
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
    CMD wget -q -O /dev/null http://127.0.0.1:3000/ || exit 1
ENTRYPOINT ["polaris"]
# The bundled polaris.toml defaults to 127.0.0.1 — useless inside a container.
# CLI args win over config, so listening on all interfaces is forced here.
CMD ["serve", "--config", "/app/polaris.toml", "--host", "0.0.0.0"]
