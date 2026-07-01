# syntax=docker/dockerfile:1.7

FROM node:24-bookworm-slim AS web-builder

WORKDIR /app/web
COPY web/package.json web/pnpm-lock.yaml web/pnpm-workspace.yaml ./
RUN corepack enable && pnpm install --frozen-lockfile
COPY web ./
RUN pnpm build

FROM rust:1.95-bookworm AS rust-builder

WORKDIR /app
ARG CPR_VERSION=dev
ARG CPR_GIT_SHA=unknown
ARG CPR_BUILD_TIME=unknown
ENV CPR_VERSION=${CPR_VERSION}
ENV CPR_GIT_SHA=${CPR_GIT_SHA}
ENV CPR_BUILD_TIME=${CPR_BUILD_TIME}
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
COPY --from=web-builder /app/web/dist ./web/dist
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build --release --locked --bins \
    && cp /app/target/release/codex-proxy-rs /tmp/codex-proxy-rs \
    && cp /app/target/release/codex-proxy-rs-updater /tmp/codex-proxy-rs-updater \
    && strip /tmp/codex-proxy-rs /tmp/codex-proxy-rs-updater

FROM debian:bookworm-slim AS updater-runtime

ARG CPR_VERSION=dev
ARG CPR_GIT_SHA=unknown
ARG CPR_BUILD_TIME=unknown

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates docker.io docker-compose \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /workspace
ENV CPR_DOCKER_COMPOSE_BIN=docker-compose \
    CPR_VERSION=${CPR_VERSION} \
    CPR_GIT_SHA=${CPR_GIT_SHA} \
    CPR_BUILD_TIME=${CPR_BUILD_TIME}
COPY --from=rust-builder /tmp/codex-proxy-rs-updater /usr/local/bin/codex-proxy-rs-updater

EXPOSE 8090

CMD ["codex-proxy-rs-updater"]

FROM debian:bookworm-slim AS runtime

ARG CPR_VERSION=dev
ARG CPR_GIT_SHA=unknown
ARG CPR_BUILD_TIME=unknown

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
ENV CPR_DEPLOYMENT_MODE=docker \
    CPR_VERSION=${CPR_VERSION} \
    CPR_GIT_SHA=${CPR_GIT_SHA} \
    CPR_BUILD_TIME=${CPR_BUILD_TIME}
COPY --from=rust-builder /tmp/codex-proxy-rs /usr/local/bin/codex-proxy-rs

EXPOSE 8080

CMD ["codex-proxy-rs"]
