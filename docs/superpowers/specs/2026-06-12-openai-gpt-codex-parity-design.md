# OpenAI GPT Codex Parity Design

## Goal

Port the OpenAI GPT/Codex compatibility logic from `/home/zyy/桌面/Codes/codex-proxy` into this Rust service while keeping the Rust project intentionally scoped to Codex-backed OpenAI-compatible APIs.

## Explicit Non-Goals

- Do not port outbound proxy pools, proxy assignment, or proxy health checks.
- Do not port non-OpenAI/Codex routing, non-OpenAI local model bridges, Electron, frontend UI, or update flows.
- Do not add direct third-party OpenAI API-key provider routing. The `/v1` API remains backed by imported ChatGPT/Codex accounts.

## Architecture

The Rust service uses a shared Codex upstream lifecycle for `/v1/responses` and `/v1/chat/completions`. Route-specific code parses and translates client API shapes, then delegates account acquisition, Codex transport, retry/fallback, usage recording, cookie capture, and event logging through v1 services into `codex/upstream`.

OpenAI Chat compatibility is implemented as a real route, not an alias to Responses. Chat requests translate to Codex Responses requests, and Codex SSE output translates back to OpenAI chat completion JSON or chat completion chunks.

Responses compatibility keeps native Responses passthrough semantics but adds the TS implementation's in-scope fields: reasoning/service tier, tool choice, parallel tool calls, text formats, prompt cache key, include, client metadata, and Codex context headers. `previous_response_id` uses WebSocket transport rather than being rejected.

Current progress: Tasks 1-7 are implemented for the OpenAI GPT/Codex scope. Commits `3ecd5b6`, `3960a35`, and `6d18d2c` implement retry/fallback across non-streaming Responses, HTTP SSE setup, non-history WebSocket setup, and Chat Completions while preserving `previous_response_id` account affinity. Commits `1738f24`, `f75db60`, and `4b19846` add persisted backend model snapshots, admin refresh, and runtime model-plan allowlist sync. Commits `0b6cc7e` through `93cfee3` rebuild scoped admin account import/export/mutation/cookie/health/quota operations. Commits `b879dd6`, `0daa96b`, and `efbbe8a` add quota warnings, durable quota/Cloudflare cooldown restore, and Cloudflare-blocked account Cookie clearing. Remaining parity gaps are tracked in the plan/status docs: richer OpenAI-compatible error mapping, live Chat streaming transform, selected Responses field edge cases, durable release-time slot state, quota-window recovery, and background scheduler lifecycle.

## Components

- `src/codex/protocol/openai_to_codex.rs`: OpenAI Chat request schema and Chat-to-Codex translation, including messages, tools/functions, multimodal input, response formats, reasoning effort, and service tier.
- `src/codex/protocol/codex_to_openai.rs`: Codex SSE to OpenAI Chat JSON/SSE conversion, including text, reasoning, tool calls, usage, and errors.
- `src/codex/transport/types.rs`: Expanded Codex request/response types for Responses and WebSocket.
- `src/codex/transport/websocket.rs`: WebSocket transport selection and `response.create` conversion into SSE-compatible events for upstream-history requests such as `previous_response_id`.
- `src/codex/transport/client.rs`: HTTP SSE and WebSocket Codex transport, context headers, cookies, and rate-limit header capture.
- `src/codex/upstream/*`: shared account acquire, dispatch, retry/fallback, refresh retry, stream collection, usage recording, cookie persistence, and lifecycle logging.
- `src/http/v1/{chat,responses,models,router}.rs` plus `src/service/{chat,responses}.rs`: separate `/v1/responses` and `/v1/chat/completions` handlers delegating orchestration to services.
- `src/codex/accounts/pool.rs` and repositories: fallback selection, quota/rate-limit updates, durable usage release, and account state mutation hooks.
- `src/codex/models/catalog.rs`: static catalog plus persisted backend snapshots, plan allowlist derivation, and admin refresh trigger.
- `src/http/admin/*.rs`, `src/service/{api_key,settings,usage,log,diagnostics}.rs`, and `src/codex/accounts/service/*`: scoped account/API-key/account-health/quota/cookie/admin model operations needed to operate Codex-backed GPT compatibility. Account import/export now uses only the native Rust format; external legacy and proxy-pool payloads are intentionally removed.

## Data Flow

For Chat Completions, the route validates the local client API key, parses the OpenAI request, and delegates to `ChatService`. The service validates the model against the catalog, translates to a Codex request, acquires an account through the shared upstream lifecycle, sends the request, and converts Codex output into OpenAI Chat output. Streaming clients receive OpenAI chat completion chunks and `[DONE]`; non-streaming clients receive a `chat.completion` object.

For Responses, the route validates the local client API key and delegates to `ResponsesService`. The service parses native Responses fields, builds a Codex request, acquires an account, sends via WebSocket when required, and otherwise streams raw Codex SSE or collects `response.completed` for non-streaming clients.

## Error Handling

Client authentication errors stay OpenAI-compatible. Model misses return `model_not_found`. Upstream 401 triggers refresh and one retry for the same account. For non-streaming Responses, HTTP SSE Responses setup, non-history WebSocket setup, and Chat Completions, 429 applies a durable quota cooldown and records the failed attempt, 402 marks quota exhausted, Cloudflare 403 applies a durable cooldown and clears the challenged account Cookies, non-Cloudflare 403 marks banned, and each retryable account-state error tries another imported account when available. `previous_response_id` WebSocket failures remain account-affine. Terminal upstream SSE errors are mapped to OpenAI-compatible errors for non-streaming clients and recorded while streaming clients keep passthrough events.

## Testing

Development follows test-first changes. Tests cover translation units, route behavior with `wiremock`, WebSocket transport boundaries, fallback account retry, rate-limit/quota state changes, model refresh cache behavior, and admin account operations. Existing full verification remains `cargo fmt --check`, `cargo test`, and `cargo clippy --all-targets --all-features --locked -- -D warnings`.
