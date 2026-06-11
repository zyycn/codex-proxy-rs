FROM rust:1.95-bookworm AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
RUN cargo build --release --locked

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/codex-proxy-rs /usr/local/bin/codex-proxy-rs
COPY config ./config
COPY migrations ./migrations

ENV RUST_LOG=codex_proxy_rs=info,tower_http=info
EXPOSE 8080

CMD ["codex-proxy-rs"]
