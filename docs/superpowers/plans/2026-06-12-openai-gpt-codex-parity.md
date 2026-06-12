# OpenAI GPT Codex Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port the seven OpenAI GPT/Codex parity gaps from the TypeScript reference into the Rust service without adding outbound proxy pools or non-GPT providers.

**Architecture:** Add real Chat Completions support beside Responses, expand Codex request types and transport, then harden the shared upstream lifecycle. Keep route parsing, translation, transport, account state, model catalog, and admin operations in focused modules that match the existing Rust layout.

**Tech Stack:** Rust, Axum, Reqwest/rustls, serde, sqlx/SQLite, wiremock, tokio, futures, optional WebSocket transport dependency.

**Checkpoint 2026-06-12:** Commit `b9c30ba` completed Tasks 1-3 for the in-scope OpenAI GPT/Codex path: Chat Completions route/translation/output, Responses request field parity, default Responses streaming, Codex context headers, and local-only WebSocket flag serialization cleanup. Commit `350ffbe` completed Task 4: `previous_response_id` requests use WebSocket `response.create` and return SSE-compatible output. Commit `3ecd5b6` partially completed Task 5 for non-streaming `/v1/responses`: 429/402/403 account-state classification and fallback retry across imported accounts. Commit `3960a35` extends the same fallback path to HTTP SSE Responses setup and Chat Completions. Commit `1738f24` adds Task 6 cache groundwork: persisted backend model snapshots and cached `/v1/models*` reads. Commit `0b6cc7e` adds Task 7 import compatibility for Sub2API OpenAI OAuth exports and marks the detected source format while ignoring proxy-only fields. Commit `f75db60` adds `/admin/refresh-models` backed by imported accounts. Commit `4b19846` syncs refreshed model-plan allowlists into the runtime account pool. Commit `db6cb98` adds admin account label/status/delete mutations with runtime pool synchronization. Remaining major work continues with WebSocket/history fallback policy and scoped admin operations.

---

### Task 1: Chat Completions Translation

**Files:**
- Modify: `src/translation/openai_to_codex.rs`
- Modify: `src/codex/types.rs`
- Test: `tests/routes_chat_test.rs`

- [x] Write failing tests for system/developer instructions, tool calls, function outputs, image parts, response_format, reasoning effort, and service tier.
- [x] Run: `cargo test --test routes_chat_test`
- [x] Expand Chat request structs and translation helpers.
- [x] Run: `cargo test --test routes_chat_test`

### Task 2: Chat Completions Route And Output

**Files:**
- Modify: `src/app.rs`
- Modify: `src/http/v1.rs`
- Modify: `src/translation/codex_to_openai.rs`
- Test: `tests/chat_completions_route_test.rs`

- [x] Write failing route tests proving `/v1/chat/completions` sends translated Codex payloads and returns OpenAI chat JSON/SSE.
- [x] Run: `cargo test --test chat_completions_route_test`
- [x] Add a dedicated `chat_completions` handler and Codex-to-OpenAI collectors/streamers.
- [x] Run: `cargo test --test chat_completions_route_test`

### Task 3: Responses Field Parity

**Files:**
- Modify: `src/codex/types.rs`
- Modify: `src/http/v1.rs`
- Modify: `src/codex/client.rs`
- Test: `tests/responses_field_parity_test.rs`

- [x] Write failing tests for `service_tier`, `tool_choice`, `parallel_tool_calls`, `text.format`, `prompt_cache_key`, `include`, `client_metadata`, and Codex context headers.
- [x] Run: `cargo test --test v1_upstream_route_test`
- [x] Expand request parsing, serde output, and header/body forwarding.
- [x] Run: `cargo test --test v1_upstream_route_test`

### Task 4: WebSocket Previous Response Support

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/codex/websocket.rs`
- Modify: `src/codex/client.rs`
- Modify: `src/http/v1.rs`
- Test: `tests/codex_websocket_test.rs`
- Test: `tests/v1_upstream_route_test.rs`

- [x] Write failing tests proving `previous_response_id` uses WebSocket and HTTP fallback is not used when server-side history is required.
- [x] Run: `cargo test --test codex_websocket_test`
- [x] Add WebSocket response.create transport that emits SSE-compatible output.
- [x] Run: `cargo test --test codex_websocket_test`
- [x] Run: `cargo test --test v1_upstream_route_test v1_responses_should_use_websocket_for_previous_response_id_streaming`

### Task 5: Upstream Retry, Fallback, And Rate Limits

**Files:**
- Modify: `src/http/v1.rs`
- Modify: `src/accounts/pool.rs`
- Modify: `src/accounts/repository.rs`
- Test: `tests/v1_upstream_route_test.rs`
- Test: `tests/account_pool_scheduling_test.rs`

- [x] Write failing tests for non-streaming Responses 429 retry-after handling, fallback account retry, 402 quota exhaustion, 403 banned classification, and Cloudflare 403 cooldown.
- [x] Run targeted tests: `cargo test --test v1_upstream_route_test v1_responses_should_retry_next_account_after_429_retry_after`; `cargo test --test v1_upstream_route_test v1_responses_should_mark_quota_exhausted_after_402_and_retry_next_account`; `cargo test --test v1_upstream_route_test v1_responses_should_mark_banned_after_403_and_retry_next_account`; `cargo test --test v1_upstream_route_test v1_responses_should_cool_down_cloudflare_403_and_retry_next_account`
- [x] Add non-streaming Responses error classification and fallback acquire/release paths without adding proxy-pool support.
- [x] Run: `cargo test --test v1_upstream_route_test`
- [x] Run: `cargo test --test account_pool_scheduling_test`
- [x] Extend fallback classification and account retry to HTTP SSE Responses setup and Chat Completions.
- [x] Run: `cargo test --test v1_upstream_route_test`
- [x] Run: `cargo test --test chat_completions_route_test`
- [ ] Define and implement WebSocket-backed Responses fallback policy without breaking `previous_response_id` account affinity.
- [ ] Add tests for exhausted no-account responses, refresh retry preservation under fallback, successful rate-limit header capture, and durable quota cooldown persistence if needed.

### Task 6: Model Catalog Refresh

**Files:**
- Modify: `src/models/catalog.rs`
- Create: `src/models/repository.rs`
- Modify: `src/http/v1.rs`
- Test: `tests/model_catalog_test.rs`

- [x] Write failing tests for backend model snapshots, suffix parsing including `none` and `minimal`, plan allowlist generation, cached `/v1/models/catalog`, and model snapshot storage.
- [x] Run: `cargo test --test model_catalog_test`; `cargo test --test storage_schema_test`; `cargo test --test routes_responses_test model_catalog_route_returns_cached_backend_models`
- [x] Add SQLite-backed model snapshot cache and make `/v1/models*` use cached backend snapshots when present.
- [x] Write failing tests for `/admin/refresh-models`.
- [x] Add refresh route using existing imported Codex accounts, without proxy-pool support.
- [x] Sync refreshed model-plan allowlist into the runtime account pool.
- [x] Run: `cargo test --test admin_models_route_test`

### Task 7: Scoped Admin Account Operations

**Files:**
- Modify: `src/http/admin.rs`
- Modify: `src/accounts/repository.rs`
- Modify: `src/cookies/repository.rs`
- Test: `tests/admin_accounts_route_test.rs`
- Test: `tests/admin_api_keys_route_test.rs`

- [x] Write failing tests for Sub2API OpenAI OAuth import payloads and native exports that carry proxy/runtime metadata.
- [x] Run: `cargo test --test admin_accounts_route_test`
- [x] Implement Sub2API import normalization, `sourceFormat` response marking, and proxy-field ignoring.
- [x] Write failing tests for account label/status/delete mutations and runtime pool synchronization.
- [x] Implement account label/status/delete mutations without adding proxy-pool support.
- [x] Run: `cargo test --test admin_accounts_route_test`
- [ ] Write failing tests for manual add, batch status, quota fetch, health check, cookie get/set/delete, remaining import/export, API key disable/delete.
- [ ] Run: `cargo test --test admin_accounts_route_test --test admin_api_keys_route_test`
- [ ] Implement scoped admin operations needed to operate Codex-backed OpenAI GPT routes.
- [ ] Run: `cargo test --test admin_accounts_route_test --test admin_api_keys_route_test`

### Task 8: Final Verification And Docs

**Files:**
- Modify: `docs/implementation-status.md`
- Modify: `README.md` if route behavior changes

- [ ] Run: `cargo fmt --check`
- [ ] Run: `cargo test`
- [ ] Run: `cargo clippy --all-targets --all-features --locked -- -D warnings`
- [ ] Update implementation status with completed parity items and any intentionally omitted non-goals.
