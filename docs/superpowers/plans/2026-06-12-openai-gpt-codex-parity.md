# OpenAI GPT Codex Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port the seven OpenAI GPT/Codex parity gaps from the TypeScript reference into the Rust service without adding outbound proxy pools or non-GPT providers.

**Architecture:** Add real Chat Completions support beside Responses, expand Codex request types and transport, then harden the shared upstream lifecycle. Keep route parsing, translation, transport, account state, model catalog, and admin operations in focused modules that match the existing Rust layout.

**Tech Stack:** Rust, Axum, Reqwest/rustls, serde, sqlx/SQLite, wiremock, tokio, futures, optional WebSocket transport dependency.

**Checkpoint 2026-06-12:** Commit `b9c30ba` completed Tasks 1-3 for the in-scope OpenAI GPT/Codex path: Chat Completions route/translation/output, Responses request field parity, default Responses streaming, Codex context headers, and local-only WebSocket flag serialization cleanup. Commit `350ffbe` completed Task 4: `previous_response_id` requests use WebSocket `response.create` and return SSE-compatible output. Commit `3ecd5b6` partially completed Task 5 for non-streaming `/v1/responses`: 429/402/403 account-state classification and fallback retry across imported accounts. Commit `3960a35` extends the same fallback path to HTTP SSE Responses setup and Chat Completions. Commit `6d18d2c` completes Task 5 policy for WebSocket-backed Responses: handshake upstream errors are classified before request body send, non-history WebSocket requests may fallback accounts, successful handshakes capture rate-limit headers, and `previous_response_id` requests remain account-affine for streaming and non-streaming clients. Commit `1738f24` adds Task 6 cache groundwork: persisted backend model snapshots and cached `/v1/models*` reads. Commit `0b6cc7e` adds Task 7 import compatibility for Sub2API OpenAI OAuth exports and marks the detected source format while ignoring proxy-only fields. Commit `f75db60` adds `/admin/refresh-models` backed by imported accounts. Commit `4b19846` syncs refreshed model-plan allowlists into the runtime account pool. Commit `db6cb98` adds admin account label/status/delete mutations with runtime pool synchronization. Commit `16eb4ed` adds batch delete/status mutations. Commit `8af350d` adds local client API key status/delete mutations. Commit `203270f` adds encrypted account Cookie get/set/delete. Commit `960bbb2` adds local client API key label and batch-delete mutations. Commit `72438c6` adds native and Sub2API account export. Commit `16938e6` adds manual account creation for Codex OAuth tokens. Commit `869b6f8` narrows `POST /admin/accounts` to TS-like `token`/`refreshToken` import semantics for OpenAI GPT/Codex accounts. Commit `e1f35e0` adds deterministic Codex CLI `auth.json` import through the same validated JWT-claim path. Commit `93cfee3` adds admin health-check, per-account refresh, reset-usage, and quota routes for imported Codex accounts. Commit `bfe561d` maps HTTP SSE terminal failures to OpenAI-compatible upstream errors for non-streaming clients while preserving streaming passthrough. Commit `cf5ff0b` adds Rust-local client API key metadata export/import with target-local key rotation. Commit `a32a53c` adds admin-session-gated `/admin/auth/status` and `/admin/auth/logout`, returning sanitized account/pool state and clearing SQLite accounts plus the runtime pool without token exposure. The current device login pass adds admin-session-gated `/admin/auth/device-login` and `/admin/auth/device-poll/{device_code}` backed by the OpenAI OAuth device-code flow and shared validated account import path. Remaining major work continues with OAuth PKCE login-start/code-relay/callback.

**Scoped HTTP SSE error update:** Non-WebSocket `/v1/responses` HTTP SSE collection now detects upstream `event: error` and `event: response.failed` terminal frames. Non-streaming clients receive a `502` OpenAI-compatible `upstream_error` body with the upstream message, while streaming clients keep passthrough SSE frames and record the lifecycle event as an upstream SSE failure.

---

### Parity Drift Audit 2026-06-12

This audit is the correction point against the TypeScript reference at `/home/zyy/桌面/Codes/codex-proxy`. Only OpenAI GPT/Codex-compatible behavior backed by imported ChatGPT/Codex accounts is in scope. Outbound proxy pools, non-OpenAI provider pools, Electron/frontend/update flows, and third-party provider API key management remain non-goals.

| Area / commit | TypeScript reference behavior | Rust current behavior | Classification | Action |
| --- | --- | --- | --- | --- |
| Sub2API OpenAI OAuth import / `0b6cc7e` | Account import accepts token/refresh-token account exports and Sub2API-shaped OpenAI OAuth data through `AccountImportService`; proxy fields can exist in source data. | `/admin/accounts/import` accepts native and Sub2API OpenAI OAuth payloads, returns `sourceFormat`, and ignores proxy-only fields. | Original parity, scoped. | Keep. Use the real Sub2API export only for private smoke verification; never print or commit secrets. |
| Account label/status/delete, health, refresh, reset, quota, and batch mutations / `db6cb98`, `16eb4ed`, current Task 7 pass | `/auth/accounts/:id/label`, `/status`, delete, batch-delete, batch-status, health-check, refresh/probe, reset-usage, and quota mutate or inspect the account pool. | `/admin/accounts/*` mutates SQLite and synchronizes the runtime account pool with the admin envelope contract. Health/quota call the Codex backend through imported account tokens, refresh uses the existing OAuth token refresher, reset-usage clears local counters and pool last-used state, and quota stores normalized quota JSON. | Partial parity, adapted. | Keep. Finish login/import variants separately. |
| Account Cookie get/set/delete / `203270f` | Account routes can store browser Cookie headers for account-scoped replay. | `/admin/accounts/{id}/cookies` stores, reads, and clears encrypted per-account Cookies. | Original parity, adapted. | Keep; preserve account-scoped replay and encryption invariants. |
| Account export / `72438c6` | `/auth/accounts/export` supports `full`, `minimal`, `cockpit_tools`, `sub2api`, and `cpa`. | `/admin/accounts/export` supports native/full Rust export and Sub2API OpenAI OAuth export only, without proxy fields. | Partial parity, scoped. | Keep native and Sub2API. Treat `minimal`, `cockpit_tools`, and `cpa` as omitted unless an OpenAI/Codex operation needs them. |
| Manual account creation / `16938e6` + current correction | `POST /auth/accounts` accepts only `token` and/or `refreshToken`, runs `AccountImportService.importOne`, exchanges refresh-token-only imports, derives account identity/profile from JWT claims, updates an existing `chatgpt_account_id` + `chatgpt_user_id` entry, and never returns tokens. | `POST /admin/accounts` accepts only `token` and/or `refreshToken`, rejects missing/invalid/expired JWTs and tokens without `https://api.openai.com/auth.chatgpt_account_id`, derives email/accountId/userId/planType/expiresAt from JWT claims, ignores caller metadata/status/label, exchanges refresh-token-only imports with `AppState`'s OpenAI refresher, preserves or rotates refresh tokens, encrypts secrets, and synchronizes the runtime account pool. It does not yet cache quota from a verification probe. | Corrected scoped parity. | Keep. Finish quota fetch/health/login variants separately without adding proxy pools or non-OpenAI provider support. |
| Local client API key status/delete / `8af350d` | `/auth/api-keys` manages third-party provider key pools such as OpenAI, Anthropic, Gemini, OpenRouter/custom, plus model bindings and capabilities. | `/admin/api-keys/*` manages local `cpr_` client auth keys used only for this Rust service's `/v1/*` authorization. | Rust local extension, not TS parity. | Keep only as local admin utility if accepted. Do not count as provider API-key parity and do not add non-OpenAI provider pools. |
| Local client API key label/batch-delete / `960bbb2` | TS label/batch-delete applies to third-party provider key pool entries. | Rust label/batch-delete applies to local HMAC-hashed `cpr_` client keys. | Rust local extension, possible drift if strict parity is required. | Reclassify in docs as local admin utility. Remove only if strict original parity excludes local client-key management beyond create/list/status/delete. |
| API key export/import variants | TS can export/import third-party provider keys for reimport. | Rust local `cpr_` key plaintext is shown only at creation and then HMAC-hashed; provider key pools are not implemented. Local `/admin/api-keys/export` exports only metadata, and `/admin/api-keys/import` rotates metadata into new target-local `cpr_` keys with plaintext returned once. | Rust local extension, intentional provider scope cut. | Keep local metadata export/import as client auth utility only. Do not port TS provider key import/export, provider/model binding, proxy pools, or non-OpenAI provider behavior. |
| Model refresh and plan allowlist sync / `f75db60`, `4b19846` | TS has provider/key model catalog behavior mixed with account/provider management. | Rust probes Codex backend models via imported accounts and syncs plan allowlists into the account pool. | Partial parity, scoped Rust adaptation. | Keep; avoid third-party provider catalog behavior. Finish full Codex model metadata parity later if needed. |
| OAuth/device/CLI account login/import variants | Original scope includes ChatGPT/Codex OAuth login, device login, CLI token import, manual validated token import, logout/status, quota, and health checks. | Rust now supports validated manual `token`/`refreshToken` import, deterministic admin CLI import from Codex `auth.json`, OpenAI OAuth device login/poll, admin quota fetch, health check, per-account refresh, reset-usage, sanitized admin auth status, and admin logout for imported accounts. Rust still lacks OAuth PKCE login-start/code-relay/callback. | Partial OpenAI/Codex parity. | Continue with OAuth PKCE login variants without adding proxy pools or non-OpenAI providers. |
| Proxy/provider metadata and routes | TS project includes per-account proxy assignment, proxy health, and non-OpenAI provider routes. | Rust imports ignore proxy fields and has no outbound proxy/provider pools. | Intentional scope cut. | Keep omitted. Do not add proxy routing, proxy UI/API, Anthropic/Gemini/Ollama/custom provider support, or Electron/frontend flows. |

Immediate priority after this audit:

1. Finish in-scope account operation parity: OAuth PKCE login-start/code-relay/callback.
2. Run final verification for the completed Task 5 fallback paths, including WebSocket history affinity.
3. Keep local client API key utilities documented as Rust-local auth, not TypeScript provider-key parity.

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
- [x] Write failing tests for HTTP SSE `event: error` and `response.failed` in non-streaming collection and streaming passthrough audit.
- [x] Map non-streaming HTTP SSE `error`/`response.failed` terminal frames to OpenAI-compatible `upstream_error` bodies, and mark streaming passthrough terminal error frames as failed lifecycle logs.
- [x] Run targeted tests: `cargo test --test v1_upstream_route_test v1_responses_non_stream_should_return_upstream_error`; `cargo test --test v1_upstream_route_test v1_responses_stream_should_passthrough`
- [x] Define and implement WebSocket-backed Responses fallback policy without breaking `previous_response_id` account affinity.
- [x] Add tests for exhausted no-account responses, refresh retry preservation under fallback, and successful rate-limit header capture.
- [x] Document durable quota cooldown limitation: current SQLite account schema has no dedicated quota cooldown column, so 429 cooldown remains in-memory until a safe migration is added.
- [x] Run: `cargo test --test codex_websocket_test`
- [x] Run targeted WebSocket route tests: `cargo test --test v1_upstream_route_test v1_responses_previous_response_id_websocket_429_should_not_retry_different_account`; `cargo test --test v1_upstream_route_test v1_responses_non_stream_previous_response_id_websocket_429_should_not_retry_different_account`; `cargo test --test v1_upstream_route_test v1_responses_websocket_without_history_should_fallback_and_refresh_fallback_account`; `cargo test --test v1_upstream_route_test v1_responses_websocket_without_history_should_return_429_when_fallback_accounts_exhausted`

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
- [x] Write failing tests for batch delete/status mutations and partial not-found results.
- [x] Implement batch delete/status mutations without adding proxy-pool support.
- [x] Run: `cargo test --test admin_accounts_route_test`
- [x] Write failing tests for local client API key disable/delete mutations.
- [x] Implement local client API key disable/delete mutations without third-party provider key pools.
- [x] Run: `cargo test --test admin_api_keys_route_test`
- [x] Write failing tests for local client API key label and batch-delete mutations.
- [x] Implement local client API key label and batch-delete mutations without third-party provider key pools.
- [x] Run: `cargo test --test admin_api_keys_route_test`
- [x] Write failing tests for local client API key metadata export and rotation import.
- [x] Implement local client API key metadata export/import without plaintext export, hash export, pepper export, or third-party provider key pools.
- [x] Run: `cargo test --test admin_api_keys_route_test admin_api_keys_export_should_return_metadata_without_secret_material`; `cargo test --test admin_api_keys_route_test admin_api_keys_import_should_rotate_exported_metadata_and_return_new_plaintext_once`
- [x] Write failing tests for account Cookie get/set/delete routes.
- [x] Implement encrypted account Cookie get/set/delete routes.
- [x] Run: `cargo test --test admin_accounts_route_test`
- [x] Write failing tests for native and Sub2API account export.
- [x] Implement native and Sub2API account export without proxy fields.
- [x] Run: `cargo test --test admin_accounts_route_test`
- [x] Write failing tests for manual account creation.
- [x] Implement manual account creation without proxy/provider compatibility.
- [x] Run: `cargo test --test admin_accounts_route_test`
- [x] Write failing tests for TS-like manual `token`/`refreshToken` import semantics, JWT claim derivation, refresh-token-only exchange, duplicate account update, encrypted storage, runtime pool sync, and sanitized responses.
- [x] Correct manual account creation to TS-like `token`/`refreshToken` import semantics for OpenAI GPT/Codex accounts.
- [x] Run: `cargo test --test admin_accounts_route_test`
- [x] Write failing tests for Codex CLI `auth.json` import through admin account import.
- [x] Implement `/admin/accounts/import-cli` using a reusable CLI auth parser and the shared validated JWT-claim account import path, without frontend/proxy/provider behavior.
- [x] Run: `cargo test --test cli_auth_import_test`; `cargo test --test admin_accounts_route_test admin_accounts_import_cli_should_read_codex_auth_file_store_encrypted_and_sync_pool`
- [x] Write failing tests for admin account health-check, per-account refresh, reset-usage, and quota routes.
- [x] Implement `/admin/accounts/health-check`, `/admin/accounts/{id}/refresh`, `/admin/accounts/{id}/reset-usage`, and `/admin/accounts/{id}/quota` for imported Codex accounts only.
- [x] Run: `cargo test --test admin_accounts_route_test`
- [x] Reclassify local client API key label/batch/export/import operations as Rust-local admin utilities, and avoid treating TS provider API key export/import as in-scope parity.
- [x] Write failing tests for admin auth status/logout variants.
- [x] Implement `/admin/auth/status` and `/admin/auth/logout` as admin-session-gated account-login utilities without exposing tokens or adding public `/auth/*` compatibility routes.
- [x] Run: `cargo test --test admin_accounts_route_test`
- [x] Write failing tests for OpenAI OAuth device login/poll.
- [x] Implement `/admin/auth/device-login` and `/admin/auth/device-poll/{device_code}` with the OpenAI device-code grant and shared validated account import path.
- [x] Run: `cargo test --test admin_accounts_route_test admin_auth_device`; `cargo test --test oauth_refresh_test`; `cargo test --test admin_accounts_route_test`
- [ ] Write failing tests for OAuth PKCE login-start/code-relay/callback.
- [ ] Implement remaining OAuth PKCE scoped operations needed to operate Codex-backed OpenAI GPT routes.
- [ ] Run: `cargo test --test admin_accounts_route_test`

### Task 8: Final Verification And Docs

**Files:**
- Modify: `docs/implementation-status.md`
- Modify: `README.md` if route behavior changes

- [ ] Run: `cargo fmt --check`
- [ ] Run: `cargo test`
- [ ] Run: `cargo clippy --all-targets --all-features --locked -- -D warnings`
- [ ] Update implementation status with completed parity items and any intentionally omitted non-goals.
