# codex-proxy-rs

`codex-proxy-rs` is a lean Rust rewrite of `codex-proxy`.

The service exposes OpenAI-compatible local endpoints backed only by ChatGPT/Codex
accounts and the official Codex backend:

- Keep `/v1/chat/completions`, `/v1/responses`, and `/v1/models`.
- Remove OpenAI official API key direct upstream.
- Remove Anthropic, Gemini, custom provider routing, Ollama, Electron, and per-account proxy assignment.
- Keep Codex Desktop TLS/header parity, Cloudflare Cookie persistence, and fingerprint auto-update as first-class subsystems.

Current implementation status is tracked in `docs/implementation-status.md`.
