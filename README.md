# codex-proxy-rs

Rust rewrite of Codex Proxy focused only on ChatGPT/Codex accounts and the Codex backend.

## Included

- `/v1/chat/completions`
- `/v1/responses`
- `/v1/models`
- ChatGPT/Codex OAuth and refresh-token based account pool
- Codex Desktop-style TLS, headers, Cookies, and fingerprint updates
- SQLite storage with sqlx-managed schema
- Structured logs with pagination and file rotation

## Excluded

- OpenAI official API Key upstream
- Anthropic, Gemini, custom providers, OpenRouter-style routing
- Ollama bridge
- Per-account proxy assignment
- Electron app
- Legacy compatibility layers and old-project data migration scripts

## Auth Model

- Admin login uses admin password and HttpOnly session Cookie.
- Client API keys call `/v1/*` and start with `cpr_`.
- Codex account tokens are internal upstream credentials.

## Development

Runtime config is loaded from `config.yaml` in the project root, then optional
`local.yaml` / `local.yml`.

```bash
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test
cargo run
```

Health check:

```bash
curl -s http://127.0.0.1:8080/health
```
