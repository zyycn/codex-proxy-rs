# Backend Naming Architecture Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rename backend modules so every directory and file has one clear domain meaning while reserving `/admin` for the future Web console.

**Architecture:** Move the current management JSON handlers from `admin/http` to `admin/api`, keep `/v1/*` under `codex/serving`, add a `web` backend shell for future static assets, and split ambiguous support modules into domain-specific names. The work is mechanical and must not change upstream Codex behavior.

**Tech Stack:** Rust 2021, Axum, SQLx, Tokio, tracing, existing integration tests.

---

## File Structure

Create:
- `src/web/mod.rs`
- `src/web/router.rs`
- `src/web/assets.rs`
- `src/web/shell.rs`
- `src/web/security.rs`
- `src/platform/crypto/mod.rs`
- `src/platform/crypto/secret_box.rs`
- `src/platform/logging/mod.rs`
- `src/admin/api/client_keys/mod.rs`
- `src/admin/api/logs/mod.rs`
- `src/admin/session/mod.rs`
- `src/admin/client_keys/mod.rs`
- `src/codex/events/mod.rs`
- `src/codex/models/mod.rs`

Move or rename:
- `src/admin/http` to `src/admin/api`
- `src/admin/auth/repository.rs` to `src/admin/session/repository.rs`
- `src/admin/auth/service.rs` to `src/admin/session/service.rs`
- `src/admin/auth/api_key.rs` to `src/admin/client_keys/service.rs`
- `src/admin/http/api_keys.rs` to `src/admin/api/client_keys/mod.rs`
- `src/admin/http/auth.rs` to `src/admin/api/session.rs`
- `src/admin/http/logs.rs` to `src/admin/api/logs/mod.rs`
- `src/admin/http/accounts/mutate.rs` to `src/admin/api/accounts/lifecycle.rs`
- `src/codex/accounts/cf_path_block.rs` to `src/codex/accounts/cloudflare_challenge.rs`
- `src/codex/accounts/models` to `src/codex/models`
- `src/codex/accounts/repository/lease.rs` to `src/codex/accounts/repository/leases.rs`
- `src/codex/accounts/repository/quota.rs` to `src/codex/accounts/repository/quotas.rs`
- `src/codex/accounts/repository/token.rs` to `src/codex/accounts/repository/tokens.rs`
- `src/codex/accounts/service/mutation.rs` to `src/codex/accounts/service/lifecycle.rs`
- `src/codex/accounts/service/runtime_pool.rs` to `src/codex/accounts/service/pool_sync.rs`
- `src/codex/gateway/identity.rs` to `src/codex/gateway/conversation_identity.rs`
- `src/codex/gateway/installation.rs` to `src/codex/gateway/installation_id.rs`
- `src/codex/gateway/oauth/cli_import.rs` to `src/codex/gateway/oauth/codex_cli.rs`
- `src/codex/gateway/transport/client.rs` to `src/codex/gateway/transport/http_client.rs`
- `src/codex/gateway/transport/usage.rs` to `src/codex/gateway/transport/usage_events.rs`
- `src/codex/logs` to `src/codex/events`
- `src/codex/logs/rotation.rs` to `src/platform/logging/rotation.rs`
- `src/codex/tasks/model.rs` to `src/codex/tasks/model_refresh.rs`
- `src/codex/tasks/quota.rs` to `src/codex/tasks/quota_refresh.rs`
- `src/codex/tasks/refresh.rs` to `src/codex/tasks/token_refresh.rs`
- `src/codex/serving/dispatch/audit.rs` to `src/codex/serving/dispatch/stream_audit.rs`
- `src/codex/serving/dispatch/refresh.rs` to `src/codex/serving/dispatch/account_refresh.rs`
- `src/platform/crypto.rs` to `src/platform/crypto/secret_box.rs`
- `src/platform/http/middleware.rs` to `src/platform/http/request_id.rs`
- `src/platform/identity/api_key.rs` to `src/platform/identity/client_key.rs`
- `src/platform/identity/api_key_repository.rs` to `src/platform/identity/client_key_repository.rs`

## Task 1: Freeze the naming contract

**Files:**
- Create: `docs/superpowers/specs/2026-06-14-backend-naming-architecture-design.md`
- Create: `docs/superpowers/plans/2026-06-14-backend-naming-architecture.md`

- [x] **Step 1: Write the design spec**

Record route boundaries, target source tree, naming decisions, and migration policy.

- [x] **Step 2: Write this implementation plan**

List every file move and the verification gates.

## Task 2: Move management API and backend Web shell

**Files:**
- Move: `src/admin/http/**` to `src/admin/api/**`
- Create: `src/web/{mod.rs,router.rs,assets.rs,shell.rs,security.rs}`
- Modify: `src/admin/mod.rs`
- Modify: `src/runtime/router.rs`
- Test: `tests/architecture/admin_boundary.rs`

- [x] **Step 1: Move the management API directory**

Run file moves so handlers live under `admin::api`.

- [x] **Step 2: Update admin module exports**

Expose `pub mod api;`, `pub mod session;`, and `pub mod client_keys;`.

- [x] **Step 3: Add empty Web shell modules**

Add `src/web` modules with module-level documentation only. Do not serve frontend assets yet.

- [x] **Step 4: Update route mounting**

Import `crate::admin::api as admin_api` in `runtime/router.rs` and merge `admin_api::router()`.

## Task 3: Rename session and client key domains

**Files:**
- Move: `src/admin/auth/service.rs` to `src/admin/session/service.rs`
- Move: `src/admin/auth/repository.rs` to `src/admin/session/repository.rs`
- Move: `src/admin/auth/api_key.rs` to `src/admin/client_keys/service.rs`
- Move: `src/admin/api/api_keys.rs` to `src/admin/api/client_keys/mod.rs`
- Move: `src/admin/api/auth.rs` to `src/admin/api/session.rs`
- Modify: `src/runtime/state.rs`
- Modify: `src/runtime/bootstrap.rs`
- Test: `tests/architecture/admin_boundary.rs`

- [x] **Step 1: Move files into the new domains**

Keep type names stable unless a rename is required by compiler errors.

- [x] **Step 2: Replace imports**

Replace `admin::auth::service` with `admin::session::service`, `admin::auth::repository` with `admin::session::repository`, and `admin::auth::api_key` with `admin::client_keys::service`.

- [x] **Step 3: Update API router imports**

Import the API key handlers from `admin::api::client_keys` and session handlers from `admin::api::session`.

## Task 4: Rename Codex catalog, events, gateway, transport, and task modules

**Files:**
- Move the Codex files listed in the File Structure section.
- Modify: `src/codex/mod.rs`
- Modify: `src/codex/accounts/mod.rs`
- Modify: `src/codex/accounts/repository.rs`
- Modify: `src/codex/accounts/service/mod.rs`
- Modify: `src/codex/gateway/mod.rs`
- Modify: `src/codex/gateway/oauth/mod.rs`
- Modify: `src/codex/gateway/transport/mod.rs`
- Modify: `src/codex/tasks/mod.rs`
- Modify: `src/codex/serving/dispatch/mod.rs`
- Test: `tests/architecture/{accounts_boundary.rs,gateway_boundary.rs,serving_boundary.rs}`

- [x] **Step 1: Move model catalog to `codex/models`**

Update references from `codex::accounts::models` to `codex::models`.

- [x] **Step 2: Move business logs to `codex/events`**

Update references from `codex::logs` to `codex::events`.

- [x] **Step 3: Move gateway and transport files**

Update `identity`, `installation`, `cli_import`, `client`, and `usage` references to the new names.

- [x] **Step 4: Move task and dispatch helper names**

Update `model`, `quota`, `refresh`, `audit`, and dispatch refresh references to the new names.

## Task 5: Rename platform foundations

**Files:**
- Move: `src/platform/crypto.rs` to `src/platform/crypto/secret_box.rs`
- Move: `src/platform/http/middleware.rs` to `src/platform/http/request_id.rs`
- Move: `src/platform/identity/api_key.rs` to `src/platform/identity/client_key.rs`
- Move: `src/platform/identity/api_key_repository.rs` to `src/platform/identity/client_key_repository.rs`
- Modify: `src/platform/mod.rs`
- Modify: `src/platform/http/mod.rs`
- Modify: `src/platform/identity/mod.rs`
- Test: `tests/architecture/platform_boundary.rs`

- [x] **Step 1: Move files**

Create `platform::crypto::secret_box`, re-export `SecretBox`, `CryptoError`, and `CryptoResult` from `platform::crypto`.

- [x] **Step 2: Replace imports**

Update all imports of `platform::http::middleware`, `platform::identity::api_key`, and `platform::identity::api_key_repository`.

## Task 6: Update tests, docs references, and verify

**Files:**
- Modify tests under `tests/**`
- Modify current docs under `docs/api.md`, `docs/architecture-capability-plan.md`, `docs/architecture-reorganization.md`, `docs/implementation-status.md`, and this spec/plan if paths drift.

- [x] **Step 1: Update architecture tests**

Make architecture tests assert the new public module paths.

- [x] **Step 2: Update integration test imports**

Replace old paths with new paths while leaving test behavior unchanged.

- [x] **Step 3: Run formatting**

Run: `cargo fmt --check`

Expected: no formatting diff. If it fails, run `cargo fmt` and re-run `cargo fmt --check`.

- [x] **Step 4: Run tests**

Run: `cargo test --locked`

Expected: all tests pass.

- [x] **Step 5: Run Clippy**

Run: `cargo clippy --all-targets --all-features --locked -- -D warnings`

Expected: no warnings.

- [x] **Step 6: Check whitespace**

Run: `git diff --check`

Expected: no whitespace errors.
