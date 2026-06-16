# Architecture

This document is the canonical Rust workspace architecture for
`codex-proxy-rs`. The Rust implementation must converge to this directory
shape and naming exactly. A migration is not complete while any extra Rust
source file, extra Rust source directory, or forbidden transitional name remains.

This document intentionally governs Rust workspace source layout. The `web/`
frontend has its own package layout and is not part of the Rust source
white-list below.

## Architectural Style

The application is a modular monolith with hexagonal boundaries.

- `core` owns domain model, use cases, protocol semantics, policy, and ports.
- `adapters` owns concrete implementations of `core` ports.
- `runtime` owns composition, application state, and background task wiring.
- `server` owns the Axum HTTP boundary.
- `platform` owns shared infrastructure primitives.
- `assets` owns compiled frontend asset serving.
- `xtask` owns local automation.

## Non-Negotiable Rules

- Rust source directories and files must match this document exactly at
  completion.
- Do not keep transitional module names after migration.
- Do not hide structure drift behind compatibility re-exports.
- `core` must not depend on any project crate in the final architecture.
- `core` must not own concrete SQLx, Reqwest, Axum, filesystem path, TLS, or
  upstream IO implementations.
- `server` must not decide business policy.
- `runtime` must not contain raw SQL, HTTP handlers, or domain rules.
- `adapters` must not define domain policy.
- Architecture tests must enforce directory shape, dependency direction,
  forbidden names, and forbidden imports.

## Root Workspace Layout

```text
codex-proxy-rs/
├── Cargo.toml
├── Cargo.lock
├── README.md
├── AGENTS.md
├── src/
│   └── lib.rs
├── crates/
│   ├── core/
│   ├── adapters/
│   ├── runtime/
│   ├── server/
│   ├── platform/
│   ├── assets/
│   └── xtask/
├── tests/
├── web/
└── docs/
```

Root `src/lib.rs` is the only root facade. The root `Cargo.toml` must use the
default library path or explicitly point to `src/lib.rs`. Files such as
`crates/server/facade.rs` are forbidden.

## Dependency Direction

Final dependency graph:

```text
root facade -> server

server   -> runtime + core + assets + platform
runtime  -> core + adapters + platform
adapters -> core + platform
core     -> no project crate dependencies
assets   -> no project crate dependencies
xtask    -> workspace tooling only
platform -> no domain crate dependencies
```

Forbidden project-crate dependencies:

- `core` must not depend on `platform`, `server`, `runtime`, `adapters`, or
  `assets`.
- `platform` must not depend on `core`, `server`, `runtime`, `adapters`, or
  `assets`.
- `adapters` must not depend on `runtime` or `server`.
- `runtime` must not depend on `server`.
- `assets` must not depend on project crates.

`runtime` maps `platform` configuration into `core` configuration DTOs before
constructing core services. This keeps `core` independent from config loading,
filesystem paths, environment variables, and storage bootstrap concerns.

## Crate Responsibilities

### `crates/core`

Owns domain concepts, invariants, use case orchestration, protocol semantics,
and ports.

Allowed:

- account pool and scheduling policy
- admin business use cases
- authentication/session domain behavior
- model catalog rules
- usage and event policy
- OpenAI/Codex protocol data structures and pure conversion
- request dispatch, fallback, retry, quota, affinity, recovery, and usage policy
- port traits
- typed domain and application errors

Forbidden:

- Axum extractors, routers, responses, or `IntoResponse`
- SQLx pools, queries, rows, transactions, or migrations
- concrete `reqwest::Client`
- concrete WebSocket/TLS connection setup
- concrete filesystem path decisions
- platform config loader/types
- public re-exports of project crates

### `crates/adapters`

Owns concrete implementations of ports defined by `core`.

Allowed:

- SQLx implementations of storage ports
- encrypted SQLite stores
- Reqwest HTTP clients
- concrete OpenAI OAuth client
- concrete Codex upstream HTTP/SSE/WebSocket clients
- concrete fingerprint update client
- adapter-specific translation from framework errors into `core` errors

Forbidden:

- business policy
- Axum route handlers
- runtime task orchestration

### `crates/runtime`

Owns application composition.

Allowed:

- `AppState`
- service construction
- repository construction
- adapter construction
- dependency injection
- background task startup and shutdown
- startup restore flows
- platform-to-core config mapping

Forbidden:

- HTTP request/response mapping
- raw SQL query strings
- concrete upstream protocol side effects
- account selection, fallback, quota, model, or recovery policy

### `crates/server`

Owns the HTTP boundary.

Allowed:

- Axum routers and route handlers
- extractors and middleware
- request IDs, tracing, CORS, and auth extraction
- admin response envelopes
- OpenAI-compatible HTTP response bodies
- HTTP status and header mapping
- SSE framing for client responses

Forbidden:

- SQL queries
- concrete upstream Codex IO
- account selection, fallback, quota, model, or recovery policy
- background task orchestration

### `crates/platform`

Owns cross-cutting infrastructure primitives.

Allowed:

- config loading and config schema types
- crypto primitives
- identity hashing primitives
- SQLite connection setup and schema files
- filesystem path primitives
- logging primitives
- JSON and pagination helpers

Forbidden:

- Codex/OpenAI business use cases
- admin business use cases
- Axum handlers
- implementations of `core` ports

### `crates/assets`

Owns static frontend asset serving.

Allowed:

- root `index.html`
- `/assets/*`
- SPA fallback after API routes
- static cache and security headers

### `crates/xtask`

Owns local automation.

Allowed:

- frontend build orchestration
- architecture checks
- release/package commands
- developer maintenance commands

## Exact Rust Source Shape

### Root Facade

```text
src/
└── lib.rs
```

### Core

```text
crates/core/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── error.rs
    ├── admin/
    │   ├── mod.rs
    │   ├── ports.rs
    │   ├── auth.rs
    │   ├── client_keys.rs
    │   └── settings.rs
    ├── accounts/
    │   ├── mod.rs
    │   ├── model.rs
    │   ├── ports.rs
    │   ├── service.rs
    │   ├── pool.rs
    │   ├── lifecycle.rs
    │   ├── cookies.rs
    │   ├── jwt.rs
    │   └── usage.rs
    ├── auth/
    │   ├── mod.rs
    │   ├── ports.rs
    │   ├── oauth.rs
    │   └── session.rs
    ├── models/
    │   ├── mod.rs
    │   ├── model.rs
    │   ├── ports.rs
    │   ├── catalog.rs
    │   └── service.rs
    ├── events/
    │   ├── mod.rs
    │   ├── model.rs
    │   ├── ports.rs
    │   └── service.rs
    ├── usage/
    │   ├── mod.rs
    │   ├── model.rs
    │   ├── ports.rs
    │   └── service.rs
    ├── protocol/
    │   ├── mod.rs
    │   ├── openai/
    │   │   ├── mod.rs
    │   │   ├── chat.rs
    │   │   ├── responses.rs
    │   │   ├── models.rs
    │   │   ├── errors.rs
    │   │   └── schema.rs
    │   └── codex/
    │       ├── mod.rs
    │       ├── chat.rs
    │       ├── responses.rs
    │       ├── events.rs
    │       ├── sse.rs
    │       ├── websocket.rs
    │       └── schema.rs
    ├── gateway/
    │   ├── mod.rs
    │   ├── ports.rs
    │   ├── fingerprint.rs
    │   ├── conversation.rs
    │   └── installation.rs
    └── serving/
        ├── mod.rs
        ├── chat.rs
        ├── responses.rs
        ├── errors.rs
        ├── routing.rs
        ├── fallback.rs
        ├── affinity.rs
        ├── quota.rs
        ├── stream.rs
        ├── recovery.rs
        └── usage.rs
```

`protocol` contains only protocol data structures and pure conversion.
`gateway` contains upstream-facing ports and transport-neutral request context.
Concrete HTTP, SSE, WebSocket, TLS, and cookie-jar behavior lives in adapters.

### Adapters

```text
crates/adapters/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── sqlite/
    │   ├── mod.rs
    │   ├── accounts.rs
    │   ├── account_tokens.rs
    │   ├── account_usage.rs
    │   ├── refresh_leases.rs
    │   ├── cookies.rs
    │   ├── events.rs
    │   ├── models.rs
    │   ├── session_affinity.rs
    │   ├── admin_sessions.rs
    │   └── client_keys.rs
    ├── http/
    │   ├── mod.rs
    │   ├── reqwest_client.rs
    │   └── headers.rs
    ├── codex/
    │   ├── mod.rs
    │   ├── client.rs
    │   ├── sse.rs
    │   ├── models.rs
    │   ├── fingerprint.rs
    │   └── websocket/
    │       ├── mod.rs
    │       ├── client.rs
    │       ├── pool.rs
    │       ├── codec.rs
    │       ├── deflate.rs
    │       └── opening.rs
    └── oauth/
        ├── mod.rs
        └── openai.rs
```

### Runtime

```text
crates/runtime/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── bootstrap.rs
    ├── state.rs
    ├── services.rs
    ├── repositories.rs
    ├── upstream.rs
    ├── config.rs
    └── tasks/
        ├── mod.rs
        ├── coordinator.rs
        ├── token_refresh.rs
        ├── quota_refresh.rs
        ├── model_refresh.rs
        ├── cookie_cleanup.rs
        ├── session_cleanup.rs
        └── fingerprint_update.rs
```

`runtime/src/config.rs` maps `platform` configuration into `core` configuration
DTOs.

### Server

```text
crates/server/
├── Cargo.toml
└── src/
    ├── main.rs
    ├── lib.rs
    ├── router.rs
    ├── error/
    │   ├── mod.rs
    │   ├── admin.rs
    │   └── openai.rs
    ├── middleware/
    │   ├── mod.rs
    │   ├── request_id.rs
    │   ├── trace.rs
    │   ├── auth.rs
    │   └── cors.rs
    ├── admin_api/
    │   ├── mod.rs
    │   ├── router.rs
    │   ├── response.rs
    │   ├── session.rs
    │   ├── settings.rs
    │   ├── diagnostics.rs
    │   ├── models.rs
    │   ├── usage.rs
    │   ├── accounts/
    │   │   ├── mod.rs
    │   │   ├── list.rs
    │   │   ├── create.rs
    │   │   ├── import.rs
    │   │   ├── export.rs
    │   │   ├── lifecycle.rs
    │   │   ├── quota.rs
    │   │   ├── cookies.rs
    │   │   ├── oauth.rs
    │   │   └── health.rs
    │   ├── client_keys/
    │   │   ├── mod.rs
    │   │   ├── list.rs
    │   │   ├── create.rs
    │   │   ├── import.rs
    │   │   ├── export.rs
    │   │   └── lifecycle.rs
    │   └── logs/
    │       ├── mod.rs
    │       ├── query.rs
    │       ├── detail.rs
    │       └── state.rs
    └── openai_api/
        ├── mod.rs
        ├── router.rs
        ├── auth.rs
        ├── chat.rs
        ├── responses.rs
        ├── models.rs
        ├── diagnostics.rs
        ├── error.rs
        └── sse.rs
```

`server/src/error/openai.rs` maps application errors into OpenAI-compatible HTTP
responses. `server/src/openai_api/error.rs` and `server/src/openai_api/sse.rs`
own route-local OpenAI API error/event framing.

### Platform

```text
crates/platform/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── config/
    │   ├── mod.rs
    │   ├── loader.rs
    │   └── types.rs
    ├── crypto/
    │   ├── mod.rs
    │   ├── secret_box.rs
    │   └── hash.rs
    ├── identity/
    │   ├── mod.rs
    │   ├── admin_password.rs
    │   └── client_key.rs
    ├── storage/
    │   ├── mod.rs
    │   ├── sqlite.rs
    │   ├── schema.sql
    │   └── paths.rs
    ├── logging/
    │   ├── mod.rs
    │   └── rotation.rs
    └── json/
        ├── mod.rs
        └── pagination.rs
```

### Assets

```text
crates/assets/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── router.rs
    └── headers.rs
```

### Xtask

```text
crates/xtask/
├── Cargo.toml
└── src/
    ├── main.rs
    ├── build_web.rs
    ├── check_architecture.rs
    └── release.rs
```

## Cargo Integration Test Shape

Cargo integration test entry files must remain at `tests/*.rs`; submodules live
under matching directories.

```text
tests/
├── admin.rs
├── openai_api.rs
├── accounts.rs
├── codex_upstream.rs
├── runtime.rs
├── architecture.rs
├── admin/
├── openai_api/
├── accounts/
├── codex_upstream/
├── runtime/
├── architecture/
│   ├── mod.rs
│   ├── dependency_direction.rs
│   ├── forbidden_imports.rs
│   └── directory_shape.rs
└── support/
    ├── mod.rs
    ├── app.rs
    ├── fixtures.rs
    └── wiremock.rs
```

Additional fixture files are allowed under `tests/fixtures/`.

## Docs Shape

```text
docs/
├── architecture.md
├── adr/
├── api/
└── operations/
```

Historical migration plans may remain under `docs/superpowers/` while active
development is ongoing. They are not part of the final architecture white-list.

## Web Shape

The `web/` directory is outside this Rust workspace architecture white-list.
It is governed by frontend build tooling and must not be checked by Rust
directory-shape tests. `web/node_modules/` and `web/dist/` are build artifacts
from the frontend toolchain, not Rust architecture inputs.

## Required Names

- Root facade: `src/lib.rs`
- HTTP admin boundary: `server/src/admin_api`
- HTTP OpenAI-compatible boundary: `server/src/openai_api`
- Core port files: `ports.rs`
- Concrete SQLite adapters: `adapters/src/sqlite`
- Concrete OAuth adapter: `adapters/src/oauth/openai.rs`
- Concrete Codex upstream adapters: `adapters/src/codex`
- Concrete Codex WebSocket adapter: `adapters/src/codex/websocket`
- Shared middleware: `server/src/middleware`
- HTTP response errors: `server/src/error`
- OpenAI API route-local SSE framing: `server/src/openai_api/sse.rs`
- Business errors: `core/src/error.rs` or domain-local `errors.rs`

## Forbidden Names

- `crates/admin`
- `crates/server/facade.rs`
- `server/src/admin.rs`
- `server/src/codex_http`
- `server/src/openai_api/stream.rs`
- `core/src/**/repository.rs`
- `core/src/**/repository/`
- `adapters/src/*_repository.rs`
- `runtime/src/*_repository.rs`
- `core/src/codex/gateway/transport`
- `core/src/codex/gateway/oauth/client.rs`
- `core/src/codex/serving/http`
- `platform/src/platform`
- `platform/src/utils`

## Port Placement

Ports are traits owned by `core`.

- account storage ports live in `core/src/accounts/ports.rs`
- admin session/client-key ports live in `core/src/admin/ports.rs`
- OAuth and token refresh ports live in `core/src/auth/ports.rs`
- model snapshot ports live in `core/src/models/ports.rs`
- event log ports live in `core/src/events/ports.rs`
- usage ports live in `core/src/usage/ports.rs`
- upstream Codex and fingerprint ports live in `core/src/gateway/ports.rs`

Concrete implementations live in `adapters`.

- `SqliteAccountStore` lives in `adapters/src/sqlite/accounts.rs`
- `SqliteAdminSessionStore` lives in `adapters/src/sqlite/admin_sessions.rs`
- `OpenAiOAuthClient` lives in `adapters/src/oauth/openai.rs`
- `ReqwestCodexClient` lives in `adapters/src/codex/client.rs`
- `CodexWebSocketPool` implementation lives in `adapters/src/codex/websocket/pool.rs`

## Migration Rules

Every migration step must move the codebase closer to this document.

Required order:

1. Add architecture tests for directory shape and forbidden names.
2. Move the root facade to `src/lib.rs`.
3. Rename server HTTP boundaries to `admin_api` and `openai_api`.
4. Move admin business code into `core/src/admin`.
5. Move all core repository traits into `ports.rs`.
6. Move all SQLx implementations into `adapters/src/sqlite`.
7. Move all Reqwest/SSE/WebSocket/OAuth implementations into `adapters`.
8. Collapse runtime into composition-only files.
9. Collapse platform into config/crypto/identity/storage/logging/json primitives.
10. Update facade exports after internal structure is correct.
11. Run full verification only after the structure is fully aligned.

Do not use transitional aliases as a substitute for migration. A transitional
alias is acceptable only if it is removed in the same migration series before
claiming completion.

## Architecture Tests

Architecture tests must enforce:

- every required Rust source file exists
- every required Rust source directory exists
- forbidden Rust source files and directories do not exist
- no extra Rust source files exist under `src/` and `crates/*/src/`
- no extra Rust source directories exist under `src/` and `crates/*/src/`
- `Cargo.toml` workspace members match this document
- root library path is `src/lib.rs`
- `core` has no project-crate dependencies
- `core` has no `axum`, `sqlx`, or concrete `reqwest::Client`
- `server` has no SQLx queries
- `runtime` has no SQLx query strings and no Axum handlers
- `adapters` owns concrete SQLx/Reqwest/OAuth/Codex IO
- `core` ports are declared in `ports.rs`
- root facade does not hide invalid internal directory names

The required architecture test files are:

```text
tests/architecture/dependency_direction.rs
tests/architecture/forbidden_imports.rs
tests/architecture/directory_shape.rs
```

## Completion Criteria

The migration is complete only when all of the following are true:

- The Rust workspace source tree matches this document exactly.
- Forbidden names are absent from Rust source directories.
- Architecture tests enforce this document.
- `cargo fmt --check` passes.
- `cargo check --workspace --all-targets` passes.
- `cargo test --workspace` passes.
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` passes.
