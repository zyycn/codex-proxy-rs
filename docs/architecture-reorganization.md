# Architecture Reorganization Roadmap

## Purpose

This document defines the next cleanup track for `codex-proxy-rs`.

The project direction is still valid: a single Rust service exposing OpenAI-compatible
`/v1/*` APIs backed by imported ChatGPT/Codex accounts, plus admin routes for local
operation. The problem is now structural growth. First-stage implementation optimized
for shipping behavior quickly, so HTTP handlers and `AppState` accumulated too many
responsibilities.

The reorganization goal is not academic Clean Architecture. The goal is to keep feature
work cheap by making boundaries explicit:

```text
http -> service -> codex / logs / storage / system auth
```

`http` parses requests, enforces route-level auth, and shapes responses. Business flow
orchestration moves into `service`. Codex-specific capability groups under the `codex`
domain: imported upstream accounts, Codex transport, OAuth/token refresh, protocol
translation, Codex model catalog, account-scoped cookies, and fingerprinting. Outside
`codex`, modules represent the backend system itself: app startup, HTTP, admin/client
authentication, logs, storage, config, and shared helpers.

## Initial Signals

The cleanup started from these pressure points:

| File | Lines | Signal |
| --- | ---: | --- |
| `src/http/admin.rs` | 5169 | Admin routes, response DTOs, auth helpers, account import/export, account health, quota, cookies, API keys, model refresh, logs, usage, and helper logic are all in one module. |
| `src/http/v1.rs` | 1998 | Responses, Chat Completions, models, fallback/retry, refresh, usage logging, SSE collection, WebSocket bridging, and auth are mixed. |
| `src/state.rs` | 257 | `AppServices` is still a service locator with many optional dependencies. |
| `tests/admin_accounts_route_test.rs` | 2588 | Admin account scenarios are too large to scan or move safely. |
| `tests/v1_upstream_route_test.rs` | 2115 | Responses HTTP SSE, WebSocket, fallback, error mapping, usage, and logging side effects are mixed. |
| `tests/chat_completions_route_test.rs` | 710 | Manageable, but should eventually align with the v1 split. |
| `tests/admin_client_keys_route_test.rs` | 659 | Manageable, but can share common admin test setup. |

The main coupling pattern is:

```text
handler -> AppState -> repo / pool / codex client / refresh / logs / translation
```

That is acceptable for early delivery. It becomes expensive once new behavior needs to
touch fallback, account state, usage logs, and protocol conversion at the same time.

## Current Progress

As of this cleanup pass:

- `src/http/api/admin/mod.rs` is the admin module entry, backed by resource-specific files
  under `src/http/api/admin/` and a dedicated admin router.
- `src/http/v1/mod.rs` is the v1 module entry, backed by thin route handlers for auth,
  chat, responses, models, errors, and router mounting under `src/http/v1/`.
- `src/service/` now contains backend-facing HTTP use-case services:
  `AdminAuthService`, `ChatService`, `ResponsesService`, `ApiKeyService`,
  `UsageService`, and `LogService`.
- `src/codex/` is now the upstream Codex integration domain. It contains imported
  account state and account service orchestration, transport, protocol translation,
  OAuth/token refresh, Codex model catalog and refresh orchestration, desktop
  fingerprinting, account-scoped cookies, and v1 upstream dispatch/fallback helpers.
- v1 chat and responses handlers delegate orchestration to services.
- admin API key, usage, and logs handlers delegate repository work to services.
- admin login, logout, and status checks delegate business logic to `AdminAuthService`.
- admin OAuth device login/polling and PKCE start/callback/code-relay flows delegate
  session acquisition, OAuth token exchange, account import, and duplicate-completion
  handling to `AdminAuthService`.
- admin account list/export/status/label/delete/batch/reset-usage/cookies operations
  delegate local repository, cookie repository, and account-pool coordination to
  `codex::accounts::service::AccountService`.
- admin manual add, CLI import, OAuth token import, bulk account import, single-account
  refresh, quota fetch, and health check now share
  `codex::accounts::service::AccountService` storage, token refresh, Codex usage
  probing, and account-pool synchronization.
- admin model catalog refresh orchestration lives in `codex::models::service::ModelService`.
- `tests/common/` now contains shared integration-test helpers, including generic
  response/session helpers plus admin account and v1 upstream route fixtures.
- `tests/admin_accounts_route_test.rs` has been split into list, lifecycle,
  import/export, OAuth/auth, and cookies/quota/refresh/health scenario files.
- `tests/v1_upstream_route_test.rs` has been split into Responses HTTP/SSE,
  Responses WebSocket, upstream fallback/refresh, and upstream error scenario files.
- `src/app.rs` and `src/state.rs` have been moved into `src/runtime/router.rs` and
  `src/runtime/state.rs`; `src/runtime/bootstrap.rs` owns production `AppState` construction.
- `src/config.rs` has been split into `src/config/types.rs` and
  `src/config/loader.rs`.
- `src/crypto.rs` and `src/pagination.rs` have been moved into `src/utils/`, and the
  duplicated JSON string traversal helper now lives in `src/utils/json.rs`.
- `src/error.rs` has been moved to `src/codex/protocol/error.rs`, matching its only
  remaining protocol-translation use.
- service modules now use the target names `admin_auth`, `api_key`, `chat`,
  `responses`, `usage`, and `log` instead of the temporary `*_service` filenames.
  Codex-specific model refresh orchestration moved to `codex/models/service.rs`.
- `codex/upstream/mod.rs` exposes a `CodexUpstreamService` service dependency facade backed
  by explicit `CodexUpstreamDependencies`, so request sending, refresh retry, fallback,
  cookie persistence, usage recording, stream audit, and v1 logging no longer require
  public helper wrappers that accept `AppState`.
- `codex/upstream/dispatch.rs`, `fallback.rs`, `refresh.rs`, `stream.rs`, and
  `usage.rs` now own the obvious helper groups for dispatch response helpers, retry
  classification, token refresh retry, SSE response collection, and usage recording.
- `codex/accounts/service/import.rs`, `lifecycle.rs`, `cookies.rs`, `quota.rs`, `health.rs`,
  `refresh.rs`, and `runtime_pool.rs` now own account import, lifecycle, cookie, quota,
  health-check, single-account refresh, and runtime-pool workflows instead of leaving
  them in `service/account/mod.rs`.
- `ChatService` and `ResponsesService` own a cloned `CodexUpstreamService` instance injected from
  `AppState` construction, so v1 orchestration no longer passes raw `AppState` through
  upstream helper calls.
- `ChatService` and `ResponsesService` now receive only `ModelConfig`, `ModelService`,
  and `CodexUpstreamService` as construction dependencies. v1 handlers no longer pass raw
  `AppState` into service `handle` calls.
- admin session validation now runs through `AdminAuthService`; the HTTP auth helper only
  extracts the admin session cookie and maps failures to the existing admin envelope
  shape.
- old `AppState` raw dependency accessors for repositories, OAuth clients, refreshers,
  hashers, event logs, the database, and the runtime account pool have been removed.
- runtime account-pool assertions in integration tests now go through narrow
  `AccountService` methods instead of `AppState` exposing the pool.
- `AppServices` no longer stores construction-only raw dependencies such as the
  database pool, secret box, API key hasher, token refresher, OAuth client, OAuth
  sessions, event-log repository, or account pool. Runtime account-pool restore is now
  delegated to `AccountService`.
- `AppState` public constructors now feed a private dependency bundle into one assembly
  path, so service wiring is centralized instead of repeated across each test/runtime
  constructor.
- v1 Responses, upstream fallback/error, and Chat Completions SSE payloads have been
  moved out of route tests into `tests/fixtures/*.sse`.
- v1 Responses WebSocket mocked response payloads have been moved into
  `tests/fixtures/*.json`.
- `http/api/admin/response.rs` now provides `AdminError`, and the admin route modules use
  `Result<impl IntoResponse, AdminError>` for standard admin-envelope failures.
- admin auth handlers now propagate session, login, OAuth, and PKCE failures through
  `AdminError`; success paths that must set cookies or redirect still build explicit
  `Response` values.
- admin account handlers now use `Result` plus focused error mappers for repository,
  import, refresh, health-check, quota, and validation failures. The quota route keeps a
  small route-specific error type only to preserve the existing upstream fetch-error
  detail under `data`.

## Deferred Follow-Ups

The core single-crate layering pass is complete. These items are intentionally left as
follow-up choices because doing them now would either change route contracts or split
already manageable files further than the current risk requires:

- follow-up test cleanup for remaining medium-sized route files, especially Chat
  Completions and admin account import/OAuth scenarios if they grow further.
- moving remaining large inline JSON bodies into `tests/fixtures/` only where they are
  response/test data rather than request-shape assertions.
- deciding whether to preserve or redesign the admin quota fetch-error `data.error`
  body before moving it fully onto the standard null-data `AdminError` contract.

## Non-Goals

- Do not split into a Rust workspace yet.
- Do not create `crates/core`, `crates/server`, `crates/store`, or `crates/upstream` in
  this phase.
- Do not rewrite behavior while splitting files.
- Do not change route contracts, JSON envelopes, request IDs, auth semantics, or error
  bodies during the mechanical split.
- Do not merge test cleanup with service extraction in the same change.

The project is still moving quickly. A single crate with clearer internal modules is
the lowest-cost structure right now.

## Rust Implementation Standards

All code changes in this cleanup track should follow the repository's existing style
and the Rust best-practice rules below.

### Ownership And Borrowing

- Prefer borrowed inputs: use `&str` instead of `String`, `&[T]` instead of `Vec<T>`,
  and `&T` instead of `T` when the callee only reads data.
- Clone only when a new owned value is actually needed. Acceptable clones include
  `Arc` handles, cheap client handles that share internal pools, immutable snapshots,
  and owned values required by a downstream API.
- Avoid cloning large request bodies, `Vec`, `HashMap`, account lists, or JSON values in
  loops. Prefer iterators, references, or moving ownership at the boundary where the
  value is consumed.
- Small `Copy` types can be passed by value. Large structs and enums should be passed
  by reference unless ownership transfer is part of the API contract.
- Use `Cow<'_, str>` or `Cow<'_, [T]>` only when an API genuinely accepts either
  borrowed or owned data.

### Error Handling

- Production code should return `Result<T, E>` for fallible operations. Do not use
  `unwrap()` or `expect()` outside tests unless failure is impossible and the reason is
  documented.
- Prefer precise error enums with `thiserror` for service/domain/repository errors.
  Reserve `anyhow` for binaries, smoke helpers, or test helpers.
- Use `?` for propagation and `map_err`, `or_else`, or `inspect_err` where conversion,
  recovery, or logging is needed.
- Avoid converting meaningful errors into `Option` too early. Preserve enough detail
  for service-level classification and HTTP response mapping.
- Service errors should convert into admin envelopes or OpenAI-compatible v1 errors at
  the HTTP boundary, not inside repository or domain modules.

### Options And Control Flow

- Use `let Some(value) = ... else { ... };` or `let Ok(value) = ... else { ... };`
  when early return is the clearest path.
- Use `ok_or_else`, `unwrap_or_else`, and `map_or_else` when constructing fallback values
  would allocate or do work.
- Use `match` when every variant matters or when transforming between nested
  `Result`/`Option` shapes.

### Async, Sharing, And Trait Objects

- Shared runtime state should use `Arc` and explicit synchronization primitives such as
  `tokio::sync::Mutex` only where lifecycle is required.
- Keep lock scopes short. Do not hold account-pool or session locks while performing
  network requests, database calls, or expensive serialization.
- Prefer generics/static dispatch for local helper abstractions. Use `Arc<dyn Trait>`
  only at boundaries that need runtime substitution, such as token refreshers or OAuth
  clients in tests and production.
- Ensure async errors and trait objects used across tasks are `Send + Sync + 'static`
  where the runtime requires it.

### Performance Discipline

- Do not optimize by guessing. Keep code simple first, then measure if a route or
  service becomes hot.
- Avoid intermediate `collect()` calls when an iterator can be consumed directly.
- Avoid boxing or heap allocation until a type is large, recursive, or must cross a
  trait-object boundary.
- Use `cargo clippy --all-targets --all-features --locked -- -D warnings` as the
  authoritative lint gate. Pay special attention to `redundant_clone`,
  `clone_on_copy`, `needless_collect`, `manual_ok_or`, and `large_enum_variant`.
- If a clippy lint must be suppressed, use `#[expect(clippy::...)]` with a short reason
  instead of a broad `#[allow(...)]`.

### Comments And Documentation

- Prefer clear names and small functions over explanatory comments.
- Add `//` comments only for non-obvious reasoning: protocol quirks, safety guarantees,
  workaround context, or route-contract constraints.
- Public service/domain types should have doc comments when their purpose or invariants
  are not obvious from the type name.
- Do not leave untracked `TODO` comments. Any TODO that remains in code should reference
  an issue or a concrete follow-up document.

### Tests

- Use descriptive test names that read like behavior, for example
  `responses_should_retry_next_account_after_retry_after`.
- Prefer one behavior per test. Integration tests may need multiple assertions for a
  route contract, but the scenario should still be narrow.
- Put pure logic tests next to the source module when they do not need HTTP setup.
- Use integration tests for route contracts, SQLite side effects, account-pool behavior,
  mocked Codex transport, SSE, and WebSocket flows.
- Use fixture files for large JSON/SSE/WebSocket payloads. Keep snapshots small if
  snapshot testing is added later.

### Type-State Pattern

Type-state is optional, not a default style. Consider it only when it makes invalid
states unrepresentable without excessive generic complexity, such as a future builder
that must prove required OAuth/session fields before a request can be sent.

## Target Source Layout

Target shape inside the existing crate:

```text
src/
  main.rs
  lib.rs

  app/
    mod.rs
    router.rs
    state.rs
    bootstrap.rs

  config/
    mod.rs
    types.rs
    loader.rs

  utils/
    mod.rs
    pagination.rs
    json.rs
    crypto.rs

  http/
    mod.rs
    auth.rs
    health.rs
    middleware.rs
    admin/
      mod.rs
      router.rs
      response.rs
      auth.rs
      accounts.rs
      api_keys.rs
      logs.rs
      models.rs
      settings.rs
      usage.rs
    v1/
      mod.rs
      router.rs
      auth.rs
      chat.rs
      responses.rs
      models.rs
      errors.rs
  service/
    mod.rs
    admin_auth.rs
    api_key.rs
    chat.rs
    responses.rs
    usage.rs
    log.rs

  auth/
    mod.rs
    admin_session.rs
    api_key.rs
    api_key_repository.rs

  codex/
    mod.rs
    accounts/
      mod.rs
      model.rs
      repository.rs
      pool.rs
      lifecycle.rs
      service/
        mod.rs
        import.rs
        lifecycle.rs
        cookies.rs
        quota.rs
        health.rs
        refresh.rs
        runtime_pool.rs
    transport/
      mod.rs
      client.rs
      headers.rs
      sse.rs
      types.rs
      usage.rs
      websocket.rs
    oauth/
      mod.rs
      cli_import.rs
      client.rs
      refresh.rs
      token.rs
    protocol/
      mod.rs
      openai_to_codex.rs
      codex_to_openai.rs
      schema.rs
      error.rs
    models/
      mod.rs
      catalog.rs
      repository.rs
      service.rs
    fingerprint/
      mod.rs
      model.rs
      repository.rs
      updater.rs
    cookies/
      mod.rs
      jar.rs
      repository.rs
    upstream/
      mod.rs
      dispatch.rs
      fallback.rs
      refresh.rs
      stream.rs
      usage.rs

  logs/
  storage/
```

This is a target, not a one-commit requirement. The project should stay as one crate
until module responsibilities stabilize.

### Module Boundary Rules

- `main.rs` should remain the binary entry point. Startup wiring should move toward
  `app/bootstrap.rs`, route assembly toward `app/router.rs`, and runtime state toward
  `app/state.rs`.
- `config` owns configuration data, loading, and environment parsing. Runtime business
  modules should consume typed config, not environment variables.
- `utils` contains cross-domain primitives only. It can depend on standard crates and
  small general-purpose dependencies such as `serde_json`, `base64`, and encryption
  crates, but it must not depend on `http`, `service`, `auth`, `codex`, or other domain
  modules.
- `utils/pagination.rs` owns cursor encoding, decoding, limit clamping, and the
  reusable `Page<T>` shape.
- `utils/json.rs` owns small JSON traversal helpers used by import/parsing code. It
  should not contain route-specific DTOs or validation policy.
- `utils/crypto.rs` owns reusable secret encryption primitives such as `SecretBox`,
  `CryptoError`, and `CryptoResult`. API-key hashing remains in `auth/api_key.rs`.
- `http` owns route registration, auth extraction, request parsing, and response
  mapping. It should not construct repositories, lock account pools, call Codex
  clients directly, or perform protocol translation workflows.
- `service` owns backend-facing orchestration for HTTP use cases. It delegates Codex
  account, model, upstream retry/fallback, protocol, OAuth, fingerprint, and transport
  details into `codex/*`.
- `codex/accounts` owns imported Codex account data shapes, repository contracts,
  lifecycle state, and the runtime account pool. It is not a `utils` helper and should
  not be named only `accounts`, because this backend can also have its own local admin
  users and client API keys.
- `codex/transport` owns direct Codex backend communication: request client, headers,
  SSE parsing/encoding, usage extraction, transport types, and WebSocket bridging.
- `codex/oauth` owns Codex/OpenAI account authorization and token refresh mechanics.
  System authentication remains in top-level `auth`.
- `codex/protocol` owns OpenAI-compatible request/response translation and
  translation-specific errors. Do not recreate a top-level `src/error.rs` unless a
  genuinely crate-wide error boundary is introduced.
- `codex/models`, `codex/fingerprint`, and `codex/cookies` own Codex-specific model
  catalog, desktop fingerprint, and account-scoped cookie replay.
- `codex/upstream` owns v1 upstream dispatch, fallback, refresh retry, stream
  collection, usage recording, and lifecycle logging helpers used by Chat and
  Responses services.
- Top-level `logs`, `storage`, `config`, `utils`, `runtime`, `http`, and system `auth`
  remain backend-system modules.

### Completed Structural Increment

The current low-risk structural increment moved the remaining top-level helper and
application wiring files into the target directories without changing behavior:

```text
src/app.rs        -> src/runtime/router.rs
src/state.rs      -> src/runtime/state.rs
src/config.rs     -> src/config/{types,loader}.rs
src/crypto.rs     -> src/utils/crypto.rs
src/pagination.rs -> src/utils/pagination.rs
src/error.rs      -> src/codex/protocol/error.rs
src/service/*_service.rs -> src/service/*.rs
src/service/v1_upstream.rs -> src/codex/upstream/mod.rs
src/service/account_service.rs -> src/codex/accounts/service/mod.rs
src/service/model.rs -> src/codex/models/service.rs
src/service/upstream/* -> src/codex/upstream/*
```

### Codex Domain Migration Order

This structural track makes `codex/` the only top-level home for upstream Codex
concerns. Each step is behavior-preserving and should keep passing the verification
gate:

1. Move the imported upstream account module into `codex/accounts` and update imports
   to `crate::codex::accounts`.
2. Move direct Codex backend files into `codex/transport` and update imports to
   `crate::codex::transport`.
3. Move OpenAI/Codex translation into `codex/protocol`.
4. Move Codex OAuth/token refresh/CLI import into `codex/oauth`, leaving system admin
   sessions and client API keys in top-level `auth`.
5. Move Codex model catalog, fingerprint, and account-scoped cookies under
   `codex/models`, `codex/fingerprint`, and `codex/cookies`.
6. Move remaining Codex-specific orchestration out of top-level `service/account`,
   `service/model`, and `service/upstream` into `codex/accounts/service`,
   `codex/models/service`, and `codex/upstream`.

Callers should import cross-domain helpers through:

```rust
use crate::utils::{
    crypto::{CryptoError, SecretBox},
    pagination::{clamp_limit, decode_cursor, encode_cursor, Page},
};
```

Integration tests should import public helpers through `codex_proxy_rs::utils::*`.
Do not keep top-level compatibility wrappers for `crypto` or `pagination`; leaving both
paths active would make the target boundary less clear.

## Phase 1: Split HTTP Files Without Behavior Changes

### Goal

Reduce the former `src/http/admin.rs` and `src/http/v1.rs` large files into
resource-focused module directories while keeping handler bodies and helper logic
intact.

This phase should be mostly mechanical:

- move related structs/functions into new files;
- keep function names stable where tests or `app.rs` reference them;
- preserve route paths and methods;
- preserve response body shapes;
- preserve existing helper behavior, even when it is not ideally placed yet.

### Admin Split

Create:

```text
src/http/api/admin/
  mod.rs
  router.rs
  response.rs
  auth.rs
  accounts.rs
  api_keys.rs
  logs.rs
  models.rs
  settings.rs
  usage.rs
```

Suggested ownership:

| Module | Initial contents |
| --- | --- |
| `response.rs` | `AdminEnvelope`, `AdminPageEnvelope`, `AdminResponse`, shared admin DTO helpers that are not resource-specific. |
| `auth.rs` | `login`, `auth_status`, `auth_logout`, OAuth PKCE/device routes, admin session cookie helpers. |
| `accounts.rs` | account list/create/import/export/delete/status/label, health check, refresh, reset usage, quota, cookies, account import parsing helpers. |
| `api_keys.rs` | local client key list/create/import/export/delete/status/label/batch helpers. |
| `logs.rs` | `/api/admin/logs` handler and log DTOs. |
| `models.rs` | `/api/admin/refresh-models` handler and model refresh DTOs. |
| `settings.rs` | `/api/admin/settings` handler and settings DTOs. |
| `usage.rs` | `/api/admin/usage-stats` and `/api/admin/usage-stats/summary`. |
| `router.rs` | admin route mounting only. |

Keep cross-resource helpers private when possible. If a helper is needed by multiple
admin modules during the split, expose it as `pub(super)` and revisit it in Phase 2.

The admin router should own admin paths:

```rust
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/login", post(auth::login))
        .route("/api/admin/auth/status", get(auth::auth_status))
        .route("/api/admin/accounts", get(accounts::accounts).post(accounts::create_account))
        .route("/api/admin/api-keys", get(api_keys::api_keys).post(api_keys::create_api_key))
}
```

`src/runtime/router.rs` should then merge admin routes instead of importing every admin handler:

```rust
Router::new()
    .route("/health", get(health))
    .merge(v1::router())
    .merge(admin::router())
```

### V1 Split

Create:

```text
src/http/v1/
  mod.rs
  router.rs
  auth.rs
  chat.rs
  responses.rs
  models.rs
  errors.rs
  logging.rs
  upstream.rs
```

Suggested ownership:

| Module | Initial contents |
| --- | --- |
| `chat.rs` | `/v1/chat/completions` handler and Chat-specific response conversion calls. |
| `responses.rs` | `/v1/responses` handler, SSE collection, non-streaming collection, WebSocket response path. |
| `models.rs` | `/v1/models`, catalog/detail/info/debug model routes. |
| `auth.rs` | client API key route authorization and missing-key response. |
| `errors.rs` | OpenAI-compatible v1 error body helpers such as model-not-found and upstream client errors. |
| `logging.rs` | `CodexRequestLogContext`, Codex upstream response logging, stream metadata helpers. |
| `upstream.rs` | account acquire/release guard, refresh retry, upstream account retry classification, cookie persistence, usage recording. |
| `router.rs` | v1 route mounting only. |

In Phase 1, `upstream.rs` can still depend on `AppState` directly. The dependency
direction improves in Phase 2 when services become the orchestration boundary.

### Phase 1 Acceptance

Run after each small move:

```bash
cargo fmt --check
cargo test --test admin_accounts_list_test
cargo test --test admin_accounts_mutation_test
cargo test --test admin_accounts_import_export_test
cargo test --test admin_accounts_oauth_test
cargo test --test admin_accounts_cookies_quota_test
cargo test --test admin_client_keys_route_test
cargo test --test v1_responses_http_sse_test
cargo test --test v1_responses_websocket_test
cargo test --test v1_upstream_fallback_test
cargo test --test v1_upstream_errors_test
cargo test --test chat_completions_route_test
```

Run before finishing the phase:

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features --locked -- -D warnings
```

## Phase 2: Introduce Service Layer

### Goal

Move business workflow orchestration out of handlers. Handlers should become thin:

```rust
pub async fn chat_completions(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Json(req): Json<ChatCompletionRequest>,
) -> Response {
    match state.services.chat.handle(request_id, headers, req).await {
        Ok(resp) => resp.into_response(),
        Err(err) => err.into_response(),
    }
}
```

Do this after Phase 1 so the move is readable and test failures point to business
logic, not file-boundary churn.

### Initial Services

Start with v1 because it is the core proxy path:

| Service | Responsibility |
| --- | --- |
| `ChatService` | Chat Completions flow: auth result input, model resolution, Chat-to-Codex translation, account acquisition, Codex transport, retry/fallback, Chat output conversion, usage/log side effects. |
| `ResponsesService` | Responses flow: model resolution, HTTP SSE/WebSocket transport selection, previous-response affinity, fallback policy, collection/streaming behavior, usage/log side effects. |
| `ModelService` | model catalog reads, cached backend model snapshots, v1 model response shaping. |

Then move admin orchestration:

| Service | Responsibility |
| --- | --- |
| `AdminAuthService` | admin login, session lifecycle, OAuth PKCE/device flows, logout/status. |
| `AccountService` | manual import, CLI import, native import/export, status/label/delete, refresh, quota, cookies, runtime pool synchronization. |
| `ApiKeyService` | local client key create/list/update/delete/import/export. |
| `UsageService` | usage list and summary queries. |
| `LogService` | admin log queries and v1 lifecycle log writes if useful. |

The service layer should depend on repositories and domain modules. HTTP should depend
on services. Domain modules should not depend on HTTP.

### Phase 2 Acceptance

Each migrated handler should preserve its current route test. For every service moved,
add or retain focused tests at the cheapest level:

- pure conversion and classification tests stay near source modules;
- route contract tests stay in `tests/`;
- service tests are useful when the orchestration has many branches and can be tested
  without HTTP request boilerplate.

Do not migrate all handlers at once. Move one vertical flow at a time:

1. `ModelService`
2. `ResponsesService`
3. `ChatService`
4. `AdminAuthService`
5. `AccountService`
6. `ApiKeyService`

## Phase 3: Reshape AppState

### Current Shape

`AppState` already wraps `Arc<AppServices>`, but `AppServices` still exposes raw,
optional dependencies:

```rust
pub struct AppServices {
    pub config: AppConfig,
    pub db: Option<SqlitePool>,
    pub event_logs: Option<EventLogRepository>,
    pub secret_box: Option<SecretBox>,
    pub api_key_hasher: Option<ApiKeyHasher>,
    pub token_refresher: Option<Arc<dyn TokenRefresher>>,
    pub oauth_client: Option<Arc<dyn OAuthClient>>,
    pub oauth_sessions: Arc<Mutex<PkceSessionStore>>,
    pub account_pool: Arc<Mutex<AccountPool>>,
}
```

This creates two problems:

- handlers and helpers can reach almost everything;
- every business path has to repeatedly decide what a missing dependency means.

### Target Shape

Move toward:

```rust
pub struct AppState {
    pub services: Arc<AppServices>,
}

pub struct AppServices {
    pub config: AppConfig,
    pub auth: AdminAuthService,
    pub accounts: AccountService,
    pub api_keys: ApiKeyService,
    pub chat: ChatService,
    pub responses: ResponsesService,
    pub models: ModelService,
    pub usage: UsageService,
    pub logs: LogService,
}
```

Repositories, Codex clients, account pool handles, OAuth clients, token refreshers,
and hashers should be injected into the service that actually uses them.

Keep constructors practical. During migration, it is fine to support both old helper
accessors and new service fields. Remove the old accessors only after handlers stop
using them.

### Dependency Rule

After this phase:

- HTTP modules should not construct repositories.
- HTTP modules should not lock the account pool directly.
- HTTP modules should not call Codex clients directly.
- HTTP modules should not update usage or event logs directly.
- Service modules can orchestrate those operations.
- Domain/repository modules should remain HTTP-agnostic.

## Phase 4: Unify Errors And Response Envelopes

### Goal

Make route error handling less repetitive without changing external contracts.

Targets:

```text
src/codex/protocol/error.rs
src/http/response.rs
src/http/api/admin/response.rs
src/http/v1/errors.rs
```

Admin and v1 should keep different public contracts:

- admin routes return the admin envelope and lower camelCase body fields;
- v1 routes return OpenAI-compatible error objects and headers.

The unification is internal: shared error types, shared conversion points, and fewer
ad hoc `(StatusCode, Json<Value>)` helpers in handlers.

Preferred handler shape:

```rust
pub async fn handler(...) -> Result<impl IntoResponse, AppError> {
    let data = state.services.accounts.list(...).await?;
    Ok(AdminEnvelope::ok(data, request_id))
}
```

Do not force one public response format across admin and v1.

## Test Reorganization

The test suite should be split for the same reason as the source: current files prove
behavior, but they are too large to maintain.

Rust integration tests compile each `tests/*.rs` file as a separate crate, so avoid
extreme fragmentation. Prefer 20-40 focused integration test files rather than one
file per endpoint.

### Target Test Layout

Recommended shape:

```text
tests/
  common/
    mod.rs
    app.rs
    fixtures.rs
    http.rs
    mock.rs
    assertions.rs
  fixtures/
    openai_chat_basic.json
    codex_response_basic.sse
    codex_response_error.sse
    websocket_response_completed.json
  admin_auth_test.rs
  admin_accounts_list_test.rs
  admin_accounts_mutation_test.rs
  admin_accounts_import_export_test.rs
  admin_accounts_oauth_test.rs
  admin_accounts_cookies_quota_test.rs
  admin_api_keys_test.rs
  admin_logs_models_settings_test.rs
  admin_usage_test.rs
  v1_chat_test.rs
  v1_responses_http_sse_test.rs
  v1_responses_websocket_test.rs
  v1_upstream_fallback_test.rs
  v1_upstream_errors_test.rs
  v1_models_test.rs
  account_pool_test.rs
  account_repository_test.rs
  codex_client_test.rs
  codex_websocket_test.rs
  translation_test.rs
```

### `tests/common`

Move shared setup first, before splitting large tests:

```rust
pub struct TestApp {
    pub router: Router,
    pub state: AppState,
    pub db: SqlitePool,
    pub client_key: Option<String>,
    pub admin_cookie: Option<String>,
}
```

Useful helpers:

```rust
pub async fn test_app() -> TestApp;
pub async fn test_app_with_config(config: AppConfig) -> TestApp;
pub async fn create_admin_session(app: &TestApp) -> String;
pub async fn create_client_key(app: &TestApp) -> String;
pub async fn import_test_account(app: &TestApp) -> Account;
pub async fn json_get(app: &TestApp, path: &str) -> Response;
pub async fn json_post<T: Serialize>(app: &TestApp, path: &str, body: &T) -> Response;
pub fn assert_status(response: &Response, expected: StatusCode);
```

If upstream mocking is repeated, wrap it:

```rust
pub struct MockCodex {
    pub base_url: Url,
    pub server: MockServer,
}
```

### First Test Splits

Split `tests/admin_accounts_route_test.rs` first:

| New file | Scenario group |
| --- | --- |
| `admin_accounts_list_test.rs` | list, status read, labels in list, reset usage visibility. |
| `admin_accounts_mutation_test.rs` | create, delete, batch delete, batch status, label/status updates. |
| `admin_accounts_import_export_test.rs` | manual import, native import/export, CLI import. |
| `admin_accounts_oauth_test.rs` | device login, device poll, PKCE login start, callback/code relay. |
| `admin_accounts_cookies_quota_test.rs` | cookies, health check, refresh, quota, reset usage side effects. |

Then split `tests/v1_upstream_route_test.rs`:

| New file | Scenario group |
| --- | --- |
| `v1_responses_http_sse_test.rs` | Responses HTTP SSE setup, non-stream collection, passthrough streaming, terminal SSE failures. |
| `v1_responses_websocket_test.rs` | previous response WebSocket, explicit WebSocket, account affinity, handshake failures. |
| `v1_upstream_fallback_test.rs` | 429/402/403 classification, retry-after, fallback account acquisition, exhausted account behavior. |
| `v1_upstream_errors_test.rs` | upstream error bodies, transport error classification, refresh failure mapping, lifecycle log status. |
| `v1_models_test.rs` | model list/catalog/detail/info/debug routes if not already covered elsewhere. |

Keep `tests/chat_completions_route_test.rs` until v1 helpers are extracted, then rename or
merge into `v1_chat_test.rs`.

### Unit Tests Near Source

Move pure logic tests closer to source modules where practical:

- translation request mapping;
- Codex-to-OpenAI response conversion;
- model catalog suffix parsing;
- upstream error classification;
- retry-after parsing;
- usage extraction;
- quota normalization;
- header construction.

Integration tests should prove route contracts and side effects. Unit tests should cover
branchy pure functions without HTTP setup.

### Fixture Files

Move large JSON/SSE/WebSocket payloads into `tests/fixtures/`:

```rust
const BASIC_CHAT: &str = include_str!("fixtures/openai_chat_basic.json");
const CODEX_ERROR_SSE: &str = include_str!("fixtures/codex_response_error.sse");
```

Do this after test setup extraction and initial file splits. Fixture movement is cleanup,
not a prerequisite for architectural splitting.

## Recommended Execution Order

The branch has already completed the large HTTP file split, most initial service
extraction, `tests/common` extraction, the two largest integration test splits,
`AdminAuthService`-backed admin session validation, raw `AppState` dependency accessor
removal, v1 upstream dependency injection, the `app/`, `config/`, `utils/`, and
`translation/error.rs` migrations, and the first account/upstream service submodule
splits. From the current state, continue in this order:

1. Continue moving remaining account/upstream methods from `mod.rs` into the workflow
   submodules when a helper group can move without changing route behavior.
2. Finish moving large inline JSON route payloads into `tests/fixtures/`
   where they are test data rather than request-shape assertions.
3. Continue unifying internal error/response conversion, starting with the larger admin
   `auth` and `accounts` modules after the smaller Result-style handlers have stayed
   green.
4. Revisit medium-sized route tests only when they grow or start duplicating setup.

The original sequence below is kept as a reference for starting from an unorganized
tree or reviewing why earlier moves were ordered that way.

### Step 1: Establish Test Common Ground

Create `tests/common` and move duplicated app/database/session/key setup there. Do not
split test files yet.

Verification:

```bash
cargo test --test admin_accounts_list_test
cargo test --test admin_accounts_mutation_test
cargo test --test admin_accounts_import_export_test
cargo test --test admin_accounts_oauth_test
cargo test --test admin_accounts_cookies_quota_test
cargo test --test v1_responses_http_sse_test
cargo test --test v1_responses_websocket_test
cargo test --test v1_upstream_fallback_test
cargo test --test v1_upstream_errors_test
cargo test
```

### Step 2: Split Admin Tests

Mechanically move scenarios out of `admin_accounts_route_test.rs` by scenario group.
Keep assertions unchanged unless a shared helper removes duplication.

Verification after each new file:

```bash
cargo test --test admin_accounts_list_test
cargo test --test admin_accounts_mutation_test
cargo test --test admin_accounts_import_export_test
cargo test --test admin_accounts_oauth_test
cargo test --test admin_accounts_cookies_quota_test
```

### Step 3: Split V1 Tests

Mechanically move scenarios out of `v1_upstream_route_test.rs`.

Verification after each new file:

```bash
cargo test --test v1_responses_http_sse_test
cargo test --test v1_responses_websocket_test
cargo test --test v1_upstream_fallback_test
cargo test --test v1_upstream_errors_test
```

### Step 4: Split Former `src/http/admin.rs`

Move admin code into `src/http/api/admin/*.rs`, with `src/http/api/admin/mod.rs` as the module
entry. Preserve behavior.

Verification:

```bash
cargo test --test admin_auth_test
cargo test --test admin_accounts_list_test
cargo test --test admin_accounts_mutation_test
cargo test --test admin_accounts_import_export_test
cargo test --test admin_accounts_oauth_test
cargo test --test admin_accounts_cookies_quota_test
cargo test --test admin_client_keys_route_test
cargo test --test admin_logs_route_test
cargo test --test admin_models_route_test
cargo test --test admin_settings_route_test
cargo test --test admin_usage_stats_route_test
```

### Step 5: Split Former `src/http/v1.rs`

Move v1 code into `src/http/v1/*.rs`, with `src/http/v1/mod.rs` as the module entry.
Preserve behavior.

Verification:

```bash
cargo test --test chat_completions_route_test
cargo test --test v1_responses_http_sse_test
cargo test --test v1_responses_websocket_test
cargo test --test v1_upstream_fallback_test
cargo test --test v1_upstream_errors_test
cargo test --test routes_responses_test
```

### Step 6: Extract Services One Flow At A Time

Start with low-risk service extraction:

1. `ModelService`
2. `ResponsesService`
3. `ChatService`
4. `AdminAuthService`
5. `AccountService`
6. `ApiKeyService`

For each service:

- move orchestration out of the handler;
- keep HTTP request parsing and response wrapping in HTTP;
- keep repository SQL in repository modules;
- keep Codex transport in `codex`;
- keep translation in `translation`;
- keep route tests green.

### Step 7: Tighten `AppState`

After services own dependencies, remove direct handler access to raw repositories,
pool locks, token refreshers, and Codex clients. Keep this as a separate phase so
behavior regressions are easier to isolate.

### Step 8: Response/Error Cleanup

Unify internal error conversion after service boundaries are stable. Do not change
public admin or v1 response contracts.

## Guardrails For Each PR

Each PR or commit should state which kind of change it is:

```text
mechanical move only
test split only
service extraction
state dependency cleanup
error/response cleanup
```

Avoid mixing categories.

Before merging a mechanical move, use diff review to confirm:

- no route path changed;
- no HTTP method changed;
- no JSON field changed;
- no status code changed;
- no auth requirement changed;
- no request-id behavior changed;
- no upstream fallback policy changed;
- no database schema changed.

Minimum verification for mechanical HTTP moves:

```bash
cargo fmt --check
cargo test --test admin_accounts_list_test
cargo test --test admin_accounts_mutation_test
cargo test --test admin_accounts_import_export_test
cargo test --test admin_accounts_oauth_test
cargo test --test admin_accounts_cookies_quota_test
cargo test --test admin_client_keys_route_test
cargo test --test v1_responses_http_sse_test
cargo test --test v1_responses_websocket_test
cargo test --test v1_upstream_fallback_test
cargo test --test v1_upstream_errors_test
cargo test --test chat_completions_route_test
```

Full verification before declaring a phase complete:

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features --locked -- -D warnings
```

## Decision Record

- Keep a single crate for now.
- Split source files before changing architecture.
- Split tests before or alongside source file movement so failures are local.
- Extract v1 services before admin services because v1 is the core proxy path.
- Keep admin and v1 public response contracts separate.
- Treat local `cpr_` API key management as a Rust-local admin utility, not TypeScript
  provider-key parity.
- Do not add proxy pools, non-OpenAI providers, frontend/Electron flows, or workspace
  crate boundaries as part of this cleanup.
