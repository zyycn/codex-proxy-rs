# Codex Desktop Responses WebSocket Parity Design

Date: 2026-06-16

## Objective

Reproduce the observable official Codex Desktop model request chain for Responses WebSocket 1:1 in `codex-proxy-rs`, using the official DMG bundled Rust binary as the target:

`/tmp/codex-desktop-fingerprint/dmg/Codex Installer/Codex.app/Contents/Resources/codex`

The target chain is the bundled `codex` Rust engine's `codex-api/src/endpoint/responses_websocket.rs`, not Electron `net.fetch` and not the webview/browser WebSocket path.

## Scope

This work covers:

- Responses WebSocket connection behavior.
- WebSocket opening handshake bytes.
- TLS ClientHello and rustls configuration visible from the client side.
- Business headers used during the WebSocket upgrade.
- First-frame `response.create` JSON payload.
- WebSocket-to-HTTP SSE fallback behavior.
- HTTP SSE fallback dependency/config parity where it affects the downgrade path.

This work does not cover Electron `net.fetch`, dictation/realtime WebSocket, app-server/local IPC WebSocket, remote-control WebSocket, or visual UI behavior.

## Current Evidence

Official Desktop evidence collected from the DMG:

- App version: `26.609.71450`.
- Bundle version: `3965`.
- Bundle id: `com.openai.codex`.
- Bundled `Contents/Resources/codex` contains:
  - `codex-api/src/endpoint/responses_websocket.rs`
  - `codex-api/src/endpoint/responses.rs`
  - `tokio-tungstenite`
  - `tungstenite`
  - `tokio-rustls-0.26.4`
  - `rustls-0.23.36`
  - `reqwest-0.12.28`
  - `reqwest-0.13.4`
  - `hyper-rustls-0.27.7`
  - `response.create`
  - `previous_response_id`

Current `codex-proxy-rs` evidence:

- `/v1/responses` defaults to `WebSocketPreferred` unless `force_http_sse` is set.
- `previous_response_id` forces `WebSocketRequired`.
- WebSocket opening is manually written as HTTP/1.1 upgrade.
- WebSocket opening currently uses `tokio-rustls` and `rustls 0.23.36`.
- WebSocket opening emits `Sec-WebSocket-Extensions: permessage-deflate; client_max_window_bits`.
- HTTP fallback currently uses `reqwest 0.12.28` without the `http2` feature.

CodeGraph baseline:

- `transport_for_request` is at `src/codex/gateway/transport/websocket/mod.rs:160`.
- `connect_with_original_opening_handshake` is at `src/codex/gateway/transport/websocket/opening.rs:59`.
- `create_response_via_websocket` is at `src/codex/gateway/transport/websocket/mod.rs:184`.
- `CodexBackendClient::create_response` dispatches WS-first then HTTP fallback at `src/codex/gateway/transport/http_client.rs:162`.
- CodeGraph index was initialized on 2026-06-16 and can be refreshed with `codegraph sync .`.

## Design

### Audit-First Workflow

The implementation must not make behavioral parity changes from static guesses alone. Each change needs an audit record that states:

- observed current `codex-proxy-rs` behavior,
- observed official `codex` behavior or the reason direct observation is not available,
- exact difference,
- proposed code change,
- verification command.

All audit records are appended to the "Analysis Journal" section of this document before the related code change is made.

### Local WebSocket Audit Harness

Add a local audit harness that accepts a test WebSocket target and records:

- TLS ClientHello summary: SNI, ALPN, cipher suites, supported groups, signature algorithms, TLS extensions, JA4-style stable summary when available.
- HTTP/1.1 upgrade raw bytes up to the blank line.
- Header order and casing.
- `Sec-WebSocket-Key` position and generated-key behavior.
- `Sec-WebSocket-Extensions`.
- First WebSocket data frame payload.

The harness should run locally and must not require real OpenAI credentials. It may use a local certificate for TLS observation, or a passive capture path if the implementation proves simpler and more reliable.

### Official Binary Sampling

Use the official bundled binary as the target reference. The first phase is to determine the smallest practical invocation path that makes it open a `responses_websocket` connection to a controlled endpoint.

Acceptable sampling paths, in priority order:

1. Run the official `codex` binary with environment/config overrides that point API base URL to the local audit harness.
2. If direct override is unavailable, intercept DNS/proxy locally while preserving the target URL shape.
3. If live execution is blocked, extract more static evidence from the binary and mark the missing observation explicitly in the Analysis Journal.

### `codex-proxy-rs` Sampling

Add an internal audit mode gated by configuration or environment variable. It should record local-only artifacts for the current implementation:

- opening request bytes from `opening_request_bytes`,
- ordered business headers passed to the opening request,
- `response.create` JSON before it is sent,
- selected transport mode and fallback decision,
- error classification when fallback occurs.

The audit mode must redact tokens, cookies, account ids, and request body content that can contain user data. Structural fields, header names, and redacted sentinel values may be logged.

### Parity Changes

Once both sides are sampled, apply minimal changes in this order:

1. TLS and dependency parity:
   - confirm rustls provider and features,
   - align `tokio-rustls` / `tokio-tungstenite` behavior where practical,
   - enable `reqwest` `http2` for HTTP fallback.
2. WebSocket opening parity:
   - request line,
   - fixed opening headers,
   - business header order and casing,
   - extension negotiation.
3. Payload parity:
   - JSON field order where the serializer path controls it,
   - default fields,
   - `client_metadata`,
   - `prompt_cache_key`,
   - `previous_response_id`.
4. Fallback parity:
   - network/opening failures may fall back when the request is only `WebSocketPreferred`,
   - upstream WS availability statuses may fall back,
   - `previous_response_id` never falls back,
   - quota/auth/business errors do not fall back,
   - early terminal/closed-before-terminal behavior follows the official sample.

### Verification

Required verification commands after implementation:

- `cargo fmt --all --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- Focused tests for WebSocket default routing and fallback behavior.
- A sample audit run that produces both `rs` and official reference artifacts, or a documented blocker in the Analysis Journal.

## Progress Log

- 2026-06-16: User selected target A: official DMG bundled `Contents/Resources/codex` Rust `responses_websocket` chain.
- 2026-06-16: User selected approach 1: dual-end audit comparison before parity changes.
- 2026-06-16: CodeGraph initialized in `/home/zyy/Codes/codex-proxy-rs`; index contains 285 files, 3,933 nodes, and 12,834 edges.
- 2026-06-16: Design document created. No production code has been changed yet.
- 2026-06-16: Spec self-review completed; no incomplete placeholders or scope contradictions remain.

## Analysis Journal

### 2026-06-16 Initial Chain Assessment

The earlier Electron `net.fetch` fingerprint remains useful for Desktop HTTP fetches but is not the target for model Responses parity. The official bundled `codex` binary includes `responses_websocket`, `tokio-tungstenite`, and `rustls`, which makes it the correct target for `/v1/responses` WebSocket parity.

Current `codex-proxy-rs` already follows the same broad transport choice: Responses defaults to WebSocket, `previous_response_id` forces WebSocket, and only preferred WebSocket requests may fall back to HTTP SSE. The remaining work is not to change the high-level direction; it is to prove and align the low-level opening, TLS, payload, and downgrade details.

### 2026-06-16 Current Known Gaps

The HTTP fallback path lacks `reqwest` `http2`, so fallback does not yet match the bundled Rust HTTP stack closely enough. The WebSocket path uses `rustls` with the `ring` provider, while the official binary contains both `aws-lc-rs` and `ring` symbols; the active provider must be verified by observation or stronger binary evidence. Header and payload order are currently modeled from local assumptions and must be compared against official samples.

## Update Discipline

This document is the durable context for the task. Before starting any substantial analysis or code edit, append a short entry to either Progress Log or Analysis Journal. After each implementation step, append:

- command or code area touched,
- observed result,
- next decision,
- verification status.

If context is compacted, resume from this document before continuing.
