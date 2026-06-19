# Architecture Migration Audit

Date: 2026-06-19

This document records the current audit state for the workspace architecture
defined in `docs/architecture.md`.

## Current Verdict

The architecture migration is no longer blocked by empty source files, empty
source directories, empty Rust function/module shells, unwired `xtask`, root
behavior tests left under `tests/`, or an unmapped baseline behavior-test suite.

Current source-level audit result:

- `codegraph sync . && codegraph status .` was rerun on 2026-06-19 and reports
  the index up to date: 285 files, 4,332 nodes, 13,241 edges.
- `tests/` now contains root architecture tests only; behavior tests are under
  `crates/*/tests`.
- Baseline root behavior-test migration is now guarded for all 46 old files
  that contain `#[test]` / `#[tokio::test]`; the 8 old root `mod` aggregators
  are not counted as behavior suites.
- Current test inventory after the latest migration pass: 495 test attributes
  across 65 Rust test files, including 43 crate-local integration test files.
- `find tests crates src -type f \( -name '*.rs' -o -name '*.toml' -o -name
  '*.md' \) -empty` returned no source/document/manifest empty files.
- `find tests crates src -type d -empty ...` returned no empty source/test
  directories.
- Placeholder/empty-shell scan in `crates src tests` only matched the
  architecture guard's own sample strings in
  `tests/architecture/placeholder_implementations.rs`.
- `crates/xtask` is wired and intentionally thin: it dispatches
  `build-web`, `check-architecture`, and `release` to real commands.

The current completion state is therefore: source migration placement and
obvious scaffold cleanup are complete by current evidence. The broad
verification gate below has passed, and older OpenAI/Codex parity documents have
been reconciled to the current crate-local coverage.

## Audit Inputs

- Baseline before the migration squash:
  `f07442a28b0c186bd22b598a53cc4856ab4b2445`
- Current audited head:
  `2aa27475723474469d442efab690f70b8f1f0fc9`
- CodeGraph:
  - `codegraph sync . && codegraph status .` run during this audit.
  - Result: synced after the latest diagnostics/trace/log-rotation/test-mapping
    migration pass.
  - Status after latest sync: 285 files, 4,332 nodes, 13,241 edges.
- Empty local directories:
  - Follow-up `find tests crates src ... -type d -empty` found no remaining
    empty source/test directories.

## 2026-06-19 Current Full Migration Audit Pass

Status: complete; source/test placement gaps found in this pass have been
implemented, and the full verification gate has passed.

New gaps found and migrated in this pass:

- Old `tests/codex_serving/diagnostics_route.rs` behavior was only partially
  represented in crate-local tests. Added
  `crates/server/tests/openai_diagnostics_routes.rs` and implemented:
  `/debug/diagnostics` path/fingerprint/capacity fields, `/debug/fingerprint`,
  `/debug/upstream`, local-only guards, and secret redaction.
- Old `tests/runtime/http_trace.rs` behavior had no clear crate-local target.
  Added `crates/server/tests/http_trace_middleware.rs`, implemented
  `server::middleware::trace::http_trace_layer()`, and mounted it in the
  server router.
- Old `tests/platform/log_rotation.rs` rolling appender behavior had no
  crate-local test. Added `crates/platform/tests/log_rotation.rs`.
- `tests/architecture/test_migration.rs` now maps all 46 old behavior files to
  current crate-local test files, instead of only a high-risk subset.

Targeted verification run in this pass:

- `cargo test -p codex-proxy-server --test openai_diagnostics_routes -- --nocapture`
  -> 4 passed.
- `cargo test -p codex-proxy-server --test http_trace_middleware -- --nocapture`
  -> 1 passed.
- `cargo test -p codex-proxy-platform --test log_rotation -- --nocapture`
  -> 2 passed.
- `cargo test --test architecture test_migration -- --nocapture`
  -> 3 passed.
- `cargo fmt --all -- --check` -> passed.
- `cargo check --workspace --tests` -> passed.
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
  -> passed after removing a `runtime` boundary violation in the diagnostics
  probe and factoring the HTTP trace layer type.
- `cargo test --workspace --tests` -> passed.
- `cargo run -p xtask -- check-architecture` -> passed; 27 architecture
  tests passed.
- `cargo doc --workspace --no-deps` -> passed.
- `cargo run -p xtask -- release` -> passed:
  `cargo fmt --all -- --check`, `cargo test --workspace --all-targets`,
  `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`,
  `pnpm install --frozen-lockfile`, and `pnpm build`.
- Final `codegraph sync . && codegraph status .` -> index up to date with 285
  files, 4,332 nodes, and 13,241 edges.
- Final empty-file and empty-directory scans across `tests crates src` returned
  no entries.
- Final placeholder/empty-shell scan across `tests crates src` only matched the
  architecture guard's own sample strings.

Current remaining work before claiming final migration completion: none found
by this pass. The migration is complete by current source, test, architecture,
documentation, and release-gate evidence.

Note: the long slice log below is retained as historical evidence. Earlier
"Blocking Findings" entries may describe states that were later closed by
subsequent slices and by this current audit pass.

## Implementation Progress

### 2026-06-18 Slice 1: Core Installation ID IO Boundary

Status: implemented and verified in this slice.

Changes:

- Added an architecture guard so `crates/core/src` fails tests if it uses
  filesystem or environment/path IO patterns: `std::fs`, `std::env`, `dirs::`,
  `std::path`, `PathBuf`, or `Path::`.
- Reworked `crates/core/src/gateway/installation.rs` into pure UUID rules:
  `generate_installation_id()` and `parse_installation_id()`.
- Moved Codex Desktop/data-dir installation ID file resolution and persistence
  to `platform::storage`.
- Removed `dirs` and `tempfile` from `codex-proxy-core` dependencies.
- Added crate-local tests for the old lookup order:
  Codex Desktop file, data-dir file, then generate-and-persist.

Verification run so far:

- `cargo test --test architecture source_imports_should_respect_layer_boundaries`
- `cargo test -p codex-proxy-platform storage::paths::tests::resolve_installation_id`
- `cargo test -p codex-proxy-core gateway::installation::tests`
- `cargo fmt --all -- --check`
- `cargo test --test architecture`
- `cargo check --workspace --all-targets`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `cargo test --workspace --all-targets`
- `codegraph sync .`

Note:

- The first full workspace test attempt exhausted disk space while writing
  `target/`. `cargo clean` removed 72.1 GiB of build artifacts, then
  `cargo test --workspace --all-targets` was rerun successfully.

Remaining after this slice:

- Runtime startup/request wiring still needs to call the new
  `platform::storage` installation ID API where the real startup path is
  restored. **Closed later in Slice 7.**
- This only closes the direct `core` IO boundary violation; it does not close
  the server entrypoint, xtask, runtime task, WebSocket, serving dispatch, or
  deleted-test migration findings.

### 2026-06-18 Slice 2: Placeholder Guard, Xtask Wiring, And Server Main

Status: implemented and targeted verification passed in this slice.

Changes:

- Added `tests/architecture/placeholder_implementations.rs` to fail when Rust
  sources contain machine-detectable placeholder markers:
  - `fn main() {}`
  - `not wired yet`
  - `后续承载`
- Replaced `crates/server/src/main.rs` empty entrypoint with a real startup
  path that:
  - loads `config.yaml`/local config through `platform::config`;
  - initializes logging when enabled;
  - connects SQLite through `platform::storage`;
  - loads/creates the master secret box and API key pepper;
  - constructs `runtime::state::AppState`;
  - serves the Axum router with `ctrl_c` graceful shutdown.
- Wired `cargo run -p xtask -- check-architecture` to actually execute
  `cargo test --test architecture`.
- Wired `cargo run -p xtask -- build-web` to run
  `pnpm install --frozen-lockfile` and `pnpm build` in `web/`.
- Wired `cargo run -p xtask -- release` to run Rust format/test/clippy gates
  and then the web build command.
- Replaced root crate docs that described future placeholder behavior with
  current responsibility descriptions.

Verification run so far:

- `cargo test --test architecture rust_sources_should_not_keep_placeholder_markers`
- `cargo check -p codex-proxy-server --all-targets`
- `cargo check -p xtask --all-targets`
- `cargo test --test architecture`
- `cargo run -p xtask -- check-architecture`
- `cargo run -p xtask -- build-web`
- `cargo run -p xtask -- release`

Note:

- The first `xtask build-web` run failed on npm registry `ECONNRESET`/fetch
  errors while `pnpm install --frozen-lockfile` was retrying. The command was
  restarted and then completed successfully, including `pnpm build`.

Remaining after this slice:

- Server startup is no longer an empty function, but it still does not prove the
  full baseline startup parity: account restoration, session-affinity
  restoration, remaining background task parity details, and installation ID
  request wiring still need explicit migration and tests. Installation ID
  request wiring was closed later in Slice 7.
- `xtask release` is now wired and has been run successfully once. It should be
  kept in the final verification gate after the remaining migration slices
  stabilize.

### 2026-06-18 Slice 3: Runtime Cleanup Task Wiring

Status: cookie/session cleanup task slices and migrated-task coordinator wiring
implemented; targeted verification passed.

Changes:

- Added `SqliteCookieStore::cleanup_expired(now)` to persistently delete
  expired account cookies.
- Added `SqliteAdminSessionStore::cleanup_expired_sessions(now)` to persistently
  delete expired admin sessions.
- Implemented `runtime::tasks::cookie_cleanup::CookieCleanupTask` with:
  - default 5-minute interval;
  - `start()` returning the shared `SchedulerHandle`;
  - `cleanup_once()` and testable `cleanup_once_at(now)`.
- Implemented `runtime::tasks::session_cleanup::SessionCleanupTask` with:
  - configured interval;
  - `start()` returning the shared `SchedulerHandle`;
  - `cleanup_once()` and testable `cleanup_once_at(now)`.
- Added crate-local runtime tests proving cleanup deletes only expired rows and
  leaves unexpired rows intact.
- Added `BackgroundTaskCoordinator::task_names()` so runtime task wiring can be
  regression-tested without depending on private handle internals.
- Added runtime task stores to `Services` so background tasks are assembled from
  the same SQLite adapters that power request/admin services.
- Wired `start_background_tasks(&AppState)` to start the migrated
  `cookie_cleanup`, `session_cleanup`, and `model_refresh` tasks.
- Wired the server entrypoint to own the background task coordinator and shut it
  down after Axum graceful shutdown returns.
- Added an architecture guard requiring the server entrypoint to start runtime
  background tasks and shut the coordinator down.

Verification run so far:

- `cargo test -p codex-proxy-runtime --test tasks cleanup_task_should_delete_only_expired`
- `cargo test -p codex-proxy-runtime --test tasks start_background_tasks_should_register_migrated_runtime_tasks`
- `cargo test --test architecture server_entrypoint_should_own_background_task_lifecycle`

Remaining after this slice:

- `token_refresh.rs` and `quota_refresh.rs` are still one-line modules.
- `start_background_tasks(&AppState)` now wires the migrated cleanup/model
  tasks, but it still does not wire token refresh, quota refresh, or fingerprint
  update.
- Startup now owns the coordinator lifetime for the wired tasks, but full
  baseline parity still depends on migrating the remaining schedulers.

### 2026-06-18 Slice 4: Token And Quota Refresh Task Migration

Status: token/quota task minimum runtime loops implemented and targeted
verification passed.

Changes:

- Added `core::accounts::jwt::jwt_expiration(token)` so runtime token refresh
  can persist refreshed access-token expiry without duplicating JWT parsing.
- Implemented `runtime::tasks::token_refresh::TokenRefreshTask` with:
  - core `RefreshScheduler` and `TokenRefresher` port usage;
  - SQLite account scanning via `SqliteAccountStore`;
  - refreshed access-token persistence through `update_from_claims`;
  - status-only persistence for scheduler status transitions;
  - periodic `start()` integration with `SchedulerHandle`.
- Added a runtime integration test proving a due account refresh writes the new
  access token, preserves the refresh token when upstream omits it, stores the
  new JWT expiry, and keeps the account active.
- Added `SqliteAccountStore::update_quota_json()` and `get_quota_json()` based
  on the old quota repository behavior.
- Migrated the old usage-to-quota snapshot normalization into
  `core::serving::quota::quota_from_usage()`.
- Implemented `runtime::tasks::quota_refresh::QuotaRefreshTask` with:
  - scan of active `quota_limit_reached` accounts;
  - Codex usage fetch through the existing `CodexBackendClient`;
  - normalized quota JSON persistence in SQLite;
  - in-process minimum refresh interval tracking for the periodic loop.
- Added runtime quota refresh test using a local mocked Codex usage endpoint.
- Extended `RuntimeConfig` with auth/quota/admin task settings.
- Exposed the assembled Codex backend client from `Services` so runtime tasks
  reuse the same adapter instance.
- Wired `start_background_tasks(&AppState)` to include `token_refresh` when
  auth refresh is enabled, and `quota_refresh` using configured quota interval.

Verification run so far:

- `cargo test -p codex-proxy-core accounts::jwt::tests::jwt_expiration_should_return_exp_as_utc_datetime`
- `cargo test -p codex-proxy-runtime --test token_refresh token_refresh_task_should_persist_refreshed_access_token_and_keep_refresh_token`
- `cargo test -p codex-proxy-adapters --test account_repository account_repository_should_update_quota_json_and_fetched_at`
- `cargo test -p codex-proxy-runtime --test quota_refresh quota_refresh_task_should_fetch_usage_for_quota_locked_accounts_and_store_quota`
- `cargo test -p codex-proxy-runtime --test tasks start_background_tasks_should_register_migrated_runtime_tasks`

Remaining after this slice:

- Token refresh is functional but not yet at full old parity: no per-account
  timer map, in-flight tracking, crash recovery states, exponential retry loop,
  or refresh lease store usage.
- Quota refresh is functional for active quota-locked accounts, but admin
  explicit quota route parity and all old quota edge-case tests are still
  incomplete.
- Server startup still needs account/session-affinity restoration.

### 2026-06-18 Slice 5: Fingerprint Update Task Wiring

Status: implemented and targeted coordinator verification passed.

Changes:

- Added `FingerprintRepository` to runtime repository/task-store assembly.
- Stored the runtime fingerprint in `Services` so background tasks use the same
  version/build values as the request clients.
- Added default runtime constants for the Codex Desktop appcast URL and optional
  `data/extracted-fingerprint.json` path.
- Wired `start_background_tasks(&AppState)` to start `fingerprint_update` with
  the assembled repository and current runtime fingerprint version/build.
- Extended the runtime coordinator test to require `fingerprint_update` in the
  task list.

Verification run so far:

- `cargo test -p codex-proxy-runtime --test tasks start_background_tasks_should_register_migrated_runtime_tasks`

Remaining after this slice:

- Fingerprint update is wired, but the broader old fingerprint update tests
  still need to be audited against current crate-local coverage.
- Server startup still needs account/session-affinity restoration.

### 2026-06-18 Slice 6: WebSocket Adapter Shell Cleanup

Status: placeholder unit structs removed; targeted adapter and architecture
verification passed.

Changes:

- Replaced `CodexWebSocketConnection` unit struct with a connection description
  that stores the WebSocket endpoint and ordered opening headers.
- Added `CodexWebSocketConnection::opening_audit_snapshot()` so opening header
  order can be audited through the core `OpeningAuditSnapshot` type.
- Replaced `CodexWebSocketPool` unit struct with a concrete pool policy holding
  `max_per_account` and `max_age`.
- Added pool policy helpers for connection capacity and age-based recycling.
- Extended the placeholder architecture guard to reject the WebSocket adapter
  unit structs if they regress.
- Added adapter tests for connection header-order preservation and pool policy.

Verification run so far:

- `cargo test -p codex-proxy-adapters --test codex websocket_`
- `cargo test --test architecture rust_sources_should_not_keep_placeholder_markers`
- `cargo test --workspace --all-targets`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`

Remaining after this slice:

- This removes the obvious WebSocket adapter shells, but it is not full
  WebSocket parity. Real `tokio-tungstenite` connect/send/receive, pooling of
  live sockets, deflate handling, and the large old WebSocket behavior tests are
  still incomplete.

### 2026-06-18 Slice 7: Installation ID Request Context Wiring

Status: implemented and targeted verification passed.

Changes:

- Added `AppState::with_pool_secret_api_key_hasher_and_installation_id()` and a
  lower-level fingerprint-aware constructor so tests and startup can inject the
  runtime installation ID without touching `core` filesystem rules.
- Updated the server entrypoint to resolve the data directory through
  `platform::storage`, load/create the installation ID there, and inject it into
  runtime state.
- Stored the optional installation ID in `Services` and threaded it through:
  - OpenAI Chat Completions dispatch;
  - OpenAI Responses dispatch;
  - admin account health probes;
  - admin model refresh requests;
  - background model refresh;
  - background quota refresh.
- Extended the pure `core::gateway::ports::CodexModelCatalogRequest` with an
  optional `installation_id` field so model catalog fetches can carry runtime
  request context without introducing IO into `core`.
- Updated the Codex adapter so auxiliary JSON requests (`usage`, model catalog,
  connectivity probes) include `x-codex-installation-id` when context provides
  it, matching the existing Responses/compact request behavior.
- Added a server route regression test proving `/v1/chat/completions` forwards
  the runtime installation ID to the mocked Codex upstream.
- Added an architecture guard requiring the server entrypoint to load the
  platform installation ID and inject it into runtime state.

Verification run so far:

- `cargo test -p codex-proxy-adapters --test codex codex_backend_client_usage_should_use_original_auxiliary_headers`
- `cargo test -p codex-proxy-server --test openai_chat_upstream chat_completions_should_forward_runtime_installation_id_to_codex`
- `cargo test -p codex-proxy-adapters --test codex`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo test -p codex-proxy-core --test models`
- `cargo test --test architecture`

Remaining after this slice:

- Server startup still needs account/session-affinity restoration.
- Full serving dispatch/fallback/recovery parity remains incomplete.
- Full live WebSocket transport/pooling/parity remains incomplete, although
  pure transport selection plus opening/payload audit snapshots are restored.

### 2026-06-18 Slice 8: Session Affinity Store And Startup Restore

Status: core/store/runtime restore path implemented and targeted verification
passed.

Changes:

- Replaced the thin `core::serving::affinity` placeholder with pure
  `SessionAffinityEntry`, `StoredSessionAffinity`, and `SessionAffinityMap`
  types.
- Implemented active-record restore, previous-response account lookup,
  conversation lookup, turn-state lookup, variant-aware latest-response lookup,
  and forget/size helpers in the pure core map.
- Implemented `SqliteSessionAffinityStore` over the existing
  `session_affinities` schema:
  - upsert response affinity with TTL-derived expiry;
  - list active records;
  - delete expired records;
  - parse persisted timestamps and function-call ID JSON.
- Added the session affinity store to runtime repository assembly.
- Added `RuntimeSessionAffinityService` with an in-memory core map guarded by
  `tokio::sync::RwLock` and restore/lookup/record APIs.
- Added `AppState::restore_session_affinity_from_repository()` and
  `restore_session_affinity_from_repository_now()`.
- Wired the server entrypoint to restore session affinity mappings from SQLite
  before background task startup and serving.
- Added an architecture guard requiring server startup to restore session
  affinity.

Verification run so far:

- `cargo test -p codex-proxy-core --test session_affinity`
- `cargo test -p codex-proxy-adapters --test session_affinity`
- `cargo test -p codex-proxy-runtime --test session_affinity`
- `cargo check -p codex-proxy-server --all-targets`
- `cargo test --test architecture`

Remaining after this slice:

- Startup still needs active account pool restoration into a runtime pool.
  **Closed later in Slice 9.**
- The runtime OpenAI dispatch path still does not use the session affinity map
  for preferred-account selection or record new response affinities; that
  belongs with the broader serving dispatch/fallback/recovery migration.
- Expired session affinity cleanup is available in the store but is not yet
  wired into a background task.

### 2026-06-18 Slice 9: Account Pool Startup Restore

Status: implemented and targeted verification passed.

Changes:

- Added `RuntimeAccountPoolService` that owns a core `AccountPool` behind a
  `tokio::sync::Mutex` and restores its contents from the existing
  `AccountStore` port.
- Mapped runtime account pool options from application config:
  max concurrent requests per account, stale slot TTL, rotation strategy,
  quota-limited skipping, and tier priority.
- Exposed account acquisition/release helpers on the runtime account pool
  service for later serving dispatch integration.
- Added `Services::account_pool` and
  `AppState::restore_account_pool_from_repository()`.
- Wired the server entrypoint to restore the runtime account pool from SQLite
  before restoring session affinity, starting background tasks, and serving.
- Added an architecture guard requiring server startup to restore the runtime
  account pool.
- Added a runtime integration test proving an active persisted account is loaded
  into the runtime pool and can be acquired.

Verification run so far:

- `cargo test -p codex-proxy-runtime --test account_pool_restore`
- `cargo check -p codex-proxy-server --all-targets`
- `cargo test --test architecture`

Remaining after this slice:

- Runtime OpenAI Chat/Responses dispatch still directly scans the account store
  and must be moved to the restored account pool. **Closed later in Slice 10.**
- Session affinity is restored but is not yet used for preferred-account
  selection or recording new response affinities.
- Serving fallback/recovery, quota transitions, implicit resume, and stream
  audit parity remain incomplete.

### 2026-06-18 Slice 10: Dispatch Uses Restored Account Pool

Status: implemented and targeted verification passed.

Changes:

- Reworked `ChatDispatchService` and `ResponseDispatchService` to acquire
  upstream accounts from `RuntimeAccountPoolService` instead of scanning
  `AccountStore` for the first active account.
- Released the account pool slot after the upstream Codex request returns,
  preserving the core pool's concurrency accounting for subsequent dispatches.
- Updated server upstream route fixtures to restore the account pool before
  exercising OpenAI routes.
- Added a regression test proving `/v1/chat/completions` dispatches from the
  restored runtime pool even if the SQLite row is changed after startup restore.

Verification run so far:

- `cargo test -p codex-proxy-server --test openai_chat_upstream`

Remaining after this slice:

- Session affinity is restored but is not yet used to set
  `preferred_account_id` for previous-response requests. **Closed later in
  Slice 11.**
- New response affinities are not yet recorded after successful upstream
  responses.
- Fallback/recovery, quota transitions, implicit resume, and stream audit parity
  remain incomplete.

### 2026-06-18 Slice 11: Session Affinity Preferred Account Dispatch

Status: implemented and targeted verification passed.

Changes:

- Extended OpenAI Responses request translation to preserve
  `previous_response_id` in the Codex request.
- Updated `ResponseDispatchService` to use restored session affinity state for
  previous-response requests:
  - look up preferred account ID and pass it to `AccountAcquireRequest`;
  - restore prompt cache key from the affinity conversation ID when absent;
  - restore turn state from affinity when the incoming request lacks it.
- Added a server regression test with two restored accounts proving
  `/v1/responses` chooses the account bound to `previous_response_id` instead of
  the default pool winner.

Verification run so far:

- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_prefer_session_affinity_account_for_previous_response`

Remaining after this slice:

- New response affinities are not yet recorded after successful upstream
  responses. **Closed later in Slice 12.**
- Fallback/recovery, quota transitions, implicit resume, and stream audit parity
  remain incomplete.

### 2026-06-18 Slice 12: Session Affinity Recording

Status: implemented and targeted verification passed.

Changes:

- Updated `ResponseDispatchService` to record a session affinity entry after a
  successful `response.completed` upstream result.
- Persisted the completed response ID, selected account ID, conversation ID,
  upstream turn state, input token count when available, and created timestamp
  through `RuntimeSessionAffinityService`.
- Kept affinity recording best-effort: persistence failures are logged and do
  not turn an otherwise successful upstream response into a 5xx.
- Added a server regression test proving a successful `/v1/responses` call
  writes the completed response ID to SQLite session affinity storage.

Verification run so far:

- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_record_session_affinity_for_completed_response`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo fmt --all -- --check`
- `cargo test --test architecture`
- `codegraph sync .`

Remaining after this slice:

- Fallback/recovery, dirty quota transitions, implicit resume edge cases, and
  stream audit parity remain incomplete.
- Expired session affinity cleanup is available in the store but is not yet
  wired into a background task. **Closed later in Slice 13.**

### 2026-06-18 Slice 13: Session Affinity Cleanup Task Wiring

Status: implemented and targeted verification passed.

Changes:

- Added `runtime::tasks::session_affinity_cleanup::SessionAffinityCleanupTask`
  with:
  - periodic `start()` integration through the shared `SchedulerHandle`;
  - `cleanup_once()` and testable `cleanup_once_at(now)`;
  - cleanup through the existing `SqliteSessionAffinityStore::delete_expired`.
- Added the SQLite session-affinity store to `BackgroundTaskStores` so
  background tasks use runtime-assembled repositories.
- Wired `start_background_tasks(&AppState)` to register
  `session_affinity_cleanup` after admin session cleanup.
- Added runtime task coverage proving only expired session-affinity rows are
  deleted and the coordinator registers the new task.
- Updated `docs/architecture.md` and architecture source-tree whitelists so the
  new task file is part of the precise runtime task layout rather than an
  out-of-band source file.

Verification run so far:

- `cargo test -p codex-proxy-runtime --test tasks session_affinity_cleanup_task_should_delete_only_expired_affinities`
- `cargo test -p codex-proxy-runtime --test tasks start_background_tasks_should_register_migrated_runtime_tasks`
- `cargo test -p codex-proxy-runtime --test tasks`
- `cargo test --test architecture`
- `codegraph sync .`

Remaining after this slice:

- Fallback/recovery, dirty quota transitions, implicit resume edge cases, and
  stream audit parity remain incomplete.
- Token refresh still lacks the old per-account timer, in-flight, retry, and
  crash-recovery behavior.
- Admin explicit quota route parity and old quota edge-case tests remain
  incomplete.

### 2026-06-18 Slice 14: Admin Explicit Account Quota Route

Status: implemented and targeted verification passed.

Changes:

- Migrated the old explicit `GET /api/admin/accounts/{account_id}/quota`
  behavior into the new `server`/`runtime` split.
- Added `AdminAccountService::account_quota()` to:
  - load the selected active account from SQLite;
  - call the Codex usage endpoint through the assembled `CodexBackendClient`;
  - normalize the usage response with `core::serving::quota::quota_from_usage`;
  - persist the normalized quota snapshot via `SqliteAccountStore`;
  - return normalized `quota` plus upstream `raw` usage without exposing stored
    token secrets.
- Added the admin HTTP handler and router entry for
  `/api/admin/accounts/{account_id}/quota`.
- Added a server regression test migrated from the old quota suite proving the
  route calls upstream with the selected account token, stores quota JSON, and
  does not return access/refresh tokens.

Verification run so far:

- `cargo test -p codex-proxy-server --test admin_accounts_routes admin_account_quota_should_fetch_usage_store_quota_and_not_return_secrets`
- `cargo test -p codex-proxy-server --test admin_accounts_routes`
- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `codegraph sync .`

Remaining after this slice:

- Old quota edge-case tests still need migration: inactive account errors,
  upstream failure mapping, quota persistence failure behavior, and quota
  warning edge cases beyond the current cached-warning test.
- Fallback/recovery, dirty quota transitions, implicit resume edge cases, and
  stream audit parity remain incomplete.
- Token refresh still lacks the old per-account timer, in-flight, retry, and
  crash-recovery behavior.

### 2026-06-18 Slice 15: Admin Account Quota Upstream Failure Shape

Status: implemented and targeted verification passed.

Changes:

- Added a server regression test for the old explicit quota route behavior when
  Codex usage fetch fails.
- Reworked the account quota HTTP handler to use a quota-specific error
  response so upstream fetch failures return:
  - HTTP `502 Bad Gateway`;
  - admin code `50201`;
  - message `Failed to fetch quota from Codex API`;
  - `data.error` containing the public upstream failure detail.
- Kept standard admin errors (`401`, `404`, inactive account conflicts, and
  persistence failures) on the normal admin error path.

Verification run so far:

- `cargo test -p codex-proxy-server --test admin_accounts_routes admin_account_quota_should_return_bad_gateway_when_usage_fetch_fails`
- `cargo test -p codex-proxy-server --test admin_accounts_routes`
- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`

Remaining after this slice:

- Old quota edge-case tests still need migration: inactive account route
  assertions, quota persistence failure behavior, and quota warning edge cases
  beyond the current cached-warning test.
- Fallback/recovery, dirty quota transitions, implicit resume edge cases, and
  stream audit parity remain incomplete.
- Token refresh still lacks the old per-account timer, in-flight, retry, and
  crash-recovery behavior.

### 2026-06-18 Slice 16: Responses Rate-Limit Account Fallback

Status: implemented and targeted verification passed.

Changes:

- Added a server regression test migrated from the old upstream fallback suite
  proving non-streaming `/v1/responses` retries the next restored runtime-pool
  account after the first account returns HTTP `429` with `retry-after`.
- Added `RuntimeAccountPoolService::mark_quota_limited_until()` so runtime
  dispatch can update the in-memory core pool instead of keeping fallback state
  in the HTTP/server layer.
- Reworked `ResponseDispatchService::complete()` to:
  - acquire accounts through the restored runtime pool with an exclusion list;
  - call Codex using the selected account;
  - release the selected account slot after each attempt;
  - on upstream `429`, mark that account quota-limited until the retry-after
    cooldown and retry with the next available account;
  - preserve the existing completed-response session affinity recording path
    for the account that actually succeeds.

Verification run so far:

- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_fallback_to_next_account_after_rate_limit`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`

Remaining after this slice:

- Fallback/recovery remains incomplete: streaming Responses fallback, Chat
  fallback, 5xx same-account retry, exhausted-account error aggregation,
  auth/cloudflare recovery, implicit resume, stream audit, and persisted dirty
  quota transitions still need migration.
- Token refresh still lacks the old per-account timer, in-flight, retry, and
  crash-recovery behavior.
- Old quota edge-case tests still need migration: inactive account route
  assertions, quota persistence failure behavior, and quota warning edge cases
  beyond the current cached-warning test.

### 2026-06-18 Slice 17: Chat Rate-Limit Account Fallback

Status: implemented and targeted verification passed.

Changes:

- Added a server regression test proving `/v1/chat/completions` retries the
  next restored runtime-pool account after the first account returns HTTP `429`
  with `retry-after`.
- Reused the runtime rate-limit fallback path for Chat dispatch:
  - acquire accounts through `RuntimeAccountPoolService`;
  - release the selected account slot after each attempt;
  - on upstream `429`, mark the account quota-limited until the retry-after
    cooldown and exclude it from the next acquisition.
- Extracted a shared runtime helper for constructing Codex response requests
  with selected account context so Chat and Responses do not drift in auxiliary
  headers, turn state, installation ID, or account ID handling.
- Stabilized the OpenAI upstream route test fixtures by moving
  `access_token_expires_at` from a same-day timestamp to a far-future timestamp;
  the fixed timestamp had started expiring during the audit run and caused
  account-pool acquisition to fail before any upstream request.

Verification run so far:

- `cargo test -p codex-proxy-server --test openai_chat_upstream chat_completions_should_fallback_to_next_account_after_rate_limit`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`

Remaining after this slice:

- Fallback/recovery remains incomplete: streaming Responses fallback, 5xx
  same-account retry, exhausted-account error aggregation,
  auth/cloudflare recovery, implicit resume, stream audit, and persisted dirty
  quota transitions still need migration. The HTTP 429 cooldown persistence
  path was closed later in Slice 18.
- Token refresh still lacks the old per-account timer, in-flight, retry, and
  crash-recovery behavior.
- Old quota edge-case tests still need migration: inactive account route
  assertions, quota persistence failure behavior, and quota warning edge cases
  beyond the current cached-warning test.

### 2026-06-18 Slice 18: Persisted Quota Cooldown Transition

Status: implemented and targeted verification passed.

Changes:

- Extended the existing Chat rate-limit fallback regression test to assert that
  the first account is persisted as quota-limited after an upstream HTTP `429`:
  `quota_limit_reached = 1` and `quota_cooldown_until` is a future timestamp.
- Added `AccountStore::mark_quota_limited_until()` to the core account-store
  port so runtime dispatch can persist quota state without depending on a
  concrete SQLite adapter.
- Implemented the new port in `SqliteAccountStore`, updating
  `quota_limit_reached`, clearing `quota_verify_required`, writing
  `quota_cooldown_until`, and refreshing `updated_at`.
- Updated `RuntimeAccountPoolService::mark_quota_limited_until()` to write the
  quota cooldown to storage and still update the in-memory account pool. Storage
  errors are logged and do not prevent the current request from retrying another
  account.
- Updated the runtime task fake account store for the expanded port.

TDD evidence:

- Before the implementation, the targeted test failed as expected at the new
  SQLite assertion with `quota_limit_reached` still `0`.
- After the implementation, the same test passed.

Verification run so far:

- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream chat_completions_should_fallback_to_next_account_after_rate_limit`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`

Remaining after this slice:

- HTTP 429 cooldown persistence is restored for the shared runtime fallback
  path used by non-streaming Chat and Responses dispatch, but fallback/recovery
  remains incomplete: streaming Responses fallback, 5xx same-account retry,
  exhausted-account error aggregation, auth/cloudflare recovery, implicit
  resume, and stream audit still need migration. The non-streaming Responses
  5xx same-account retry path was closed later in Slice 19.
- Token refresh still lacks the old per-account timer, in-flight, retry, and
  crash-recovery behavior.
- Old quota edge-case tests still need migration: inactive account route
  assertions, quota persistence failure behavior, and quota warning edge cases
  beyond the current cached-warning test.

### 2026-06-18 Slice 19: Responses 5xx Same-Account Retry

Status: implemented and targeted verification passed.

Changes:

- Migrated the old upstream fallback behavior test proving non-streaming
  `/v1/responses` retries the same account after transient upstream HTTP 5xx
  failures before falling back to another account.
- Added a Responses-specific runtime helper that retries the selected account
  for upstream HTTP 5xx responses up to two additional attempts.
- Kept HTTP 429 handling on the existing quota-cooldown fallback path; 429 still
  marks the account quota-limited and acquires the next eligible account.
- Kept Chat dispatch unchanged in this slice; the migrated old behavior covered
  non-streaming Responses.

TDD evidence:

- Before implementation, the migrated test failed with HTTP `502` and only one
  request to the primary account.
- After implementation, the same test passed and confirmed three primary
  requests with zero secondary requests.

Verification run so far:

- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_retry_same_account_after_5xx_before_fallback`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`

Remaining after this slice:

- Fallback/recovery remains incomplete: streaming Responses fallback, streaming
  5xx retry, exhausted-account error aggregation, auth/cloudflare recovery,
  implicit resume, and stream audit still need migration.
- Persisted per-request usage for selected accounts is not yet restored.
  **Closed for request_count in Slice 20; token deltas remain incomplete.**
- Token refresh still lacks the old per-account timer, in-flight, retry, and
  crash-recovery behavior.
- Old quota edge-case tests still need migration: inactive account route
  assertions, quota persistence failure behavior, and quota warning edge cases
  beyond the current cached-warning test.

### 2026-06-18 Slice 20: Persist Selected-Account Request Count

Status: implemented and targeted verification passed.

Changes:

- Migrated the old fallback test expectation that an account selected for a
  failed non-streaming `/v1/responses` request still records one persisted
  request in `account_usage`.
- Added `AccountStore::record_usage_delta()` plus a default
  `record_request()` helper to the core account-store port so runtime account
  acquisition can persist request-count usage through the same abstraction used
  by the account pool.
- Implemented `record_usage_delta()` in `SqliteAccountStore` using the existing
  `record_usage(..., UsageDelta::default())` semantics for request-count usage.
- Updated `RuntimeAccountPoolService::acquire_with()` and `acquire()` so every
  selected account records one persisted external request. Persistence failures
  are logged and do not block dispatch.
- Updated the runtime fake account store for the expanded port.

TDD evidence:

- Before implementation, the migrated test failed with `request_count = 0`.
- After implementation, the same test passed with `request_count = 1` and three
  upstream attempts on the selected account.

Verification run so far:

- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_record_request_count_when_5xx_retries_are_exhausted`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo test -p codex-proxy-runtime --test account_pool_restore`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`

Remaining after this slice:

- Request-count persistence is restored for selected accounts, including failed
  requests and multi-account fallback attempts, but persisted token deltas from
  successful Responses/Chat completions are still incomplete. **Closed for
  non-streaming Responses in Slice 21; Chat and streaming paths remain
  incomplete.**
- The old exhausted-account HTTP/status/error aggregation shape is still not
  restored; current OpenAI upstream failures still map through the current 502
  handler.
- Fallback/recovery remains incomplete: streaming Responses fallback, streaming
  5xx retry, auth/cloudflare recovery, implicit resume, and stream audit still
  need migration.
- Token refresh still lacks the old per-account timer, in-flight, retry, and
  crash-recovery behavior.
- Old quota edge-case tests still need migration: inactive account route
  assertions, quota persistence failure behavior, and quota warning edge cases
  beyond the current cached-warning test.

### 2026-06-18 Slice 21: Persist Responses Token Usage Delta

Status: implemented and targeted verification passed.

Changes:

- Extended the migrated Responses HTTP 429 fallback test with the old usage
  assertions:
  - the primary 429 account records one request and zero tokens;
  - the successful secondary account records one request plus the completed
    response input/output token usage.
- Extended `core::accounts::usage::AccountUsageDelta` with cached and image
  token fields so runtime can pass token deltas through a core-owned type.
- Reworked SQLite `UsageDelta` and `RECORD_USAGE_SQL` so `request_count` is a
  bound delta rather than a fixed `1`; this allows token-only updates with
  `request_count = 0`.
- Added conversion from core `AccountUsageDelta` into SQLite `UsageDelta` with
  saturating `u64 -> i64` conversion.
- Updated `RuntimeAccountPoolService` to persist successful Responses token
  usage after `response.completed` is parsed and to sync the in-memory account
  pool window token counters.

TDD evidence:

- Before implementation, the extended 429 fallback test failed with the
  secondary account usage row `(1, 0, 0)` instead of `(1, 5, 2)`.
- After implementation, the same test passed.

Verification run so far:

- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_fallback_to_next_account_after_rate_limit`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo test -p codex-proxy-adapters --test account_repository`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`

Remaining after this slice:

- Persisted token usage is restored for non-streaming Responses completion
  paths, but Chat completion token persistence and streaming Responses token
  persistence remain incomplete. **Chat completion token persistence closed in
  Slice 22.**
- Empty-response counters, image request success/failure counters, and old
  exhausted-account HTTP/status/error aggregation are still not restored.
- Fallback/recovery remains incomplete: streaming Responses fallback, streaming
  5xx retry, auth/cloudflare recovery, implicit resume, and stream audit still
  need migration.
- Token refresh still lacks the old per-account timer, in-flight, retry, and
  crash-recovery behavior.
- Old quota edge-case tests still need migration: inactive account route
  assertions, quota persistence failure behavior, and quota warning edge cases
  beyond the current cached-warning test.

### 2026-06-18 Slice 22: Persist Chat Token Usage Delta

Status: implemented and targeted verification passed.

Changes:

- Extended the Chat Completions upstream regression test to assert that a
  successful `/v1/chat/completions` request persists `account_usage` with one
  request plus the completed response input/output token usage.
- Updated `ChatDispatchService` to keep the selected account ID from the
  successful dispatch attempt and call the shared
  `RuntimeAccountPoolService::record_token_usage()` after the Chat SSE response
  is parsed successfully.
- Kept token persistence after successful SSE parsing so invalid or empty
  upstream responses do not record completed token deltas.

TDD evidence:

- Before implementation, the extended Chat test failed with persisted usage
  `(1, 0, 0)` instead of `(1, 9, 3)`.
- After implementation, the same test passed.

Verification run so far:

- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream chat_completions_should_dispatch_to_codex_and_return_openai_response`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`

Remaining after this slice:

- Persisted token usage is restored for non-streaming Responses and Chat
  completion paths, but streaming Responses token persistence remains
  incomplete.
- Empty-response counters, image request success/failure counters, and old
  exhausted-account HTTP/status/error aggregation are still not restored.
- Fallback/recovery remains incomplete: streaming Responses fallback, streaming
  5xx retry, auth/cloudflare recovery, implicit resume, and stream audit still
  need migration. Non-streaming Responses HTTP 402 quota fallback was closed in
  Slice 23.
- Token refresh still lacks the old per-account timer, in-flight, retry, and
  crash-recovery behavior.
- Old quota edge-case tests still need migration: inactive account route
  assertions, quota persistence failure behavior, and quota warning edge cases
  beyond the current cached-warning test.

### 2026-06-18 Slice 23: Responses 402 Quota Exhausted Fallback

Status: implemented and targeted verification passed.

Changes:

- Migrated the old upstream fallback behavior proving non-streaming
  `/v1/responses` treats upstream HTTP `402` as quota exhaustion for the
  selected account, marks that account `quota_exhausted`, and retries the next
  available account.
- Added `AccountStore::set_status()` to the core account-store port.
- Implemented the new status port in `SqliteAccountStore` by reusing the
  existing status update path.
- Added `RuntimeAccountPoolService::set_status()` to persist status changes and
  update the in-memory account pool, logging persistence failures without
  panicking.
- Updated Responses dispatch to handle upstream HTTP `402` by marking the
  selected account `QuotaExhausted`, excluding it, and continuing the fallback
  acquire loop.

TDD evidence:

- Before implementation, the migrated 402 fallback test failed with HTTP `502`
  instead of retrying the secondary account.
- After implementation, the same test passed and confirmed the primary account
  was persisted as `quota_exhausted` with one failed request recorded.

Verification run so far:

- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_mark_quota_exhausted_after_402_and_fallback`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo test -p codex-proxy-adapters --test account_repository`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`

Remaining after this slice:

- Non-streaming Responses HTTP 402 quota fallback is restored, but SSE
  `response.failed` quota classification, streaming quota fallback, and no
  fallback exhausted-account error aggregation remain incomplete. Non-streaming
  SSE quota failure fallback was closed in Slice 24.
- Auth/cloudflare recovery, implicit resume, and stream audit still need
  migration.
- Token refresh still lacks the old per-account timer, in-flight, retry, and
  crash-recovery behavior.
- Old quota edge-case tests still need migration: inactive account route
  assertions, quota persistence failure behavior, and quota warning edge cases
  beyond the current cached-warning test.

### 2026-06-18 Slice 24: Responses SSE Quota Failure Fallback

Status: implemented and targeted verification passed.

Changes:

- Migrated the old non-streaming Responses SSE failure behavior proving
  `response.failed` with a quota error marks the selected account
  `quota_exhausted` and retries the next available account.
- Moved Responses SSE collection into the per-account attempt loop so fallback
  decisions can inspect `CollectedResponse::Failed` before the dispatch result
  is finalized.
- Added quota-failure classification for SSE failures using known upstream
  codes (`quota_exceeded`, `insufficient_quota`) and quota-bearing messages.
- Preserved the existing error path for non-quota SSE failures by breaking out
  of the attempt loop and returning `ResponseDispatchError::Failed`.

TDD evidence:

- Before implementation, the migrated SSE quota failure test failed with HTTP
  `502` instead of retrying the secondary account.
- After implementation, the same test passed and confirmed the primary account
  was persisted as `quota_exhausted`.

Verification run so far:

- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_classify_sse_quota_failure_and_fallback`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`

Remaining after this slice:

- Non-streaming Responses HTTP 402 and SSE quota failure fallback are restored,
  but streaming quota fallback and no-fallback exhausted-account error
  aggregation remain incomplete.
- Chat 402 quota fallback remains incomplete. **Closed for multi-account HTTP
  402 fallback in Slice 25; no-fallback aggregation remains incomplete.**
- Auth/cloudflare recovery, implicit resume, and stream audit still need
  migration.
- Token refresh still lacks the old per-account timer, in-flight, retry, and
  crash-recovery behavior.
- Old quota edge-case tests still need migration: inactive account route
  assertions, quota persistence failure behavior, and quota warning edge cases
  beyond the current cached-warning test.

### 2026-06-18 Slice 25: Chat 402 Quota Exhausted Fallback

Status: implemented and targeted verification passed.

Changes:

- Added Chat Completions coverage proving a primary account that returns
  upstream HTTP `402` is marked `quota_exhausted` and excluded before retrying
  the next restored runtime-pool account.
- Reused the runtime status migration added for Responses quota fallback, so
  Chat and Responses persist and update the in-memory account pool consistently.
- Kept this slice scoped to multi-account fallback; old no-fallback Chat quota
  error aggregation remains open.

TDD evidence:

- Before implementation, the new Chat 402 fallback test failed with HTTP `502`.
- After implementation, the same test passed and confirmed the primary account
  was persisted as `quota_exhausted`.

Verification run so far:

- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream chat_completions_should_mark_quota_exhausted_after_402_and_fallback`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`

Remaining after this slice:

- Multi-account Chat and non-streaming Responses quota fallback paths are
  restored for HTTP 402, but no-fallback exhausted-account error aggregation is
  still incomplete. Chat HTTP 402 no-fallback aggregation was closed in Slice
  26.
- Streaming quota fallback, auth/cloudflare recovery, implicit resume, and
  stream audit still need migration.
- Token refresh still lacks the old per-account timer, in-flight, retry, and
  crash-recovery behavior.
- Old quota edge-case tests still need migration: inactive account route
  assertions, quota persistence failure behavior, and quota warning edge cases
  beyond the current cached-warning test.

### 2026-06-18 Slice 26: Chat 402 No-Fallback Quota Error

Status: implemented and targeted verification passed.

Changes:

- Migrated the old Chat no-fallback quota behavior for a single account that
  returns upstream HTTP `402`.
- Added `ChatDispatchError::QuotaExhausted` so dispatch can preserve the number
  of quota-exhausted accounts and the last upstream error body instead of
  collapsing the result to generic `NoActiveAccount`.
- Updated the Chat dispatch acquire loop to return the quota-exhausted aggregate
  once all retried accounts have been excluded by quota exhaustion.
- Mapped the new Chat error to an OpenAI-compatible HTTP `402 Payment Required`
  response with the old `All accounts exhausted (N quota-exhausted)` wording
  and `upstream_error` code.

TDD evidence:

- Before implementation, the migrated test failed with HTTP `503` instead of
  `402`.
- After implementation, the same test passed and confirmed the account was
  persisted as `quota_exhausted`.

Verification run so far:

- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream chat_completions_should_return_quota_error_when_402_fallback_is_exhausted`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`

Remaining after this slice:

- Chat HTTP 402 no-fallback aggregation is restored, but analogous Responses
  no-fallback quota aggregation remains incomplete. **Closed for Responses HTTP
  402 in Slice 27; streaming/SSE no-fallback aggregation remains incomplete.**
- Streaming quota fallback, auth/cloudflare recovery, implicit resume, and
  stream audit still need migration.
- Token refresh still lacks the old per-account timer, in-flight, retry, and
  crash-recovery behavior.
- Old quota edge-case tests still need migration: inactive account route
  assertions, quota persistence failure behavior, and quota warning edge cases
  beyond the current cached-warning test.

### 2026-06-18 Slice 27: Responses 402 No-Fallback Quota Error

Status: implemented and targeted verification passed.

Changes:

- Added non-streaming Responses coverage for a single account that returns
  upstream HTTP `402` with no fallback account available.
- Added `ResponseDispatchError::QuotaExhausted`, mirroring the Chat aggregate
  error, to preserve the quota-exhausted account count and last upstream error
  body.
- Updated the Responses dispatch acquire loop to return the quota aggregate
  after all candidates are excluded by HTTP 402 or quota-classified SSE
  failures.
- Mapped the new Responses error to HTTP `402 Payment Required` with the old
  `All accounts exhausted (N quota-exhausted)` message and `upstream_error`
  code.

TDD evidence:

- Before implementation, the migrated Responses no-fallback test failed with
  HTTP `503` instead of `402`.
- After implementation, the same test passed and confirmed the account was
  persisted as `quota_exhausted`.

Verification run so far:

- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_return_quota_error_when_402_fallback_is_exhausted`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`

Remaining after this slice:

- Chat and non-streaming Responses HTTP 402 no-fallback aggregation are
  restored, but streaming quota fallback and SSE no-fallback aggregation remain
  incomplete.
- Chat HTTP 429 no-fallback aggregation remains incomplete. **Closed in Slice
  28; analogous Responses 429 no-fallback aggregation remains incomplete.**
- Auth/cloudflare recovery, implicit resume, and stream audit still need
  migration.
- Token refresh still lacks the old per-account timer, in-flight, retry, and
  crash-recovery behavior.
- Old quota edge-case tests still need migration: inactive account route
  assertions, quota persistence failure behavior, and quota warning edge cases
  beyond the current cached-warning test.

### 2026-06-18 Slice 28: Chat 429 No-Fallback Rate-Limit Error

Status: implemented and targeted verification passed.

Changes:

- Migrated the old Chat no-fallback rate-limit behavior for a single account
  that returns upstream HTTP `429`.
- Added `ChatDispatchError::RateLimited` so Chat dispatch can preserve the count
  of rate-limited accounts and the last upstream error body instead of
  collapsing the result to generic `NoActiveAccount`.
- Updated the Chat dispatch loop to return the rate-limited aggregate after all
  candidates are excluded by HTTP 429 cooldown.
- Mapped the new Chat error to an OpenAI-compatible HTTP `429 Too Many
  Requests` response with the old `All accounts exhausted (N rate-limited)`
  wording and `upstream_error` code.
- The test also asserts the account's persisted quota cooldown state is updated.

TDD evidence:

- Before implementation, the migrated test failed with HTTP `503` instead of
  `429`.
- After implementation, the same test passed and confirmed the account was
  marked quota-limited with a cooldown timestamp.

Verification run so far:

- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream chat_completions_should_return_rate_limit_error_when_429_fallback_is_exhausted`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`

Remaining after this slice:

- Chat HTTP 429 no-fallback aggregation is restored, but analogous Responses
  HTTP 429 no-fallback aggregation remains incomplete. **Closed in Slice 29.**
- Streaming quota/rate-limit fallback, auth/cloudflare recovery, implicit
  resume, and stream audit still need migration. **The buffered HTTP SSE
  `stream: true` fallback/usage path is partially restored in Slice 30; live
  chunk-by-chunk streaming, WebSocket stream parity, and stream audit still
  remain.**
- Token refresh still lacks the old per-account timer, in-flight, retry, and
  crash-recovery behavior.
- Old quota edge-case tests still need migration: inactive account route
  assertions, quota persistence failure behavior, and quota warning edge cases
  beyond the current cached-warning test.

### 2026-06-18 Slice 29: Responses 429 No-Fallback Rate-Limit Error

Status: implemented and targeted verification passed.

Changes:

- Migrated the old Responses no-fallback rate-limit behavior for a single
  account that returns upstream HTTP `429`.
- Added `ResponseDispatchError::RateLimited`, mirroring the Chat aggregate
  error restored in Slice 28.
- Updated `ResponseDispatchService::complete()` to count HTTP 429 accounts,
  preserve the last upstream error body, and return the rate-limit aggregate
  after all candidates are excluded by cooldown.
- Mapped the new Responses error to an OpenAI-compatible HTTP `429 Too Many
  Requests` response with the old `All accounts exhausted (N rate-limited)`
  wording and `upstream_error` code.
- The regression test also asserts the account is persisted as quota-limited
  with a cooldown timestamp.

TDD evidence:

- Before implementation,
  `responses_should_return_rate_limit_error_when_429_fallback_is_exhausted`
  failed with HTTP `503` instead of `429`.
- After implementation, the same test passed and confirmed the persisted
  cooldown state.

Verification run so far:

- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_return_rate_limit_error_when_429_fallback_is_exhausted`
- `codegraph sync .`

Remaining after this slice:

- Chat and Responses HTTP 429 no-fallback aggregation are restored for
  non-streaming paths.
- Buffered HTTP SSE `stream: true` fallback and usage persistence still needed
  restoration. **Partially closed in Slice 30.**
- Auth/cloudflare recovery, implicit resume, live stream audit, WebSocket stream
  parity, and token-refresh old parity still need migration.

### 2026-06-18 Slice 30: Buffered Responses Stream Fallback And Usage

Status: implemented and targeted verification passed.

Changes:

- Added a `ResponseDispatchService::stream()` path for OpenAI
  `/v1/responses` requests with `stream: true`.
- The stream path now returns `text/event-stream` instead of collecting Codex
  SSE into a JSON Responses object.
- Restored the HTTP SSE fallback behaviors covered by the migrated tests:
  - completed SSE usage is persisted to `account_usage`;
  - upstream HTTP `429` marks the first account quota-limited, excludes it, and
    retries the next account;
  - transient upstream HTTP 5xx responses retry the same account before
    falling back;
  - upstream HTTP `402` marks the first account `QuotaExhausted`, excludes it,
    and retries the next account.
- The stream path also classifies upstream SSE `response.failed` quota failures
  for fallback using the same collector already restored for non-streaming
  Responses.
- Added server-side SSE error encoding for dispatch errors on `stream: true`.

Scope note:

- This is a buffered HTTP SSE compatibility path: the proxy returns the Codex
  SSE body with the correct OpenAI streaming content type after the upstream
  response has been read. The old live chunk-by-chunk forwarding,
  premature-close handling, heartbeat behavior, tuple stream reconversion,
  WebSocket streaming, and stream audit still need separate migration.

TDD evidence:

- The first RED run showed three new stream tests failing because the handler
  returned JSON instead of `text/event-stream`; the 5xx test was then tightened
  because it had passed by only checking the JSON body.
- The corrected RED run failed all four stream tests on the missing
  `text/event-stream` response type.
- After implementation, all four tests passed:
  `responses_stream_should_proxy_sse_and_record_usage`,
  `responses_stream_should_fallback_to_next_account_after_rate_limit`,
  `responses_stream_should_retry_same_account_after_5xx_before_fallback`, and
  `responses_stream_should_mark_quota_exhausted_after_402_and_fallback`.

Verification run so far:

- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_stream_should`
- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`

Remaining after this slice:

- Buffered HTTP SSE `stream: true` fallback and usage persistence are partially
  restored, but true live streaming and stream audit are still incomplete.
- WebSocket transport/pool streaming parity remains incomplete.
- Stream no-fallback edge cases, auth/cloudflare recovery, implicit resume,
  and token-refresh old parity still need migration. **HTTP 401/token-invalid
  account recovery is partially restored in Slice 31; Cloudflare/path-block
  recovery remains incomplete.**

### 2026-06-18 Slice 31: Auth Failure Account Recovery

Status: implemented and targeted verification passed.

Changes:

- Restored account-level recovery for upstream HTTP `401` authentication
  failures:
  - Chat Completions marks the failed account `expired`, excludes it, and
    retries the next account.
  - non-streaming Responses marks the failed account `expired`, excludes it,
    and retries the next account.
  - buffered Responses `stream: true` marks the failed account `expired`,
    excludes it, and retries the next account.
- Restored non-streaming Responses recovery for upstream SSE
  `response.failed` auth/token failures such as `token_invalid`.
- Persisted status changes through `RuntimeAccountPoolService::set_status()`,
  so SQLite and the in-memory pool stay consistent.

TDD evidence:

- Before implementation, `cargo test ... 401` failed as expected:
  Chat and non-streaming Responses returned HTTP `502` instead of falling back,
  and the stream case did not produce the fallback account usage row.
- Before implementation, `cargo test ... sse_failed_event` failed with HTTP
  `502` instead of falling back from the token-invalid SSE failure.
- After implementation, both targeted filters passed.

Verification run so far:

- `cargo test -p codex-proxy-server --test openai_chat_upstream 401`
- `cargo test -p codex-proxy-server --test openai_chat_upstream sse_failed_event`
- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`

Remaining after this slice:

- HTTP 401 and token-invalid SSE account recovery are restored for the covered
  Chat/Responses paths.
- Cloudflare/path-block recovery, auth no-fallback aggregate/error parity,
  live stream audit, WebSocket stream parity, implicit resume, and token-refresh
  old parity still need migration. **Auth no-fallback aggregate/error parity is
  restored for the covered Chat/Responses paths in Slice 32.**

### 2026-06-18 Slice 32: Auth No-Fallback Expired Aggregation

Status: implemented and targeted verification passed.

Changes:

- Added expired-account aggregate errors to Chat and Responses dispatch:
  `ChatDispatchError::Expired` and `ResponseDispatchError::Expired`.
- Dispatch now counts HTTP `401`/token-invalid accounts, preserves the last
  upstream error body, and returns an expired aggregate after all candidates are
  excluded.
- Mapped non-streaming Chat and Responses expired aggregates to HTTP `401
  Unauthorized` with `All accounts exhausted (N expired)` wording and
  `upstream_error` code.
- Mapped Responses `stream: true` expired aggregates to a `response.failed`
  SSE event with authentication error semantics.

TDD evidence:

- Before implementation,
  `cargo test -p codex-proxy-server --test openai_chat_upstream auth_error_when_401`
  failed for all three new tests:
  - Chat and non-streaming Responses returned HTTP `503` instead of `401`.
  - Responses `stream: true` returned a no-active stream error instead of the
    expired aggregate.
- After implementation, the same filter passed.

Verification run so far:

- `cargo test -p codex-proxy-server --test openai_chat_upstream auth_error_when_401`
- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`

Remaining after this slice:

- HTTP 401/token-invalid fallback and no-fallback aggregation are restored for
  the covered Chat/Responses paths.
- Cloudflare/path-block recovery, banned/deactivated account status edge cases,
  live stream audit, WebSocket stream parity, implicit resume, and token-refresh
  old parity still need migration. **HTTP 401 deactivated/banned status
  classification is restored in Slice 33.**

### 2026-06-18 Slice 33: Deactivated Auth Marks Banned

Status: implemented and targeted verification passed.

Changes:

- Added auth failure account-status classification for upstream HTTP `401`
  bodies and SSE auth failures.
- Auth failures containing `account_deactivated`, `account deactivated`,
  `account has been deactivated`, `deactivated`, or `banned` now set
  `AccountStatus::Banned`; other token auth failures still set
  `AccountStatus::Expired`.
- Covered both Chat Completions and non-streaming Responses HTTP `401`
  deactivation bodies.

TDD evidence:

- Before implementation,
  `cargo test -p codex-proxy-server --test openai_chat_upstream deactivated`
  failed because both accounts were persisted as `expired` instead of `banned`.
- After implementation, the same filter passed.

Verification run so far:

- `cargo test -p codex-proxy-server --test openai_chat_upstream deactivated`
- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`

Remaining after this slice:

- HTTP 401 token-invalid/deactivated fallback and no-fallback status handling
  are restored for the covered Chat/Responses paths.
- Cloudflare/path-block recovery, live stream audit, WebSocket stream parity,
  implicit resume, and token-refresh old parity still need migration.

### 2026-06-18 Slice 34: HTTP 403 Banned Fallback

Status: implemented and targeted verification passed.

Changes:

- Restored account-level fallback for upstream HTTP `403` bodies that signal a
  banned or deactivated account.
- Chat Completions, non-streaming Responses, and buffered Responses
  `stream: true` now mark the failed account `banned`, exclude it, and retry
  the next eligible account.
- HTTP `403` bodies without a banned/deactivated signal still use the normal
  upstream-error path; Cloudflare challenge/path-block recovery remains a
  separate migration gap.

TDD evidence:

- Before implementation,
  `cargo test -p codex-proxy-server --test openai_chat_upstream after_403`
  failed because Chat and non-streaming Responses returned HTTP `502` instead
  of falling back, and the buffered stream response did not contain the fallback
  account response id.
- After implementation, the same filter passed.

Verification run so far:

- `cargo test -p codex-proxy-server --test openai_chat_upstream after_403`
- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`

Remaining after this slice:

- HTTP 401 token-invalid/deactivated and HTTP 403 banned/deactivated account
  fallback are restored for the covered Chat/Responses paths.
- Cloudflare challenge/path-block recovery, live stream audit, WebSocket stream
  parity, implicit resume, and token-refresh old parity still need migration.

### 2026-06-18 Slice 35: Cloudflare Challenge And Path-Block Recovery

Status: implemented and targeted verification passed.

Changes:

- Added `core::accounts::cloudflare::CloudflarePathBlockTracker` for pure
  per-account path-block counting, including the old 3-hit disable threshold
  and 1-hour stale counter window.
- Extended `AccountStore` and `SqliteAccountStore` with
  `set_cloudflare_cooldown_until()` so runtime can persist Cloudflare
  cooldowns without reaching around the port boundary.
- Added a shared runtime Cloudflare recovery helper used by Chat Completions,
  non-streaming Responses, and buffered Responses `stream: true`:
  - reads stored `chatgpt.com` cookies into upstream Codex requests;
  - clears the failing account's cookies after Cloudflare challenge/path-block;
  - sets `cloudflare_cooldown_until` after HTTP `403` Cloudflare challenge;
  - counts HTTP `404` empty-body path-blocks and disables the account after
    three recent hits;
  - excludes the failed account and retries the next eligible account.
- Added Cloudflare aggregate dispatch errors and mapped them to HTTP `502`
  JSON errors for non-streaming requests and `response.failed` SSE events for
  buffered streaming requests.
- Updated the exact architecture source whitelist for
  `crates/core/src/accounts/cloudflare.rs`.

TDD evidence:

- Before implementation,
  `cargo test -p codex-proxy-server --test openai_chat_upstream cloudflare`
  failed 7/7 newly added tests: fallback cases returned HTTP `502`, cooldowns
  were not written, cookies were not cleared, path-block errors were generic,
  and the 3-hit disabled transition did not occur.
- After implementation, the same filter passed 7/7.
- The first post-implementation architecture run failed because
  `crates/core/src/accounts/cloudflare.rs` was not yet listed in the exact
  architecture shape; `docs/architecture.md` and the architecture test
  whitelist were updated, then the architecture suite passed.

Verification run so far:

- `cargo test -p codex-proxy-server --test openai_chat_upstream cloudflare`
  (RED before implementation; failed 7/7 as expected)
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream cloudflare`
- `cargo fmt --all`
- `cargo test -p codex-proxy-core cloudflare`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture` (failed once on the new source whitelist)
- `cargo test --test architecture`
- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream cloudflare`
- `cargo test -p codex-proxy-core cloudflare`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`

Remaining after this slice:

- Cloudflare challenge/path-block recovery is restored for the covered
  non-streaming Chat, non-streaming Responses, and buffered Responses
  `stream: true` paths.
- The old live chunk-by-chunk stream behavior, WebSocket stream parity, stream
  audit, implicit resume, and token-refresh old parity still need migration.

### 2026-06-18 Slice 36: Responses Model-Unsupported Fallback

Status: implemented and targeted verification passed.

Changes:

- Restored Responses account fallback for model-plan incompatibility signals:
  - upstream HTTP `400` bodies with `model_not_supported`,
    `model_not_available`, `not supported`, or `not available`;
  - upstream SSE `response.failed` events with the same model unsupported
    signals.
- Non-streaming Responses and buffered Responses `stream: true` now exclude the
  failed account and retry the next account once without changing account
  status.
- Added model-unsupported aggregate errors for exhausted fallback:
  non-streaming requests map to HTTP `400`, and buffered streaming requests map
  to a `response.failed` SSE event.

TDD evidence:

- Before implementation,
  `cargo test -p codex-proxy-server --test openai_chat_upstream model_unsupported`
  failed 4/4 newly added tests: HTTP model unsupported returned `502`, SSE
  model unsupported did not retry the next account, buffered stream did not
  contain the fallback response id, and exhausted fallback returned a generic
  stream error.
- After implementation, the same filter passed 4/4.

Verification run so far:

- `cargo test -p codex-proxy-server --test openai_chat_upstream model_unsupported`
  (RED before implementation; failed 4/4 as expected)
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream model_unsupported`
- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream model_unsupported`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`

Remaining after this slice:

- Responses model-unsupported fallback is restored for the covered
  non-streaming and buffered `stream: true` paths.
- Chat model-unsupported fallback, implicit resume/history stripping, live
  stream audit, WebSocket stream parity, and token-refresh old parity still
  need migration.

### 2026-06-18 Slice 37: Responses History Recovery

Status: implemented and targeted verification passed.

Changes:

- Added `turnState` / `turn_state` parsing to OpenAI Responses requests and
  propagation to the Codex `x-codex-turn-state` upstream header.
- Restored one-shot Responses request history recovery for upstream SSE
  `response.failed` signals:
  - `previous_response_not_found`;
  - messages containing `Previous response ... not found`;
  - messages containing `No tool output found for function call`.
- On recovery, runtime clears `previous_response_id`, `turn_state`,
  `turn_metadata`, and non-explicit `prompt_cache_key`, then retries the request
  once instead of switching accounts or changing account status.
- Covered non-streaming Responses and buffered Responses `stream: true`.

TDD evidence:

- Before implementation,
  `cargo test -p codex-proxy-server --test openai_chat_upstream strip_history`
  failed 3/3 newly added tests because only one upstream request was sent.
- After implementation and fixing the same-account WireMock fixtures to use the
  old `up_to_n_times(1)` pattern, the same filter passed 3/3.

Verification run so far:

- `cargo test -p codex-proxy-server --test openai_chat_upstream strip_history`
  (RED before implementation; failed 3/3 as expected)
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream strip_history`
- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream strip_history`
- `cargo test -p codex-proxy-core --test protocol openai_response_request_should_translate_to_codex_request`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`

Remaining after this slice:

- Responses history recovery is restored for the covered non-streaming and
  buffered `stream: true` SSE failure paths.
- HTTP upstream history errors, invalid encrypted reasoning replay recovery,
  Chat model-unsupported fallback, live stream audit, WebSocket stream parity,
  and token-refresh old parity still need migration.

### 2026-06-18 Slice 38: Chat Model Fallback And Invalid Reasoning Recovery

Status: implemented and targeted verification passed.

Changes:

- Restored Chat Completions account fallback for upstream HTTP model-plan
  incompatibility signals such as `model_not_supported`,
  `model_not_available`, `not supported`, and `not available`.
- Chat model-unsupported fallback now excludes the failed account, retries one
  alternate account once, preserves the failed account status, and maps
  exhausted fallback to OpenAI-style HTTP `400`.
- Extended Responses history recovery to recognize invalid encrypted reasoning
  replay signals:
  - upstream HTTP error bodies containing `invalid_encrypted_content`;
  - equivalent `invalid` + `encrypted` + `content` text;
  - upstream SSE `response.failed` events carrying the same signals.
- Invalid encrypted reasoning replay now bypasses generic same-account 5xx
  retry, clears `previous_response_id`, `turn_state`, `turn_metadata`, and
  non-explicit `prompt_cache_key`, then retries the stripped request once.

TDD evidence:

- Before implementation,
  `cargo test -p codex-proxy-server --test openai_chat_upstream model_unsupported`
  failed the 2 newly added Chat tests: fallback returned `502` instead of
  retrying, and exhausted fallback returned `502` instead of HTTP `400`.
- Before implementation,
  `cargo test -p codex-proxy-server --test openai_chat_upstream invalid_encrypted_reasoning`
  failed 3/3 newly added tests: SSE paths sent only one upstream request, and
  the HTTP 503 path retried without stripping `x-codex-turn-state`.
- After implementation, both filters passed.

Verification run so far:

- `cargo test -p codex-proxy-server --test openai_chat_upstream model_unsupported`
  (RED before implementation; failed the 2 new Chat tests as expected)
- `cargo test -p codex-proxy-server --test openai_chat_upstream invalid_encrypted_reasoning`
  (RED before implementation; failed 3/3 as expected)
- `cargo test -p codex-proxy-server --test openai_chat_upstream model_unsupported`
- `cargo test -p codex-proxy-server --test openai_chat_upstream invalid_encrypted_reasoning`
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo fmt --all`
- `cargo check --workspace --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `git diff --check`
- `codegraph sync . && codegraph status .`
  (synced 14 changed files; index up to date with 274 files, 3,733 nodes, and
  10,666 edges before this documentation-only update)
- `cargo test --test architecture`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo fmt --all -- --check`
- `git diff --check`

Remaining after this slice:

- Chat HTTP model-unsupported fallback is restored for the covered
  non-streaming Chat path.
- Responses invalid encrypted reasoning replay recovery is restored for covered
  HTTP error, non-streaming SSE failure, and buffered `stream: true` SSE failure
  paths.
- Live stream audit, WebSocket stream parity, implicit resume/reasoning replay
  cache parity, and token-refresh old parity still need migration.

### 2026-06-19 Slice 39: Token Refresh Lease Coordination

Status: implemented and targeted verification passed.

Changes:

- Implemented `SqliteRefreshLeaseStore` behavior on top of the existing
  `account_refresh_leases` table:
  - acquire creates a lease when absent;
  - acquire is blocked while another owner holds an unexpired lease;
  - acquire succeeds after expiry or for the same owner;
  - release only succeeds for the current owner.
- Added the refresh lease store to runtime repository assembly and
  `BackgroundTaskStores`.
- Wired the background token refresh task to use refresh leases before invoking
  the upstream refresher.
- Token refresh scans now skip a due account when another owner holds a live
  lease, and do not call the refresher for that account.

TDD evidence:

- Before implementation,
  `cargo test -p codex-proxy-adapters --test refresh_leases` failed to compile
  because `SqliteRefreshLeaseStore` had no `try_acquire` or `release` methods.
- Before implementation,
  `cargo test -p codex-proxy-runtime --test token_refresh` failed to compile
  because the lease store methods and `TokenRefreshTask::with_refresh_lease_store`
  did not exist.
- After implementation, both filters passed.

Verification run so far:

- `cargo test -p codex-proxy-adapters --test refresh_leases`
  (RED before implementation; missing lease API as expected)
- `cargo test -p codex-proxy-runtime --test token_refresh`
  (RED before implementation; missing lease API/task wiring as expected)
- `cargo test -p codex-proxy-adapters --test refresh_leases`
- `cargo test -p codex-proxy-runtime --test token_refresh`
- `codegraph sync .`
- `cargo test -p codex-proxy-runtime --test tasks`
- `cargo fmt --all`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `cargo test -p codex-proxy-adapters --test refresh_leases`
- `cargo test -p codex-proxy-runtime --test token_refresh`
- `cargo fmt --all -- --check`
- `git diff --check`

Remaining after this slice:

- Token refresh now uses the SQLite refresh lease table for cross-process
  single-owner coordination on scanned accounts.
- Token refresh still lacks the old per-account timer map, explicit in-flight
  tracking, scheduled `refreshing` crash recovery, exponential retry loop, and
  delayed recovery scheduling.
- Live stream audit, WebSocket stream parity, and implicit resume/reasoning
  replay cache parity still need migration.

### 2026-06-19 Slice 40: Token Refresh Refreshing Recovery

Status: implemented and targeted verification passed.

Changes:

- Token refresh scans now distinguish refresh candidates before taking a
  refresh lease, so non-due accounts are skipped without being marked
  `refreshing`.
- Due active accounts are persisted as `refreshing` before invoking the
  upstream token refresher, matching the old scheduled refresh state transition.
- Accounts found persisted as `refreshing` are treated as restart/crash
  recovery candidates and refreshed immediately even when their access token is
  not inside the normal pre-expiry margin.
- Transport failures during a refresh attempt now restore the persisted account
  status to `active` instead of leaving the account stuck in `refreshing`.
- Exposed a read-only core `RefreshScheduler::should_refresh_account_at` helper
  so runtime can reuse the domain refresh policy before it mutates persisted
  state.

TDD evidence:

- Before implementation,
  `cargo test -p codex-proxy-runtime --test token_refresh` failed the 3 newly
  added tests:
  - a due active account was still observed as `Active` when the refresher ran;
  - a persisted `Refreshing` account was skipped with `summary.refreshed = 0`;
  - a transport-failure attempt was still observed as `Active` instead of
    `Refreshing`.
- After implementation, the same test target passed 5/5.

Verification run so far:

- `cargo test -p codex-proxy-runtime --test token_refresh`
  (RED before implementation; failed 3/5 as expected)
- `cargo test -p codex-proxy-runtime --test token_refresh`
- `codegraph sync .`
- `cargo test -p codex-proxy-runtime --test tasks`
- `cargo fmt --all`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `cargo fmt --all -- --check`
- `git diff --check`

Remaining after this slice:

- Token refresh now covers SQLite lease coordination, refresh-before-call
  `refreshing` persistence, restart/crash recovery from persisted `refreshing`,
  and active restoration after a transport failure.
- Token refresh still lacks the old per-account timer map, explicit in-flight
  tracking, exponential retry loop, and delayed recovery scheduling.
- Live stream audit, WebSocket stream parity, and implicit resume/reasoning
  replay cache parity still need migration.

### 2026-06-19 Slice 41: Token Refresh Retry Confirmation

Status: implemented and targeted verification passed.

Changes:

- Added token-refresh retry delays with production defaults matching the old
  scheduler's five-attempt exponential retry shape:
  `5s`, `15s`, `45s`, and `135s` between attempts, capped at `300s`.
- Added a configurable `TokenRefreshTask::with_retry_delays(...)` hook so
  tests can run the retry loop with zero delays without pausing Tokio's global
  clock.
- Transport failures now retry before the account is restored to `active` and
  counted as failed.
- A transient transport failure followed by a successful refresh now persists
  the refreshed token and reports the account as refreshed.
- Permanent refresh failures represented by terminal account statuses are now
  confirmed twice before the final status is persisted, matching the old
  invalid-grant/permanent-failure confirmation behavior.
- Accounts without a refresh token still skip the retry loop and are marked
  `expired` immediately.

TDD evidence:

- Before implementation,
  `cargo test -p codex-proxy-runtime --test token_refresh` first failed because
  the new tests needed a missing `TokenRefreshTask::with_retry_delays(...)`
  hook.
- Before adding the retry implementation, the behavior expectations were also
  missing: transport failure made only one refresher call, transient success
  after a retry was not reached, and invalid-grant was persisted after one hit
  instead of two.
- After implementation, the same test target passed 7/7.

Verification run so far:

- `cargo test -p codex-proxy-runtime --test token_refresh`
  (RED before implementation; missing retry-delay hook as expected)
- `cargo test -p codex-proxy-runtime --test token_refresh`
- `codegraph sync .`
- `cargo test -p codex-proxy-runtime --test tasks`
- `cargo fmt --all`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `cargo test -p codex-proxy-runtime --test token_refresh`
- `cargo fmt --all -- --check`
- `git diff --check`

Remaining after this slice:

- Token refresh now covers SQLite lease coordination, refresh-before-call
  `refreshing` persistence, restart/crash recovery from persisted `refreshing`,
  active restoration after retry exhaustion, transient retry success, and
  two-hit permanent failure confirmation.
- Token refresh still lacks the old per-account timer map, explicit in-flight
  tracking, and delayed recovery scheduling after retry exhaustion.
- Live stream audit, WebSocket stream parity, and implicit resume/reasoning
  replay cache parity still need migration.

### 2026-06-19 Slice 42: Token Refresh In-Flight Guard

Status: implemented and targeted verification passed.

Changes:

- Added process-local in-flight tracking to `TokenRefreshTask`.
- Concurrent scans for the same account now skip the duplicate in-process
  refresh attempt instead of treating the persisted `refreshing` status as a
  crash recovery signal while the first refresh is still running.
- The in-flight guard is acquired before the SQLite refresh lease, so duplicate
  scans from the same task owner cannot acquire and then release another
  in-process refresh attempt's lease.
- The in-flight marker is released on lease-skip, lease-error, refresh success,
  refresh failure, and status-update paths.

TDD evidence:

- Before implementation,
  `cargo test -p codex-proxy-runtime --test token_refresh token_refresh_task_should_skip_duplicate_in_flight_refresh`
  failed because the second concurrent scan blocked inside the refresher instead
  of returning a skipped summary.
- After implementation, the filtered test passed, and the full token-refresh
  test target passed 8/8.

Verification run so far:

- `cargo test -p codex-proxy-runtime --test token_refresh token_refresh_task_should_skip_duplicate_in_flight_refresh`
  (RED before implementation; duplicate refresh waited in the refresher)
- `cargo test -p codex-proxy-runtime --test token_refresh token_refresh_task_should_skip_duplicate_in_flight_refresh`
- `cargo test -p codex-proxy-runtime --test token_refresh`
- `codegraph sync .`
- `cargo test -p codex-proxy-runtime --test tasks`
- `cargo fmt --all`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `cargo test -p codex-proxy-runtime --test token_refresh`
- `cargo fmt --all -- --check`
- `git diff --check`

Remaining after this slice:

- Token refresh now covers SQLite lease coordination, process-local in-flight
  protection, refresh-before-call `refreshing` persistence, restart/crash
  recovery from persisted `refreshing`, retry exhaustion restoration,
  transient retry success, and two-hit permanent failure confirmation.
- Token refresh still lacks the old per-account timer map and delayed recovery
  scheduling after retry exhaustion.
- Live stream audit, WebSocket stream parity, and implicit resume/reasoning
  replay cache parity still need migration.

### 2026-06-19 Slice 43: Token Refresh Delayed Recovery

Status: implemented and targeted verification passed.

Changes:

- Added process-local delayed recovery tracking for token refresh retry
  exhaustion.
- After all transport retry attempts are exhausted, the task restores the
  account to `active`, records a 10-minute recovery window, and skips scans for
  that account until the window expires.
- Once the recovery window expires, the next scan clears the delay and attempts
  refresh again.
- Successful refreshes, terminal status updates, and missing-refresh-token
  expiry clear any pending delayed recovery entry for that account.

TDD evidence:

- Before implementation,
  `cargo test -p codex-proxy-runtime --test token_refresh token_refresh_task_should_delay_recovery_after_retry_exhaustion`
  failed because a scan 5 minutes after retry exhaustion did not skip the
  account.
- After implementation, the filtered test passed, and the full token-refresh
  test target passed 9/9.

Verification run so far:

- `cargo test -p codex-proxy-runtime --test token_refresh token_refresh_task_should_delay_recovery_after_retry_exhaustion`
  (RED before implementation; recovery-window skip missing)
- `cargo test -p codex-proxy-runtime --test token_refresh token_refresh_task_should_delay_recovery_after_retry_exhaustion`
- `cargo test -p codex-proxy-runtime --test token_refresh`
- `codegraph sync .`
- `cargo test -p codex-proxy-runtime --test tasks`
- `cargo fmt --all`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `cargo test -p codex-proxy-runtime --test token_refresh`
- `cargo fmt --all -- --check`
- `git diff --check`

Remaining after this slice:

- Token refresh now covers SQLite lease coordination, process-local in-flight
  protection, refresh-before-call `refreshing` persistence, restart/crash
  recovery from persisted `refreshing`, retry exhaustion delayed recovery,
  transient retry success, and two-hit permanent failure confirmation.
- Token refresh still lacks the old per-account timer map; the current
  implementation remains scan-driven rather than scheduling one timer per
  account at `exp - margin`.
- Live stream audit, WebSocket stream parity, and implicit resume/reasoning
  replay cache parity still need migration.

### 2026-06-19 Slice 44: Responses Stream No-Fallback Coverage

Status: migrated tests added; existing implementation passed.

Changes:

- Added crate-local server tests for Responses `stream: true` no-fallback
  exhausted-account aggregation:
  - HTTP 429 rate-limit exhaustion returns `text/event-stream` with
    `response.failed`, `rate_limit_exceeded`, and the upstream error message;
  - HTTP 402 quota exhaustion returns `text/event-stream` with
    `response.failed`, the quota-exhausted aggregate message, and the upstream
    error message.
- The tests also verify the persisted account side effects:
  - HTTP 429 sets quota cooldown state;
  - HTTP 402 persists `quota_exhausted`.

TDD / coverage evidence:

- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_stream_should_return_`
  passed after adding the migrated tests, showing the behavior was already
  restored and the gap was missing crate-local coverage rather than missing
  production code.

Verification run so far:

- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_stream_should_return_`
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo fmt --all`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_stream_should_return_`
- `cargo fmt --all -- --check`
- `git diff --check`

Remaining after this slice:

- Responses stream no-fallback HTTP 429 and HTTP 402 aggregate error coverage is
  now migrated.
- True live chunk-by-chunk stream proxying, live stream usage audit, and
  WebSocket stream parity still need migration.
- Token refresh still lacks the old per-account timer map.

### 2026-06-19 Slice 45: Admin Quota Edge-Case Coverage

Status: migrated tests added; existing implementation passed.

Changes:

- Added crate-local server tests for explicit admin quota edge cases from the
  old admin account quota suite:
  - inactive accounts return HTTP `409` with code `40901` and do not call the
    upstream usage endpoint;
  - quota persistence failures after a successful upstream usage fetch return
    HTTP `500` with code `50001`;
  - quota warnings require an admin session cookie;
  - invalid cached quota JSON and below-threshold quota snapshots are ignored
    without producing warnings or an `updatedAt` timestamp.
- Existing happy-path and upstream-failure quota route coverage remains in the
  same crate-local test target.

TDD / coverage evidence:

- The first new edge-case run exposed a bad test input: the secondary
  `used_percent` was `89`, above the configured `80` warning threshold. After
  correcting the fixture to stay below threshold, the new edge-case tests passed.
- No production code changes were needed for these old quota route semantics;
  the gap was missing migrated coverage.

Verification run so far:

- `cargo test -p codex-proxy-server --test admin_accounts_routes admin_account_quota`
  (first run failed because the new below-threshold fixture used an above-threshold
  secondary quota value)
- `cargo test -p codex-proxy-server --test admin_accounts_routes admin_account_quota`
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test admin_accounts_routes`
- `cargo fmt --all`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `cargo test -p codex-proxy-server --test admin_accounts_routes admin_account_quota`
- `cargo fmt --all -- --check`
- `git diff --check`

Remaining after this slice:

- Explicit admin quota route happy path, upstream failure shape, inactive-account
  rejection, persistence-failure response, quota-warning auth, and quota-warning
  invalid/below-threshold edge coverage are migrated.
- Broader old quota/task behavior still needs final parity review, especially
  quota refresh task and usage/quota helper edge cases outside the explicit
  admin route.
- True live chunk-by-chunk stream proxying, live stream usage audit, WebSocket
  stream parity, and token refresh per-account timer parity still need migration.

### 2026-06-19 Slice 46: Fingerprint Runtime Startup Coverage

Status: migrated tests added; existing implementation passed.

Changes:

- Added runtime task coverage proving `start_fingerprint_update_task(...)`
  performs the initial appcast check and writes the `auto_update` fingerprint
  row through `FingerprintRepository`.
- Added coverage for matching current appcast version/build: the task checks
  appcast but does not persist an `auto_update` row when no update is available.
- Added coverage documenting the current `repository: None` semantics: the task
  still checks appcast but has no persistence target.

TDD / coverage evidence:

- `cargo test -p codex-proxy-runtime --test tasks fingerprint_update_task`
  passed after adding the migrated tests, showing the runtime wrapper already
  drove the adapter behavior and the gap was missing crate-local coverage.

Verification run so far:

- `cargo test -p codex-proxy-runtime --test tasks fingerprint_update_task`
- `codegraph sync .`
- `cargo test -p codex-proxy-runtime --test tasks fingerprint_update_task`

Remaining after this slice:

- Fingerprint update startup/update behavior from the old updater tests is now
  covered at adapter and runtime-wrapper levels.
- True live chunk-by-chunk stream proxying, live stream usage audit, WebSocket
  stream parity, and token refresh per-account timer parity still need migration.

### 2026-06-19 Slice 47: Quota Refresh Request Spacing

Status: implemented and targeted verification passed.

Changes:

- Restored the old quota refresh task's request staggering behavior for
  multiple quota-locked accounts.
- Added `QuotaRefreshTask::default_request_spacing()` with the old 3-second
  default and `with_request_spacing(...)` for fast tests.
- The refresh loop now sleeps between consecutive quota refresh candidates when
  the configured spacing is non-zero.

TDD evidence:

- Before implementation, `cargo test -p codex-proxy-runtime --test quota_refresh`
  failed to compile because `default_request_spacing()` and
  `with_request_spacing(...)` did not exist.
- The first implementation still failed the stagger behavior test because it
  used `Filter::size_hint()` to detect a remaining candidate; the lower bound
  was `0`, so the second request was sent immediately. Switching to
  `peekable()` made the test pass.

Verification run so far:

- `cargo test -p codex-proxy-runtime --test quota_refresh`
  (RED: missing request spacing API)
- `cargo test -p codex-proxy-runtime --test quota_refresh`
  (failed once: second request was not delayed)
- `cargo test -p codex-proxy-runtime --test quota_refresh`
- `codegraph sync .`
- `cargo test -p codex-proxy-runtime --test quota_refresh`

Remaining after this slice:

- The quota refresh task now covers the old scan interval shape, per-account
  minimum refresh window, active quota-locked filtering, usage fetch/persist
  path, and inter-account request spacing.
- Token refresh still lacks old per-account timer parity.
- True live chunk-by-chunk stream proxying, live stream usage audit, and
  WebSocket stream parity still need migration.

### 2026-06-19 Slice 48: Assets Static Router And Server Fallback

Status: implemented and targeted verification passed.

Changes:

- Replaced empty `crates/assets` modules with real static asset behavior:
  - cache policy helper for SPA HTML vs fingerprinted `/assets/*` files;
  - security headers including `x-content-type-options`, `x-frame-options`,
    `referrer-policy`, and CSP;
  - content type selection for common frontend artifacts;
  - Axum SPA router serving `/`, `/assets/{*path}`, and deep-link fallback.
- Added crate-local assets tests for header policy, index serving, SPA fallback,
  and cached asset serving.
- Added `codex-proxy-assets` as a server dependency and wired the assets router
  as the final fallback behind the OpenAI/admin API routes.
- Added server integration coverage proving `/` and `/assets/*` serve frontend
  files while `/api/admin/settings` still resolves to the admin API handler.
- Removed one redundant `serde_json::json!` clone in the Responses stream error
  mapping after the final workspace clippy gate exposed it.

TDD evidence:

- Before implementation, `cargo test -p codex-proxy-assets` failed because the
  assets crate had no `cache_control_for_path(...)` or `spa_router(...)`; it
  also exposed a missing `tokio` test dependency.
- After implementing the assets crate, its tests passed.
- Before server wiring, `cargo test -p codex-proxy-server --test assets_routes`
  failed because `router_with_assets(...)` did not exist.
- After adding the server dependency and fallback wiring, the server assets
  integration test passed.

Verification run so far:

- `cargo test -p codex-proxy-assets` (RED: missing API/test dependency)
- `cargo test -p codex-proxy-assets`
- `cargo test -p codex-proxy-server --test assets_routes`
  (RED: missing `router_with_assets(...)`)
- `cargo test -p codex-proxy-server --test assets_routes`
- `codegraph sync .`
- `cargo test -p codex-proxy-assets`
- `cargo test -p codex-proxy-server --test assets_routes`
- `cargo fmt --all`
- `cargo test -p codex-proxy-runtime --test tasks`
- `cargo test -p codex-proxy-runtime --test quota_refresh`
- `cargo test -p codex-proxy-assets`
- `cargo test -p codex-proxy-server --test assets_routes`
- `cargo test -p codex-proxy-server --test admin_settings_routes`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `cargo fmt --all -- --check`
- `git diff --check`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
  (failed once on an existing redundant `json!` clone in
  `crates/server/src/openai_api/responses.rs`)
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `cargo test -p codex-proxy-server --test assets_routes`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `cargo fmt --all -- --check`
- `git diff --check`

Additional source/empty-folder audit in this slice:

- `rg 'fn ... {}'` over `crates`, `src`, and `tests` found no empty source
  functions beyond the architecture placeholder test's own marker string.
- Placeholder scan for `todo!`, `unimplemented!`, `not wired yet`,
  `后续承载`, `placeholder`, and `stub` found only architecture guard/doc
  references in Rust sources.
- Empty-directory scan found only `web/node_modules/.vite-temp`, an ignored
  frontend dependency cache directory, not a migration leftover.

Remaining after this slice:

- `assets` is no longer an empty scaffold and is wired into the server router.
- True live chunk-by-chunk stream proxying, live stream usage audit, WebSocket
  stream parity, and token refresh per-account timer parity still need migration.

### 2026-06-19 Slice 49: Token Refresh Per-Account Timers

Status: implemented and targeted verification passed.

Changes:

- Restored an account-level token refresh timer map in
  `crates/runtime/src/tasks/token_refresh.rs`, replacing the scan-only loop for
  normal background scheduling.
- The scheduler now computes the planned refresh instant from
  `access_token_expires_at`/JWT exp and stores one timer per account.
- Already-due active or persisted `refreshing` accounts refresh immediately
  during scheduling, matching the old `schedule_one`/crash-recovery behavior
  instead of leaving a zero-delay stale timer behind.
- Timer callbacks use the planned trigger timestamp when asking the core
  refresh policy whether an account is due. This preserves deterministic tests
  and avoids skipping refresh if wall-clock time moves backward after a timer
  was scheduled.
- Successful scheduled refreshes immediately schedule the next per-account
  timer from the newly persisted token expiry, restoring the old
  `schedule_next_refresh` behavior.
- Existing in-flight, SQLite lease, retry, permanent-failure confirmation, and
  delayed-recovery logic remains shared by scan-triggered and timer-triggered
  refreshes.

TDD evidence:

- Before implementation, `cargo test -p codex-proxy-runtime --test token_refresh timer -- --nocapture`
  failed:
  - `token_refresh_task_should_fire_per_account_timer_at_refresh_time` timed out
    because the timer callback recomputed due-ness from real `Utc::now()` rather
    than the planned trigger timestamp.
  - `token_refresh_task_should_reschedule_next_timer_after_scheduled_refresh`
    failed with `left: 0, right: 1`, proving a successful scheduled refresh did
    not leave the next account-level timer installed.
- The first implementation attempt exposed a `Send` future compile error from
  the recursive shape `timer -> refresh -> schedule next -> timer`; boxing the
  timer scheduling future keeps the recursion out of the concrete async type.
- A second run exposed a JWT-vs-stored-expiry precision issue in the new
  sub-second timer test. The scheduler now keeps stored
  `access_token_expires_at` as the primary expiry source and only writes the new
  JWT exp into the refreshed account snapshot before scheduling the next timer.

Verification run so far:

- `codegraph sync .`
- `cargo test -p codex-proxy-runtime --test token_refresh timer -- --nocapture`
  (RED: timed-out due timer and missing next timer)
- `cargo test -p codex-proxy-runtime --test token_refresh timer -- --nocapture`
  (compile failed once on mutable `updated`, then once on recursive async
  `Send`, then once on JWT/stored-expiry precision)
- `cargo test -p codex-proxy-runtime --test token_refresh timer -- --nocapture`
- `cargo test -p codex-proxy-runtime --test token_refresh -- --nocapture`
- `cargo fmt --all`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
  (failed once on `large_enum_variant` for `TokenRefreshOutcome::Refreshed(Account)`)
- `cargo fmt --all && cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `cargo test -p codex-proxy-runtime --test token_refresh`
- `cargo test -p codex-proxy-runtime --test tasks`
- `cargo test -p codex-proxy-runtime --test quota_refresh`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `cargo fmt --all -- --check`
- `git diff --check`
- `codegraph sync . && codegraph status .`
  (synced 2 changed files; index up to date with 271 files, 3,213 nodes, and
  8,855 edges)

Remaining after this slice:

- Token refresh now covers the old per-account timer map, immediate due refresh,
  scheduled callback due-time semantics, next-refresh scheduling, process-local
  in-flight guard, SQLite lease coordination, retry, delayed recovery, and
  permanent-failure confirmation behavior.
- True live chunk-by-chunk stream proxying, live stream usage audit, and
  WebSocket stream parity still need migration.

### 2026-06-19 Slice 50: Responses Stream Done Termination

Status: implemented and targeted verification passed.

Changes:

- All Responses `stream: true` bodies emitted by the server now append the
  OpenAI compatible `data: [DONE]` SSE terminator when the body does not
  already end with one.
- This covers buffered successful upstream SSE bodies and stream errors
  produced by the proxy before a live upstream stream exists, such as no active
  account, exhausted fallback, invalid upstream response, and other
  `ResponseDispatchError` mappings.
- The change does not make the buffered HTTP SSE path a true live
  chunk-by-chunk proxy, but it closes a client-visible stream lifecycle gap:
  clients can now terminate cleanly after both buffered success and generated
  error streams.

TDD evidence:

- Before implementation,
  `cargo test -p codex-proxy-server --test openai_responses_routes generated_stream_errors -- --nocapture`
  failed because the route body ended after `event: response.failed` and did
  not include `data: [DONE]\n\n`.
- After the first fix, generated stream errors passed.
- A second RED test,
  `cargo test -p codex-proxy-server --test openai_chat_upstream responses_stream_should_proxy_sse_and_record_usage -- --nocapture`,
  proved successful buffered upstream SSE bodies also lacked `[DONE]`.
- Moving the terminator normalization into `event_stream_response(...)` made
  both generated error streams and successful buffered streams terminate
  consistently without duplicating an existing terminator.

Verification run so far:

- `cargo test -p codex-proxy-server --test openai_responses_routes generated_stream_errors -- --nocapture`
  (RED: missing `[DONE]` terminator)
- `cargo test -p codex-proxy-server --test openai_responses_routes generated_stream_errors -- --nocapture`
- `cargo test -p codex-proxy-server --test openai_responses_routes`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_stream_should_proxy_sse_and_record_usage -- --nocapture`
  (RED: successful buffered stream missing `[DONE]` terminator)
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_stream_should_proxy_sse_and_record_usage -- --nocapture`
- `cargo test -p codex-proxy-server --test openai_responses_routes generated_stream_errors -- --nocapture`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo fmt --all`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `cargo fmt --all -- --check`
- `git diff --check`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `codegraph sync . && codegraph status .`
  (synced 2 changed files; index up to date with 271 files, 3,215 nodes, and
  8,879 edges)

Remaining after this slice:

- Responses `stream: true` success and generated error bodies now terminate
  cleanly.
- True live chunk-by-chunk stream proxying, live stream usage audit, and
  WebSocket stream parity still need migration.

### 2026-06-19 Slice 51: WebSocket Transport Decision And Audit Snapshots

Status: implemented and targeted verification passed.

Changes:

- Restored the old three-state Responses transport decision in
  `core::serving::responses`:
  - default Responses requests without history are `WebSocketPreferred`;
  - requests with `previous_response_id` are `WebSocketRequired`;
  - `force_http_sse` forces `HttpSse` and allows fallback.
- Added `http_sse_fallback_allowed(...)` so the no-fallback history path is a
  first-class core policy instead of an implicit runtime/server detail.
- Added core coverage proving `use_websocket` and `force_http_sse` remain
  internal routing flags and are not serialized into upstream Codex JSON.
- Restored pure WebSocket payload audit snapshots for Responses
  `response.create` payloads, including ordered top-level key reporting and
  redaction for instructions, input, previous response ID, prompt cache key,
  client metadata, and tool definitions.
- Expanded opening audit snapshots to include a request line and redacted
  ordered headers while preserving the existing `header_order` compatibility
  field.

TDD evidence:

- Before implementation,
  `cargo test -p codex-proxy-core --test protocol codex_responses_transport -- --nocapture`
  failed to compile because `CodexTransport`, `transport_for_request(...)`, and
  `http_sse_fallback_allowed(...)` did not exist.
- After adding the transport policy, the four transport/serialization tests
  passed and the full core protocol test suite passed.
- Before payload audit implementation,
  `cargo test -p codex-proxy-core --test protocol codex_websocket_payload -- --nocapture`
  failed to compile because `websocket_payload_audit_snapshot(...)` did not
  exist.
- After implementing the snapshot builder/redaction, the payload audit test
  passed.
- Before opening audit expansion,
  `cargo test -p codex-proxy-adapters --test codex websocket_connection_opening_audit -- --nocapture`
  failed to compile because `OpeningAuditSnapshot` had no `request_line` or
  `headers` fields.
- After expanding the snapshot type and adapter builder, the opening audit test
  and existing WebSocket adapter tests passed.

Verification run so far:

- `cargo test -p codex-proxy-core --test protocol codex_responses_transport -- --nocapture`
  (RED: missing transport policy API)
- `cargo test -p codex-proxy-core --test protocol codex_responses_transport -- --nocapture`
- `cargo test -p codex-proxy-core --test protocol`
- `cargo test -p codex-proxy-core --test protocol codex_websocket_payload -- --nocapture`
  (RED: missing payload audit API)
- `cargo test -p codex-proxy-core --test protocol codex_websocket_payload -- --nocapture`
- `cargo test -p codex-proxy-core --test protocol`
- `cargo test -p codex-proxy-adapters --test codex websocket_connection_opening_audit -- --nocapture`
  (RED: missing opening audit fields)
- `cargo test -p codex-proxy-adapters --test codex websocket_connection_opening_audit -- --nocapture`
- `cargo test -p codex-proxy-adapters --test codex websocket_`
- `cargo test -p codex-proxy-core --test protocol`
- `cargo fmt --all`
- `cargo test -p codex-proxy-core --test protocol`
- `cargo test -p codex-proxy-adapters --test codex`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `cargo fmt --all -- --check`
- `git diff --check`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `codegraph sync . && codegraph status .`
  (synced 6 changed files; index up to date with 271 files, 3,242 nodes, and
  8,977 edges)

Remaining after this slice:

- WebSocket transport selection and audit snapshot primitives are no longer
  empty shells.
- Real `tokio-tungstenite` connection/opening bytes, permessage-deflate,
  ping/pong, pooled live sockets, live stream proxying, live stream audit, and
  WebSocket serving parity still need migration.

### 2026-06-19 Slice 52: WebSocket Core Codec Diagnostics

Status: implemented and targeted verification passed.

Changes:

- Migrated more of the old WebSocket codec's pure event semantics into
  `crates/core/src/protocol/codex/websocket.rs`:
  - public WebSocket event to SSE frame conversion for public JSON events;
  - `response.metadata` turn-state extraction from case-insensitive
    `x-codex-turn-state` headers, including array-valued headers;
  - terminal event detection for `response.completed`, `response.failed`, and
    `error`;
  - upstream WebSocket error-frame classification to HTTP-style status codes
    for rate limit, quota, auth, forbidden, bad request, overload, and
    WebSocket connection-limit codes;
  - retry-after extraction from wrapped WebSocket `error.headers` values;
  - `response.completed` shape validation against the old typed usage/end-turn
    expectations;
  - basic official stream-event shape diagnostics;
  - missing-required-field diagnostics for `response.created`,
    `response.output_text.delta`, `response.output_item.*`, and message output
    items.
- Kept this slice in `core` as pure JSON/protocol logic. It does not introduce
  socket IO, TLS, deflate, pooling, filesystem audit writes, or adapter
  dependencies.
- Added crate-local protocol tests for the migrated semantics instead of
  relying on the deleted root `tests/codex_gateway/websocket.rs` coverage.

TDD evidence:

- Before the first implementation batch,
  `cargo test -p codex-proxy-core --test protocol codex_websocket_ -- --nocapture`
  failed to compile because the new metadata, terminal-event, error
  classification, wrapped retry-after, and completed-response parse APIs did
  not exist.
- After the first implementation batch, the same test command found a real
  implementation bug: the `response.completed` usage field was not mapped onto
  the private deserialization struct, so the invalid-usage test failed. After
  fixing the serde rename, the filtered test passed 8/8.
- Before the second diagnostics batch, the same filtered command failed to
  compile because the event-shape and output-item diagnostic APIs did not exist.
- After implementing those diagnostics, the filtered command passed 12/12.

Verification run so far:

- `cargo test -p codex-proxy-core --test protocol codex_websocket_ -- --nocapture`
  (RED: missing first codec API batch)
- `cargo test -p codex-proxy-core --test protocol codex_websocket_ -- --nocapture`
  (GREEN after serde rename fix: 8/8 filtered tests passed)
- `cargo test -p codex-proxy-core --test protocol codex_websocket_ -- --nocapture`
  (RED: missing second diagnostics API batch)
- `cargo test -p codex-proxy-core --test protocol codex_websocket_ -- --nocapture`
  (GREEN: 12/12 filtered tests passed)
- `cargo fmt --all`
- `cargo test -p codex-proxy-core --test protocol`
  (36/36 tests passed)
- `codegraph sync .`
  (synced 2 changed files)
- `codegraph status .`
  (index up to date with 271 files, 3,289 nodes, and 9,059 edges)

Remaining after this slice:

- WebSocket codec helpers are materially less skeletal, but they are not yet
  wired into a live WebSocket response path in the new crate split.
- The remaining old codec diagnostics still need migration, especially
  incomplete-response handling, custom/function/tool/local-shell/web-search/
  image-generation/compaction item validators, and reasoning summary index
  checks.
- Real `tokio-tungstenite` connection/opening bytes, permessage-deflate,
  ping/pong, pooled live sockets, live stream proxying, live stream audit, and
  WebSocket serving parity still need migration.

### 2026-06-19 Slice 53: WebSocket Core Item Validators

Status: implemented and verification passed for the current batch.

Changes:

- Migrated the next old WebSocket codec diagnostics batch into
  `crates/core/src/protocol/codex/websocket.rs`:
  - `response.incomplete` reason extraction;
  - missing `response` detection for `response.completed`;
  - official required-field checks for custom-tool, reasoning-summary, and
    reasoning-text delta events;
  - output-item metadata validation;
  - agent-message, reasoning, function-call, function-call-output,
    custom-tool-call, custom-tool-call-output, tool-search-call,
    tool-search-output, local-shell-call, web-search-call,
    image-generation-call, compaction, and reasoning-summary-part validators.
- Added protocol tests covering each migrated validator category with concrete
  old-style bad-frame examples.
- Kept all logic in `core` as pure `serde_json::Value` inspection, so adapter
  and runtime wiring can reuse it without bringing IO or transport dependencies
  into the protocol crate.

TDD evidence:

- Before implementation,
  `cargo test -p codex-proxy-core --test protocol codex_websocket_ -- --nocapture`
  failed to compile because the new validator APIs did not exist.
- After implementation, the same filtered command passed 19/19 WebSocket tests.

Verification run so far:

- `cargo test -p codex-proxy-core --test protocol codex_websocket_ -- --nocapture`
  (RED: missing second validator batch)
- `cargo test -p codex-proxy-core --test protocol codex_websocket_ -- --nocapture`
  (GREEN: 19/19 filtered tests passed)
- `cargo fmt --all`
- `cargo test -p codex-proxy-core --test protocol`
  (43/43 tests passed)
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
  (22/22 tests passed)
- `cargo fmt --all -- --check`
- `git diff --check`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `codegraph sync .`
  (synced 2 changed files)
- `codegraph status .`
  (index up to date with 271 files, 3,324 nodes, and 9,144 edges)

Remaining after this slice:

- Most of the old codec's pure bad-frame diagnostics are now represented in
  `core`, but they are not yet wired into live WebSocket response forwarding.
- The old WebSocket request/response loop, real `tokio-tungstenite`
  connection/opening bytes, permessage-deflate, ping/pong handling, pooled live
  sockets, audit artifact IO, live stream proxying, and WebSocket serving parity
  still need migration.

### 2026-06-19 Slice 54: WebSocket Opening Request Descriptor

Status: implemented and verification passed for the current batch.

Changes:

- Added adapter-level Responses WebSocket endpoint construction:
  - `https://...` backend bases map to `wss://.../codex/responses`;
  - `http://...` backend bases map to `ws://.../codex/responses`;
  - existing backend base paths are preserved.
- Added `CodexWebSocketConnection::responses(...)` to build the standard
  opening descriptor around business headers:
  `Host`, `Connection`, `Upgrade`, `Sec-WebSocket-Version`,
  `Sec-WebSocket-Key`, caller-provided business headers, then
  `sec-websocket-extensions`.
- Added `CodexWebSocketRequest` and
  `CodexWebSocketConnection::responses_create_request(...)` so adapter code can
  hold the opening descriptor and first `response.create` text frame together.
- Exposed `websocket_response_create_payload(...)` and
  `websocket_response_create_payload_text(...)` from `core` so the adapter does
  not duplicate payload construction.
- Added adapter tests for endpoint conversion, opening header ordering,
  redaction through the existing opening audit snapshot, and response-create
  payload text generation.

TDD evidence:

- Before implementation,
  `cargo test -p codex-proxy-adapters --test codex websocket_ -- --nocapture`
  failed to compile because `responses_websocket_endpoint(...)` and
  `CodexWebSocketConnection::responses(...)` did not exist.
- After implementing the opening descriptor, the filtered adapter WebSocket
  tests passed 5/5.
- Before adding the payload descriptor implementation, the same filtered command
  failed to compile because
  `CodexWebSocketConnection::responses_create_request(...)` did not exist.
- After exposing core payload text construction and adding the request
  descriptor, the filtered adapter WebSocket tests passed 6/6.

Verification run so far:

- `cargo test -p codex-proxy-adapters --test codex websocket_ -- --nocapture`
  (RED: missing endpoint/opening descriptor API)
- `cargo test -p codex-proxy-adapters --test codex websocket_ -- --nocapture`
  (GREEN: 5/5 filtered tests passed)
- `cargo test -p codex-proxy-adapters --test codex websocket_ -- --nocapture`
  (RED: missing response-create request descriptor API)
- `cargo test -p codex-proxy-adapters --test codex websocket_ -- --nocapture`
  (GREEN: 6/6 filtered tests passed)
- `cargo fmt --all`
- `cargo test -p codex-proxy-core --test protocol`
  (43/43 tests passed)
- `cargo test -p codex-proxy-adapters --test codex`
  (23/23 tests passed)
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
  (22/22 tests passed)
- `cargo fmt --all -- --check`
- `git diff --check`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `codegraph sync .`
  (synced 3 changed files)
- `codegraph status .`
  (index up to date with 271 files, 3,337 nodes, and 9,176 edges)

Remaining after this slice:

- The adapter can now describe the opening request and first text frame, but it
  still does not open a network WebSocket, drive ping/pong, decompress frames,
  pool live sockets, or forward events into the runtime/server response path.

### 2026-06-19 Slice 55: Minimal Live WebSocket Exchange

Status: implemented and verification passed for the current batch.

Changes:

- Added `futures` and `tokio-tungstenite` to the adapters crate using existing
  workspace dependency versions.
- Added `execute_response_create_request(...)` for a prepared Responses
  WebSocket request:
  - builds a tungstenite opening request from the ordered descriptor;
  - opens the WebSocket;
  - sends the first `response.create` text frame;
  - collects public text events into SSE frames until a terminal event;
  - extracts completed usage from the collected SSE body;
  - captures metadata turn-state frames without forwarding them into SSE.
- Added `CodexWebSocketExchange` and `CodexWebSocketExchangeError`.
- Restored the old response-failed error behavior for this one-shot adapter
  path:
  - classified WebSocket `response.failed` / `error` frames through the core
    classifier;
  - surfaced HTTP-style status codes;
  - extracted retry-after from wrapped headers or official rate-limit error
    messages.
- Added adapter tests with a local WebSocket server for:
  - successful `response.completed` collection into SSE and usage extraction;
  - `response.failed` rate-limit classification into an upstream error.

TDD evidence:

- Before implementation,
  `cargo test -p codex-proxy-adapters --test codex websocket_execute_response_create_request -- --nocapture`
  failed to compile because `futures`, `tokio_tungstenite`, and
  `execute_response_create_request(...)` were missing.
- After adding the live one-shot exchange, the successful exchange test passed.
- Before adding upstream-error handling,
  `cargo test -p codex-proxy-adapters --test codex websocket_execute_response_create_request_should_surface_response_failed -- --nocapture`
  failed to compile because `CodexWebSocketExchangeError::Upstream` did not
  exist.
- After wiring core error classification and retry-after extraction, the
  response-failed test passed.

Verification run so far:

- `cargo test -p codex-proxy-adapters --test codex websocket_execute_response_create_request -- --nocapture`
  (RED: missing live dependency/function)
- `cargo test -p codex-proxy-adapters --test codex websocket_execute_response_create_request -- --nocapture`
  (GREEN: success path passed)
- `cargo test -p codex-proxy-adapters --test codex websocket_execute_response_create_request_should_surface_response_failed -- --nocapture`
  (RED: missing upstream error variant)
- `cargo test -p codex-proxy-adapters --test codex websocket_execute_response_create_request_should_surface_response_failed -- --nocapture`
  (GREEN: response-failed error path passed)
- `cargo fmt --all`
- `cargo test -p codex-proxy-core --test protocol`
  (43/43 tests passed)
- `cargo test -p codex-proxy-adapters --test codex`
  (25/25 tests passed)
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
  (22/22 tests passed)
- `cargo fmt --all -- --check`
- `git diff --check`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `codegraph sync .`
  (synced 2 changed files)
- `codegraph status .`
  (index up to date with 271 files, 3,356 nodes, and 9,224 edges)

Remaining after this slice:

- A minimal adapter-level live WebSocket exchange exists, but it is not wired
  into `CodexBackendClient`, runtime dispatch, or server routes.
- The current live adapter path is one-shot and buffered; it does not yet expose
  a streaming body, pooled live sockets, ping/pong recovery, permessage-deflate
  handling, audit artifact IO, or runtime fallback integration.

### 2026-06-19 Slice 56: Codex Backend WebSocket Required Path

Status: implemented and verification passed for the current batch.

Changes:

- Wired `CodexBackendClient::create_response(...)` through the core Responses
  transport policy:
  - `HttpSse` keeps the existing HTTP/SSE POST path;
  - `WebSocketRequired` uses the adapter live WebSocket exchange;
  - `WebSocketPreferred` attempts WebSocket and falls back to HTTP/SSE when the
    core policy allows fallback.
- Mapped WebSocket `Upstream` errors back into the existing
  `CodexClientError::Upstream` shape so runtime fallback/recovery code can keep
  matching status code and retry-after semantics.
- Added WebSocket request encoding and generic WebSocket transport errors to
  `CodexClientError`.
- Added a client-level adapter test proving a `CodexResponsesRequest` with
  `previous_response_id` opens a local WebSocket server, sends
  `response.create` with `previous_response_id`, collects `response.completed`
  into SSE, and extracts usage.

TDD evidence:

- Before implementation,
  `cargo test -p codex-proxy-adapters --test codex codex_backend_client_should_use_websocket_when_previous_response_id_is_present -- --nocapture`
  failed because the client sent an HTTP POST to the local WebSocket server. The
  server rejected the request with `Protocol(WrongHttpMethod)` and the client
  returned a reqwest incomplete-message error.
- After wiring the transport policy and WebSocket path, the same test passed.

Verification run so far:

- `cargo test -p codex-proxy-adapters --test codex codex_backend_client_should_use_websocket_when_previous_response_id_is_present -- --nocapture`
  (RED: client still used HTTP POST)
- `cargo test -p codex-proxy-adapters --test codex codex_backend_client_should_use_websocket_when_previous_response_id_is_present -- --nocapture`
  (GREEN)
- `cargo test -p codex-proxy-adapters --test codex`
  (26/26 tests passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream previous_response -- --nocapture`
  (3/3 filtered tests passed)
- `cargo test -p codex-proxy-core --test protocol`
  (43/43 tests passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
  (54/54 tests passed)
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
  (22/22 tests passed)
- `cargo fmt --all`
- `cargo fmt --all -- --check`
- `git diff --check`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `codegraph sync .`
  (synced 2 changed files)
- `codegraph status .`
  (index up to date with 271 files, 3,365 nodes, and 9,276 edges)

Remaining after this slice:

- Non-streaming `previous_response_id` requests can now take the required
  WebSocket path in the adapter client, but the runtime/server surface is still
  buffered and not a live streaming proxy.
- WebSocketPreferred fallback is wired, but full old parity still needs
  streaming body forwarding, validator integration in the live loop,
  permessage-deflate, ping/pong recovery, pooled live sockets, audit artifact
  IO, and the broader old WebSocket behavior-test migration.

### 2026-06-19 Slice 57: WebSocket Live Invalid-Event Filtering

Status: implemented and verification passed for the current batch.

Changes:

- Wired the core WebSocket bad-frame validators into the live event-to-SSE path.
- `websocket_event_to_sse_frame(...)` now skips invalid public stream events
  before encoding an SSE frame, matching the old transport behavior that
  ignored malformed/non-forwardable WebSocket events instead of forwarding them
  to clients.
- The skip decision reuses the migrated core diagnostics for metadata events,
  missing `response` objects, missing delta payloads, output item required
  fields, reasoning summary indexes, and the old agent/function/tool/local
  shell/web/image/compaction validator categories.
- Added an adapter live WebSocket regression test proving an invalid
  `response.output_text.delta` frame is dropped while a following
  `response.completed` terminal frame still completes the exchange.

TDD evidence:

- Before implementation,
  `cargo test -p codex-proxy-adapters --test codex websocket_execute_response_create_request_should_skip_invalid_stream_events -- --nocapture`
  failed because the invalid delta frame was forwarded into the buffered SSE
  body.
- After wiring the core skip decision into event-to-SSE conversion, the same
  test passed.

Verification run so far:

- `cargo test -p codex-proxy-adapters --test codex websocket_execute_response_create_request_should_skip_invalid_stream_events -- --nocapture`
  (RED: invalid delta frame was forwarded)
- `cargo test -p codex-proxy-adapters --test codex websocket_execute_response_create_request_should_skip_invalid_stream_events -- --nocapture`
  (GREEN)
- `cargo fmt --all`
- `cargo test -p codex-proxy-core --test protocol`
  (43/43 tests passed)
- `cargo test -p codex-proxy-adapters --test codex`
  (27/27 tests passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
  (54/54 tests passed)
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
  (22/22 tests passed)
- `cargo fmt --all -- --check`
- `git diff --check`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `codegraph sync .`
  (synced 2 changed files)
- `codegraph status .`
  (index up to date with 271 files, 3,367 nodes, and 9,295 edges)

Remaining after this slice:

- The live WebSocket path now avoids forwarding the migrated invalid-event
  categories, but it remains buffered and one-shot rather than a live streaming
  body proxy.
- Full old parity still needs streaming body forwarding, permessage-deflate,
  ping/pong recovery, pooled live sockets, audit artifact IO, and broader old
  WebSocket behavior test migration.

### 2026-06-19 Slice 58: WebSocket Live Control Frames And Handshake Metadata

Status: implemented and verification passed for the current batch.

Changes:

- Extended the minimal live WebSocket exchange to preserve handshake response
  metadata:
  - `x-codex-turn-state`;
  - `set-cookie`;
  - retry/rate-limit style headers.
- Mapped those handshake values through `CodexBackendClient::create_response(...)`
  so required WebSocket requests expose the same metadata shape as HTTP/SSE
  responses.
- Added live handling for `response.incomplete`, returning a typed
  `IncompleteResponse { reason }` error.
- Added live validation for malformed `response.completed` payloads using the
  core completed-response parser.
- Changed terminal handling so a skipped invalid terminal frame, such as
  `response.completed` without `response`, does not end the exchange
  successfully.
- Matched the old behavior for unclassified success-status `error` frames by
  ignoring them until a real terminal frame or close-before-terminal error.
- Added adapter tests for handshake metadata, `response.incomplete`, malformed
  `response.completed`, missing-response completed frames, success-status
  `error` frames, and client-level WebSocket metadata propagation.

TDD evidence:

- Before implementation,
  `cargo test -p codex-proxy-adapters --test codex websocket_execute_response_create_request_should_ -- --nocapture`
  failed to compile because `CodexWebSocketExchange` did not expose
  `set_cookie_headers` / `rate_limit_headers`, and
  `CodexWebSocketExchangeError` did not expose `IncompleteResponse` /
  `InvalidCompletedResponse`.
- After implementing the live-loop and metadata changes, the same filtered
  command passed 8/8 tests.
- The first full clippy run found the new `accept_hdr_async` test callback
  matched the existing large-error lint pattern. The test was annotated with a
  local `#[expect(clippy::result_large_err)]` and clippy then passed.

Verification run so far:

- `cargo test -p codex-proxy-adapters --test codex websocket_execute_response_create_request_should_ -- --nocapture`
  (RED: missing exchange fields and error variants)
- `cargo test -p codex-proxy-adapters --test codex websocket_execute_response_create_request_should_ -- --nocapture`
  (GREEN: 8/8 filtered tests passed)
- `cargo fmt --all`
- `cargo test -p codex-proxy-adapters --test codex`
  (32/32 tests passed)
- `cargo test -p codex-proxy-core --test protocol`
  (43/43 tests passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream previous_response -- --nocapture`
  (3/3 filtered tests passed)
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
  (22/22 tests passed)
- `cargo fmt --all -- --check`
- `git diff --check`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
  (54/54 tests passed)
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
  (first run failed on the test callback large-error lint; rerun passed after a
  local `#[expect]`)
- `cargo test -p codex-proxy-adapters --test codex codex_backend_client_should_use_websocket_when_previous_response_id_is_present -- --nocapture`
  (client-level WebSocket metadata propagation passed)
- `cargo test -p codex-proxy-adapters --test codex`
  (32/32 tests passed after the client-level assertion update)
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `git diff --check`
- `codegraph sync . && codegraph status .`
  (synced 5 changed files; index up to date with 272 files, 3,656 nodes, and
  10,555 edges before this documentation-only update)
- `codegraph sync . && codegraph status .`
  (synced 9 changed files; index up to date with 272 files, 3,652 nodes, and
  10,554 edges before this documentation-only update)
- `codegraph sync . && codegraph status .`
  (index up to date with 271 files, 3,378 nodes, and 9,334 edges)

Remaining after this slice:

- The one-shot live exchange now restores more old control-frame and metadata
  behavior, but it is still buffered rather than a live streaming body proxy.
- Full old parity still needs streaming body forwarding, permessage-deflate,
  ping/pong recovery, pooled live sockets, audit artifact IO, and broader old
  WebSocket behavior test migration.

### 2026-06-19 Slice 59: WebSocket Ping Coverage And Audit Artifact IO

Status: implemented and verification passed for the current batch.

Changes:

- Migrated old server-ping coverage into `crates/adapters/tests/codex.rs`:
  the local WebSocket server sends a ping before `response.completed`, waits for
  the client pong, then completes the response. The test passed with the current
  `tokio-tungstenite` read loop, so no extra ping production code was needed
  for this one-shot path.
- Restored structured WebSocket audit artifact data in core:
  - `WebSocketAuditArtifact`;
  - `WebSocketAuditErrorSnapshot`;
  - structured `WebSocketParityDiff` / `WebSocketParityDifference`;
  - `websocket_audit_artifact_from_attempt(...)`;
  - `websocket_parity_diff(...)`.
- Added explicit adapter IO for audit artifacts through
  `write_websocket_audit_artifact_for_dir(...)`, with no file writes unless a
  caller supplies a non-empty directory.
- Added environment-gated client wiring through `CODEX_PROXY_WS_AUDIT_DIR` so
  WebSocket attempts can emit the redacted opening/payload artifact when
  explicitly enabled.
- Kept artifact construction pure in `core`; filesystem writes stay in the
  adapter layer.

TDD evidence:

- Before implementation,
  `cargo test -p codex-proxy-core --test protocol codex_websocket_audit -- --nocapture`
  failed to compile because the artifact/diff types and builders did not exist.
- Before implementation,
  `cargo test -p codex-proxy-adapters --test codex websocket_audit_artifact_should_require_explicit_directory -- --nocapture`
  failed to compile because the core artifact types and adapter write function
  did not exist.
- After implementation, the core audit artifact, core parity diff, and adapter
  explicit-write tests passed.

Verification run so far:

- `cargo test -p codex-proxy-adapters --test codex websocket_execute_response_create_request_should_reply_to_server_ping_before_terminal -- --nocapture`
  (server ping coverage passed)
- `cargo test -p codex-proxy-core --test protocol codex_websocket_audit -- --nocapture`
  (RED: missing artifact/diff APIs)
- `cargo test -p codex-proxy-adapters --test codex websocket_audit_artifact_should_require_explicit_directory -- --nocapture`
  (RED: missing artifact types/write function)
- `cargo test -p codex-proxy-core --test protocol codex_websocket_audit -- --nocapture`
  (GREEN: artifact test passed)
- `cargo test -p codex-proxy-core --test protocol codex_websocket_parity -- --nocapture`
  (GREEN: parity diff test passed)
- `cargo test -p codex-proxy-adapters --test codex websocket_audit_artifact_should_require_explicit_directory -- --nocapture`
  (GREEN)
- `cargo test -p codex-proxy-core --test protocol codex_websocket_ -- --nocapture`
  (21/21 filtered WebSocket tests passed)
- `cargo test -p codex-proxy-adapters --test codex codex_backend_client_should_use_websocket_when_previous_response_id_is_present -- --nocapture`
  (client WebSocket path passed with audit wiring present)
- `cargo fmt --all`
- `cargo test -p codex-proxy-core --test protocol`
  (45/45 tests passed)
- `cargo test -p codex-proxy-adapters --test codex`
  (34/34 tests passed)
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
  (22/22 tests passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
  (54/54 tests passed)
- `cargo fmt --all -- --check`
- `git diff --check`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `codegraph sync . && codegraph status .`
  (index up to date with 271 files, 3,404 nodes, and 9,433 edges)

Remaining after this slice:

- The one-shot live WebSocket path now has server-ping response coverage and
  environment-gated audit artifact IO.
- It is still buffered rather than a live streaming body proxy.
- Full old parity still needs streaming body forwarding, live
  permessage-deflate integration, pooled socket reuse/keepalive, and broader
  old WebSocket behavior test migration.

### 2026-06-19 Slice 60: WebSocket Deflate Frame Rewriter

Status: implemented and verification passed for the current batch.

Changes:

- Added `flate2` to `codex-proxy-adapters`.
- Expanded `crates/adapters/src/codex/websocket/deflate.rs` beyond extension
  detection:
  - raw permessage-deflate payload inflation;
  - WebSocket server frame parsing;
  - compressed RSV1 data-frame rewriting into a normal uncompressed server
    frame;
  - uncompressed/partial/non-data frame no-op handling.
- Added adapter tests proving compressed server text frames are rewritten with
  RSV1 cleared and original JSON payload restored, while uncompressed frames are
  left unchanged.

TDD evidence:

- Before implementation,
  `cargo test -p codex-proxy-adapters --test codex websocket_deflate -- --nocapture`
  failed to compile because `flate2` was not an adapters dependency and
  `rewrite_permessage_deflate_server_frame(...)` did not exist.
- The first implementation attempt failed the compressed-frame test because a
  single `Decompress::decompress_vec` call produced an empty payload for the
  test fixture. The root cause was incomplete decoder driving, so the inflater
  was changed to `DeflateDecoder::read_to_end(...)`.
- The test fixture then used `DeflateEncoder` to produce a real raw deflate
  payload, and the filtered deflate tests passed 2/2.

Verification run so far:

- `cargo test -p codex-proxy-adapters --test codex websocket_deflate -- --nocapture`
  (RED: missing dependency/API)
- `cargo test -p codex-proxy-adapters --test codex websocket_deflate -- --nocapture`
  (intermediate failure: empty inflated payload; fixed by full decoder read and
  a real raw-deflate test fixture)
- `cargo test -p codex-proxy-adapters --test codex websocket_deflate -- --nocapture`
  (GREEN: 2/2 filtered tests passed)
- `cargo fmt --all`
- `cargo test -p codex-proxy-adapters --test codex`
  (36/36 tests passed)
- `cargo test -p codex-proxy-core --test protocol`
  (45/45 tests passed)
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
  (22/22 tests passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
  (54/54 tests passed)
- `cargo fmt --all -- --check`
- `git diff --check`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `find crates tests src -path '*/target' -prune -o -type f -empty -print | sort`
  (no empty files reported)
- `rg -n "fn main\(\) \{\}|not wired yet|后续承载|todo!\(|unimplemented!\(|TODO\b|panic!\(\"TODO|stub|placeholder" crates src tests docs/architecture-audit.md`
  (matches were limited to this audit document and the architecture
  placeholder guard/test names)
- `codegraph sync . && codegraph status .`
  (index up to date with 271 files, 3,422 nodes, and 9,482 edges)

Remaining after this slice:

- Deflate frame-level rewrite logic is no longer a one-line shell, but it is
  not yet inserted into the live `tokio-tungstenite` connection path. Because
  `tokio-tungstenite` rejects RSV1 frames before yielding messages, live
  integration still needs a TLS-aware stream insertion point or a custom
  connector.
- The WebSocket path is still buffered rather than a live streaming body proxy.
- Full old parity still needs streaming body forwarding, live
  permessage-deflate integration, pooled socket reuse/keepalive, and broader
  old WebSocket behavior test migration.

### 2026-06-19 Slice 61: Live HTTP SSE Body Forwarding

Status: implemented and targeted verification passed for this batch.

Changes:

- Added a live HTTP SSE response path to `CodexBackendClient`:
  - `CodexBackendSseStream`;
  - `CodexBackendStreamingResponse`;
  - `CodexBackendClient::create_response_stream(...)`.
- Kept non-2xx upstream responses on the existing typed `CodexClientError::Upstream`
  path, including capped error body reads and retry-after extraction, so HTTP
  `429`/`402`/`401`/`403`/`5xx` fallback remains before any downstream bytes are
  sent.
- Changed `ResponseDispatchService::stream(...)` from a buffered `String` result
  to `ResponseDispatchStream`, a runtime-owned byte stream that does not expose
  axum types.
- Added first-event SSE prefetch in runtime before the live stream is returned.
  This preserves existing fallback/recovery when the first upstream event is a
  recoverable `response.failed` while allowing normal delta streams to be
  forwarded before upstream completion.
- Moved stream usage/session-affinity finalization to stream end:
  - token usage is extracted from the observed SSE body and recorded after the
    upstream stream ends;
  - session affinity is recorded from `response.completed`;
  - the runtime account-pool slot is held until the stream ends instead of being
    released immediately after response headers;
  - `[DONE]` is appended for live streams when the upstream body omits it.
- Wired the server Responses handler to return `Body::from_stream(...)` for
  successful streaming Responses while keeping generated SSE error responses on
  the existing string path.
- Added a server integration regression test with a raw chunked upstream that
  writes the first SSE event, keeps the connection open, and only later sends
  `response.completed`. The test proves the proxy returns the HTTP response and
  first SSE chunk before upstream completion.

TDD evidence:

- Before implementation,
  `cargo test -p codex-proxy-server --test openai_chat_upstream responses_stream_should_forward_first_chunk_before_upstream_completes -- --nocapture`
  failed as expected with:
  `stream response should be returned before upstream completes: Elapsed(())`.
- After implementing the live stream path, the same test passed.
- The existing `responses_stream_should...` batch stayed green, including SSE
  failure fallback, 429/402/401/403 fallback, 5xx retry, history recovery, usage
  persistence, and generated stream error termination coverage.

Verification run so far:

- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_stream_should_forward_first_chunk_before_upstream_completes -- --nocapture`
  (RED: timeout because the old implementation waited for upstream completion)
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_stream_should_forward_first_chunk_before_upstream_completes -- --nocapture`
  (GREEN: 1/1 filtered test passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_stream_should -- --nocapture`
  (16/16 filtered stream tests passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
  (55/55 tests passed)
- `cargo fmt --all`
- `cargo check --workspace --all-targets`
- `codegraph sync . && codegraph status .`
  (synced 4 changed files; index up to date with 271 files, 3,456 nodes, and
  9,738 edges)

Remaining after this slice:

- HTTP SSE `stream: true` now has a live chunk-by-chunk success path plus
  first-event fallback preservation, but once normal downstream bytes have been
  sent, later upstream `response.failed` events cannot be retried on another
  account without violating HTTP streaming semantics. Late-failure stream audit
  behavior still needs explicit policy and tests.
- WebSocket Responses are still buffered in the one-shot exchange path.
- Full WebSocket parity still needs live WebSocket body forwarding, live
  permessage-deflate integration, pooled socket reuse/keepalive, and broader old
  behavior-test migration.

### 2026-06-19 Slice 62: WebSocket Pool Reuse And Error Discard

Status: implemented and adapter verification passed for this batch.

Changes:

- Replaced the WebSocket pool policy-only struct with a real async pool state:
  - `CodexWebSocketPoolKey` keyed by normalized base URL, account id, and
    conversation id;
  - idle/busy slots guarded by `tokio::sync::Mutex`;
  - max-per-account admission checks;
  - max-age recycling for idle connections;
  - `acquire`, `put`, `discard`, and explicit `gc_sweep`.
- Added pooled connection metadata and a concrete `CodexWsStream` type for
  `tokio-tungstenite` WebSocket streams.
- Added `CodexBackendClient::with_websocket_pool(...)` and optional pool key
  derivation from request/context data. Pooling is only used when both an
  upstream account id and a conversation key are available; otherwise the
  existing one-shot WebSocket path is preserved.
- Split the WebSocket execution path so fresh and reused connections share the
  same response collector:
  - successful terminal responses return the socket to the pool;
  - upstream error frames, transport errors, and invalid terminal behavior
    discard the pool slot instead of returning a bad socket;
  - bypass remains available when a key is already busy or account capacity is
    exhausted.
- Migrated adapter tests proving:
  - two same-account/same-conversation Responses requests reuse one accepted
    upstream WebSocket connection;
  - a pooled upstream error discards the connection, so the next request opens a
    fresh upstream WebSocket and succeeds.

TDD evidence:

- Before implementation,
  `cargo test -p codex-proxy-adapters --test codex pooled_websocket -- --nocapture`
  failed because `CodexBackendClient::with_websocket_pool(...)` did not exist.
- The first implementation attempt exposed a test setup false positive: the
  requests did not pass `account_id`, so the pool was correctly bypassed. The
  tests were corrected to pass the same account id, proving actual pool usage.
- After implementation and test correction, the filtered pooled WebSocket tests
  passed 2/2.

Verification run so far:

- `cargo test -p codex-proxy-adapters --test codex pooled_websocket -- --nocapture`
  (RED: missing `with_websocket_pool` API)
- `cargo test -p codex-proxy-adapters --test codex pooled_websocket -- --nocapture`
  (intermediate failure: test omitted account id and bypassed the pool; fixed)
- `cargo test -p codex-proxy-adapters --test codex pooled_websocket -- --nocapture`
  (GREEN: 2/2 filtered tests passed)
- `cargo test -p codex-proxy-adapters --test codex`
  (38/38 tests passed)
- `cargo fmt --all`
- `cargo check --workspace --all-targets`
- `cargo test -p codex-proxy-core --test protocol`
  (45/45 tests passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
  (55/55 tests passed)
- `cargo test --test architecture`
  (22/22 tests passed)
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
  (first run failed on a `needless_collect` in pool GC and too-many-arguments in
  the live SSE finalizer helpers; both were refactored, and the rerun passed)
- `cargo test -p codex-proxy-adapters --test codex`
  (38/38 tests passed after the clippy refactor)
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
  (55/55 tests passed after the clippy refactor)
- `cargo test --test architecture`
  (22/22 tests passed after the clippy refactor)
- `cargo fmt --all -- --check`
- `git diff --check`
- `codegraph sync . && codegraph status .`
  (final sync after lint cleanup synced 2 changed files; index up to date with
  271 files, 3,493 nodes, and 9,871 edges)

Remaining after this slice:

- WebSocket pooling now reuses successful same-conversation sockets and discards
  errored sockets, but it does not yet have the old background maintenance task,
  ping probe/timeout keepalive, liveness timeout, or shutdown/evict-account API.
- WebSocket Responses are still a buffered one-shot response collector from the
  server/runtime perspective; live WebSocket body forwarding still needs a
  streaming API shape.
- Live permessage-deflate integration is still not inserted into the
  `tokio-tungstenite` stream path.

### 2026-06-19 Slice 63: WebSocket Pool Lifecycle And Runtime Wiring

Status: implemented and verified for this batch.

Changes:

- Restored the adapter pool lifecycle surface from the old WebSocket pool:
  - added `CodexWebSocketPoolConfig` with `enabled`, `max_age`,
    `max_per_account`, `maintenance_interval`, `ping_interval`,
    `ping_timeout`, and `liveness_timeout`;
  - preserved the existing `CodexWebSocketPool::new(max_per_account, max_age)`
    test-friendly constructor without background maintenance;
  - added `with_config(...)`, `with_default_max_age()`, and `with_limits(...)`;
  - added `evict_account(...)`, `shutdown()`, and
    `maintain_idle_connections()`;
  - kept `gc_sweep()` as the public maintenance entry point while expanding it
    beyond age-only cleanup.
- Added real idle lifecycle state:
  - pool state now tracks `shutting_down`;
  - slots include `Checking` so a socket under keepalive probe cannot be reused
    concurrently;
  - pooled connections track `last_activity_at` and `last_ping_at`.
- Restored idle maintenance behavior:
  - max-age and liveness-timeout cleanup close idle sockets;
  - ping keepalive sends a WebSocket `Ping`, waits up to `ping_timeout` for an
    upstream response, keeps the socket on success, and closes/discards it on
    timeout, close, or transport error;
  - optional background maintenance starts when a Tokio runtime is available and
    `maintenance_interval` is configured.
- Wired platform `ws_pool` config into runtime Codex client construction:
  - `codex_proxy_runtime::upstream::codex_backend_client(...)` now accepts
    `&WebSocketPoolConfig`;
  - `Services::with_installation_id(...)` passes `config.ws_pool`;
  - enabled configs inject a shared `CodexWebSocketPool` into
    `CodexBackendClient`.

TDD evidence:

- Before implementation,
  `cargo test -p codex-proxy-adapters --test codex websocket_pool -- --nocapture`
  still only ran the old policy test, proving the lifecycle tests were not yet
  covered by the existing filter.
- After adding lifecycle tests, the correct RED command
  `cargo test -p codex-proxy-adapters --test codex websocket_pool -- --nocapture`
  failed at compile time because `CodexWebSocketPoolConfig` and
  `CodexWebSocketPool::with_config(...)` did not exist.
- After the initial adapter implementation,
  `cargo test -p codex-proxy-adapters --test codex pooled_websocket -- --nocapture`
  passed the manual keepalive/evict/shutdown/liveness tests but exposed a
  background-maintenance test design issue: a 1 ms ping interval let the
  background task repeatedly move the socket into `Checking`. The test was
  narrowed to one expected background probe by using a long post-probe
  `ping_interval`, then passed.
- Runtime wiring RED:
  `cargo test -p codex-proxy-runtime --test upstream codex_backend_client_should_apply_configured_websocket_pool -- --nocapture`
  failed because `codex_backend_client(...)` accepted only `base_url` and
  `fingerprint`, with no `ws_pool` config argument.
- Runtime wiring GREEN: the same runtime upstream test passed after mapping
  platform `WebSocketPoolConfig` to adapter `CodexWebSocketPoolConfig` and
  injecting the pool.

Verification run so far:

- `cargo test -p codex-proxy-adapters --test codex websocket_pool -- --nocapture`
  (RED: missing pool config/constructor API)
- `cargo test -p codex-proxy-adapters --test codex pooled_websocket -- --nocapture`
  (GREEN: 7/7 filtered pooled WebSocket tests passed)
- `cargo test -p codex-proxy-runtime --test upstream codex_backend_client_should_apply_configured_websocket_pool -- --nocapture`
  (RED: runtime backend client constructor did not accept `ws_pool` config)
- `cargo test -p codex-proxy-runtime --test upstream -- --nocapture`
  (GREEN: 1/1 runtime upstream wiring test passed)
- `cargo test -p codex-proxy-adapters --test codex`
  (43/43 tests passed)
- `cargo check --workspace --all-targets`
  (first run showed one warning caused by a temporarily removed test attribute;
  fixed by restoring `#[tokio::test]`)
- `cargo check --workspace --all-targets`
  (rerun passed without warnings)
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
  (55/55 tests passed)
- `cargo test --test architecture`
  (22/22 tests passed)
- `codegraph sync .`
  (synced 6 changed files; 1 added and 5 modified; 496 nodes in 671 ms)
- `codegraph status .`
  (index up to date with 272 files, 3,535 nodes, and 10,058 edges)

Remaining after this slice:

- Adapter-level WebSocket pool reuse, error discard, keepalive maintenance,
  liveness cleanup, shutdown/evict-account behavior, and runtime config
  injection are restored.
- WebSocket Responses are still buffered through the one-shot exchange from the
  server/runtime perspective; live WebSocket body forwarding still needs a
  streaming API shape.
- Live permessage-deflate rewriting exists as tested frame helpers, but it is
  still not inserted into the `tokio-tungstenite` stream path.
- Broader old WebSocket behavior-test migration remains incomplete, including
  server/runtime dispatch parity for live WebSocket routes.

### 2026-06-19 Slice 64: Server Responses WebSocketRequired Path

Status: implemented and verified for this batch.

Changes:

- Removed the unconditional `force_http_sse = true` from OpenAI Responses
  translation for non-streaming requests with `previous_response_id`.
  Non-streaming history-continuation requests now reach
  `CodexTransport::WebSocketRequired` instead of being forced through HTTP SSE.
- Kept current live streaming behavior intentionally conservative:
  `stream: true` requests with `previous_response_id` still force HTTP SSE until
  live WebSocket streaming is implemented.
- After history recovery strips stale `previous_response_id` state, the runtime
  now also clears `use_websocket` and sets `force_http_sse = true` so the retry
  matches a normal no-history OpenAI Responses request.
- Migrated server/runtime tests that previously used HTTP-only mocks for
  non-streaming `previous_response_id`:
  - session-affinity account preference now uses a captured WebSocket opening
    and asserts `authorization`, `chatgpt-account-id`, and payload
    `previous_response_id`;
  - previous-response-not-found, unanswered-function-call, and invalid encrypted
    reasoning recovery now use a first WebSocket `response.failed` frame followed
    by an HTTP SSE retry after history is stripped;
  - added an end-to-end server test proving two same-history non-streaming
    `/v1/responses` requests use WebSocket and reuse the configured runtime
    WebSocket pool.

TDD evidence:

- RED:
  `cargo test -p codex-proxy-server --test openai_chat_upstream responses_with_previous_response_id_should_use_websocket_and_configured_pool -- --nocapture`
  failed with `Protocol(WrongHttpMethod)`, proving the server path still sent an
  HTTP POST to a WebSocket upstream for `previous_response_id`.
- GREEN:
  the same test passed after changing the translator to leave
  `force_http_sse=false` for non-streaming history-continuation requests.
- Full server test RED after the first fix exposed five old HTTP mock tests that
  still assumed non-streaming `previous_response_id` used HTTP. Those tests were
  migrated to WebSocket/hybrid upstream fixtures, and the full server test then
  passed.

Verification run so far:

- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_with_previous_response_id_should_use_websocket_and_configured_pool -- --nocapture`
  (RED: WebSocket upstream received HTTP POST / wrong method)
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_with_previous_response_id_should_use_websocket_and_configured_pool -- --nocapture`
  (GREEN: targeted server WebSocketRequired path passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
  (intermediate RED: five previous-response tests still used HTTP-only mocks)
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_strip_history_after -- --nocapture`
  (4/4 migrated non-streaming history recovery tests passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream previous_response -- --nocapture`
  (4/4 previous-response filtered tests passed after fixing the affinity fixture
  timestamp)
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
  (56/56 tests passed)
- `cargo test -p codex-proxy-core --test protocol`
  (46/46 tests passed)
- `cargo test -p codex-proxy-adapters --test codex`
  (43/43 tests passed)
- `cargo test -p codex-proxy-runtime --test upstream -- --nocapture`
  (1/1 test passed)
- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
  (first run flagged `accept_hdr_async` test callback large-error types; fixed
  with local `#[expect(clippy::result_large_err)]` annotations and reran green)
- `cargo test --test architecture`
  (22/22 tests passed)
- `git diff --check`
- `codegraph sync .`
  (synced 4 changed files; 515 nodes in 449 ms)
- `codegraph status .`
  (index up to date with 272 files, 3,550 nodes, and 10,120 edges)

Remaining after this slice:

- Non-streaming OpenAI Responses with `previous_response_id` now exercise the
  server/runtime WebSocketRequired path and configured WebSocket pool.
- Streaming `previous_response_id` requests and live permessage-deflate were
  still open at the end of Slice 64; both are addressed in Slice 65 below.
- Broader old WebSocket behavior-test migration remains incomplete beyond the
  migrated previous-response server/runtime coverage above.

### 2026-06-19 Slice 65: Live WebSocket Streaming And Deflate Integration

Status: implemented and verified for this batch.

Changes:

- Enabled OpenAI Responses translation to keep `force_http_sse=false` for all
  requests with `previous_response_id`, including `stream: true`, so streaming
  history-continuation requests now reach `CodexTransport::WebSocketRequired`.
- Added live WebSocket SSE byte-stream forwarding in the adapter path used by
  `CodexBackendClient::create_response_stream(...)`. The stream converts public
  WebSocket events to SSE bytes before terminal completion, returns pooled
  sockets after successful terminal events, and discards sockets on transport or
  upstream errors.
- Replaced the live WebSocket opening path with the migrated original-handshake
  flow: the adapter writes the ordered upgrade request, reads the opening
  response and any preloaded frame bytes, validates `Sec-WebSocket-Accept`, wraps
  the stream in a `PerMessageDeflateStream`, and then hands it to
  `tokio-tungstenite`.
- Inserted tested live permessage-deflate support into both aggregated and
  streaming WebSocket paths. Server RSV1 text/binary frames are inflated before
  tungstenite parses them, while the existing pure frame rewriter tests remain
  in place.
- Restored streaming history recovery for WebSocket-first failures that surface
  while prefetching the first SSE chunk. `previous_response_not_found` and
  `invalid_encrypted_content` now strip history and retry over HTTP SSE before
  any downstream bytes are returned.
- Migrated server streaming history-recovery tests from HTTP-only mocks to a
  first WebSocket `response.failed` frame followed by HTTP SSE recovery, and
  added coverage that a streaming `previous_response_id` response forwards a
  WebSocket chunk before upstream completion.
- Added live adapter coverage for negotiated permessage-deflate by using a raw
  local WebSocket upstream that replies with
  `Sec-WebSocket-Extensions: permessage-deflate` and an RSV1 compressed
  `response.completed` frame.

TDD evidence:

- RED:
  `cargo test -p codex-proxy-server --test openai_chat_upstream responses_stream_with_previous_response_id_should_forward_websocket_chunks_before_completion -- --nocapture`
  failed with `Protocol(WrongHttpMethod)`, proving streaming
  `previous_response_id` still went to HTTP instead of WebSocket.
- GREEN:
  the same test passed after enabling WebSocketRequired routing for streaming
  `previous_response_id` and adding adapter live WebSocket byte streaming.
- RED:
  `cargo test -p codex-proxy-adapters --test codex codex_backend_client_should_decode_live_permessage_deflate_websocket_frame -- --nocapture`
  failed with `Protocol(NonZeroReservedBits)`, proving live RSV1 frames still
  reached tungstenite without decompression.
- GREEN:
  the same test passed after inserting `PerMessageDeflateStream` into the live
  connection path.
- Full server test then exposed two streaming history-recovery tests that still
  assumed HTTP-first behavior. They were migrated to WS-first recovery fixtures,
  and the runtime prefetch error branch was fixed to strip history and retry
  before returning downstream bytes.

Verification run so far:

- `cargo test -p codex-proxy-core --test protocol openai_streaming_response_with_previous_response_should_require_websocket -- --nocapture`
  (1/1 filtered test passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_stream_with_previous_response_id_should_forward_websocket_chunks_before_completion -- --nocapture`
  (RED then GREEN; final filtered test passed)
- `cargo test -p codex-proxy-adapters --test codex codex_backend_client_should_decode_live_permessage_deflate_websocket_frame -- --nocapture`
  (RED then GREEN; final filtered test passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_stream_should_strip_history_after -- --nocapture`
  (2/2 streaming WS-first history recovery tests passed after the prefetch
  recovery fix)
- `cargo test -p codex-proxy-adapters --test codex websocket_ -- --nocapture`
  (26/26 filtered WebSocket tests passed)
- `cargo test -p codex-proxy-adapters --test codex`
  (44/44 tests passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
  (57/57 tests passed)
- `cargo test -p codex-proxy-runtime --test upstream`
  (1/1 test passed)
- `cargo test -p codex-proxy-core --test protocol`
  (46/46 tests passed)
- `cargo check --workspace --all-targets`
- `cargo fmt --all -- --check`
- `cargo test --test architecture`
  (22/22 tests passed)
- `git diff --check`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
  (first run flagged one new `accept_hdr_async` test helper large-error
  callback; fixed with a local
  `#[expect(clippy::result_large_err)]` annotation and reran green)
- `codegraph sync . && codegraph status .`
  (synced 9 changed files; index up to date with 272 files, 3,595 nodes, and
  10,382 edges before the documentation-only update)

Remaining after this slice:

- Live WebSocket streaming body forwarding, streaming `previous_response_id`
  WebSocket dispatch, and live permessage-deflate integration are now covered by
  adapter/server/core tests.
- Remaining old WebSocket behavior-test migration still needs a focused pass for
  parity/audit edge cases not represented by the restored live, pool, deflate,
  ping, history-recovery, and previous-response coverage.
- Late-failure stream audit/reporting policy after downstream bytes have already
  been sent remains a serving-layer gap.

### 2026-06-19 Slice 66: Live Stream Late-Failure SSE Reporting

Status: implemented and verified for this batch.

Changes:

- Restored the old serving-layer client reporting policy for live stream
  failures after at least one downstream SSE chunk has already been sent. The
  runtime no longer forwards late upstream body read errors as Hyper body
  errors; it appends a synthetic OpenAI-compatible
  `response.failed` event with `code: "stream_disconnected"` and then appends
  `data: [DONE]`.
- Covered the clean premature EOF case separately. If the upstream stream ends
  without `response.completed`, `response.failed`, or `error`, the runtime now
  emits the same `stream_disconnected` failure event before `[DONE]`.
- Preserved terminal-event semantics: if a live stream has already received
  `response.completed`, `response.failed`, or `error`, the runtime only appends
  `[DONE]` when needed and does not synthesize a second failure.
- Added partial-SSE safety before synthetic failures. If upstream closes while
  a frame is incomplete, the runtime first emits the missing blank line so the
  synthetic `response.failed` event is parsed as a separate SSE frame.
- Kept retry behavior conservative: once bytes have been sent downstream, the
  serving layer does not attempt account fallback or history replay, because
  doing so would violate HTTP streaming semantics.

TDD evidence:

- RED:
  `cargo test -p codex-proxy-server --test openai_chat_upstream responses_stream_should_emit_failed_event -- --nocapture`
  failed for both new tests. The abrupt-close case surfaced a body
  `UnexpectedEof` error to the downstream reader, and the clean-close case
  lacked `event: response.failed`.
- GREEN:
  the same filtered command passed after runtime stream finalization converted
  late read errors and missing-terminal EOF into SSE
  `response.failed(stream_disconnected)` frames followed by `[DONE]`.

Verification run so far:

- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_stream_should_emit_failed_event -- --nocapture`
  (RED then GREEN; final filtered tests passed 2/2)
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_stream_should -- --nocapture`
  (18/18 filtered stream tests passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream -- --nocapture`
  (59/59 tests passed)
- `cargo test -p codex-proxy-runtime --test upstream -- --nocapture`
  (1/1 test passed)
- `cargo test -p codex-proxy-core --test protocol -- --nocapture`
  (46/46 tests passed)
- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture -- --nocapture`
  (22/22 tests passed)
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`

Remaining after this slice:

- Client-visible late-failure SSE reporting is restored for live stream body
  errors and clean premature EOF after downstream bytes have been sent.
- Full persisted stream audit parity remains incomplete. The new crate split
  still lacks the old `StreamAudit` / `WebSocketStreamAudit` event-log style
  artifacts for all late-success and late-failure stream outcomes.
- Remaining old WebSocket behavior-test migration still needs a focused pass for
  parity/audit edge cases not represented by the restored live, pool, deflate,
  ping, history-recovery, previous-response, and late-failure coverage.

### 2026-06-19 Slice 67: Live Stream Event-Log Audit Restoration

Status: implemented and verified for this batch.

Changes:

- Added a runtime write path for admin event logs through
  `AdminLogService::record(...)`, reusing the existing core `EventLogService`
  policy: when logging is enabled, all events are recorded; when disabled,
  error-level events are still retained.
- Injected the shared `AdminLogService` into `ResponseDispatchService` and
  carried request audit context into `LiveResponseStreamContext`: request id,
  account id, model, route, stream metadata, status code, and latency.
- Restored persisted `v1.response` event-log records for completed live
  Responses streams. Successful terminal streams now write an info event with
  status `200`, `stream: true`, `completed: true`, and `responseId`.
- Restored persisted `v1.response` event-log records for late failed live
  Responses streams. Synthetic `stream_disconnected` failures, upstream
  `response.failed` events, and missing-terminal streams now write error/warn
  events with failure metadata and the mapped status code.
- Updated the admin logs error mapper for the new append failure variant so
  management routes remain exhaustive.

TDD evidence:

- RED:
  `cargo test -p codex-proxy-server --test openai_chat_upstream responses_stream_should_record_event_log -- --nocapture`
  failed for both new tests with `expected a v1.response event log`, proving live
  stream completion and late-disconnect paths still did not persist audit
  records.
- GREEN:
  the same filtered command passed after wiring `AdminLogService` into the live
  stream finalizer and writing completed/failed stream events.

Verification run so far:

- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_stream_should_record_event_log -- --nocapture`
  (RED then GREEN; final filtered tests passed 2/2)
- `cargo test -p codex-proxy-server --test admin_logs_routes -- --nocapture`
  (2/2 tests passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_stream_should -- --nocapture`
  (20/20 filtered stream tests passed)
- `cargo test -p codex-proxy-runtime --test upstream -- --nocapture`
  (1/1 test passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream -- --nocapture`
  (61/61 tests passed)
- `cargo test --test architecture -- --nocapture`
  (22/22 tests passed)
- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `codegraph sync . && codegraph status .`
  (synced 3 changed files; index up to date with 272 files, 3,623 nodes, and
  10,460 edges before the documentation-only update)

Remaining after this slice:

- Persisted live stream event-log audit is restored for completed streams and
  late-failure streams at the runtime/server boundary.
- Full old `StreamAudit` / `WebSocketStreamAudit` parity is still broader than
  this slice. Body capture policy, rate-limit/header snapshots, WebSocket pool
  update snapshots, and every old parity edge-case artifact still need a focused
  pass before calling stream audit migration complete.
- Remaining old WebSocket behavior-test migration still needs a focused pass for
  parity/audit edge cases not represented by the restored live, pool, deflate,
  ping, history-recovery, previous-response, late-failure, and event-log
  coverage.

### 2026-06-19 Slice 68: Stream Audit Metadata And Log Policy Batch

Status: implemented and verified for this batch.

Changes:

- Restored the old admin log state mutation surface for migrated routes:
  `PATCH /api/admin/logs/state` now updates `enabled`, `capacity`, and
  `captureBody`, rejects zero capacity, and trims persisted `event_logs` to the
  configured newest-N capacity without adding a schema migration.
- Restored the old log body-capture policy in `AdminLogService::record(...)`.
  Events can attach `body`, `rawBody`, `requestBody`, `responseBody`, and
  `upstreamBody`; when `captureBody` is false these keys are removed before
  persistence, and when true they are retained.
- Extended live Responses stream event logs with old `StreamAudit` metadata:
  completed and failed streams now include normalized `usage`,
  `rateLimitHeaders`, and capture-policy-controlled `requestBody` /
  `responseBody`.
- Added an actual upstream transport marker to streaming adapter responses.
  Runtime stream audit records `transport: "websocket"` only when the adapter
  actually used WebSocket, so WebSocketPreferred HTTP fallback cannot be logged
  as a WebSocket stream by request-policy inference.
- Restored live WebSocket stream rate-limit update snapshots. Adapter live
  forwarding now captures internal `codex.rate_limits` events, converts them
  back into the old `x-codex-*` header-pair shape, shares those updates with the
  runtime stream finalizer, and merges them into persisted
  `rateLimitHeaders`.
- Preserved the existing sensitive-cookie stance: `set-cookie` response headers
  are still not added to event metadata. The restored header snapshot is scoped
  to rate-limit and Codex review/quota headers.

TDD evidence:

- RED:
  `cargo test -p codex-proxy-server --test admin_logs_routes admin_logs_should_update_state_and_trim_to_capacity -- --nocapture`
  failed with HTTP `405`, proving the migrated admin log state update route was
  missing.
- RED:
  `cargo test -p codex-proxy-server --test openai_chat_upstream responses_stream_should_record_event_log_after_completed_stream -- --nocapture`
  failed because stream metadata lacked `usage` and rate-limit header snapshot
  fields.
- RED:
  `cargo test -p codex-proxy-server --test openai_chat_upstream responses_stream_should_preserve_body_metadata_when_capture_body_enabled -- --nocapture`
  failed because capture-body-enabled logs still had no `requestBody` /
  `responseBody`.
- RED:
  `cargo test -p codex-proxy-server --test openai_chat_upstream responses_stream_should_record_event_log_after_late_disconnect -- --nocapture`
  failed because late-failure stream logs lacked `rateLimitHeaders`.
- RED:
  `cargo test -p codex-proxy-server --test openai_chat_upstream responses_stream_with_previous_response_id_should_record_websocket_audit_metadata -- --nocapture`
  first failed on missing `transport: "websocket"`, then exposed missing live
  WebSocket rate-limit update propagation.
- GREEN:
  the same targeted tests passed after restoring admin log state mutation,
  body-capture policy, actual stream transport propagation, HTTP stream
  metadata enrichment, and WebSocket `codex.rate_limits` update sharing.

Verification run so far:

- `cargo test -p codex-proxy-server --test admin_logs_routes -- --nocapture`
  (3/3 tests passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream -- --nocapture`
  (63/63 tests passed)
- `cargo test -p codex-proxy-adapters --test codex websocket -- --nocapture`
  (27/27 filtered WebSocket tests passed)
- `cargo test -p codex-proxy-core --test protocol websocket -- --nocapture`
  (25/25 filtered WebSocket/protocol tests passed)
- `cargo test -p codex-proxy-runtime --test upstream -- --nocapture`
  (1/1 test passed)
- `cargo test -p codex-proxy-runtime --test tasks -- --nocapture`
  (9/9 tests passed)
- `cargo test --test architecture -- --nocapture`
  (22/22 tests passed)
- `cargo check --workspace --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `git diff --check`

Remaining after this slice:

- Stream audit metadata parity is restored for body capture policy, usage,
  rate-limit/header snapshots, actual WebSocket transport marking, and live
  WebSocket rate-limit update snapshots.
- Remaining old WebSocket behavior-test migration still needs a focused pass for
  parity/audit edge cases not represented by the restored live, pool, deflate,
  ping, history-recovery, previous-response, late-failure, event-log, and
  rate-limit-update coverage.
- Full persisted stream audit parity still needs any old pool-specific artifact
  details that are separate from event-log metadata.

### 2026-06-19 Slice 69: Serving Toy Helper Cleanup Guard

Status: implemented and verified for this batch.

Changes:

- Added an architecture guard that fails if the serving modules keep the
  previously audited toy helpers:
  `fallback::should_fallback(...)`, `recovery::is_recoverable(...)`, and
  `responses::prefers_websocket(...)`.
- The same guard verifies that `quota::quota_reached(...)`, which remains a
  useful pure threshold helper, has a non-test production caller.
- Replaced `fallback::should_fallback(...)` with explicit pure status helpers:
  `status_code_is_rate_limited`, `status_code_is_quota_exhausted`, and
  `status_code_is_transient_upstream`.
- Replaced `recovery::is_recoverable(...)` with the narrower
  `status_code_allows_same_account_retry(...)`, matching the runtime's current
  same-account retry behavior instead of implying broader recovery semantics.
- Removed `responses::prefers_websocket(...)`; production transport decisions
  already use `transport_for_request(...)` and `http_sse_fallback_allowed(...)`.
- Wired runtime dispatch classification and admin quota-warning threshold
  matching through the core serving helpers, so these modules are no longer just
  architecture-shaped files with uncalled convenience functions.

TDD evidence:

- RED:
  `cargo test --test architecture serving_modules_should_not_keep_uncalled_toy_helpers -- --nocapture`
  failed and listed all four audited helpers: `should_fallback`,
  `is_recoverable`, `prefers_websocket`, and uncalled `quota_reached`.
- GREEN:
  the same architecture test passed after removing/renaming the toy helpers and
  wiring runtime production callers through the remaining pure policy helpers.

Verification run so far:

- `cargo test --test architecture serving_modules_should_not_keep_uncalled_toy_helpers -- --nocapture`
  (RED then GREEN; final filtered test passed)
- `cargo test -p codex-proxy-server --test admin_accounts_routes quota_warnings -- --nocapture`
  (3/3 filtered quota-warning tests passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream -- --nocapture`
  (63/63 tests passed)
- `cargo test --test architecture -- --nocapture`
  (23/23 tests passed)
- `cargo test -p codex-proxy-core --test protocol -- --nocapture`
  (46/46 tests passed)
- `cargo check --workspace --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `git diff --check`

Remaining after this slice:

- The specific audited uncalled serving toy helpers are removed or wired to
  production callers, and architecture tests now catch their return.
- Broader serving dispatch parity is still not complete: old fallback/recovery
  edge-case behavior should continue to be migrated through runtime/server tests
  rather than inferred from helper presence.

### 2026-06-19 Slice 70: Admin Settings Patch And Test Migration Guards

Status: implemented and verified for this batch.

Changes:

- Migrated the old admin settings PATCH behavior into the crate split:
  - `core::admin::settings` now owns the retained settings patch model,
    validation, allowed rotation strategies, quota-warning threshold validation,
    and pure patch application.
  - `platform::config::AppConfig::write_settings_overlay(...)` now writes the
    retained runtime settings subset to a local YAML overlay without putting
    filesystem IO in `core`.
  - `RuntimeSettingsService` now stores a same-process current config snapshot,
    applies core settings patches, writes `local.yaml`, and exposes an injected
    local-config path for tests.
  - `PATCH /api/admin/settings` is restored, admin-session gated, rejects
    unknown/invalid fields, persists retained fields to local YAML, and updates
    the same-process `GET /api/admin/settings` view.
- Added crate-local tests migrated from the old `tests/admin/settings_route.rs`
  coverage for PATCH auth, retained-field persistence, reload through
  `AppConfig::load_from_dir(...)`, same-process GET visibility, and
  unknown/invalid field rejection.
- Added `tests/architecture/test_migration.rs` with guards that:
  - keep root `tests/` limited to architecture tests plus shared fixtures;
  - require high-risk baseline suites to have crate-local migration targets;
  - fail when exported core service modules have no production callers.
- Wired previously thin core service modules into production paths:
  - `AccountService` now supplies account quota/cloudflare availability helpers
    used by `AccountPool`;
  - `UsageService` now converts normalized Codex token usage into
    `AccountUsageDelta` for runtime persistence;
  - `SettingsService` is now used by runtime settings updates.

TDD evidence:

- RED:
  `cargo test -p codex-proxy-core --test admin_settings -- --nocapture`
  failed before implementation because the retained settings patch types and
  full settings fields did not exist.
- RED:
  `cargo test -p codex-proxy-server --test admin_settings_routes admin_settings_patch -- --nocapture`
  failed before implementation because `AppState` had no local-config-path
  constructor and the server test lacked the YAML writer dependency; the route
  was also still only registered as `GET`.
- GREEN:
  both targets passed after implementing the core/runtime/platform/server
  settings path.

Verification run so far:

- `cargo test -p codex-proxy-core --test admin_settings -- --nocapture`
  (2/2 tests passed)
- `cargo test -p codex-proxy-server --test admin_settings_routes admin_settings_patch -- --nocapture`
  (3/3 filtered PATCH tests passed)
- `cargo test --test architecture test_migration -- --nocapture`
  (3/3 filtered architecture migration tests passed)
- `cargo test -p codex-proxy-server --test admin_settings_routes -- --nocapture`
  (6/6 tests passed)
- `cargo test -p codex-proxy-core --test account_pool -- --nocapture`
  (25/25 tests passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream -- --nocapture`
  (63/63 tests passed)
- `cargo test -p codex-proxy-platform --test config -- --nocapture`
  (3/3 tests passed)
- `cargo test --test architecture -- --nocapture`
  (26/26 tests passed)
- `cargo fmt --all`
- `cargo check --workspace --all-targets`

Remaining after this slice:

- The old admin settings PATCH/local YAML behavior is restored for the retained
  settings surface.
- Root behavior test files are now guarded against returning under `tests/`;
  high-risk old suites now have crate-local migration-target evidence, but this
  does not by itself prove every individual old assertion has a one-to-one
  migrated equivalent.
- Generic empty/placeholder detection is stronger for exported core service
  modules, but broader non-empty semantic completeness still depends on
  behavior tests and focused old-code parity passes.

### 2026-06-19 Slice 71: Admin Client-Key Assertion Coverage Migration

Status: crate-local behavior coverage migrated and verified for this batch.

Changes:

- Migrated additional old `tests/admin/client_keys_route.rs` assertions into
  `crates/server/tests/admin_client_keys_routes.rs`.
- Added crate-local coverage for:
  - label update, label clearing, overlong-label rejection, and missing-key
    `404`;
  - batch delete with found/missing IDs, authorization invalidation, and empty
    ID rejection;
  - export metadata shape without plaintext, hash, or pepper material;
  - import of exported metadata with forced key rotation, one-time plaintext
    return, old plaintext rejection, and new plaintext authorization.

TDD/coverage evidence:

- The migrated tests passed on the first run, so this slice did not require
  production-code changes. That means the behavior had already been restored in
  earlier migration work, but the old assertions were not yet present in the
  crate-local suite.

Verification run so far:

- `cargo test -p codex-proxy-server --test admin_client_keys_routes -- --nocapture`
  (7/7 tests passed)
- `cargo fmt --all`
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `git diff --check`
- `codegraph sync . && codegraph status .`
  (synced 1 changed file; index up to date with 274 files, 3,737 nodes, and
  10,716 edges before this documentation-only update)

Remaining after this slice:

- The highest-risk old client-key route assertions are now represented in the
  server crate-local suite.
- Assertion-level migration is still incomplete for other old suites called out
  in this audit, especially the largest WebSocket and serving suites.

### 2026-06-19 Slice 72: Admin Account Manual Create, Refresh-Only, And CLI Import

Status: implemented and verified for this batch.

Changes:

- Migrated a larger batch of old `tests/admin/accounts/import_export.rs`
  assertions into `crates/server/tests/admin_accounts_routes.rs`.
- Strengthened manual account create coverage for:
  - ignoring caller-supplied metadata/status and deriving email/account/user/plan
    from JWT claims;
  - encrypting both access and refresh tokens at rest;
  - immediately syncing created/updated active accounts into the runtime account
    pool without requiring process restart;
  - rejecting missing, invalid, expired, and account-claim-less tokens.
- Restored refresh-token-only manual create:
  - `AdminAccountService` now accepts a `TokenRefresher` port;
  - production service assembly injects the OpenAI OAuth client as the default
    refresher;
  - tests can inject a static refresher through an `AppState` constructor;
  - refresh-only create stores a rotated refresh token when returned;
  - refresh-only create preserves the input refresh token for new accounts when
    the exchange omits rotation;
  - refresh-only update preserves the existing stored refresh token when the
    exchange omits rotation.
- Restored same-account manual update behavior:
  - same ChatGPT account/user claims update the existing row rather than
    inserting a duplicate;
  - access token, email, plan, status, and pool snapshot are updated;
  - existing refresh token is preserved when the new request does not provide a
    replacement.
- Restored `POST /api/admin/accounts/import-cli`:
  - server reads `codexHome/auth.json`;
  - runtime parses `access_token` / `refresh_token` from the CLI auth payload and
    reuses the same manual-create path;
  - caller metadata is ignored in favor of JWT claims;
  - imported tokens are encrypted and not returned in the response;
  - imported active accounts are synced into the runtime account pool.
- Updated `docs/architecture.md` and the directory-shape whitelist for the new
  `crates/server/src/admin_api/accounts/import_cli.rs` handler.
- Added runtime account-pool synchronization for successful admin account
  create/import/delete paths. Label/status updates now attempt pool sync
  best-effort so metadata writes do not fail when the existing stored token is
  intentionally corrupt or undecryptable.
- Added `AccountStore::get_pool_account(...)` and a SQLite implementation that
  reads a single pool snapshot by ID. This avoids full-table pool scans during
  admin write synchronization and prevents unrelated corrupt token rows from
  breaking otherwise valid create/import sync.

TDD/debugging evidence:

- RED:
  `cargo test -p codex-proxy-server --test admin_accounts_routes -- --nocapture`
  failed before implementation because `AppState` had no injected token
  refresher constructor. The missing constructor blocked refresh-only migration
  tests exactly where the runtime had no refresh-token-only create path.
- GREEN attempt:
  the same command initially reached 22/23 tests and exposed a regression in
  `admin_accounts_lifecycle_should_update_and_delete_accounts`: label/status
  updates returned `500` because the new pool sync tried to decrypt a deliberately
  corrupt token row.
- Root-cause fix:
  sync now reads the target pool account by ID, and label/status updates treat
  pool sync as best-effort. Manual create/import still fail visibly if their own
  valid account cannot be synced.
- GREEN:
  `cargo test -p codex-proxy-server --test admin_accounts_routes -- --nocapture`
  passed with 23/23 tests.

Verification run so far:

- `cargo test -p codex-proxy-server --test admin_accounts_routes -- --nocapture`
  (23/23 tests passed)
- `cargo test -p codex-proxy-adapters --test account_repository -- --nocapture`
  (8/8 tests passed)
- `cargo test -p codex-proxy-runtime --test account_pool_restore -- --nocapture`
  (1/1 test passed)
- `cargo test --test architecture test_migration -- --nocapture`
  (3/3 filtered architecture migration tests passed)
- `cargo check --workspace --all-targets`
- `cargo fmt --all`
- `cargo fmt --all -- --check`
- `cargo test --test architecture -- --nocapture`
  (26/26 architecture tests passed after adding `import_cli.rs` to the
  architecture whitelist)
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `git diff --check`
- `codegraph sync . && codegraph status .`
  (synced 9 changed files; index up to date with 275 files, 3,780 nodes, and
  10,931 edges)

Current migration counters after this slice:

- Approximately 358 `#[test]` / `#[tokio::test]` occurrences.
- 15 root Rust test files, all architecture tests.
- 35 crate-local Rust test files under `crates/*/tests`.

Remaining after this slice:

- The highest-risk old manual account create/refresh-only/import-cli assertions
  from `tests/admin/accounts/import_export.rs` are now represented in the server
  crate-local suite.
- Other admin account old assertions are still not fully migrated, especially
  OAuth-route edge cases and any import/export cases not covered by this batch.
- The largest remaining unmigrated suites are still WebSocket/serving behavior
  suites called out below; this slice should not be read as full migration
  completion.

### 2026-06-19 Slice 73: Admin Account OAuth Route Migration

Status: implemented and verified for this batch.

Changes:

- Migrated a broad batch of old `tests/admin/accounts/oauth.rs` behavior into
  `crates/server/tests/admin_accounts_routes.rs`.
- Restored `GET /api/admin/auth/status`:
  - requires an admin session cookie;
  - reads account metadata without decrypting tokens;
  - reports `authenticated`, a sanitized active user, and pool counts by status;
  - does not expose access token, refresh token, or token aliases.
- Restored `POST /api/admin/auth/logout`:
  - deletes all accounts and associated `account_usage`, `account_cookies`, and
    `account_refresh_leases` rows explicitly;
  - clears the runtime account pool so old accounts cannot still be scheduled.
- Restored device-code OAuth routes:
  - `POST /api/admin/auth/device-login` calls the injected OAuth client and
    returns device code, user code, verification URI, expiry, and poll interval;
  - `GET /api/admin/auth/device-poll/{device_code}` maps
    `authorization_pending` / `slow_down` to a non-importing pending response;
  - successful polling imports the returned token pair through the same
    admin-account create path used in Slice 72, preserving encryption, claims
    derivation, and runtime pool sync while not returning secrets.
- Restored PKCE relay/callback flow:
  - `POST /api/admin/auth/code-relay` parses a callback URL, exchanges
    code/state through the injected OAuth client, imports the account, and
    returns a sanitized success envelope;
  - invalid callback URLs return `400`;
  - `GET /auth/callback` exchanges code/state, imports the account, and
    redirects back to the original admin host with `303 See Other`;
  - unsupported legacy callback paths remain unregistered.
- `AdminOAuthService` now owns an injected `OAuthClient` port in addition to the
  PKCE session store.
- Runtime service assembly now supports both production OpenAI OAuth clients and
  test-injected OAuth clients. The test constructor still builds a full
  `AppState`; only the OAuth upstream port is substituted.

TDD evidence:

- RED:
  `cargo test -p codex-proxy-server --test admin_accounts_routes admin_auth -- --nocapture`
  failed before implementation because `AppState` had no
  `with_pool_secret_api_key_hasher_and_oauth_client(...)` constructor. This
  showed the runtime had no testable OAuth-client port for the old device/code
  relay behaviors.
- GREEN:
  the same filtered command passed with 11/11 OAuth tests after implementing the
  runtime OAuth client port and server routes.

Verification run so far:

- `cargo test -p codex-proxy-server --test admin_accounts_routes admin_auth -- --nocapture`
  (11/11 filtered OAuth tests passed)
- `cargo test -p codex-proxy-server --test admin_accounts_routes -- --nocapture`
  (33/33 tests passed)
- `cargo test -p codex-proxy-adapters --test account_repository -- --nocapture`
  (8/8 tests passed)
- `cargo test -p codex-proxy-runtime --test account_pool_restore -- --nocapture`
  (1/1 test passed)
- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture -- --nocapture`
  (26/26 architecture tests passed)
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `git diff --check`
- `codegraph sync . && codegraph status .`
  (synced 7 changed files; index up to date with 275 files, 3,841 nodes, and
  11,275 edges)

Current migration counters after this slice:

- Approximately 368 `#[test]` / `#[tokio::test]` occurrences.
- 15 root Rust test files, all architecture tests.
- 35 crate-local Rust test files under `crates/*/tests`.

Remaining after this slice:

- The highest-risk old admin account OAuth route assertions are now represented
  in the server crate-local suite.
- Some admin account import/export edge cases outside the Slice 72 and Slice 73
  batches still needed assertion-level comparison with the old baseline.
  **Closed for the audited import/export edge cases in Slice 74.**
- The largest remaining unmigrated suites are still WebSocket/serving behavior
  suites called out below; this slice should not be read as full migration
  completion.

### 2026-06-19 Slice 74: Admin Import/Export Strictness And Route Assertion Sweep

Status: implemented and verified for this larger batch.

Changes:

- Migrated the remaining audited old `tests/admin/accounts/import_export.rs`
  assertions into `crates/server/tests/admin_accounts_routes.rs`:
  - `POST /api/admin/accounts/import` now has crate-local coverage for missing
    admin session cookies;
  - imported tokens are verified encrypted at rest and absent from list
    responses;
  - non-native external export shapes are rejected without inserting accounts;
  - native account objects with unknown fields are rejected instead of silently
    importing;
  - native containers with unknown fields are rejected instead of silently
    importing;
  - account export filters by requested IDs and returns only native account token
    material for the selected accounts;
  - unsupported `format=external` and `format=full` exports are rejected.
- Tightened runtime native account import parsing:
  - `parse_account_import_payload(...)` now returns `Result` instead of silently
    filtering every shape;
  - native top-level envelopes, native account containers, account arrays, and
    single account objects use explicit key allow-lists;
  - `sourceFormat` is accepted only when it is empty or `native`;
  - current native export metadata fields (`addedAt` / `updatedAt`, and snake
    case aliases) are accepted so native exports remain re-importable;
  - legacy/external fields such as `type`, `legacy`, `credentials`,
    `legacyField`, or `legacyContainer` now map to
    `No importable accounts found`.
- Folded additional old `tests/admin/client_keys_route.rs` assertions into the
  existing crate-local client-key suite:
  - unknown client API keys are rejected before any key is created;
  - re-enabled keys authorize `/v1/models` again;
  - invalid key status values return `400`;
  - deleted keys disappear from the list, no longer authorize, and repeated
    deletion returns `404`.
- Expanded `crates/server/tests/admin_logs_routes.rs` from 3 to 9 tests using
  old `tests/admin/logs_route.rs` behavior:
  - log list requires an admin session cookie;
  - cursor pagination includes `requestId`, page limit, first item, and
    `nextCursor`;
  - unsupported level filters return `400`;
  - detail lookup returns `404` for missing events;
  - log-state PATCH requires an admin session cookie;
  - zero log capacity is rejected;
  - clearing logs is verified by reading the empty list afterward.
- Expanded `crates/server/tests/admin_models_routes.rs` with old
  `tests/admin/models_route.rs` assertions:
  - admin model refresh now verifies configured fingerprint headers reach the
    mocked Codex model endpoint;
  - refreshed backend models are visible through `/v1/models/catalog` using a
    stored client API key.

TDD/coverage evidence:

- RED:
  `cargo test -p codex-proxy-server --test admin_accounts_routes admin_accounts_import -- --nocapture`
  failed with 4/7 passing before implementation. The failures showed unknown
  native account/container fields were accepted as `200`, and the non-native
  shape error message was still `no importable accounts found` instead of the
  old `No importable accounts found` surface.
- GREEN:
  the same filtered import command passed with 7/7 after strict native import
  parsing and the message restoration.
- Coverage-only migrations:
  the added client-key, log, and model assertions passed after test migration,
  so they document behavior already restored in earlier slices rather than
  requiring new production changes.

Verification run so far:

- `cargo test -p codex-proxy-server --test admin_accounts_routes admin_accounts_import -- --nocapture`
  (RED first: 4/7 passed, 3 failed; GREEN after implementation: 7/7 passed)
- `cargo test -p codex-proxy-server --test admin_accounts_routes admin_accounts_export -- --nocapture`
  (4/4 filtered export tests passed)
- `cargo test -p codex-proxy-server --test admin_accounts_routes -- --nocapture`
  (41/41 tests passed)
- `cargo test -p codex-proxy-server --test admin_client_keys_routes -- --nocapture`
  (7/7 tests passed)
- `cargo test -p codex-proxy-server --test admin_logs_routes -- --nocapture`
  (9/9 tests passed)
- `cargo test -p codex-proxy-server --test admin_models_routes -- --nocapture`
  (2/2 tests passed)
- `cargo fmt --all`
- `cargo test -p codex-proxy-server --tests -- --nocapture`
  (136 server tests passed)
- `cargo test -p codex-proxy-runtime --tests -- --nocapture`
  (30 runtime tests passed)
- `cargo test --test architecture -- --nocapture`
  (26/26 architecture tests passed)
- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `codegraph sync . && codegraph status .`
  (synced 5 changed files; index up to date with 275 files, 3,863 nodes, and
  11,376 edges)

Current migration counters after this slice:

- Approximately 382 `#[test]` / `#[tokio::test]` occurrences.
- 15 root Rust test files, all architecture tests.
- 35 crate-local Rust test files under `crates/*/tests`.

Remaining after this slice:

- The audited remaining old admin account import/export assertions are now
  represented in the server crate-local suite.
- The client-key route suite now carries the old missing assertions in the
  current crate-local file.
- The logs route suite still does not reproduce every historical fixture line
  one-for-one, but the old auth, cursor, filter, state, detail, and clear route
  behaviors are covered at the crate boundary.
- The admin models route suite now verifies the old fingerprint-header and
  catalog-read assertions.

### 2026-06-19 Slice 75: Admin Session Login, Usage Stats, And API Contract Migration

Status: implemented and verified for this larger batch.

Changes:

- Migrated admin session persistence behavior into
  `crates/adapters/tests/admin_sessions.rs` and
  `crates/adapters/src/sqlite/admin_sessions.rs`:
  - the SQLite store can create a default admin user exactly once;
  - the first admin user can be loaded with its password hash;
  - sessions are created with a user ID and expiry timestamp;
  - existing session validation and cleanup behavior remain in the adapter.
- Restored runtime admin login/session behavior in
  `crates/runtime/src/services.rs`:
  - `AdminSessionService` now owns default-admin bootstrap, credential
    verification, session creation, and TTL assignment;
  - admin password hashing/verification is delegated to `platform::identity`;
  - username validation is delegated to the pure `core::admin::auth` service.
- Added `POST /api/admin/login` in `crates/server/src/admin_api/session.rs` and
  routed it from `crates/server/src/admin_api/router.rs`:
  - valid credentials return the standard admin envelope plus `expiresAt`;
  - successful login sets the `cpr_admin_session` HTTP-only cookie;
  - invalid credentials return the old `40102` admin error surface and do not
    set a cookie.
- Restored server startup default-admin bootstrap in
  `crates/server/src/main.rs`, guarded by
  `tests/architecture/server_startup.rs`.
- Migrated remaining audited usage-stats assertions into
  `crates/server/tests/admin_accounts_routes.rs`:
  - account usage stats require an admin session cookie;
  - cursor pagination preserves account usage ordering and `nextCursor`.
- Added API response contract tests in
  `crates/server/tests/admin_api_contract.rs`:
  - request IDs stay camelCase;
  - page envelopes expose camelCase pagination;
  - HTTP status stays outside successful admin envelope bodies;
  - error bodies keep `data: null`;
  - `AdminError` serializes through the current response shape.
- Added a platform password boundary test in
  `crates/platform/tests/admin_password.rs` so admin password hashes do not
  accept client API key strings.

TDD/coverage evidence:

- RED:
  `cargo test -p codex-proxy-adapters --test admin_sessions -- --nocapture`
  failed before implementation because the SQLite admin-session store did not
  expose default-admin or session-creation persistence methods.
- RED:
  `cargo test -p codex-proxy-server --test admin_session_routes -- --nocapture`
  failed before implementation with `404` for `/api/admin/login`.
- RED:
  `cargo test --test architecture server_entrypoint_should_ensure_default_admin_before_serving -- --nocapture`
  failed before startup wiring because `main.rs` did not bootstrap the default
  admin user from config.
- GREEN:
  after implementation the adapter, server login, architecture startup,
  platform password, usage-stats, and API contract test filters all passed.

Verification run so far:

- `cargo test -p codex-proxy-adapters --test admin_sessions -- --nocapture`
  (2/2 tests passed after RED)
- `cargo test -p codex-proxy-server --test admin_session_routes -- --nocapture`
  (2/2 tests passed after RED)
- `cargo test --test architecture server_entrypoint_should_ensure_default_admin_before_serving -- --nocapture`
  (1/1 filtered architecture test passed after RED)
- `cargo test -p codex-proxy-platform --test admin_password -- --nocapture`
  (1/1 test passed)
- `cargo test -p codex-proxy-server --test admin_accounts_routes admin_usage_stats -- --nocapture`
  (3/3 filtered usage-stat tests passed)
- `cargo test -p codex-proxy-server --test admin_api_contract -- --nocapture`
  (5/5 tests passed)
- `cargo fmt --all`
- `cargo test -p codex-proxy-server --tests -- --nocapture`
  (145 server tests passed)
- `cargo test -p codex-proxy-adapters --tests -- --nocapture`
  (68 adapter tests passed)
- `cargo test -p codex-proxy-platform --tests -- --nocapture`
  (21 platform tests passed)
- `cargo test -p codex-proxy-runtime --tests -- --nocapture`
  (30 runtime tests passed)
- `cargo test --test architecture -- --nocapture`
  (27/27 architecture tests passed)
- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `git diff --check`
- `codegraph sync .`
- `codegraph status .`
  (index up to date with 279 files, 3,919 nodes, and 11,545 edges)

Current migration counters after this slice:

- Approximately 395 `#[test]` / `#[tokio::test]` occurrences.
- 15 root Rust test files, all architecture tests.
- 39 crate-local Rust test files under `crates/*/tests`.

Remaining after this slice:

- The audited admin-session login/startup bootstrap behavior is represented in
  crate-local server, runtime, adapter, platform, and architecture coverage.
- Usage-stats route authorization and cursor pagination are now represented in
  the server crate-local suite.
- Old account lifecycle, cookie/quota, and broader serving/WebSocket suites
  still need larger migrated coverage batches.
- The largest remaining unmigrated/high-risk area is still the old WebSocket and
  serving behavior surface called out below; this slice should not be read as
  full migration completion.

### 2026-06-19 Slice 76: Admin Account Batch Status, Refresh, And Reset Usage Migration

Status: implemented and verified for this larger batch.

Changes:

- Migrated old admin account lifecycle/cookie-quota behavior into
  `crates/server/tests/admin_accounts_routes.rs`:
  - `POST /api/admin/accounts/batch-status` updates found accounts and reports
    missing IDs plus invalid statuses;
  - `POST /api/admin/accounts/{account_id}/refresh` refreshes tokens, syncs the
    runtime pool, and does not return secret material;
  - refresh invalid-grant failures mark the account `expired`;
  - `POST /api/admin/accounts/{account_id}/reset-usage` clears persisted local
    counters and syncs the runtime pool.
- Added the persisted reset path in `SqliteAccountStore::reset_usage()` so
  cumulative/window counters and `last_used_at` are cleared without changing the
  retained quota window reset metadata.
- Added `AdminAccountService::batch_update_status()`,
  `AdminAccountService::refresh_account()`, and
  `AdminAccountService::reset_usage()` in `crates/runtime/src/services.rs`.
- Added server request/response handlers in
  `crates/server/src/admin_api/accounts/lifecycle.rs` and routed them from
  `crates/server/src/admin_api/router.rs`.
- Added `crates/core/tests/account_pool.rs` coverage for in-memory usage reset
  semantics.

TDD/coverage evidence:

- RED:
  `cargo test -p codex-proxy-server --test admin_accounts_routes admin_accounts_batch_status -- --nocapture`
  failed before implementation with HTTP `405` instead of `200`.
- RED:
  `cargo test -p codex-proxy-server --test admin_accounts_routes admin_account_refresh_should -- --nocapture`
  failed before implementation with HTTP `404` instead of `200`.
- RED:
  `cargo test -p codex-proxy-server --test admin_accounts_routes admin_account_reset_usage -- --nocapture`
  failed before implementation with HTTP `404` instead of `200`.
- GREEN:
  after implementation the batch-status, refresh, reset-usage, and core
  account-pool reset filters all passed.

Verification run so far:

- `cargo test -p codex-proxy-server --test admin_accounts_routes admin_accounts_batch_status -- --nocapture`
  (1/1 filtered test passed after RED)
- `cargo test -p codex-proxy-server --test admin_accounts_routes admin_account_refresh_should -- --nocapture`
  (2/2 filtered tests passed after RED)
- `cargo test -p codex-proxy-server --test admin_accounts_routes admin_account_reset_usage -- --nocapture`
  (1/1 filtered test passed after RED)
- `cargo test -p codex-proxy-core --test account_pool reset_usage_should_clear_runtime_counters_and_preserve_window_reset -- --nocapture`
  (1/1 filtered test passed)
- `cargo test -p codex-proxy-server --test admin_accounts_routes -- --nocapture`
  (47/47 tests passed)
- `cargo test -p codex-proxy-core --test account_pool -- --nocapture`
  (26/26 tests passed)
- `cargo fmt --all`
- `codegraph sync . && codegraph status .`
  (index up to date with 279 files, 3,948 nodes, and 11,879 edges)
- `cargo test -p codex-proxy-server --tests -- --nocapture`
  (149 server tests passed)
- `cargo test -p codex-proxy-core --tests -- --nocapture`
  (100 core tests passed)
- `cargo test -p codex-proxy-adapters --tests -- --nocapture`
  (68 adapter tests passed)
- `cargo test -p codex-proxy-runtime --tests -- --nocapture`
  (30 runtime tests passed)
- `cargo test --test architecture -- --nocapture`
  (27/27 architecture tests passed)
- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `git diff --check`

Current migration counters after this slice:

- Approximately 400 `#[test]` / `#[tokio::test]` occurrences.
- 15 root Rust test files, all architecture tests.
- 39 crate-local Rust test files under `crates/*/tests`.

Remaining after this slice:

- The audited batch-status, manual refresh, invalid refresh-token expiry, and
  reset-usage behavior from old admin account lifecycle/cookie-quota coverage is
  now represented in crate-local server/core suites.
- Remaining old admin cookie/quota behavior still needs a focused pass for the
  health-check OAuth-refresh semantics and exact request-field validation.
- The largest remaining unmigrated/high-risk area is still the old serving and
  WebSocket behavior surface.

### 2026-06-19 Slice 77: Admin Health-Check OAuth Refresh Migration

Status: implemented and verified for the remaining old admin cookie/quota
health-check behavior.

Changes:

- Migrated the remaining `tests/admin/accounts/cookies_quota.rs`
  health-check assertions into `crates/server/tests/admin_accounts_routes.rs`:
  - health-check now skips active accounts that have no refresh token instead
    of probing Codex usage and marking them alive;
  - health-check uses the configured `TokenRefresher` to determine alive/dead
    account status and does not touch the Codex usage backend;
  - invalid refresh-token failures persist `expired` status;
  - snake_case `stagger_ms` stays rejected by the camelCase API contract.
- Reworked `AdminAccountService::health_check_accounts()` to restore the old
  refresh-token probe path with `buffer_unordered(concurrency)` and the existing
  stagger delay.
- Added test-local `HealthCheckTokenRefresher` coverage so the server route
  proves refresh calls, response sanitization, and persisted status changes.
- Re-ran empty-directory and placeholder scans after the implementation:
  - empty directory scan returned no paths outside ignored locations;
  - placeholder scan only matched architecture guard tests themselves;
  - migration/compatibility keyword scan matched docs, architecture tests,
    product compatibility wording, and old-format rejection tests, not new
    migration shims in runtime/server source.

TDD/coverage evidence:

- RED:
  `cargo test -p codex-proxy-server --test admin_accounts_routes health_check -- --nocapture`
  failed 2/3 before implementation:
  - `admin_account_health_check_should_skip_account_without_refresh_token`
    reported `alive = 1` instead of `0`;
  - `admin_accounts_health_check_should_refresh_oauth_without_touching_codex_backend`
    reported `alive = 0` instead of `1`.
- The same RED run showed
  `admin_accounts_health_check_should_reject_unsupported_stagger_ms_field`
  already passed, confirming the route already rejected unknown snake_case
  request fields.
- GREEN:
  after implementation the health-check filter passed 3/3.

Verification run so far:

- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test admin_accounts_routes health_check -- --nocapture`
  (3/3 filtered tests passed after RED)
- `cargo test -p codex-proxy-server --test admin_accounts_routes -- --nocapture`
  (49/49 tests passed)
- `cargo test -p codex-proxy-runtime --tests -- --nocapture`
  (30 runtime tests passed)
- `cargo test -p codex-proxy-server --tests -- --nocapture`
  (151 server tests passed)
- `cargo test --test architecture -- --nocapture`
  (27/27 architecture tests passed)
- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `git diff --check`
- `codegraph sync . && codegraph status .`
  (synced 2 changed files; index up to date with 279 files, 3,955 nodes, and
  11,930 edges)
- `find . -path './.git' -prune -o -path './target' -prune -o -path './.codegraph' -prune -o -path './web/node_modules' -prune -o -type d -empty -print`
  (no output)
- `rg "not wired yet|后续承载|todo!|unimplemented!|stub|placeholder" -n crates src tests --glob '!target/**'`
  (only architecture placeholder guard tests matched)
- `rg "迁移|兼容|compat|legacy|shim|deprecated" -n crates src tests docs/architecture-audit.md docs/architecture.md --glob '!target/**'`
  (matches were docs, architecture tests, product compatibility comments, and
  old-format rejection tests)

Current migration counters after this slice:

- Approximately 402 `#[test]` / `#[tokio::test]` occurrences.
- 15 root Rust test files, all architecture tests.
- 39 crate-local Rust test files under `crates/*/tests`.

Remaining after this slice:

- The old admin `cookies_quota.rs` behavior surface audited in this pass is now
  represented in crate-local tests.
- The largest remaining unmigrated/high-risk area is still the old serving and
  WebSocket behavior surface, especially the historical HTTP SSE/WebSocket
  suites.

### 2026-06-19 Slice 78: Responses Default Streaming Migration

Status: implemented and verified for this focused HTTP SSE behavior.

Changes:

- Migrated the old `tests/codex_serving/responses_http_sse.rs`
  `v1_responses_should_default_to_streaming_when_stream_is_omitted` behavior
  into current crate-local coverage:
  - `OpenAiResponsesRequest` now deserializes omitted `stream` as `true`;
  - explicit `"stream": false` remains supported and continues to drive the
    non-streaming dispatch path;
  - `/v1/responses` with omitted `stream` returns an SSE
    `response.failed` plus `data: [DONE]` when no upstream account is
    available, matching the streaming error surface.
- Added protocol coverage in `crates/core/tests/protocol.rs`.
- Added route coverage in `crates/server/tests/openai_responses_routes.rs`.

TDD/coverage evidence:

- RED:
  `cargo test -p codex-proxy-core --test protocol openai_response_request_should_default_missing_stream_to_true -- --nocapture`
  failed because the translated Codex request had `stream == false`.
- RED:
  `cargo test -p codex-proxy-server --test openai_responses_routes responses_route_should_default_omitted_stream_to_sse -- --nocapture`
  failed with HTTP `503` instead of streaming HTTP `200`.
- GREEN:
  after changing the request default, both filtered tests passed.

Verification run so far:

- `cargo fmt --all`
- `cargo test -p codex-proxy-core --test protocol openai_response_request_should_default_missing_stream_to_true -- --nocapture`
  (1/1 filtered test passed after RED)
- `cargo test -p codex-proxy-server --test openai_responses_routes responses_route_should_default_omitted_stream_to_sse -- --nocapture`
  (1/1 filtered test passed after RED)
- `cargo test -p codex-proxy-core --test protocol -- --nocapture`
  (47/47 tests passed)
- `cargo test -p codex-proxy-server --test openai_responses_routes -- --nocapture`
  (5/5 tests passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream -- --nocapture`
  (63/63 tests passed)
- `cargo test --test architecture -- --nocapture`
  (27/27 architecture tests passed)
- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `git diff --check`
- `cargo test -p codex-proxy-server --tests -- --nocapture`
  (152 server tests passed)
- `codegraph sync . && codegraph status .`
  (synced 3 changed files; index up to date with 279 files, 3,958 nodes, and
  11,927 edges)

Current migration counters after this slice:

- Approximately 404 `#[test]` / `#[tokio::test]` occurrences.
- 15 root Rust test files, all architecture tests.
- 39 crate-local Rust test files under `crates/*/tests`.

Remaining after this slice:

- One old HTTP SSE default-streaming behavior is restored.
- The old Responses HTTP SSE suite still has broader gaps around review/compact
  routes, tuple schema reconversion, passive rate-limit caching, cookie path
  scoping, and disconnect behavior.
- The old Responses WebSocket and gateway WebSocket suites remain the largest
  high-risk migration areas.

### 2026-06-19 Slice 79: Responses HTTP SSE Review/Compact/Tuple Migration

Status: implemented and verified as one continuous migration batch from the old
`tests/codex_serving/responses_http_sse.rs` suite.

Changes:

- Migrated the old review route behavior:
  `/v1/responses/review` now forces `x-openai-subagent=review` into the Codex
  upstream request metadata and header path instead of behaving like a plain
  `/v1/responses` call.
- Migrated the old compact route behavior:
  `/v1/responses/compact` now builds a `CodexCompactRequest`, dispatches to
  `/codex/responses/compact`, keeps JSON response semantics, strips
  Responses-only fields such as `stream`, `store`, and `prompt_cache_key`, and
  uses the same account fallback classifications for rate limit/quota/auth/model
  errors.
- Expanded `OpenAiResponsesRequest` back toward the old request surface so
  `instructions`, `reasoning`, `tools`, `tool_choice`,
  `parallel_tool_calls`, `text.format`, `include`, `client_metadata`,
  context fields, and transport hints are parsed and translated.
- Restored tuple schema conversion for Responses HTTP SSE:
  - tuple-shaped JSON schema is converted before the Codex upstream request;
  - non-streaming completed Responses output is reconverted for the client;
  - live streaming SSE `response.output_text.delta`,
    `response.output_item.done`, and `response.completed` events are
    reconverted event-by-event when a tuple schema is present, without buffering
    the whole stream.
- Added crate-local migration coverage in:
  - `crates/core/tests/protocol.rs`;
  - `crates/server/tests/openai_chat_upstream.rs`.

TDD/coverage evidence:

- RED:
  `cargo test -p codex-proxy-core --test protocol openai_` failed at compile
  time because `translate_response_to_compact` did not exist.
- RED:
  `cargo test -p codex-proxy-server --test openai_chat_upstream responses_review_route_should_force_review_subagent_upstream`
  failed with HTTP `502` instead of `200`, proving the review route was not
  forcing the upstream subagent.
- RED:
  `cargo test -p codex-proxy-server --test openai_chat_upstream responses_compact`
  failed because the compact route still used the normal Responses/SSE handler
  and returned a non-JSON error body.
- RED:
  `cargo test -p codex-proxy-server --test openai_chat_upstream tuple`
  showed the upstream schema conversion and non-stream response reconversion
  passing, but `responses_stream_should_reconvert_tuple_schema_output_for_client`
  failed because live SSE was still forwarding the Codex tuple object form
  unchanged.
- GREEN:
  after implementing compact dispatch, review forcing, request translation, and
  tuple SSE event reconversion, the focused review/compact/tuple tests passed.

Verification run so far:

- `cargo fmt --all`
- `cargo test -p codex-proxy-core --test protocol openai_`
  (7/7 filtered tests passed after RED)
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_review_route_should_force_review_subagent_upstream`
  (1/1 filtered test passed after RED)
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_compact`
  (3/3 filtered tests passed after RED)
- `cargo test -p codex-proxy-server --test openai_chat_upstream tuple`
  (3/3 filtered tests passed after RED)
- `cargo test -p codex-proxy-server --tests`
  (159 server tests passed)
- `cargo test -p codex-proxy-core --test protocol`
  (50/50 protocol tests passed)
- `cargo test --test architecture`
  (27/27 architecture tests passed)
- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `git diff --check`
- Empty directory scan excluding `.git`, `target`, and `web/node_modules` found
  no empty source directories. A raw scan only found
  `web/node_modules/.vite-temp`, which is generated dependency-cache state and
  was not removed.
- Placeholder-marker scan only matched the architecture guard tests that define
  the forbidden markers.
- `codegraph sync . && codegraph status .`
  (synced 5 changed files; index up to date with 279 files, 4,011 nodes, and
  12,197 edges)

Current migration counters after this slice:

- Approximately 414 `#[test]` / `#[tokio::test]` occurrences.
- 59 Rust files contain tests.
- 15 root Rust test files, all architecture tests.
- 39 crate-local Rust test files under `crates/*/tests`.

Remaining after this slice:

- The review, compact, and tuple-schema portion of the old Responses HTTP SSE
  suite is now represented in crate-local tests.
- The next HTTP SSE audit pass should stay on the old suite order and
  cross-check passive rate-limit caching, cookie path scoping, disconnect/live
  stream audit behavior, and any remaining old fixture/golden gaps before
  moving away from this historical suite.
- The old Responses WebSocket and gateway WebSocket suites remain high-risk
  migration areas, but they should be handled by following their original test
  surfaces rather than by unrelated module-by-module cleanup.

### 2026-06-19 Slice 80: Responses HTTP SSE Non-Stream Side-Effects Migration

Status: implemented and verified as the next migration batch from the old
`tests/codex_serving/responses_http_sse.rs` suite, following the old
non-stream success/side-effect path instead of doing directory-driven cleanup.

Changes:

- Migrated old HTTP SSE non-stream side effects into crate-local server tests:
  - imported account dispatch with `authorization` and `chatgpt-account-id`
    headers;
  - non-stream account usage persistence and `v1.response` event-log metadata;
  - logging-disabled skip behavior;
  - successful and failed image-generation usage counters;
  - upstream `Set-Cookie` capture for `cf_clearance`;
  - cookie replay scoped by `/codex/responses` path;
  - passive rate-limit header caching into `accounts.quota_json`,
    cooldown/verify flags, and `account_usage` window metadata;
  - empty completed response retry exhaustion with three recorded empty
    attempts.
- Added adapter-local cookie migration coverage in
  `crates/adapters/tests/cookies.rs` for captured `cf_clearance`, path/domain
  replay, and ignoring non-capturable `Set-Cookie` headers.
- Restored `SqliteCookieStore::capture_set_cookie()` with the old allowlist and
  Domain/Path/Max-Age/Expires parsing semantics.
- Extended the account usage port so runtime can record empty responses and
  image request success/failure counters without bypassing `core` boundaries.
- Added account store port methods for quota JSON read/write and rate-limit
  window sync, with SQLite implementations ported from the old repository
  behavior.
- Updated `ResponseDispatchService::complete` to capture cookies, sync passive
  rate-limit state, record enhanced Responses usage, record non-stream event
  logs through `AdminLogService`, and retry empty completed responses twice
  before returning the upstream-response error.
- Updated runtime cookie replay to request cookies for `/codex/responses` and
  `/codex/responses/compact`, preserving old path-scoped cookie ordering.

TDD/coverage evidence:

- RED:
  `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_use_imported_account_record_usage_cookie_and_event_log`
  failed to compile because `SqliteCookieStore::capture_set_cookie()` did not
  exist, proving the old cookie capture API had not been migrated.
- RED:
  after adding the adapter API,
  `responses_should_use_imported_account_record_usage_cookie_and_event_log`
  failed because the persisted cookie header was `None` instead of
  `Some("cf_clearance=new")`, proving the non-stream runtime path still ignored
  upstream `Set-Cookie` side effects.
- GREEN:
  after wiring adapter/runtime/core behavior, the imported-account/cookie/log
  test and the image usage, failed image attempt, cookie path, passive
  rate-limit, empty-response, and logging-disabled tests all passed.

Verification run so far:

- `cargo test -p codex-proxy-adapters --test cookies`
  (2/2 cookie tests passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_use_imported_account_record_usage_cookie_and_event_log`
  (1/1 filtered test passed after RED)
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_record_image_generation`
  (1/1 filtered success-image test passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_record_failed_image_generation_attempt_when_tool_has_no_output`
  (1/1 filtered failed-image test passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_scope_upstream_cookie_by_codex_response_path`
  (1/1 filtered cookie-path test passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_passively_cache_rate_limit_headers`
  (1/1 filtered passive rate-limit test passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_record_empty_response_attempts`
  (1/1 filtered empty-response test passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_skip_event_log_when_logging_disabled`
  (1/1 filtered logging-disabled test passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_`
  (34/34 filtered Responses tests passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
  (77/77 upstream tests passed after final clippy cleanup)
- `cargo test -p codex-proxy-adapters --tests`
  (70 adapter tests passed, including the new cookie tests)
- `cargo test -p codex-proxy-core --tests`
  (104 core tests passed)
- `cargo test -p codex-proxy-runtime --tests`
  (30 runtime tests passed)
- `cargo test -p codex-proxy-server --tests`
  (166 server tests passed)
- `cargo test --test architecture`
  (27/27 architecture tests passed)
- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `git diff --check`
- Empty directory scan excluding `.git`, `target`, `.codegraph`, and
  `web/node_modules` found no empty source directories.
- Placeholder-marker scan only matched the architecture guard tests that define
  the forbidden markers.
- `codegraph sync . && codegraph status .`
  (synced 4 changed code files before this doc update; index up to date with
  280 files, 4,055 nodes, and 12,310 edges)

Current migration counters after this slice:

- Approximately 423 `#[test]` / `#[tokio::test]` occurrences.
- 60 Rust files contain tests.
- 15 root Rust test files, all architecture tests.
- 40 crate-local Rust test files under `crates/*/tests`.

Remaining after this slice:

- The old HTTP SSE non-stream side-effect block around imported accounts,
  image usage, cookie path scoping, passive rate-limit caching, empty-response
  retries, and logging-disabled behavior is now represented in crate-local
  tests.
- The next HTTP SSE pass should continue down the same old suite and verify
  parity fields/context headers, reasoning include defaults, sanitized
  reasoning/compaction input, non-stream text reconstruction, done-item fallback,
  passthrough stream logging, and downstream disconnect behavior.
- The old Responses WebSocket and gateway WebSocket suites remain high-risk,
  but should be handled only when the migration line reaches those suites.

### 2026-06-19 Slice 81: Responses HTTP SSE Parity/Reasoning/Stream Disconnect Migration

Status: implemented and verified as the next contiguous batch from the old
`tests/codex_serving/responses_http_sse.rs` suite. This slice stayed on the
HTTP SSE migration line and did not split work by module.

Changes:

- Added a core pure `apply_response_model_options(...)` policy and runtime
  wiring so Responses requests now apply:
  - parsed model id;
  - reasoning effort from the request body, model suffix, or model config;
  - default reasoning summary `auto`;
  - default `include: ["reasoning.encrypted_content"]` only when the client did
    not send a non-empty include list;
  - service tier from request body, model suffix, or config;
  - upstream service-tier normalization from `fast` to `priority`.
- Added `ModelService::config()` so runtime dispatch can apply the same model
  option rules without duplicating model configuration.
- Restored Responses prompt-cache/conversation identity behavior in runtime:
  - `complete(...)` and `stream(...)` call `ensure_prompt_cache_key(...)` after
    previous-response affinity lookup;
  - Codex upstream context now uses `build_conversation_identity(...)` to pass
    account-scoped `session_id` and `x-codex-window-id`.
- Restored `/v1/responses` header-context fallbacks for
  `x-codex-turn-state`, `x-codex-turn-metadata`, `x-codex-beta-features`,
  `x-responsesapi-include-timing-metrics`, `version`,
  `x-codex-window-id`, and `x-codex-parent-thread-id`, while preserving body
  fields when present.
- Restored normal-route `x-openai-subagent` propagation for valid subagent
  headers; the review route still forces `review`.
- Fixed WebSocket pool reuse for previous-response requests after prompt-cache
  restoration by keying the adapter pool on `previous_response_id` when the
  client did not explicitly provide a prompt cache key.
- Restored downstream disconnect propagation for live HTTP SSE streams:
  dropping the downstream body sends a cancellation signal, releases the account
  promptly, and drops the upstream SSE body/socket.
- Added crate-local tests for parity fields/context headers, include
  preservation, reasoning/compaction sanitization, text-delta reconstruction,
  done-item fallback, stream disconnect, and pure model-option defaults.

TDD/coverage evidence:

- RED:
  `cargo test -p codex-proxy-core --test protocol codex_responses_model_options_should_apply_suffix_defaults_and_include_reasoning`
  failed to compile because `apply_response_model_options(...)` did not exist.
- RED:
  `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_forward_parity_fields_context_headers_and_account_scoped_identity`
  failed with upstream `service_tier` `null` instead of `"priority"`.
- RED:
  `cargo test -p codex-proxy-server --test openai_chat_upstream responses_stream_should_close_http_sse_upstream_when_client_disconnects`
  failed because the upstream socket stayed open after the downstream body was
  dropped.
- Regression found during GREEN verification:
  `responses_with_previous_response_id_should_use_websocket_and_configured_pool`
  accepted two upstream WebSocket connections instead of one after prompt-cache
  keys were restored; the adapter pool key now preserves old previous-response
  reuse semantics.
- GREEN:
  the focused core model-option tests, new server parity/reasoning/sanitization/
  reconstruction/done-item/disconnect tests, and the existing previous-response
  WebSocket pool test passed after implementation.

Verification run for this slice:

- `cargo test -p codex-proxy-core --test protocol codex_responses_model_options`
  (2/2 filtered model-option tests passed)
- `cargo test -p codex-proxy-core --test protocol`
  (52/52 protocol tests passed)
- `cargo test -p codex-proxy-core --tests`
  (all core tests passed)
- `cargo test -p codex-proxy-runtime --tests`
  (all runtime tests passed)
- `cargo test -p codex-proxy-adapters --tests`
  (70/70 adapter tests passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
  (83/83 upstream tests passed)
- `cargo test -p codex-proxy-server --tests`
  (all server tests passed)
- `cargo test --test architecture`
  (27/27 architecture tests passed)
- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `git diff --check`
- Empty directory scan excluding `.git`, `target`, `.codegraph`, and
  `web/node_modules` found no empty source directories.
- Placeholder-marker scan matched only architecture guard tests and expected API
  compatibility/alias text; no source placeholder markers were found outside
  those guard/compatibility contexts.
- `codegraph sync . && codegraph status .`
  (synced 7 changed files before this doc update; index up to date with
  280 files, 4,082 nodes, and 12,430 edges)

Current migration counters after this slice:

- Approximately 431 `#[test]` / `#[tokio::test]` occurrences.
- 60 Rust files contain tests.
- 15 root Rust test files, all architecture tests.
- 40 crate-local Rust test files under `crates/*/tests`.

Remaining after this slice:

- The old HTTP SSE tail around parity fields, reasoning include defaults,
  sanitized reasoning/compaction input, non-stream text reconstruction,
  done-item fallback, stream usage/logging coverage, and downstream disconnect
  behavior is now substantially represented in crate-local tests.
- The next migration pass should first audit the earlier old HTTP SSE tests for
  any assertion-level gaps that were only partially covered, then move to the
  old Responses WebSocket and gateway WebSocket suites in their original
  behavior order.

### 2026-06-19 Slice 82: Responses HTTP SSE Early-Block/Stagger Migration

Status: implemented and verified as the assertion-level pass over the earlier
block of the old `tests/codex_serving/responses_http_sse.rs` suite. This stayed
on the same historical HTTP SSE behavior line.

Changes:

- Added crate-local server tests for old early-block HTTP SSE behavior:
  invalid JSON rejection without upstream traffic, non-object JSON rejection
  without upstream traffic, no-accounts Responses error shape, explicit
  `use_websocket: false` HTTP SSE transport, and same-account request
  staggering before sending upstream.
- Restored the non-streaming `/v1/responses` no-available-accounts response
  body to the old Responses-specific shape with top-level `type: "error"` and
  `error.code: "no_available_accounts"`, matching the already-migrated compact
  route.
- Restored runtime request-interval staggering:
  `RuntimeAccountPoolService` now stores `request_interval_ms`, uses the
  `previous_slot_at` metadata returned by the core account pool, and waits in
  runtime before Chat/Responses upstream dispatch. The async sleep remains in
  `runtime`; `core` still only owns pure scheduling metadata.

TDD/coverage evidence:

- RED:
  `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_return_no_available_accounts_error_when_no_accounts_are_available`
  failed because the non-streaming Responses path returned the generic
  OpenAI-style error body, leaving top-level `type` as `null` instead of
  `"error"`.
- GREEN:
  after mapping `NoActiveAccount` / `AccountStore` through
  `responses_no_available_accounts_response()`, the no-accounts test passed.
- RED:
  `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_stagger_same_account_requests_before_sending_upstream`
  failed because the second upstream request was sent after about 1.4ms instead
  of waiting for the configured request interval.
- GREEN:
  after adding runtime request-interval waiting from `previous_slot_at`, the
  same stagger test passed.

Verification run for this slice:

- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_reject`
  (2/2 invalid-json/non-object tests passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_honor_explicit_http_sse_transport`
  (1/1 filtered explicit HTTP SSE test passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_`
  (44/44 filtered Responses tests passed)
- `cargo test -p codex-proxy-runtime --tests`
  (all runtime tests passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
  (88/88 upstream tests passed)
- `cargo test -p codex-proxy-server --tests`
  (all server tests passed)
- `cargo test --test architecture`
  (27/27 architecture tests passed)
- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `git diff --check`
- Empty directory scan excluding `.git`, `target`, `.codegraph`, and
  `web/node_modules` found no empty source directories.
- Placeholder-marker scan matched only architecture audit text and architecture
  guard tests; no source placeholder markers were found outside those contexts.
- `codegraph sync . && codegraph status .`
  (synced 3 changed files after implementation; index up to date with
  280 files, 4,089 nodes, and 12,464 edges)

Current migration counters after this slice:

- 436 `#[test]` / `#[tokio::test]` occurrences.
- 60 Rust files contain tests.
- 15 root Rust test files, all architecture tests.
- 40 crate-local Rust test files under `crates/*/tests`.

Remaining after this slice:

- The old Responses HTTP SSE suite has crate-local coverage for the early
  rejection/no-account/explicit-transport/stagger block, the review/compact/
  tuple block, the non-stream side-effect block, and the parity/reasoning/
  disconnect tail. Further work should only revisit this suite for exact
  assertion-level gaps found by direct old/new comparison.
- The next migration stage should move to the old Responses WebSocket and
  gateway WebSocket suites in their original behavior order, not to unrelated
  directory cleanup.

### 2026-06-19 Slice 83: Responses WebSocket Default/Pool/Metadata Bulk Migration

Status: implemented and verified as the first large contiguous block from the
old `tests/codex_serving/responses_websocket.rs` suite.

Changes:

- Migrated old Responses WebSocket assertions into crate-local server tests for:
  default `/v1/responses` WebSocket upstream while still serving downstream SSE,
  ignored camelCase `useWebSocket`, first WebSocket stream frame reaching the
  downstream client before terminal completion, synthesized
  `response.failed(stream_disconnected)` after early upstream WebSocket close,
  recorded-conversation WebSocket pool reuse, disabled-pool non-reuse while
  keeping the recorded conversation key, and WebSocket `response.metadata`
  turn-state persistence for continuation requests.
- Restored the old default transport: Responses requests without
  `use_websocket: false` now remain `WebSocketPreferred`; only snake_case
  `use_websocket: false` forces HTTP SSE.
- Adjusted HTTP-SSE-specific server tests to opt into `use_websocket: false`
  explicitly, so those tests keep covering HTTP behavior after the default
  WebSocket route was restored.
- Fixed WebSocket pool key selection so continuation requests use the restored
  conversation/prompt-cache key before falling back to `previous_response_id`.
  This lets a response recorded in session affinity reuse the original pooled
  WebSocket.
- Propagated live WebSocket `response.metadata` turn-state updates from the
  adapter stream into runtime stream finalization so completed streaming
  responses record the latest turn state in session affinity.

TDD/coverage evidence:

- RED:
  `cargo test -p codex-proxy-server --test openai_chat_upstream responses_websocket -- --nocapture`
  first failed because the new tests referenced a missing local helper. After
  adding the helper, the same filter failed for the intended behavior gaps:
  default/no-history Responses requests were still sent as HTTP POST instead of
  WebSocket, disabled/default pool cases did not receive WebSocket payloads, and
  WebSocket metadata turn state was not present on the continuation request.
- GREEN:
  after restoring WebSocketPreferred defaults and adding live turn-state
  propagation, four of the five WebSocket-filtered tests passed. The remaining
  recorded-conversation pool test still opened a second socket because the
  adapter pool key preferred `previous_response_id` over the restored
  conversation key.
- GREEN:
  after changing pool-key priority to
  `prompt_cache_key -> client_conversation_id -> previous_response_id`, the
  filtered WebSocket block passed 5/5.
- Regression cleanup:
  the full server upstream suite initially exposed HTTP-SSE fixture pollution
  after the default transport change. Those HTTP-specific tests were made
  explicit with `use_websocket: false`; `previous_response_id` WebSocket tests
  were kept on the WebSocketRequired path.

Verification run for this slice:

- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_websocket -- --nocapture`
  (RED then GREEN; final run 5/5 passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_use_websocket_upstream_by_default_while_serving_sse -- --nocapture`
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_ignore_camel_case_use_websocket_field -- --nocapture`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
  (95/95 passed)
- `cargo test -p codex-proxy-core --test protocol websocket -- --nocapture`
  (25/25 filtered WebSocket/protocol tests passed)
- `cargo test -p codex-proxy-core --test protocol`
  (52/52 passed)
- `cargo test -p codex-proxy-adapters --test codex websocket_ -- --nocapture`
  (26/26 filtered WebSocket adapter tests passed)
- `cargo test -p codex-proxy-adapters --test codex pooled_websocket -- --nocapture`
  (7/7 filtered pooled WebSocket tests passed)
- `cargo test -p codex-proxy-runtime --tests`
  (all runtime tests passed)
- `cargo fmt --all`
- `cargo fmt --all -- --check`
- `codegraph sync . && codegraph status .`
  (synced 5 changed files; index up to date with 280 files, 4,100 nodes, and
  12,495 edges)

Current migration counters after this slice:

- 443 `#[test]` / `#[tokio::test]` occurrences.
- 60 Rust files contain tests.
- 15 root Rust test files, all architecture tests.
- 40 crate-local Rust test files under `crates/*/tests`.

Remaining after this slice:

- The first seven old Responses WebSocket behaviors are now represented in
  crate-local server coverage.
- The next Responses WebSocket block should migrate the implicit resume,
  reasoning replay, function-call replay exclusion, SQLite-restored affinity,
  cross-window exclusion, replay eviction, restored-full-history retry,
  admin-status pool eviction, and fallback/error-account routing cases from the
  rest of old `tests/codex_serving/responses_websocket.rs`.
- The old gateway WebSocket suite remains the largest unmigrated pure
  adapter/protocol behavior block after the Responses WebSocket suite.

### 2026-06-19 Slice 84: Responses WebSocket Implicit Resume / Reasoning Replay Bulk Migration

Status: implemented and verified as the next contiguous block from the old
`tests/codex_serving/responses_websocket.rs` suite.

Changes:

- Added pure serving modules for WebSocket implicit resume and reasoning replay:
  `crates/core/src/serving/implicit_resume.rs` and
  `crates/core/src/serving/reasoning_replay.rs`.
- Extended Responses protocol metadata extraction so completed WebSocket
  responses can record replayable reasoning items, function-call IDs,
  instructions hash, conversation identity, variant hash, and usage input
  tokens.
- Extended session affinity so runtime can match the latest response by
  conversation plus request variant, reject cross-window/cross-variant implicit
  resume, and distinguish unmatched function-call-output continuations.
- Updated Responses dispatch to:
  - apply implicit resume when the client omits `previous_response_id`;
  - prepend cached reasoning replay items only when the continuation matches;
  - restore the full original request history when implicit resume recovery is
    rejected by upstream `previous_response_not_found`;
  - evict reasoning replay cache entries after invalid encrypted content;
  - record affinity/replay metadata after non-streaming and streaming WebSocket
    completions.
- Updated admin account status changes to evict pooled WebSockets for both the
  internal account id and the upstream `chatgpt-account-id`, matching the pool
  key used by live sockets.

Migrated tests:

- `responses_websocket_should_implicitly_resume_full_history_with_reasoning_replay`
- `responses_websocket_should_not_implicitly_resume_unmatched_function_call_output`
- `responses_websocket_should_implicitly_resume_after_sqlite_affinity_restore`
- `responses_websocket_pool_should_be_evicted_after_admin_account_status_cycle`
- `responses_websocket_should_not_implicitly_resume_self_contained_function_call_replay`
- `responses_websocket_should_not_implicitly_resume_across_codex_windows`
- `responses_websocket_should_evict_reasoning_replay_after_invalid_encrypted_content`
- `responses_websocket_should_restore_full_history_when_implicit_resume_previous_response_is_missing`

TDD/debug evidence:

- The new WebSocket implicit-resume tests initially exposed that the translator
  was forwarding the downstream non-streaming `stream: false` flag into the
  upstream Codex request. The old service used the OpenAI `stream` flag only for
  downstream response shape and still sent Codex Responses requests with the
  upstream streaming default. Removing that assignment restored old behavior.
- The admin status-cycle test initially showed pooled sockets were not evicted
  because the pool key uses upstream `chatgpt-account-id`, while the admin route
  receives the internal account id. Evicting both identifiers closed the gap.

Verification run for this slice:

- `cargo test -p codex-proxy-server --test openai_chat_upstream implicitly_resume -- --nocapture`
- `cargo test -p codex-proxy-server --test openai_chat_upstream implicit_resume -- --nocapture`
- `cargo test -p codex-proxy-server --test openai_chat_upstream reasoning_replay -- --nocapture`
- `cargo test -p codex-proxy-server --test openai_chat_upstream affinity_restore -- --nocapture`
- `cargo test -p codex-proxy-server --test openai_chat_upstream status_cycle -- --nocapture`
- `cargo test -p codex-proxy-server --test openai_chat_upstream -- --nocapture`
  (103/103 passed)
- `cargo test -p codex-proxy-core --tests`
- `cargo test -p codex-proxy-adapters --test codex codex_backend_client_should_close_idle_pooled_websocket_when_account_is_evicted -- --nocapture`
- `cargo test -p codex-proxy-runtime --tests`
- `cargo test -p codex-proxy-adapters --tests`
- `cargo test -p codex-proxy-server --test admin_accounts_routes -- --nocapture`
  (49/49 passed)
- `cargo fmt --all`
- `cargo fmt --all -- --check`
- `cargo check --workspace --tests`
- `codegraph sync . && codegraph status .`
  (index up to date with 282 files, 4,198 nodes, and 12,743 edges)

Current migration counters after this slice:

- 454 `#[test]` / `#[tokio::test]` occurrences.
- 62 Rust files contain tests.
- 14 root Rust test files, all architecture tests.
- 40 crate-local Rust test files under `crates/*/tests`.
- Empty source-file scan found no empty files under `tests`, `crates`, or `src`.
- Empty-directory scan found no empty directories outside ignored locations
  after excluding `.git`, `target`, `.codegraph`, and `web/node_modules`.

Remaining after this slice:

- The old Responses WebSocket default/pool/metadata and implicit
  resume/reasoning-replay blocks are now represented in crate-local server
  coverage.
- The remaining Responses WebSocket block should continue with fallback and
  account-status routing cases from the old suite: previous-response routing,
  WebSocket 429 fallback, 401/402/model/path-block no-fallback stream errors,
  quota-classified `response.failed`, and any pool-specific audit artifact
  edges still not represented.
- The old gateway WebSocket suite remains a high-risk unmigrated adapter/protocol
  behavior block after the Responses WebSocket suite.

### 2026-06-19 Slice 85: Responses WebSocket Fallback / Status Routing Bulk Migration

Status: implemented and verified for the remaining audited fallback/status
block from old `tests/codex_serving/responses_websocket.rs`.

Changes:

- Migrated old Responses WebSocket fallback/status assertions into
  `crates/server/tests/openai_chat_upstream.rs` for:
  - previous-response-id routing to the recorded account;
  - WebSocket 429 fallback for streaming and non-streaming continuations;
  - fallback exhaustion after mixed 429 -> 401 errors;
  - WebSocket 402 quota exhaustion with no fallback;
  - model-unsupported and Cloudflare path-block stream failures with no
    fallback;
  - WebSocket `response.failed` quota classification and fallback retry;
  - rate-limit exhaustion across two fallback accounts.
- Updated WebSocket opening failures in `crates/adapters` so non-101 upgrade
  responses preserve upstream HTTP status, retry-after, and body for runtime
  fallback classification instead of collapsing into an opaque transport error.
- Kept `WebSocketPreferred` HTTP SSE fallback only for non-upstream transport
  failures. Business/status errors from the WebSocket upgrade now reach runtime
  fallback logic.
- Kept history recovery retries on WebSocket instead of forcing HTTP SSE after
  stripping `previous_response_id`. Older crate-local history recovery tests
  were updated to assert WebSocket -> WebSocket retry, matching the old
  Responses WebSocket suite.
- Added last-exhausted-account classification for Responses dispatch fallback
  exhaustion so mixed failures such as 429 followed by 401 return the final
  authentication failure instead of a stale rate-limit aggregate.
- Preserved current aggregate error wording while restoring the old WebSocket
  stream fragments required by the baseline for model-unsupported and
  Cloudflare path-block no-fallback cases.
- Updated `docs/architecture.md` and the architecture source-tree whitelist for
  `core/src/serving/implicit_resume.rs` and
  `core/src/serving/reasoning_replay.rs`, which were introduced in Slice 84.

Migrated tests added in this slice:

- `responses_websocket_should_route_previous_response_id_to_recorded_account`
- `responses_websocket_non_stream_previous_response_not_found_should_strip_history_and_retry_same_account`
- `responses_websocket_stream_previous_response_not_found_should_strip_history_and_retry_same_account`
- `responses_websocket_non_stream_unanswered_function_call_should_strip_history_and_retry_same_account`
- `responses_websocket_previous_response_id_should_retry_fallback_account_after_429`
- `responses_websocket_non_stream_previous_response_id_should_retry_fallback_account_after_429`
- `responses_websocket_without_history_should_mark_expired_after_fallback_401`
- `responses_websocket_without_history_should_return_rate_limit_stream_error_when_fallback_accounts_exhausted`
- `responses_websocket_response_failed_quota_should_retry_fallback_account`
- `responses_websocket_without_history_should_return_quota_stream_error_when_402_has_no_fallback`
- `responses_websocket_without_history_should_return_model_unsupported_stream_error_when_no_fallback`
- `responses_websocket_with_history_should_return_path_block_stream_error_when_no_fallback`

TDD/debug evidence:

- Initial focused run of `responses_websocket` failed for WebSocket upgrade
  status handling, history recovery transport selection, pool-reuse test
  coverage, and no-fallback stream error fragments. The new tests now exercise
  those paths directly.
- The mixed 429 -> 401 case identified a real runtime aggregation bug: fixed
  priority returned rate-limit when the last exhausted account failed auth. The
  runtime now records the last exhaustion class for Responses complete/stream
  dispatch.
- The `strip_history` regression run exposed stale crate-local tests that still
  expected WebSocket recovery to fall back to HTTP SSE. Those helpers now model
  WebSocket failure followed by WebSocket success, matching the migrated old
  Responses WebSocket behavior.
- `cargo test --test architecture` exposed that the Slice 84 core serving files
  were not reflected in the architecture whitelist; the architecture document
  and test whitelist now match the actual tree.

Verification run for this slice:

- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_websocket -- --nocapture`
  (25/25 passed)
- `cargo test -p codex-proxy-server --test openai_chat_upstream strip_history -- --nocapture`
  (9/9 passed)
- `cargo test -p codex-proxy-adapters --test codex websocket_ -- --nocapture`
  (26/26 passed)
- `cargo test -p codex-proxy-adapters --tests`
- `cargo test -p codex-proxy-runtime --tests`
- `cargo test -p codex-proxy-server --test openai_chat_upstream -- --nocapture`
  (115/115 passed)
- `cargo check --workspace --tests`
- `cargo fmt --all -- --check`
- `cargo test --test architecture`
  (27/27 passed)
- `cargo test --workspace --tests`
- `codegraph sync .`
- `codegraph status .`
  (index up to date with 282 files, 4,228 nodes, and 12,874 edges)
- Empty file scan under `tests`, `crates`, and `src` found no empty Rust/TOML/MD
  files.
- Empty-directory scan found no empty directories outside ignored `.git`,
  `target`, `.codegraph`, and `web/node_modules` paths.

Current migration counters after this slice:

- 466 `#[test]` / `#[tokio::test]` occurrences.
- 62 Rust files contain tests.
- 15 root Rust test files, all architecture tests.
- 40 crate-local Rust test files under `crates/*/tests`.
- `crates/server/tests/openai_chat_upstream.rs` now has 115 tests.

Remaining after this slice:

- The audited old Responses WebSocket fallback/status block is now represented
  in crate-local server coverage alongside the default/pool/metadata and
  implicit-resume/reasoning-replay blocks.
- The old gateway WebSocket suite remains a high-risk adapter/protocol behavior
  block for the next migration stage, especially pure codec diagnostics,
  opening/audit artifacts, transport edge cases, and pool parity.
- Continue auditing old pre-migration behavior against
  `f07442a28b0c186bd22b598a53cc4856ab4b2445`; passing current workspace tests
  still does not by itself prove all old behavior suites have been migrated.

### 2026-06-19 Slice 86: Gateway WebSocket Adapter/Protocol Bulk Migration

Status: implemented and verified for the next audited block from old
`tests/codex_gateway/websocket.rs` and `tests/codex_gateway/websocket/pool.rs`.

Changes:

- Migrated old gateway WebSocket pool parity into
  `crates/adapters/tests/codex.rs` for:
  - bypassing a busy pooled key with one-shot connections;
  - bypassing new conversation keys after the per-account pool cap;
  - evicting idle connections when maintenance ping times out;
  - garbage-collecting expired idle connections.
- Migrated old gateway WebSocket adapter/protocol boundary coverage for:
  - opening failure status/body/retry-after preservation;
  - binary upstream frames surfacing as explicit WebSocket errors instead of
    being silently ignored until close;
  - wrapped `error` frames preserving explicit status and `retry-after`;
  - `websocket_connection_limit_reached` mapping to retryable 503;
  - internal `codex.rate_limits` events updating captured rate-limit headers
    without forwarding to downstream SSE;
  - `response.metadata` turn-state updates without downstream forwarding;
  - live stream partial output followed by close-before-terminal error.
- Added `CodexWebSocketExchangeError::UnexpectedBinaryEvent` and wired both
  buffered and live-stream WebSocket paths to emit it for upstream binary
  events before a terminal response.
- Kept Ping/Pong and Close handling separate: ping/pong/control frames continue
  through tungstenite behavior, close before terminal remains
  `ClosedBeforeTerminal`, and binary application data is now a distinct
  protocol error.

Migrated tests added in this slice:

- `websocket_pool_should_bypass_busy_key_with_one_shot_connections`
- `websocket_pool_should_bypass_new_keys_after_account_cap`
- `websocket_pool_should_evict_idle_connection_when_ping_times_out`
- `websocket_pool_should_gc_expired_idle_connections`
- `websocket_execute_response_create_request_should_preserve_opening_error_status_body_and_retry_after`
- `websocket_execute_response_create_request_should_reject_binary_event`
- `websocket_execute_response_create_request_should_surface_wrapped_error_status_and_retry_after`
- `websocket_execute_response_create_request_should_surface_connection_limit_as_503`
- `websocket_execute_response_create_request_should_capture_internal_metadata_and_rate_limit_events`
- `codex_backend_client_stream_should_reject_binary_websocket_event`
- `codex_backend_client_stream_should_error_when_websocket_closes_before_terminal`

Verification run for this slice so far:

- `cargo fmt -p codex-proxy-adapters`
- `cargo test -p codex-proxy-adapters --test codex websocket_ -- --nocapture`
  (37/37 passed)
- `cargo test -p codex-proxy-adapters --tests`
  (`crates/adapters/tests/codex.rs`: 55/55 passed)
- `codegraph sync . && codegraph status .`
  (index up to date with 282 files, 4,243 nodes, and 12,942 edges)
- `cargo fmt --all -- --check`
- `cargo check --workspace --tests`
- `cargo test --test architecture`
  (27/27 passed)
- `cargo test --workspace --tests`
  (workspace test suite passed; notable migrated suites:
  `crates/adapters/tests/codex.rs` 55/55,
  `crates/server/tests/openai_chat_upstream.rs` 115/115)

Current migration counters after this slice:

- 477 `#[test]` / `#[tokio::test]` occurrences.
- 62 Rust files contain tests.
- 15 root Rust test files, all architecture tests.
- 40 crate-local Rust test files under `crates/*/tests`.
- `crates/adapters/tests/codex.rs` now has 55 tests, including 37 WebSocket
  filtered tests.

Remaining after this slice:

- The old gateway WebSocket transport/pool/status/internal-event block is now
  represented in crate-local adapter/core coverage instead of root behavior
  tests.
- Remaining gateway WebSocket risk is narrower: byte-for-byte capture harness
  parity, old audit artifact edge cases not represented by current redacted
  artifacts/event logs, and any assertion-level official-client event-shape
  cases not already covered by the pure core validators.
- Continue auditing old pre-migration behavior against
  `f07442a28b0c186bd22b598a53cc4856ab4b2445`; this slice does not close
  serving fallback/recovery, admin import/export, or the remaining
  non-WebSocket removed suites.

### 2026-06-19 Slice 87: Gateway WebSocket Core Event Classification Migration

Status: implemented and verified for the pure protocol/assertion block from old
`tests/codex_gateway/websocket.rs`.

Changes:

- Migrated old gateway WebSocket core assertions into
  `crates/core/tests/protocol.rs` for:
  - special upstream error code classification: quota, auth, banned, previous
    response missing, overload, and usage-not-included;
  - success-status `error` frames and unmapped `error` frames being ignored
    instead of treated as terminal upstream failures;
  - malformed `response.completed` shapes with missing id or incomplete usage;
  - optional `null` fields matching missing-field behavior for message,
    custom-tool, and context-compaction events;
  - optional field type mismatches for tool search, custom tool output, image
    generation, and context compaction items;
  - nested shape mismatches for local-shell and web-search actions;
  - invalid JSON and official event shape mismatches being skipped before SSE
    forwarding.
- No production code changes were needed in this slice; the migrated assertions
  validated that the current pure core validators already cover these old
  gateway behaviors.

Migrated tests added in this slice:

- `codex_websocket_error_frame_should_classify_old_gateway_special_codes`
- `codex_websocket_error_frame_should_ignore_success_status_and_unmapped_error`
- `codex_websocket_response_completed_parse_error_should_reject_missing_id_and_incomplete_usage`
- `codex_websocket_optional_null_fields_should_match_missing_field_behavior`
- `codex_websocket_output_item_optional_fields_should_reject_old_gateway_edge_cases`
- `codex_websocket_local_shell_and_web_search_should_reject_nested_shape_mismatches`
- `codex_websocket_event_to_sse_frame_should_skip_invalid_json_and_shape_mismatches`

Verification run for this slice so far:

- `cargo fmt -p codex-proxy-core`
- `cargo test -p codex-proxy-core --test protocol -- --nocapture`
  (59/59 passed)
- `codegraph sync . && codegraph status .`
  (index up to date with 282 files, 4,250 nodes, and 12,957 edges)
- `cargo fmt --all -- --check`
- `cargo check --workspace --tests`
- `cargo test --test architecture`
  (27/27 passed)
- `cargo test --workspace --tests`
  (workspace test suite passed; notable migrated suites:
  `crates/core/tests/protocol.rs` 59/59,
  `crates/adapters/tests/codex.rs` 55/55,
  `crates/server/tests/openai_chat_upstream.rs` 115/115)
- Empty Rust/TOML/MD file scan under `tests`, `crates`, and `src` found no
  empty files.
- Empty-directory scan found no empty directories outside ignored `.git`,
  `target`, `.codegraph`, and `web/node_modules` paths.
- Placeholder marker scan only found the architecture guard/test strings that
  enforce placeholder detection.

Current migration counters after this slice:

- 484 `#[test]` / `#[tokio::test]` occurrences.
- 62 Rust files contain tests.
- 15 root Rust test files, all architecture tests.
- 40 crate-local Rust test files under `crates/*/tests`.
- `crates/core/tests/protocol.rs` now has 59 tests.

Remaining after this slice:

- The old gateway WebSocket pure event-shape/status-classification block is now
  represented in `core` coverage.
- Remaining gateway WebSocket risk is primarily the old capture harness and
  byte-for-byte audit artifact parity, not the core event validator or adapter
  transport/status/pool behavior already migrated in Slices 86 and 87.

### 2026-06-19 Slice 88: Gateway WebSocket Security Chain Header/Payload Migration

Status: implemented and verified for the old gateway WebSocket
security-chain/header-body parity block.

Changes:

- Added adapter-level live WebSocket coverage proving the real
  `CodexBackendClient` path forwards security-chain context through the
  WebSocket opening and first `response.create` payload:
  - `session_id` derives `x-client-request-id`, `prompt_cache_key`,
    `session-id`, and `thread-id`;
  - client metadata preserves string-only user metadata and drops non-string
    values;
  - installation id, window id, turn metadata, and parent thread id are added
    to `client_metadata`;
  - `x-openai-subagent` is promoted from metadata into the opening headers.
- Fixed the WebSocket request header builder so HTTP/SSE-only headers are not
  forwarded into WebSocket openings:
  - removes `content-type`;
  - removes `accept`;
  - replaces HTTP `session_id` with WebSocket `session-id` and `thread-id`.

Migrated test added in this slice:

- `codex_backend_client_websocket_should_forward_security_chain_headers_and_payload_fields`

Verification run for this slice so far:

- `cargo fmt -p codex-proxy-adapters`
- `cargo test -p codex-proxy-adapters --test codex websocket_ -- --nocapture`
  (38/38 passed)
- `cargo test -p codex-proxy-adapters --tests`
  (`crates/adapters/tests/codex.rs`: 56/56 passed)
- `codegraph sync . && codegraph status .`
  (index up to date with 282 files, 4,252 nodes, and 12,947 edges)
- `cargo fmt --all -- --check`
- `cargo check --workspace --tests`
- `cargo test --test architecture`
  (27/27 passed)
- `cargo test --workspace --tests`
  (workspace test suite passed; notable migrated suites:
  `crates/adapters/tests/codex.rs` 56/56,
  `crates/core/tests/protocol.rs` 59/59,
  `crates/server/tests/openai_chat_upstream.rs` 115/115)
- Empty Rust/TOML/MD file scan under `tests`, `crates`, and `src` found no
  empty files.
- Empty-directory scan found no empty directories outside ignored `.git`,
  `target`, `.codegraph`, and `web/node_modules` paths.
- Placeholder marker scan only found the architecture guard/test strings that
  enforce placeholder detection.

Current migration counters after this slice:

- 485 `#[test]` / `#[tokio::test]` occurrences.
- 62 Rust files contain tests.
- 15 root Rust test files, all architecture tests.
- 40 crate-local Rust test files under `crates/*/tests`.
- `crates/adapters/tests/codex.rs` now has 56 tests, including 38 WebSocket
  filtered tests.

Remaining after this slice:

- The old gateway WebSocket security-chain header/body block is now represented
  in adapter coverage and production WebSocket headers no longer carry HTTP/SSE
  body headers.
- Remaining gateway WebSocket risk is now focused on byte-for-byte capture
  harness/audit artifact parity and any still-unmapped assertion-level cases
  from the historical root suite.

### 2026-06-19 Slice 89: Gateway WebSocket Capture Harness / Idle Timeout Migration

Status: implemented and verified for the old gateway WebSocket capture harness,
artifact redaction, ordered first-frame serialization, and silent-upstream
timeout block.

Changes:

- Migrated the old gateway WebSocket capture harness assertions into
  crate-local core/adapter coverage:
  - `CodexWebSocketConnection::opening_request_text()` now exposes the actual
    WebSocket opening request text used by the adapter for capture/audit parity.
  - Adapter tests assert the raw opening request line, standard WebSocket
    headers, business-header order, session/thread headers, turn metadata, and
    permessage-deflate offer order.
  - Core and adapter tests assert the first `response.create` WebSocket frame
    is serialized in the old client field order rather than `serde_json::Map`
    key order.
  - The audit artifact write test now builds real redacted opening/payload
    snapshots and asserts access tokens, account IDs, cookies, user prompt text,
    prompt cache keys, and thread metadata are not written.
- Restored the old silent-upstream behavior:
  WebSocket receive loops now fail with an explicit 20-second
  `ReceiveIdleTimeout` when the upstream accepts the opening and first frame but
  sends no events. The timeout is shared by buffered and live streaming
  WebSocket response paths.

TDD/coverage evidence:

- RED:
  `cargo test -p codex-proxy-adapters --test codex capture -- --nocapture`
  initially failed because `CodexWebSocketConnection::opening_request_text()`
  did not exist.
- RED:
  after adding the raw opening API, the same capture filter failed because
  `websocket_response_create_payload_text(...)` serialized the first frame in
  sorted `serde_json::Map` key order, with `client_metadata` before `type`.
- RED:
  `cargo test -p codex-proxy-core --test protocol codex_websocket_response_create_payload_text_should_preserve_old_field_order -- --nocapture`
  failed for the same sorted field order.
- RED:
  `cargo test -p codex-proxy-adapters --test codex silent -- --nocapture`
  failed to compile because the adapter had no `ReceiveIdleTimeout` error
  variant.
- GREEN:
  the core ordered serializer, adapter raw-opening capture tests, artifact
  redaction test, and silent-upstream timeout test now pass.

Verification run for this slice so far:

- `cargo test -p codex-proxy-core --test protocol codex_websocket_response_create_payload_text_should_preserve_old_field_order -- --nocapture`
- `cargo test -p codex-proxy-adapters --test codex capture -- --nocapture`
- `cargo test -p codex-proxy-adapters --test codex silent -- --nocapture`
- `cargo test -p codex-proxy-adapters --test codex websocket_audit_artifact_should_require_explicit_directory -- --nocapture`
- `cargo test -p codex-proxy-core --test protocol websocket -- --nocapture`
  (33/33 filtered WebSocket/core protocol tests passed)
- `cargo test -p codex-proxy-adapters --test codex websocket_ -- --nocapture`
  (41/41 filtered WebSocket adapter tests passed)
- `cargo test -p codex-proxy-adapters --tests`
  (`crates/adapters/tests/codex.rs`: 59/59 passed)
- `cargo test -p codex-proxy-core --test protocol -- --nocapture`
  (`crates/core/tests/protocol.rs`: 60/60 passed)
- `cargo fmt --all -- --check`
- `cargo check --workspace --tests`
- `cargo test --test architecture`
  (27/27 architecture tests passed)
- `cargo test --workspace --tests`
  (workspace test suite passed; notable migrated suites:
  `crates/adapters/tests/codex.rs` 59/59,
  `crates/core/tests/protocol.rs` 60/60,
  `crates/server/tests/openai_chat_upstream.rs` 115/115)
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- Empty Rust/TOML/MD file scan under `tests`, `crates`, and `src` found no
  empty files.
- Empty-directory scan found no empty directories outside ignored `.git`,
  `target`, `.codegraph`, and `web/node_modules` paths.
- Placeholder marker scan only found architecture guard/test strings,
  documented API/product compatibility wording, and model alias terminology; no
  source placeholder or migration shim introduced by this slice.

Current migration counters after this slice:

- 488 `#[test]` / `#[tokio::test]` occurrences.
- 62 Rust files contain tests.
- 15 root Rust test files, all architecture tests.
- 40 crate-local Rust test files under `crates/*/tests`.
- `crates/adapters/tests/codex.rs` now has 59 tests.

Remaining after this slice:

- The old gateway WebSocket byte-for-byte capture harness is now represented by
  raw opening text, first-frame order, redacted audit artifact, opening-status,
  deflate, ping, binary-frame, idle-timeout, wrapped-error, connection-limit,
  metadata, rate-limit, pool, previous-response, history-recovery, and
  security-chain coverage.
- Remaining gateway WebSocket risk is now limited to assertion-level parity
  gaps found by any later direct old/new comparison; no separate high-risk
  capture harness block is currently known.

## Verification After Current Implementation Pass

Commands run after Slice 12:

- `cargo fmt --all -- --check`
- `cargo test -p codex-proxy-runtime --test tasks`
- `cargo test --test architecture`
- `cargo check --workspace --all-targets`
- `cargo test -p codex-proxy-runtime --test token_refresh`
- `cargo test -p codex-proxy-runtime --test quota_refresh`
- `cargo test -p codex-proxy-adapters --test account_repository account_repository_should_update_quota_json_and_fetched_at`
- `cargo test -p codex-proxy-adapters --test codex websocket_`
- `cargo test --workspace --all-targets`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `cargo test -p codex-proxy-adapters --test codex`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo test -p codex-proxy-core --test models`
- `cargo test -p codex-proxy-core --test session_affinity`
- `cargo test -p codex-proxy-adapters --test session_affinity`
- `cargo test -p codex-proxy-runtime --test session_affinity`
- `cargo check -p codex-proxy-server --all-targets`
- `cargo test -p codex-proxy-runtime --test account_pool_restore`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_prefer_session_affinity_account_for_previous_response`
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_record_session_affinity_for_completed_response`
- `cargo check --workspace --all-targets`
- `cargo fmt --all -- --check`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo test --test architecture`
- `codegraph sync .`
- `cargo test -p codex-proxy-runtime --test tasks session_affinity_cleanup_task_should_delete_only_expired_affinities`
- `cargo test -p codex-proxy-runtime --test tasks start_background_tasks_should_register_migrated_runtime_tasks`
- `cargo test -p codex-proxy-runtime --test tasks`
- `cargo test --test architecture`
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test admin_accounts_routes admin_account_quota_should_fetch_usage_store_quota_and_not_return_secrets`
- `cargo test -p codex-proxy-server --test admin_accounts_routes`
- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test admin_accounts_routes admin_account_quota_should_return_bad_gateway_when_usage_fetch_fails`
- `cargo test -p codex-proxy-server --test admin_accounts_routes`
- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_fallback_to_next_account_after_rate_limit`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream chat_completions_should_fallback_to_next_account_after_rate_limit`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream chat_completions_should_fallback_to_next_account_after_rate_limit` (red check before Slice 18 implementation; failed as expected on `quota_limit_reached`)
- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream chat_completions_should_fallback_to_next_account_after_rate_limit`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream strip_history` (red check before Slice 37 implementation; failed 3/3 as expected because no history recovery retry was sent)
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream strip_history`
- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream strip_history`
- `cargo test -p codex-proxy-core --test protocol openai_response_request_should_translate_to_codex_request`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream model_unsupported` (red check before Slice 36 implementation; failed 4/4 as expected on missing model-unsupported fallback/no-fallback behavior)
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream model_unsupported`
- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream model_unsupported`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream cloudflare` (red check before Slice 35 implementation; failed 7/7 as expected on missing Cloudflare fallback/cooldown/cookie/path-block behavior)
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream cloudflare`
- `cargo fmt --all`
- `cargo test -p codex-proxy-core cloudflare`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture` (first post-implementation run failed on the new architecture whitelist entry, then the whitelist/docs were updated)
- `cargo test --test architecture`
- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream cloudflare`
- `cargo test -p codex-proxy-core cloudflare`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_retry_same_account_after_5xx_before_fallback` (red check before Slice 19 implementation; failed as expected with HTTP `502`)
- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_retry_same_account_after_5xx_before_fallback`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_record_request_count_when_5xx_retries_are_exhausted` (red check before Slice 20 implementation; failed as expected with `request_count = 0`)
- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_record_request_count_when_5xx_retries_are_exhausted`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo test -p codex-proxy-runtime --test account_pool_restore`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_fallback_to_next_account_after_rate_limit` (red check before Slice 21 implementation; failed as expected with secondary token usage `(1, 0, 0)`)
- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_fallback_to_next_account_after_rate_limit`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo test -p codex-proxy-adapters --test account_repository`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream chat_completions_should_dispatch_to_codex_and_return_openai_response` (red check before Slice 22 implementation; failed as expected with Chat usage `(1, 0, 0)`)
- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream chat_completions_should_dispatch_to_codex_and_return_openai_response`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_mark_quota_exhausted_after_402_and_fallback` (red check before Slice 23 implementation; failed as expected with HTTP `502`)
- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_mark_quota_exhausted_after_402_and_fallback`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo test -p codex-proxy-adapters --test account_repository`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_classify_sse_quota_failure_and_fallback` (red check before Slice 24 implementation; failed as expected with HTTP `502`)
- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_classify_sse_quota_failure_and_fallback`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream chat_completions_should_mark_quota_exhausted_after_402_and_fallback` (red check before Slice 25 implementation; failed as expected with HTTP `502`)
- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream chat_completions_should_mark_quota_exhausted_after_402_and_fallback`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream chat_completions_should_return_quota_error_when_402_fallback_is_exhausted` (red check before Slice 26 implementation; failed as expected with HTTP `503`)
- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream chat_completions_should_return_quota_error_when_402_fallback_is_exhausted`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_return_quota_error_when_402_fallback_is_exhausted` (red check before Slice 27 implementation; failed as expected with HTTP `503`)
- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_return_quota_error_when_402_fallback_is_exhausted`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream chat_completions_should_return_rate_limit_error_when_429_fallback_is_exhausted` (red check before Slice 28 implementation; failed as expected with HTTP `503`)
- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream chat_completions_should_return_rate_limit_error_when_429_fallback_is_exhausted`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_return_rate_limit_error_when_429_fallback_is_exhausted` (red check before Slice 29 implementation; failed as expected with HTTP `503`)
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_should_return_rate_limit_error_when_429_fallback_is_exhausted`
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_stream_should` (red check before Slice 30 implementation; after tightening the 5xx assertion, all four stream tests failed as expected on missing `text/event-stream`)
- `cargo test -p codex-proxy-server --test openai_chat_upstream responses_stream_should`
- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream 401` (red check before Slice 31 implementation; failed as expected with HTTP `502` for Chat/Responses fallback and missing stream fallback usage)
- `cargo test -p codex-proxy-server --test openai_chat_upstream sse_failed_event` (red check before Slice 31 implementation; failed as expected with HTTP `502`)
- `cargo test -p codex-proxy-server --test openai_chat_upstream 401`
- `cargo test -p codex-proxy-server --test openai_chat_upstream sse_failed_event`
- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream auth_error_when_401` (red check before Slice 32 implementation; failed as expected with HTTP `503`/no-active stream error)
- `cargo test -p codex-proxy-server --test openai_chat_upstream auth_error_when_401`
- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream deactivated` (red check before Slice 33 implementation; failed as expected with persisted status `expired`)
- `cargo test -p codex-proxy-server --test openai_chat_upstream deactivated`
- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`
- `cargo test -p codex-proxy-server --test openai_chat_upstream after_403` (red check before Slice 34 implementation; failed as expected with HTTP `502` and missing fallback stream response id)
- `cargo test -p codex-proxy-server --test openai_chat_upstream after_403`
- `cargo fmt --all`
- `cargo test -p codex-proxy-server --test openai_chat_upstream`
- `cargo check --workspace --all-targets`
- `cargo test --test architecture`
- `codegraph sync .`

Result:

- Commands listed before Slice 7 completed successfully in the previous pass.
- The Slice 7 through Slice 12 targeted commands completed successfully in the
  current pass.
- `codegraph sync .` was rerun after the Slice 7 implementation and synced 12
  changed files.
- `codegraph sync .` was rerun after the Slice 8 implementation, syncing 10
  changed files, then rerun after formatting and synced 1 changed file.
- `codegraph sync .` was rerun after the Slice 9 implementation, syncing 5
  changed files, then rerun after formatting and synced 1 changed file.
- `codegraph sync .` was rerun after the Slice 10 implementation, syncing 2
  changed files, then rerun after formatting and synced 1 changed file.
- `codegraph sync .` was rerun after the Slice 11 and Slice 12 implementation
  work, then rerun after fixing the `previous_response_id` protocol test and
  synced 2 changed files.
- `codegraph sync .` was rerun after the Slice 13 session-affinity cleanup task
  work and synced 5 changed files, then rerun after architecture
  spec/whitelist documentation updates and synced 2 changed files.
- `codegraph sync .` was rerun after the Slice 14 explicit admin quota route
  migration and synced 5 changed files.
- `codegraph sync .` was rerun after the Slice 15 quota upstream failure shape
  migration and synced 2 changed files.
- `codegraph sync .` was rerun after the Slice 16 Responses rate-limit account
  fallback migration and synced 2 changed files.
- `codegraph sync .` was rerun after the Slice 17 Chat rate-limit account
  fallback migration and synced 2 changed files.
- `codegraph sync .` was rerun after the Slice 18 persisted quota cooldown
  transition and synced 5 changed files.
- `codegraph sync .` was rerun after the Slice 19 Responses 5xx same-account
  retry migration and synced 2 changed files.
- `codegraph sync .` was rerun after the Slice 20 selected-account request-count
  persistence and synced 5 changed files.
- `codegraph sync .` was rerun after the Slice 21 Responses token usage delta
  persistence and synced 6 changed files.
- `codegraph sync .` was rerun after the Slice 22 Chat token usage delta
  persistence and synced 2 changed files.
- `codegraph sync .` was rerun after the Slice 23 Responses HTTP 402 quota
  exhausted fallback and synced 5 changed files.
- `codegraph sync .` was rerun after the Slice 24 Responses SSE quota failure
  fallback and synced 2 changed files.
- `codegraph sync .` was rerun after the Slice 25 Chat HTTP 402 quota fallback
  and synced 2 changed files.
- `codegraph sync .` was rerun after the Slice 26 Chat HTTP 402 no-fallback
  quota error aggregation and synced 3 changed files.
- `codegraph sync .` was rerun after the Slice 27 Responses HTTP 402
  no-fallback quota error aggregation and synced 3 changed files.
- `codegraph sync .` was rerun after the Slice 28 Chat HTTP 429 no-fallback
  rate-limit error aggregation and synced 3 changed files.
- `codegraph sync .` was rerun after the Slice 29 Responses HTTP 429
  no-fallback rate-limit error aggregation and synced 3 changed files.
- `codegraph sync .` was rerun after the Slice 30 buffered Responses
  `stream: true` fallback and usage work and synced 3 changed files.
- `codegraph sync .` was rerun after the Slice 31 auth failure account
  recovery work and synced 2 changed files.
- `codegraph sync .` was rerun after the Slice 32 auth no-fallback expired
  aggregation work and synced 4 changed files.
- `codegraph sync .` was rerun after the Slice 33 deactivated auth status
  classification work and synced 2 changed files.
- `codegraph sync .` was rerun after the Slice 34 HTTP 403 banned fallback
  work and synced 2 changed files.
- `codegraph sync .` was rerun after adding the Slice 35 Cloudflare tests and
  synced 1 changed file, then rerun after implementation/formatting and synced
  10 changed files.
- `codegraph sync .` was rerun after adding the Slice 36 model-unsupported
  tests and synced 1 changed file, then rerun after implementation/formatting
  and synced 2 changed files.
- `codegraph sync .` was rerun after adding the Slice 37 history recovery tests
  and synced 1 changed file, then rerun after implementation/formatting and
  synced 4 changed files.
- `codegraph sync .` was rerun after Slice 39 token refresh lease coordination
  implementation and synced 7 changed files.
- `codegraph sync .` was rerun after Slice 40 token refresh refreshing recovery
  implementation and synced 3 changed files.
- `codegraph sync .` was rerun after Slice 41 token refresh retry confirmation
  implementation and synced 2 changed files.
- `codegraph sync .` was rerun after Slice 42 token refresh in-flight guard
  implementation and synced 2 changed files.
- `codegraph sync .` was rerun after Slice 43 token refresh delayed recovery
  implementation and synced 2 changed files.
- `codegraph sync .` was rerun after Slice 44 Responses stream no-fallback
  coverage and synced 1 changed file.
- `codegraph sync .` was rerun after Slice 45 admin quota edge-case coverage
  and synced 1 changed file.
- `codegraph sync .` was rerun before Slice 49 token refresh timer work and
  synced the 2 changed WIP files already present at the start of the pass.
- `codegraph sync . && codegraph status .` was rerun after Slice 49 token
  refresh timer implementation and reported the index up to date with 271 files,
  3,213 nodes, and 8,855 edges.
- `codegraph sync . && codegraph status .` was rerun after Slice 50 Responses
  stream done termination and reported the index up to date with 271 files,
  3,215 nodes, and 8,879 edges.
- `codegraph sync . && codegraph status .` was rerun after Slice 51 WebSocket
  transport decision and audit snapshots and reported the index up to date with
  271 files, 3,242 nodes, and 8,977 edges.
- `codegraph sync . && codegraph status .` was rerun after Slice 52 WebSocket
  core codec diagnostics and reported the index up to date with 271 files,
  3,289 nodes, and 9,059 edges.
- `codegraph sync . && codegraph status .` was rerun after Slice 53 WebSocket
  core item validators and reported the index up to date with 271 files, 3,324
  nodes, and 9,144 edges.
- `codegraph sync . && codegraph status .` was rerun after Slice 54 WebSocket
  opening request descriptor and reported the index up to date with 271 files,
  3,337 nodes, and 9,176 edges.
- `codegraph sync . && codegraph status .` was rerun after Slice 55 minimal live
  WebSocket exchange and reported the index up to date with 271 files, 3,356
  nodes, and 9,224 edges.
- `codegraph sync . && codegraph status .` was rerun after Slice 56 Codex
  backend WebSocket required path and reported the index up to date with 271
  files, 3,365 nodes, and 9,276 edges.
- `codegraph sync . && codegraph status .` was rerun after Slice 57 WebSocket
  live invalid-event filtering and reported the index up to date with 271
  files, 3,367 nodes, and 9,295 edges.
- `codegraph sync . && codegraph status .` was rerun after Slice 58 WebSocket
  live control-frame and handshake metadata work and reported the index up to
  date with 271 files, 3,378 nodes, and 9,334 edges.
- `codegraph sync . && codegraph status .` was rerun after Slice 59 WebSocket
  ping coverage and audit artifact IO and reported the index up to date with
  271 files, 3,404 nodes, and 9,433 edges.
- `codegraph sync . && codegraph status .` was rerun after Slice 60 WebSocket
  deflate frame rewriter work and reported the index up to date with 271 files,
  3,422 nodes, and 9,482 edges.
- `codegraph sync . && codegraph status .` was rerun after Slice 61 live HTTP
  SSE body forwarding and reported the index up to date with 271 files, 3,456
  nodes, and 9,738 edges.
- `codegraph sync . && codegraph status .` was rerun after Slice 62 WebSocket
  pool reuse/error-discard work and reported the index up to date with 271 files,
  3,493 nodes, and 9,871 edges after lint cleanup.
- `codegraph sync . && codegraph status .` was rerun during Slice 63 WebSocket
  pool lifecycle/runtime wiring work and reported the index up to date with 272
  files, 3,535 nodes, and 10,058 edges before the documentation-only update.
- `codegraph sync . && codegraph status .` was rerun during Slice 64 server
  WebSocketRequired work and reported the index up to date with 272 files, 3,550
  nodes, and 10,120 edges before the documentation-only update.
- `codegraph sync . && codegraph status .` was rerun after Slice 65 live
  WebSocket streaming/deflate work and reported the index up to date with 272
  files, 3,595 nodes, and 10,382 edges before the documentation-only update.
- `codegraph sync . && codegraph status .` was rerun after Slice 66 live stream
  late-failure SSE reporting work and reported the index up to date with 272
  files, 3,609 nodes, and 10,418 edges before the documentation-only update.
- `codegraph sync . && codegraph status .` was rerun after Slice 67 live stream
  event-log audit work and reported the index up to date with 272 files, 3,623
  nodes, and 10,460 edges before the documentation-only update.
- `codegraph sync . && codegraph status .` was rerun after Slice 68 stream audit
  metadata/log-policy work and reported the index up to date with 272 files,
  3,652 nodes, and 10,554 edges before the documentation-only update.
- `codegraph sync . && codegraph status .` was rerun after Slice 69 serving toy
  helper cleanup and reported the index up to date with 272 files, 3,656 nodes,
  and 10,555 edges before the documentation-only update.
- `codegraph sync . && codegraph status .` was rerun after Slice 70 admin
  settings/test-migration guard work and reported the index up to date with 274
  files, 3,733 nodes, and 10,666 edges before this documentation-only update.
- `codegraph sync . && codegraph status .` was rerun after Slice 71 admin
  client-key assertion coverage migration and reported the index up to date with
  274 files, 3,737 nodes, and 10,716 edges before this documentation-only
  update.

Historical high-risk gap log before the current full-audit pass:

- Full WebSocket parity still needs a focused pass for byte-for-byte capture
  harness/audit artifact edge cases not represented by the restored opening,
  live streaming, deflate, pool, ping, binary-frame rejection, opening-status
  preservation, wrapped-error status/retry-after handling, connection-limit
  mapping, history-recovery, previous-response, event-log, rate-limit-update,
  metadata, special error-code classification, optional/null event field
  handling, invalid event-shape skipping, security-chain header/body forwarding,
  HTTP-header exclusion from WebSocket openings, default/pool/metadata route
  behavior, and implicit resume/reasoning-replay coverage. Live WebSocket body
  forwarding, streaming `previous_response_id` dispatch, live deflate stream
  integration, actual WebSocket stream audit transport marking, internal
  `codex.rate_limits` update snapshots, old gateway WebSocket
  pool/status/internal-event parity, security-chain parity, pure
  event-shape/status-classification parity, and the latest Responses WebSocket
  implicit-resume/replay cases are now implemented and covered by tests.
- Serving dispatch/fallback/recovery remains materially reduced, though
  non-streaming Chat and Responses now retry the next account after an HTTP 429
  and persist the quota cooldown transition for that fallback path, and
  non-streaming Responses now retry the same account for transient upstream
  HTTP 5xx responses. HTTP SSE `stream: true` now has a live chunk-by-chunk
  success path, first-event SSE failure fallback preservation, completed usage
  persistence, HTTP 429/402 fallback, same-account 5xx retry, and
  client-visible late-failure `response.failed(stream_disconnected)` reporting.
  Persisted live stream event-log audit is restored for completed and
  late-failure streams, including body-capture policy, usage, rate-limit
  snapshots, and actual WebSocket transport metadata. Full old stream audit
  artifacts remain incomplete only for any pool-specific artifact details that
  are separate from event-log metadata.
- Selected-account request-count persistence is restored, and persisted token
  usage deltas are restored for non-streaming Responses and Chat completions;
  buffered HTTP SSE completed usage is restored for `stream: true`; live
  streaming usage audit remains incomplete.
- Non-streaming Responses HTTP 402 and SSE quota failure fallback are restored,
  and buffered HTTP SSE stream HTTP 402 fallback plus no-fallback aggregation
  coverage are restored; buffered Responses stream success/error bodies now
  append `[DONE]`; true live chunk-by-chunk stream behavior remains incomplete.
- Multi-account and no-fallback Chat/Responses HTTP 402 quota behavior is
  restored for non-streaming paths, and buffered HTTP SSE stream HTTP 402
  fallback is restored; live streaming/SSE no-fallback aggregation remains
  incomplete.
- Chat and Responses HTTP 429 no-fallback aggregation is restored for
  non-streaming paths; buffered HTTP SSE stream HTTP 429 multi-account fallback
  and no-fallback aggregation coverage are restored.
- HTTP 401 and token-invalid SSE account recovery are restored for the covered
  Chat/Responses paths, including no-fallback expired aggregation and
  deactivated/banned status classification; ordinary HTTP 403 banned fallback
  is restored for covered Chat/Responses paths; Cloudflare challenge/path-block
  recovery is restored for the covered Chat/Responses HTTP and buffered SSE
  paths, but live stream/WebSocket Cloudflare parity remains incomplete.
- Responses model-unsupported fallback is restored for covered non-streaming and
  buffered SSE paths, including exhausted fallback error mapping; Chat HTTP
  model-unsupported fallback is restored for the covered non-streaming path.
- Responses request-history recovery is restored for covered SSE failure paths
  in non-streaming and buffered SSE modes; invalid encrypted reasoning replay
  recovery is restored for covered HTTP error, non-streaming SSE failure,
  buffered SSE failure, and WebSocket implicit-resume replay paths. Responses
  WebSocket implicit resume, reasoning replay, cross-window rejection,
  function-call replay exclusion, SQLite-restored affinity, replay eviction,
  and admin status-cycle pool eviction are covered by crate-local server tests.
- Token refresh now uses the SQLite refresh lease table during scans, persists
  `refreshing` before scheduled refresh calls, recovers persisted `refreshing`
  accounts after restart, restores `active` after retry exhaustion, retries
  transient transport failures, and confirms permanent failures twice before
  persisting terminal statuses. Process-local in-flight protection is also
  restored for concurrent scans, retry exhaustion now schedules a process-local
  delayed recovery window, and the old per-account timer map/next-refresh
  scheduling behavior is restored for background scheduling.
- Quota refresh now covers the old active quota-locked scan, per-account minimum
  refresh window, usage fetch/persist path, and inter-account request spacing.
- Fingerprint update startup/update coverage is migrated at adapter and runtime
  wrapper levels.
- Admin explicit quota route happy path, upstream failure shape, inactive-account
  rejection, persistence-failure response, quota-warning auth, and
  invalid/below-threshold quota-warning edge coverage are restored.
- Admin settings `PATCH /api/admin/settings` retained-field update behavior is
  restored for the current architecture: pure validation in `core`, local YAML
  overlay writing in `platform`, mutable same-process config view in `runtime`,
  and HTTP parsing/error mapping in `server`.
- Assets static serving is no longer an empty scaffold: the assets crate now
  serves SPA/index and `/assets/*` files with cache/security headers, and the
  server router uses it as the final fallback behind API routes.
- Root behavior tests are now guarded against returning under `tests/`, and the
  audited high-risk old suites have crate-local migration-target evidence.
  Remaining risk is assertion-level parity inside those broad suites, not the
  directory placement itself.

## What Is In Good Shape

- Root tracked Rust source has no remaining `src/` facade.
- Architecture boundary tests import member crates directly:
  - `codex_proxy_core`
  - `codex_proxy_runtime`
  - `codex_proxy_server`
- Tracked legacy source directories such as `src/codex`, `src/admin`,
  `src/runtime`, and `src/platform` are removed.
- Current top-level Rust tests are architecture tests only. Shared fixtures
  remain under `tests/fixtures`, which is acceptable as shared test data.
- The current workspace has crate-local tests under `crates/*/tests`.
- `assets` now owns real SPA/static-file routing and header helpers, and server
  uses it as the final fallback after OpenAI/admin routes.

## Historical Blocking Findings

The findings in this section were the original blockers found early in the
migration. They are retained to show the audit trail, but the current verdict
and the 2026-06-19 full-audit pass above supersede any item that has since been
closed.

### 1. Server Entrypoint Startup Was Incomplete

Audit finding before Slice 2:

- `crates/server/src/main.rs` is `fn main() {}`.

Baseline:

- `src/main.rs` loaded configuration, initialized tracing, built state, restored
  accounts, started background tasks, served Axum, handled `ctrl_c`, and shut
  tasks down.

Impact:

- The server binary does not implement the real startup path.
- This alone prevents claiming the runtime/server migration is complete.

Progress:

- Slice 2 replaced the empty main with config loading, logging initialization,
  SQLite connection, secret/API key material loading, runtime state
  construction, Axum serving, and `ctrl_c` graceful shutdown.
- Slice 3 wired the server entrypoint to start the runtime background task
  coordinator and shut it down after Axum graceful shutdown returns.

Remaining:

- Startup now restores persisted accounts into the runtime pool and restores
  session-affinity state. Chat/Responses dispatch uses the restored account
  pool; Responses dispatch also uses restored session affinity for
  preferred-account selection and records completed response affinities.
- Baseline parity remains incomplete in remaining serving fallback/recovery
  branches, quota/account-status WebSocket routing, stream audit artifact edge
  cases, and broader behavior-test parity. Responses WebSocket implicit
  resume/reasoning replay is now covered in Slice 84.

### 2. Xtask Commands Were Not Wired

Audit finding before Slice 2:

- `crates/xtask/src/build_web.rs` prints
  `web build orchestration is not wired yet`.
- `crates/xtask/src/release.rs` prints
  `release packaging is not wired yet`.
- `crates/xtask/src/check_architecture.rs` only prints
  `cargo test --test architecture`; it does not run it.

Impact:

- `xtask` exists as a scaffold, not as the automation crate described in
  `docs/architecture.md`.

Progress:

- Slice 2 wired:
  - `check-architecture` -> `cargo test --test architecture`;
  - `build-web` -> `pnpm install --frozen-lockfile` and `pnpm build`;
  - `release` -> Rust format/test/clippy gates plus web build.

Remaining:

- Keep `release` in the final verification gate after the remaining migration
  slices stabilize.

### 3. Runtime Tasks Were Mostly Empty

Current after Slice 4:

- `crates/runtime/src/tasks/token_refresh.rs` contains a real refresh scan,
  persistence path, and periodic task loop.
- `crates/runtime/src/tasks/quota_refresh.rs` contains a real quota-locked
  account scan, Codex usage fetch, normalized quota persistence path, and
  periodic task loop.

Baseline examples:

- `src/codex/tasks/token_refresh.rs`: 882 lines.
- `src/codex/tasks/quota_refresh.rs`: 117 lines.
- `src/codex/tasks/cookie_cleanup.rs`: 66 lines.
- `src/admin/tasks/session_cleanup.rs`: 59 lines.

Impact:

- Background task behavior has not been fully migrated.
- The current task coordinator cannot be treated as equivalent to the previous
  scheduler set.

Progress:

- Slice 3 reintroduced runtime task structs for cookie cleanup and admin session
  cleanup, plus SQLite cleanup methods and runtime tests.
- Slice 3 also wired the top-level coordinator to start `cookie_cleanup`,
  `session_cleanup`, and `model_refresh`, and wired server shutdown to stop the
  coordinator.
- Slice 4 implemented token/quota task loops and wired both into the top-level
  coordinator.
- Slice 5 wired fingerprint update into the top-level coordinator.
- Slice 13 wired expired session-affinity cleanup into the top-level
  coordinator.
- Slice 40 restored token-refresh `refreshing` persistence before refresher
  calls, restart/crash recovery from persisted `refreshing`, and active
  restoration after transport failure.
- Slice 41 restored token-refresh retry attempts, transient retry success, and
  two-hit permanent failure confirmation.
- Slice 42 restored token-refresh process-local in-flight tracking for
  duplicate concurrent scans.
- Slice 43 restored token-refresh delayed recovery after retry exhaustion.
- Slice 45 migrated explicit admin quota route edge-case coverage for inactive
  accounts, persistence failure, quota-warning auth, and invalid/below-threshold
  snapshots.
- Slice 46 migrated runtime-level fingerprint update startup coverage for
  initial appcast apply, no-update no-op, and `repository: None` semantics.
- Slice 47 restored quota refresh inter-account request spacing with the old
  3-second default.
- Slice 49 restored token-refresh per-account timer scheduling, immediate due
  refresh, planned-trigger-time callbacks, and next-refresh scheduling after a
  successful scheduled refresh.

Remaining:

- No token-refresh-task-specific gap is currently known after restoring lease
  coordination, refreshing recovery, retry/permanent-failure handling,
  in-flight protection, delayed recovery, and per-account timer parity.
- No further quota-refresh-task-specific gap was found in this pass after
  restoring request spacing; broader quota helper/admin behavior should continue
  to be covered through serving/admin tests.

### 4. WebSocket Implementation Was Reduced To Shells

Current after Slice 86:

- `crates/adapters/src/codex/websocket/connect.rs`: now stores endpoint and
  ordered opening headers, and can emit an opening audit snapshot with request
  line plus redacted sensitive headers. It also builds Responses WebSocket
  endpoints, standard opening header descriptors, and a prepared request that
  pairs the opening descriptor with the first `response.create` text frame. It
  now has a minimal one-shot live exchange that opens the WebSocket, sends the
  prepared text frame, buffers public events into SSE until terminal, extracts
  usage, skips invalid public stream events through the migrated core
  validators, captures handshake turn-state/cookies/rate-limit headers, handles
  `response.incomplete` and malformed `response.completed` frames, ignores
  success-status `error` frames, responds to server ping frames through the
  tungstenite read loop, rejects upstream binary application frames as explicit
  protocol errors, preserves non-101 opening status/body/retry-after, captures
  internal metadata and `codex.rate_limits` events without downstream forwarding,
  forwards security-chain headers/body metadata without leaking HTTP/SSE-only
  `content-type`/`accept` headers into the WebSocket opening, emits
  environment-gated redacted audit artifacts, and surfaces classified upstream
  error frames including wrapped errors and connection-limit frames.
- `crates/adapters/src/codex/websocket/pool.rs`: now owns an async idle/busy
  connection pool keyed by base URL, account id, and conversation id. It supports
  max-age/max-per-account policy, acquire/put/discard, explicit idle GC, reuse of
  successful sockets, one-shot bypass for busy keys or over-cap new keys,
  maintenance ping/liveness cleanup, and discard of errored sockets.
- `crates/adapters/src/codex/websocket/deflate.rs`: now detects
  permessage-deflate responses and contains tested raw payload inflation plus
  compressed server data-frame rewriting. It is not yet inserted into the live
  connection path.
- `crates/adapters/src/codex/websocket/opening.rs`: simple audit snapshot helper.
  It also provides explicit and environment-gated WebSocket audit artifact
  writes.
- `crates/core/src/protocol/codex/websocket.rs`: basic frame wrapper
  encode/decode plus payload/opening audit snapshot types and Responses
  payload redaction; pure event-to-SSE conversion; metadata turn-state
  extraction; terminal-event detection; error-frame classification; wrapped
  retry-after parsing; `response.completed` shape validation; and migrated
  stream/output-item diagnostics for the old core bad-frame categories. It now
  also owns pure WebSocket audit artifact and parity-diff structures plus status
  classification for old gateway wrapped errors and connection-limit frames.
- `crates/core/src/serving/responses.rs`: restores the old Responses transport
  decision policy for `HttpSse`, `WebSocketPreferred`, and
  `WebSocketRequired`.
- `crates/adapters/src/codex/client.rs`: `create_response(...)` now honors the
  core transport policy and uses the live WebSocket exchange for required
  WebSocket requests, while preserving HTTP/SSE fallback when allowed.
  `create_response_stream(...)` now exposes a live SSE byte stream for the
  OpenAI `stream: true` path and propagates WebSocket stream protocol errors
  through the stream item.

Baseline:

- `src/codex/gateway/transport/websocket/codec.rs`: 1,147 lines.
- `src/codex/gateway/transport/websocket/mod.rs`: 896 lines.
- `src/codex/gateway/transport/websocket/pool.rs`: 496 lines.
- `src/codex/gateway/transport/websocket/opening.rs`: 495 lines.
- `src/codex/gateway/transport/websocket/deflate.rs`: 273 lines.
- `src/codex/gateway/transport/websocket/audit.rs`: 229 lines.

Impact:

- WebSocket transport, pooling, opening handshake, live forwarding integration,
  and most adapter/protocol parity behavior are now migrated into crate-local
  coverage.
- Remaining risk is concentrated in old byte-for-byte capture harness and audit
  artifact edge cases rather than the previously empty/shell adapter behavior.

Progress:

- Slice 6 removed the empty adapter structs and added basic tests/guards for the
  new connection and pool policy types.
- Slice 51 restored transport decision and opening/payload audit snapshot
  primitives.
- Slice 52 restored a first core-only batch of WebSocket codec diagnostics and
  error classification.
- Slice 53 restored the remaining large batch of old pure output-item and
  stream bad-frame validators into core.
- Slice 54 restored adapter-level endpoint conversion, standard opening header
  descriptor construction, and response-create text-frame preparation.
- Slice 55 restored a minimal adapter-level live WebSocket one-shot exchange and
  response-failed error classification.
- Slice 56 wired `CodexBackendClient::create_response(...)` to use that live
  WebSocket path for required Responses requests.
- Slice 57 wired the migrated core bad-frame validators into live WebSocket
  event forwarding so invalid stream events are dropped before SSE encoding.
- Slice 58 restored live handshake metadata propagation, `response.incomplete`
  error mapping, malformed `response.completed` validation, invalid terminal
  skip semantics, and success-status `error` frame ignoring.
- Slice 59 migrated server-ping response coverage and restored core/adapters
  WebSocket audit artifact construction, diffing, explicit writes, and
  environment-gated client wiring.
- Slice 60 restored tested frame-level permessage-deflate inflation/rewrite
  helpers in the adapter crate.
- Slice 61 restored live HTTP SSE body forwarding for normal `stream: true`
  success paths while preserving first-event `response.failed` fallback/recovery
  before downstream bytes are sent.
- Slice 62 restored adapter-level WebSocket pool reuse for successful
  same-account/same-conversation requests and discard after upstream/transport
  errors.
- Slice 63 restored adapter-level WebSocket pool keepalive maintenance,
  liveness timeout cleanup, shutdown/evict-account APIs, and runtime
  `ws_pool` config injection into `CodexBackendClient`.
- Slice 64 restored non-streaming server/runtime `previous_response_id`
  dispatch through WebSocketRequired, including session-affinity header coverage,
  history-recovery retry behavior, and configured pool reuse.
- Slice 65 restored live WebSocket streaming body forwarding, streaming
  `previous_response_id` WebSocketRequired dispatch, live permessage-deflate
  insertion before tungstenite parsing, and WS-first streaming history recovery
  before downstream bytes are returned.
- Slice 66 restored serving-layer late-failure SSE reporting for live streams
  after downstream bytes have already been sent. Late body read errors and
  premature EOF without a terminal event now append
  `response.failed(stream_disconnected)` and `[DONE]` instead of surfacing a
  transport error to downstream clients.
- Slice 67 restored persisted `v1.response` event-log audit records for live
  stream completion and late-failure outcomes, including request id, account id,
  model, route, status, latency, and stream/failure metadata.
- Slice 68 restored stream audit metadata and log policy parity for body capture
  cleaning/preservation, admin log state updates and capacity trimming,
  completed/failed stream usage metadata, rate-limit header snapshots, actual
  WebSocket transport marking, and live WebSocket `codex.rate_limits` update
  propagation into persisted event logs.
- Slice 69 removed or replaced the audited serving toy helpers and added an
  architecture guard so `should_fallback`, `is_recoverable`,
  `prefers_websocket`, or an uncalled `quota_reached` cannot silently return.
- Slice 86 restored the next old gateway WebSocket adapter/protocol batch:
  pool busy/over-cap/maintenance GC behavior, opening failure status/body
  preservation, binary-frame errors, wrapped-error status/retry-after
  propagation, connection-limit 503 mapping, internal metadata/rate-limit event
  capture, and stream close-before-terminal errors.
- Slice 87 restored the old gateway WebSocket pure protocol assertion batch:
  special error-code classification, success-status/unmapped error skipping,
  malformed `response.completed` validation, optional `null` handling,
  optional-field type rejection, nested local-shell/web-search validation, and
  invalid JSON/event-shape skipping before SSE forwarding.
- Slice 88 restored the old gateway WebSocket security-chain header/payload
  block and fixed production WebSocket header construction so HTTP/SSE-only
  `content-type`, `accept`, and `session_id` are not sent in the opening.
  WebSocket openings now send `session-id` and `thread-id` when session context
  is available.

Remaining:

- Remaining old WebSocket behavior-test migration still needs a focused pass for
  byte-for-byte capture harness and audit artifact edge cases not represented by
  the restored live, pool, deflate, ping, binary-frame, opening-status,
  wrapped-error, connection-limit, history-recovery, previous-response,
  late-failure, event-log, metadata, security-chain, rate-limit-update, and pure
  event-shape validator coverage. Full persisted stream audit parity with the
  old `StreamAudit` / `WebSocketStreamAudit` artifacts remains open only for
  artifact details that are separate from event-log metadata and redacted
  WebSocket audit snapshots.

### 5. Serving Dispatch Logic Required Bulk Migration

Current symptoms:

- The previously audited uncalled toy helpers in
  `crates/core/src/serving/fallback.rs`,
  `crates/core/src/serving/recovery.rs`, and
  `crates/core/src/serving/responses.rs` were removed or replaced in Slice 69.
  Runtime production code now calls the remaining pure serving policy helpers
  for rate-limit/quota/transient status classification, same-account retry
  status classification, and quota-warning threshold matching.
- Architecture tests now fail if those exact toy helper names return or if
  `quota_reached(...)` loses its non-test production caller.
- `crates/runtime/src/services.rs` dispatch paths now acquire accounts from the
  restored runtime account pool and Responses dispatch uses restored
  session-affinity state. HTTP SSE `stream: true` now forwards normal success
  bodies live, records usage/session affinity at stream end, and converts late
  stream disconnects into SSE `response.failed` events. It also writes
  persisted `v1.response` event logs for completed and late-failed live streams,
  but full old stream-audit artifacts remain materially reduced.
- Responses WebSocket implicit resume and reasoning replay now run through
  runtime production paths, record replay/affinity metadata after completions,
  restore full history on implicit-resume rejection, and evict replay state after
  invalid encrypted content.

Baseline examples:

- `src/codex/serving/dispatch/mod.rs`: 2,063 lines.
- `src/codex/serving/responses.rs`: 1,313 lines.
- `src/codex/serving/dispatch/fallback.rs`: 632 lines.
- `src/codex/serving/dispatch/affinity.rs`: 698 lines.
- `src/codex/serving/dispatch/stream.rs`: 638 lines.
- `src/codex/serving/dispatch/reasoning_replay.rs`: 288 lines.

Impact:

- Account fallback, quota state transitions, stream audit artifacts, and the
  remaining recovery transitions are not fully proven to be preserved. Affinity,
  Responses implicit resume, reasoning replay, and the audited Responses
  WebSocket fallback/status-routing block now have dedicated migrated server
  coverage, but the old gateway WebSocket adapter/protocol suite remains
  high-risk.
- The old behavior should not be considered migrated until these paths are
  reimplemented in the intended `core`/`runtime`/`adapters` split and covered by
  tests.

### 6. Core Filesystem IO Violation

Audit finding before Slice 1:

- `crates/core/src/gateway/installation.rs` imports `std::fs`, `Path`,
  `PathBuf`, and uses `dirs::home_dir()`.
- `crates/core/Cargo.toml` depends on `dirs`.

Architecture rule:

- `core` must not own filesystem paths, environment reads, or concrete IO.

Impact:

- This is a direct architecture violation.
- Installation ID persistence and `~/.codex/installation_id` compatibility
  should move to `platform` or `adapters`, with `core` retaining only pure
  identity rules if needed.

Progress:

- Fixed in Slice 1 by moving file resolution/persistence to
  `platform::storage` and keeping only UUID generation/parsing in `core`.
- Added an architecture test guard so this class of `core` IO dependency
  regresses visibly.

### 7. Test Migration Was Incomplete

Baseline test count:

- Approximately 471 `#[test]` / `#[tokio::test]` occurrences.
- 66 top-level Rust test files.

Current test count:

- 495 `#[test]` / `#[tokio::test]` occurrences.
- 65 Rust files contain tests.
- 15 root Rust test files, all architecture tests.
- 43 crate-local Rust test files under `crates/*/tests`.

Large removed baseline tests include:

- `tests/codex_gateway/websocket.rs`: 4,983 lines, 79 tests.
- `tests/codex_serving/responses_websocket.rs`: 2,863 lines, 28 tests.
- `tests/codex_serving/responses_http_sse.rs`: 1,766 lines, 28 tests.
- `tests/codex_serving/upstream_fallback.rs`: 2,068 lines, 27 tests.
- `tests/codex_serving/chat_completions.rs`: 910 lines, 12 tests.
- `tests/admin/accounts/import_export.rs`: 863 lines, 15 tests.
- `tests/admin/accounts/oauth.rs`: 678 lines, 16 tests.
- `tests/admin/client_keys_route.rs`: 637 lines, 7 tests.

Impact:

- This was a valid blocker before the full mapping audit.
- The current architecture guard now maps all 46 old root behavior files to
  crate-local test files. Remaining risk is assertion-level parity inside those
  broad suites, not a missing crate-local migration target.

### 8. Architecture Tests Did Not Catch Empty Implementations

Current architecture tests verify:

- Directory whitelist.
- Root package has no facade target; boundary tests import member crates directly.
- Workspace dependency direction.
- Some forbidden imports and legacy path references.
- Obvious placeholder markers: empty `main`, `not wired yet`, and `后续承载`.
- Server startup owns background task lifecycle and injects the runtime
  installation ID.
- Specific serving toy helper regressions closed in Slice 69:
  `should_fallback`, `is_recoverable`, `prefers_websocket`, and an uncalled
  `quota_reached`.

They do not currently verify:

- Full assertion-level parity for every old behavior test.
- Generic semantic completeness beyond the explicit placeholder/empty-shell
  markers and full baseline test-suite mapping.

Impact:

- This was a valid concern before the full source scan and test-migration
  mapping guard. Current scans found no empty source files/directories or empty
  function/module shells in `crates src tests`, and architecture tests now
  reject root behavior tests outside the approved architecture/fixture shape.

## Tests And Fixtures Status

Current root `tests/` after local empty-directory cleanup:

- `tests/architecture/*`: root-level architecture tests.
- `tests/fixtures/*`: shared fixtures.

This shape is reasonable if the policy is:

- crate behavior tests live under `crates/<crate>/tests`;
- root `tests/` keeps only cross-workspace architecture tests and shared
  fixtures.

However, many old behavior tests still need to be migrated into the appropriate
crate-level test suites.

## Migration Compatibility / Legacy Surface

Tracked legacy source modules are removed from `src/`, which is aligned with the
architecture goal of not keeping old root modules.

Remaining concerns:

- Current Rust source scan does not find `后续承载` or `not wired yet` outside
  the architecture placeholder guard itself.
- Some older documents still reference pre-migration paths. Those are not source
  code, but they should be marked historical or updated after the migration is
  actually complete.
- `serde(alias = ...)` and OpenAI-compatible wording in current source are API
  compatibility features, not migration compatibility shims.
- Codex Desktop installation ID lookup is product interoperability with the
  upstream client identity model, not a migration compatibility shim.

## Recommended Completion Plan

1. Restore the real binary startup path in `crates/server/src/main.rs` and keep
   bootstrap/state assembly inside `runtime`. **Startup config/logging/storage,
   background-task ownership, installation ID injection, account-pool restore,
   and session-affinity restore are done in Slices 2, 3, 7, 8, and 9.**
2. Move installation ID persistence out of `core`; add an architecture test that
   forbids `std::fs`, `std::env`, `dirs::`, `Path`, and `PathBuf` in `core/src`.
   **Done in Slice 1; startup/request-context injection done in Slice 7.**
3. Re-migrate serving dispatch behavior into the intended split:
   - pure decisions in `core/src/serving`;
   - IO through `core` ports;
   - adapter calls in `adapters`;
   - wiring in `runtime`;
   - HTTP mapping in `server`.
4. Re-migrate WebSocket transport and pooling:
   - protocol frame types and pure parsing in `core`;
   - `tokio-tungstenite`, TLS opening, pooling, deflate, and audit artifact IO in
     `adapters`.
5. Rebuild background tasks in `runtime/src/tasks`.
6. Either implement `assets` and `xtask` according to `docs/architecture.md`, or
   change the architecture spec before treating them as complete. **`xtask`
   wiring is done in Slice 2; `assets` static serving and server fallback wiring
   are done in Slice 48.**
7. Migrate deleted behavior tests into `crates/*/tests`, prioritizing:
   - WebSocket codec/transport/pool tests;
   - Responses HTTP SSE and WebSocket tests;
   - upstream fallback/recovery tests;
   - admin account OAuth/import/export/cookie/quota tests;
   - startup/background task tests.
8. Add architecture tests that fail on:
   - empty required files;
   - `not wired yet` strings; **done for Rust sources in Slice 2**
   - empty `main`; **done for Rust sources in Slice 2**
   - uncalled placeholder functions in required architecture modules; **done for
     the audited serving toy helpers in Slice 69; broader generic detection is
     still open**
   - root `tests/` behavior suites outside architecture tests.

## Evidence Commands

Commands used during this audit include:

```bash
codegraph sync .
codegraph status .
git diff --shortstat f07442a28b0c186bd22b598a53cc4856ab4b2445..HEAD -- src tests crates
git diff --name-status f07442a28b0c186bd22b598a53cc4856ab4b2445..HEAD -- src tests crates
git diff --numstat f07442a28b0c186bd22b598a53cc4856ab4b2445..HEAD -- <selected paths>
git grep -n "#\\[\\(tokio::test\\|test\\)\\]" f07442a28b0c186bd22b598a53cc4856ab4b2445 -- tests src
rg "#\\[(tokio::test|test)\\]" -n tests crates src --glob '!target/**'
rg "not wired yet|后续承载|TODO|FIXME|todo!|unimplemented!|stub|placeholder|迁移|兼容|compat|legacy|alias|shim|deprecated" -n crates src tests
rg "std::fs|\\bfs::|std::env|env::|dirs::|std::path|PathBuf|Path::" -n crates/core/src crates/core/Cargo.toml
find tests -type d -empty -print -delete
find . -path './.git' -prune -o -path './target' -prune -o -path './.codegraph' -prune -o -type d -empty -print
```
