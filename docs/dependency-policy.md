# Dependency Policy

- Use current stable crate releases for normal Rust dependencies.
- Commit `Cargo.lock` because this is an application.
- Run `cargo update` in dedicated dependency-refresh changes.
- Keep `reqwest = 0.12.28` and `rustls = 0.23.36` pinned until a TLS fingerprint review proves a newer pair matches real Codex Desktop behavior.
- `reqwest` is built with `rustls-tls-native-roots`, `json`, `stream`, `cookies`, and response compression features used by the Codex client builder. Do not switch back to default native-tls.
- `tokio` includes `net` for the Axum listener, and `tower` includes `util` for route tests using `ServiceExt`.
- Do not add unmaintained crates when a maintained community-standard crate exists.
- Every dependency change must run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test
```
