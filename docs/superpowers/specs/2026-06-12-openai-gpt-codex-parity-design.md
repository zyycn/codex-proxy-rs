# OpenAI GPT Codex Parity Design

## Goal

Port the OpenAI GPT/Codex compatibility logic from `/home/zyy/桌面/Codes/codex-proxy` into this Rust service while keeping the Rust project intentionally scoped to Codex-backed OpenAI-compatible APIs.

## Explicit Non-Goals

- Do not port outbound proxy pools, proxy assignment, or proxy health checks.
- Do not port Anthropic, Gemini, Ollama, custom provider routing, Electron, frontend UI, or update flows.
- Do not add direct third-party OpenAI API-key provider routing. The `/v1` API remains backed by imported ChatGPT/Codex accounts.

## Architecture

The Rust service will use a shared Codex upstream lifecycle for `/v1/responses` and `/v1/chat/completions`. Route-specific code parses and translates client API shapes, then delegates account acquisition, Codex transport, retry/fallback, usage recording, cookie capture, and event logging to common helpers.

OpenAI Chat compatibility is implemented as a real route, not an alias to Responses. Chat requests translate to Codex Responses requests, and Codex SSE output translates back to OpenAI chat completion JSON or chat completion chunks.

Responses compatibility keeps native Responses passthrough semantics but adds the TS implementation's in-scope fields: reasoning/service tier, tool choice, parallel tool calls, text formats, prompt cache key, include, client metadata, and Codex context headers. `previous_response_id` uses WebSocket transport rather than being rejected.

Current progress: Tasks 1-4 are implemented for the OpenAI GPT/Codex scope. Commit `3ecd5b6` partially implements Task 5 for non-streaming `/v1/responses`: upstream 429/402/403 responses classify account state and retry another imported account when available. Commit `3960a35` extends that fallback to HTTP SSE Responses setup and Chat Completions. Commit `1738f24` adds Task 6 cache groundwork: backend model snapshots are stored by account plan and `/v1/models*` reads cached backend models when present. Commit `0b6cc7e` adds Task 7 import compatibility for Sub2API OpenAI OAuth exports and marks the detected source format while ignoring proxy-only fields. WebSocket-backed `previous_response_id` fallback remains a Task 5 follow-up because account affinity must be preserved.

## Components

- `src/translation/openai_to_codex.rs`: OpenAI Chat request schema and Chat-to-Codex translation, including messages, tools/functions, multimodal input, response formats, reasoning effort, and service tier.
- `src/translation/codex_to_openai.rs`: Codex SSE to OpenAI Chat JSON/SSE conversion, including text, reasoning, tool calls, usage, and errors.
- `src/codex/types.rs`: Expanded Codex request/response types for Responses and WebSocket.
- `src/codex/websocket.rs`: WebSocket transport selection and `response.create` conversion into SSE-compatible events for upstream-history requests such as `previous_response_id`.
- `src/codex/client.rs`: HTTP SSE and WebSocket Codex transport, context headers, cookies, and rate-limit header capture.
- `src/http/v1.rs`: Separate `/v1/responses` and `/v1/chat/completions` handlers sharing upstream lifecycle helpers.
- `src/accounts/pool.rs` and repositories: fallback selection, quota/rate-limit updates, durable usage release, and account state mutation hooks.
- `src/models/catalog.rs`: static catalog plus persisted backend snapshots, plan allowlist derivation, and admin refresh trigger.
- `src/http/admin.rs`: scoped account/API-key/account-health/quota/cookie/admin model operations needed to operate Codex-backed GPT compatibility, including Sub2API account import normalization without proxy-pool support.

## Data Flow

For Chat Completions, the route validates the local client API key, parses the OpenAI request, validates the model against the catalog, translates to a Codex request, acquires an account, sends the request, and converts the Codex stream into OpenAI Chat output. Streaming clients receive OpenAI chat completion chunks and `[DONE]`; non-streaming clients receive a `chat.completion` object.

For Responses, the route validates the local client API key, parses native Responses fields, builds a Codex request, acquires an account, sends via WebSocket when required, and otherwise streams raw Codex SSE or collects `response.completed` for non-streaming clients.

## Error Handling

Client authentication errors stay OpenAI-compatible. Model misses return `model_not_found`. Upstream 401 triggers refresh and one retry for the same account. For non-streaming Responses, HTTP SSE Responses setup, and Chat Completions, 429 applies a quota cooldown and records the failed attempt, 402 marks quota exhausted, Cloudflare 403 applies a short cooldown, non-Cloudflare 403 marks banned, and each retryable account-state error tries another imported account when available. Retryable transport/empty-response failures can retry another account in a later Task 5 pass. Terminal upstream errors are formatted as OpenAI-compatible errors for Chat and Responses-compatible failed events for streaming Responses.

## Testing

Development follows test-first changes. Tests cover translation units, route behavior with `wiremock`, WebSocket transport boundaries, fallback account retry, rate-limit/quota state changes, model refresh cache behavior, and admin account operations. Existing full verification remains `cargo fmt --check`, `cargo test`, and `cargo clippy --all-targets --all-features --locked -- -D warnings`.
