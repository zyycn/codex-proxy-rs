# syntax=docker/dockerfile:1.7

FROM node:24-bookworm-slim AS frontend-builder

WORKDIR /app/frontend
COPY frontend/package.json frontend/pnpm-lock.yaml frontend/pnpm-workspace.yaml ./
RUN corepack enable && pnpm install --frozen-lockfile
COPY frontend ./
RUN pnpm build

FROM rust:1.95-bookworm AS backend-builder

WORKDIR /app/backend
ARG TARGETARCH
ARG CPR_VERSION=
ARG CPR_GIT_SHA=unknown
ARG CPR_BUILD_TIME=unknown
ARG CPR_BUILD_TYPE=release
ENV CARGO_TARGET_DIR=/app/backend/target/${TARGETARCH}
ENV CPR_VERSION=${CPR_VERSION}
ENV CPR_GIT_SHA=${CPR_GIT_SHA}
ENV CPR_BUILD_TIME=${CPR_BUILD_TIME}
ENV CPR_BUILD_TYPE=${CPR_BUILD_TYPE}
COPY backend/Cargo.toml backend/Cargo.lock ./
COPY backend/build ./build
COPY backend/src ./src
COPY --from=frontend-builder /app/frontend/dist ./web/dist
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/backend/target \
    cargo build --release --locked --bin codex-proxy-rs \
    && cp "${CARGO_TARGET_DIR}/release/codex-proxy-rs" /tmp/codex-proxy-rs

FROM backend-builder AS release-asset-builder

ARG TARGETOS
ARG TARGETARCH

WORKDIR /out
RUN mkdir -p /tmp/release/codex-proxy-rs/web \
    && cp /tmp/codex-proxy-rs /tmp/release/codex-proxy-rs/ \
    && cp -r /app/backend/web/dist /tmp/release/codex-proxy-rs/web/dist \
    && tar -czf "/out/codex-proxy-rs_${CPR_VERSION}_${TARGETOS}_${TARGETARCH}.tar.gz" \
        -C /tmp/release codex-proxy-rs

FROM scratch AS release-asset

COPY --from=release-asset-builder /out /

FROM debian:bookworm-slim AS runtime-base

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
ENV CPR_DEPLOYMENT_MODE=docker

EXPOSE 8080

CMD ["codex-proxy-rs"]

FROM runtime-base AS runtime

COPY --from=backend-builder /tmp/codex-proxy-rs /usr/local/bin/codex-proxy-rs
COPY --from=frontend-builder /app/frontend/dist ./web/dist

FROM runtime-base AS runtime-prebuilt

ARG TARGETARCH
ARG CPR_PREBUILT_DIR=dist/docker

COPY ${CPR_PREBUILT_DIR}/linux-${TARGETARCH}/codex-proxy-rs /usr/local/bin/codex-proxy-rs
COPY ${CPR_PREBUILT_DIR}/web/dist ./web/dist
RUN chmod +x /usr/local/bin/codex-proxy-rs
