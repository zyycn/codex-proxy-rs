# syntax=docker/dockerfile:1.7@sha256:a57df69d0ea827fb7266491f2813635de6f17269be881f696fbfdf2d83dda33e

FROM busybox:1.37.0-musl@sha256:5cec3fc171c87218698e85a52af7087de727372aae264a787b8112901a5b0092 AS restore
ARG CACHE_STAMP
RUN --mount=type=bind,target=/cache \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/backend/target \
    test -n "$CACHE_STAMP" \
    && cp -a /cache/registry/. /usr/local/cargo/registry/ \
    && cp -a /cache/git/. /usr/local/cargo/git/ \
    && cp -a /cache/target/. /app/backend/target/

FROM busybox:1.37.0-musl@sha256:5cec3fc171c87218698e85a52af7087de727372aae264a787b8112901a5b0092 AS collect
ARG CACHE_STAMP
# 一次导出三个 mount，避免逐目录创建镜像、载入 Docker 再复制文件。
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/backend/target \
    test -n "$CACHE_STAMP" \
    && mkdir -p /cache/registry /cache/git /cache/target \
    && cp -a /usr/local/cargo/registry/. /cache/registry/ \
    && cp -a /usr/local/cargo/git/. /cache/git/ \
    && cp -a /app/backend/target/. /cache/target/

FROM scratch AS snapshot
COPY --from=collect /cache/ /
