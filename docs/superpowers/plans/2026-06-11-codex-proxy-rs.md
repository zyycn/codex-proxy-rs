# Codex Proxy RS Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `codex-proxy-rs`, a lean Rust rewrite of `codex-proxy` that exposes OpenAI-compatible local endpoints backed only by ChatGPT/Codex accounts and the official Codex backend.

**Architecture:** Create a new Rust service at `/home/zyy/桌面/Codes/codex-proxy-rs`. The only upstream is `https://chatgpt.com/backend-api`; remove OpenAI official API key passthrough, Anthropic, Gemini, custom providers, Ollama, Electron, and per-account proxy assignment. Treat Codex Desktop impersonation as a first-class subsystem: locked `reqwest` + `rustls`, exact headers, account-scoped Cookie replay, and fingerprint auto-update. Keep `src/main.rs` as the binary/bootstrap layer and keep reusable logic in `src/lib.rs` modules with typed `thiserror` errors.

**Tech Stack:** Rust 2021, `tokio`, `axum`, `tower-http`, `sqlx` + SQLite WAL, `config`, `dotenvy`, `serde`, `serde_json`, `serde_yaml`, `tracing`, `tracing-subscriber`, rotating file logging, `argon2`, `rand`, `uuid`, `chrono`, `thiserror`, `anyhow` only in `main.rs` and tests, `base64`, `aes-gcm`, `hmac`, `sha2`, `secrecy`, `zeroize`, `wiremock`, `insta`, `tempfile`. Use the latest stable crate versions at implementation time except dependencies intentionally pinned for Codex Desktop TLS fingerprint parity: `reqwest = 0.12.28` and `rustls = 0.23.36` until a fingerprint review proves a newer pair is equivalent.

**Execution Mode:** Subagent-Driven. Execute one task at a time with implementation, spec review, and code-quality review before moving to the next task. After each completed task, update this plan's checkbox state and `docs/implementation-status.md` before committing.

---

## Locked Scope

Keep:

- OpenAI-compatible inbound API: `/v1/chat/completions`, `/v1/responses`, `/v1/models`.
- Codex Responses passthrough for clients that already speak Responses format.
- ChatGPT/Codex OAuth login, device login, CLI token import, manual access-token import.
- Account pool, account rotation, quota state, and token refresh behavior reimplemented natively with the same required semantics, not through a TypeScript compatibility layer.
- Rust native TLS path using pinned `reqwest` and `rustls`.
- Codex Desktop fingerprint headers and fingerprint auto-update.
- Account-scoped Cloudflare Cookie capture and replay.
- Structured logs, queryable event records, cursor pagination, and log rotation.

Remove:

- OpenAI official API Key direct upstream.
- Anthropic, Gemini, custom provider routing, OpenRouter-style provider pools.
- Ollama bridge and Ollama settings.
- Per-account proxy assignment, proxy health checks, proxy UI, proxy routing.
- Electron desktop packaging and large dashboard UI.
- Legacy compatibility layers, old-project API adapters, and old-project data migration scripts.

Security split:

- Admin login is not API key auth.
- Client API keys call `/v1/*` only and cannot log in to admin endpoints.
- Upstream Codex account tokens are internal credentials and cannot be used as client API keys or admin passwords.

## Rust Architecture Adjustments

These adjustments come from the `rust-best-practices` review and should be treated as implementation constraints:

- Use module-local `thiserror` error enums for library code. Reserve `anyhow::Result` for `src/main.rs`, tests, and one-off setup commands.
- Add `Cargo.toml` lint configuration early. Run `cargo clippy --all-targets --all-features --locked -- -D warnings` before each task commit.
- Store secrets with explicit secret types: `secrecy::SecretString` for in-memory tokens/passwords and AES-GCM ciphertext for persisted access tokens, refresh tokens, Cookies, and API key pepper material.
- Hash admin passwords with Argon2id. Hash high-entropy client API keys with HMAC-SHA256 using a server-side pepper, not with the API key display name.
- Prefer concrete service structs over early `dyn Trait` abstraction. Use generics for test seams where useful; introduce trait objects only at boundaries that actually need runtime polymorphism.
- Keep shared runtime state in an `Arc<AppServices>` container that holds cheap-clone handles (`SqlitePool`, `reqwest::Client`, repositories, secret box, config). Avoid mutable globals and avoid `std::sync::Mutex` across async `.await`; use SQLite, channels, or `tokio::sync` primitives.
- Use the `config` crate with `config/default.yaml`, optional `config/local.yaml`, and `CPRS_*` environment overrides. Keep `.env` support via `dotenvy` for local development only.
- Prefer borrowed parameters in public APIs (`&str`, `&[T]`, references to request structs) unless ownership transfer is part of the domain model.
- Keep tests descriptive and targeted. Unit tests live beside modules for private behavior; integration tests under `tests/` cover route and storage behavior. Use `insta` only for structured payload snapshots that are worth reviewing.
- Add sparse Chinese comments only where the reason is not obvious from names or types. Good targets are protocol/security/business invariants: TLS fingerprint pinning, Codex Desktop header parity, account-scoped Cookie replay, admin-session versus client-API-key boundaries, and token refresh invariants. Do not comment every line, do not restate obvious code, and do not leave untracked TODO comments.
- This project has no old deployment or data history to preserve. Do not add compatibility shims, legacy adapters, dual-mode behavior for removed features, or scripts that migrate data from the TypeScript project. SQL schema version files are allowed only for this Rust service's own database creation and future schema evolution.
- Keep Rust identifiers idiomatic: type names use `PascalCase`, functions/variables/fields use `snake_case`, constants use `SCREAMING_SNAKE_CASE`. JSON fields exposed to the frontend use lower camelCase via `#[serde(rename_all = "camelCase")]`; do not expose PascalCase or snake_case JSON fields in admin/frontend APIs.

## Dependency Version Policy

- Resolve non-fingerprint Rust dependencies with `cargo add` at implementation time so the project starts from current stable crate releases.
- Commit `Cargo.lock` for reproducible application builds.
- Run `cargo update` during dependency-refresh tasks and include the update in a dedicated commit with test evidence.
- Keep `reqwest = 0.12.28` and `rustls = 0.23.36` pinned for the initial TLS fingerprint implementation. Upgrading either requires a separate fingerprint verification task that compares the real Codex Desktop TLS behavior, request headers, and Cloudflare response behavior before changing the pin.
- Do not introduce an unmaintained crate when a maintained Rust community standard exists. Prefer widely adopted crates already listed in the tech stack.

## API Contract and Status Codes

`codex-proxy-rs` exposes two response families. Do not mix them.

OpenAI-compatible `/v1/*` endpoints:

- Success responses follow the OpenAI-compatible shape for the endpoint.
- Error responses always use OpenAI error format:

```json
{
  "error": {
    "message": "Human readable message",
    "type": "invalid_request_error",
    "param": null,
    "code": "invalid_api_key"
  }
}
```

- Responses include an `X-Request-Id` header for tracing, but do not add `requestId` to the JSON body because that would break OpenAI compatibility.
- Streaming errors before any upstream bytes are sent return the normal HTTP status plus OpenAI error JSON.
- Streaming errors after SSE starts are sent as an SSE error event using the same `error` object.

Admin/frontend `/admin/*` endpoints:

- Use real HTTP status codes and a body-level frontend code. The HTTP status is still authoritative for transport, browser, gateway, and observability behavior. The body `code` is for frontend branching and product error handling.
- JSON field names use lower camelCase (`requestId`, `nextCursor`, `createdAt`). Rust structs keep idiomatic Rust naming and apply `#[serde(rename_all = "camelCase")]`.
- All responses include an `X-Request-Id` response header. Admin/frontend JSON bodies also include `requestId` except `204 No Content`, because a `204` response has no body.
- Success envelope:

```json
{
  "code": 200,
  "message": "OK",
  "data": {},
  "requestId": "req_..."
}
```

- List envelope:

```json
{
  "code": 200,
  "message": "OK",
  "data": [],
  "page": {
    "limit": 50,
    "nextCursor": null
  },
  "requestId": "req_..."
}
```

- Error envelope:

```json
{
  "code": 40101,
  "message": "Admin login required",
  "data": null,
  "requestId": "req_..."
}
```

Body code ranges:

| Body code | HTTP status | Meaning |
| --- | --- | --- |
| `200` | `200` | Successful read/login/mutation with body. |
| `201` | `201` | Resource created. |
| `204` | `204` | No body; use only when the frontend does not need an envelope. |
| `40000`-`40099` | `400` | Malformed JSON, invalid parameters, or invalid cursor. |
| `40100`-`40199` | `401` | Missing/expired admin session or bad admin password. |
| `40300`-`40399` | `403` | Authenticated admin lacks permission for a local-only/bootstrap action. |
| `40400`-`40499` | `404` | Admin resource not found. |
| `40900`-`40999` | `409` | Conflict, such as duplicate account import or stale state transition. |
| `42200`-`42299` | `422` | Well-formed request that fails domain validation. |
| `42900`-`42999` | `429` | Login rate limit or admin operation rate limit. |
| `50000`-`50099` | `500` | Internal service error. |

Status code matrix:

| Area | Status | Meaning |
| --- | --- | --- |
| Health | `200` | Service is running. |
| Admin | `200` | Successful read, login, or mutation with response body. |
| Admin | `201` | Resource created, such as a client API key or imported account. |
| Admin | `204` | Successful mutation with no response body, such as logout/delete when no body is useful. |
| Admin | `400` | Malformed JSON, invalid parameters, or invalid cursor. |
| Admin | `401` | Missing/expired admin session or bad admin password. |
| Admin | `403` | Authenticated admin lacks permission for a local-only/bootstrap action. |
| Admin | `404` | Admin resource not found. |
| Admin | `409` | Conflict, such as duplicate account import or stale state transition. |
| Admin | `422` | Well-formed request that fails domain validation. |
| Admin | `429` | Login rate limit or admin operation rate limit. |
| Admin | `500` | Internal service error. |
| `/v1/*` | `200` | Successful non-streaming response or SSE stream accepted. |
| `/v1/*` | `400` | Invalid OpenAI/Responses request body. |
| `/v1/*` | `401` | Missing or invalid client API key. Never use admin session as API auth. |
| `/v1/*` | `404` | Requested model is not a supported Codex model. |
| `/v1/*` | `413` | Request payload too large for safe replay/forwarding. |
| `/v1/*` | `429` | Codex quota/rate limit surfaced to client. |
| `/v1/*` | `499` | Client aborted; log internally only. Do not send this after disconnect. |
| `/v1/*` | `502` | Upstream Codex transport/protocol failure. |
| `/v1/*` | `503` | No usable Codex account is available. |
| `/v1/*` | `504` | Upstream timeout. |

All paginated admin APIs use cursor pagination with `limit` default `50`, max `200`, and stable ordering by `(created_at desc, id desc)`.

## Documentation Progress Discipline

- Each implementation task must update the matching checkboxes in this plan before its commit.
- Each completed feature must update `docs/implementation-status.md` with status, commit hash, test command, and any known limitations.
- API shape or status-code changes must update `docs/api.md` and `docs/status-codes.md` in the same commit as the code change.
- Dependency changes must update `docs/dependency-policy.md` and mention whether the change is a normal latest-stable refresh or a TLS-fingerprint-sensitive change.
- A task is not complete until code, tests, and documentation status are all updated.

## Target File Structure

Create the new project at `/home/zyy/桌面/Codes/codex-proxy-rs`:

```text
codex-proxy-rs/
  Cargo.toml
  rust-toolchain.toml
  README.md
  config/default.yaml
  docs/api.md
  docs/dependency-policy.md
  docs/implementation-status.md
  docs/status-codes.md
  migrations/0001_initial.sql
  migrations/0002_events_indexes.sql
  src/
    main.rs
    lib.rs
    app.rs
    config.rs
    error.rs
    state.rs
    crypto.rs
    pagination.rs
    http/
      mod.rs
      middleware.rs
      admin.rs
      auth.rs
      v1.rs
      health.rs
    auth/
      mod.rs
      error.rs
      admin_session.rs
      api_key.rs
      oauth.rs
      refresh.rs
      token.rs
    accounts/
      mod.rs
      model.rs
      pool.rs
      repository.rs
      lifecycle.rs
    codex/
      mod.rs
      client.rs
      headers.rs
      sse.rs
      types.rs
      usage.rs
      websocket.rs
    fingerprint/
      mod.rs
      model.rs
      updater.rs
    cookies/
      mod.rs
      repository.rs
      jar.rs
    logs/
      mod.rs
      event.rs
      repository.rs
      rotation.rs
    models/
      mod.rs
      catalog.rs
    storage/
      mod.rs
      db.rs
      migrations.rs
    translation/
      mod.rs
      openai_to_codex.rs
      codex_to_openai.rs
      schema.rs
  tests/
    admin_auth_test.rs
    api_key_auth_test.rs
    codex_headers_test.rs
    cookie_store_test.rs
    logs_pagination_test.rs
    refresh_scheduler_test.rs
    routes_chat_test.rs
    routes_responses_test.rs
```

---

### Task 1: Scaffold Rust Workspace

**Files:**
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/Cargo.toml`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/rust-toolchain.toml`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/main.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/lib.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/error.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/README.md`

- [x] **Step 1: Create project directory**

Run:

```bash
cd /home/zyy/Codes
mkdir codex-proxy-rs
cd codex-proxy-rs
git init
mkdir -p src
```

Expected: `git status --short` prints no tracked files yet.

- [x] **Step 2: Resolve current crate versions and write Cargo manifest**

Run these commands so non-fingerprint dependencies resolve to the latest stable versions available at implementation time:

```bash
cargo add anyhow aes-gcm argon2 async-stream base64 bytes dotenvy futures-util hmac http http-body-util rand serde_json serde_yaml sha2 thiserror tracing tracing-appender uuid zeroize
cargo add axum --features macros,ws
cargo add chrono --features serde
cargo add config
cargo add reqwest@=0.12.28 --no-default-features --features rustls-tls-native-roots,stream,gzip,brotli,zstd,deflate,http2,json
cargo add rustls@=0.23.36
cargo add secrecy --features serde
cargo add serde --features derive
cargo add sqlx --features runtime-tokio-rustls,sqlite,chrono,uuid,json,migrate
cargo add tokio --features macros,rt-multi-thread,signal,time,fs
cargo add tower
cargo add tower-http --features cors,trace,request-id,timeout
cargo add --dev insta --features json
cargo add --dev tempfile wiremock
```

Then edit `/home/zyy/桌面/Codes/codex-proxy-rs/Cargo.toml` so it includes package metadata and lint policy. The exact non-pinned versions may be newer than this example because `cargo add` resolves them at execution time:

```toml
[package]
name = "codex-proxy-rs"
version = "0.1.0"
edition = "2021"
license = "LicenseRef-Non-Commercial"

[dependencies]
anyhow = "1"
aes-gcm = "0.10"
argon2 = "0.5"
async-stream = "0.3"
axum = { version = "0.7", features = ["macros", "ws"] }
base64 = "0.22"
bytes = "1"
chrono = { version = "0.4", features = ["serde"] }
config = "0.15"
dotenvy = "0.15"
futures-util = "0.3"
hmac = "0.12"
http = "1"
http-body-util = "0.1"
rand = "0.8"
reqwest = { version = "=0.12.28", default-features = false, features = ["rustls-tls-native-roots", "stream", "gzip", "brotli", "zstd", "deflate", "http2", "json"] }
rustls = "=0.23.36"
secrecy = { version = "0.8", features = ["serde"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"
sha2 = "0.10"
sqlx = { version = "0.8", features = ["runtime-tokio-rustls", "sqlite", "chrono", "uuid", "json", "migrate"] }
thiserror = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread", "signal", "time", "fs"] }
tower = "0.5"
tower-http = { version = "0.6", features = ["cors", "trace", "request-id", "timeout"] }
tracing = "0.1"
tracing-appender = "0.2"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
uuid = { version = "1", features = ["v4", "serde"] }
zeroize = "1"

[dev-dependencies]
insta = { version = "1", features = ["json"] }
tempfile = "3"
wiremock = "0.6"

[lints.rust]
future_incompatible = "warn"
nonstandard_style = "deny"
unsafe_code = "forbid"

[lints.clippy]
all = { level = "deny", priority = 10 }
redundant_clone = { level = "deny", priority = 9 }
needless_collect = { level = "deny", priority = 9 }
large_enum_variant = { level = "deny", priority = 9 }
manual_ok_or = { level = "deny", priority = 9 }
pedantic = { level = "warn", priority = 3 }
```

- [x] **Step 3: Pin Rust toolchain**

Create `/home/zyy/桌面/Codes/codex-proxy-rs/rust-toolchain.toml`:

```toml
[toolchain]
channel = "stable"
components = ["rustfmt", "clippy"]
```

- [x] **Step 4: Add minimal entry points**

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/lib.rs`:

```rust
pub mod error;
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/error.rs`:

```rust
use axum::{http::StatusCode, response::{IntoResponse, Response}, Json};
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("configuration error: {0}")]
    Config(String),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("upstream error: {0}")]
    Upstream(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
    #[error("bad request: {0}")]
    BadRequest(String),
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: ErrorMessage<'a>,
}

#[derive(Serialize)]
struct ErrorMessage<'a> {
    message: &'a str,
    #[serde(rename = "type")]
    kind: &'a str,
    code: &'a str,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match self {
            AppError::Unauthorized => StatusCode::UNAUTHORIZED,
            AppError::Forbidden => StatusCode::FORBIDDEN,
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Config(_) | AppError::Database(_) | AppError::Upstream(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let message = self.to_string();
        let body = Json(ErrorBody {
            error: ErrorMessage {
                message: &message,
                kind: "server_error",
                code: "codex_proxy_rs_error",
            },
        });
        (status, body).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/main.rs`:

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("codex-proxy-rs bootstrap");
    Ok(())
}
```

- [x] **Step 5: Verify scaffold builds**

Run:

```bash
cargo test
cargo clippy --all-targets --all-features --locked -- -D warnings
```

Expected: both commands pass.

- [x] **Step 6: Commit scaffold**

```bash
git add Cargo.toml rust-toolchain.toml src README.md
git commit -m "chore: scaffold codex-proxy-rs"
```

---

### Task 1A: API Contract and Documentation Baseline

**Files:**
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/docs/api.md`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/docs/status-codes.md`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/docs/dependency-policy.md`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/docs/implementation-status.md`

- [x] **Step 1: Create API contract document**

Create `/home/zyy/桌面/Codes/codex-proxy-rs/docs/api.md`:

````markdown
# API Contract

`codex-proxy-rs` exposes two API families.

## `/v1/*`

These endpoints are OpenAI-compatible and are authenticated only by client API keys using `Authorization: Bearer cpr_...`.
Responses include an `X-Request-Id` header for tracing, but the body stays OpenAI-compatible and does not include a custom `requestId` field.

Error body:

```json
{
  "error": {
    "message": "Invalid client API key",
    "type": "invalid_request_error",
    "param": null,
    "code": "invalid_api_key"
  }
}
```

Streaming errors after SSE has started use:

```text
event: error
data: {"error":{"message":"Upstream failed","type":"server_error","param":null,"code":"upstream_error"}}
```

## `/admin/*`

Admin endpoints are authenticated only by HttpOnly admin session cookies.
Admin JSON uses lower camelCase field names. Every admin response includes an `X-Request-Id` header, and every JSON body includes `requestId`.

Use real HTTP status codes and body-level frontend codes together. Do not return HTTP `200` for failed requests. The body `code` exists for frontend branching; the HTTP status remains the transport truth.

Success body:

```json
{
  "code": 200,
  "message": "OK",
  "data": {},
  "requestId": "req_01"
}
```

List body:

```json
{
  "code": 200,
  "message": "OK",
  "data": [],
  "page": {
    "limit": 50,
    "nextCursor": null
  },
  "requestId": "req_01"
}
```

Error body:

```json
{
  "code": 40101,
  "message": "Admin login required",
  "data": null,
  "requestId": "req_01"
}
```

Pagination uses cursor ordering by `(created_at desc, id desc)`, default `limit=50`, max `limit=200`.

Rust structs use `PascalCase` type names and `snake_case` fields internally, then expose lower camelCase through serde:

```rust
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiEnvelope<T> {
    pub code: u32,
    pub message: String,
    pub data: T,
    pub request_id: String,
}
```
````

- [x] **Step 2: Create status-code document**

Create `/home/zyy/桌面/Codes/codex-proxy-rs/docs/status-codes.md`:

````markdown
# Status Codes

HTTP status codes are not replaced by body codes. Admin/frontend APIs return both an accurate HTTP status and a JSON body `code` unless the status is `204 No Content`.

## Health

| Status | Meaning |
| --- | --- |
| `200` | Service is running. |

## Admin and Frontend APIs

| HTTP status | Body code range | Meaning |
| --- | --- | --- |
| `200` | `200` | Successful read, login, or mutation with response body. |
| `201` | `201` | Resource created. |
| `204` | `204` | Successful mutation with no response body. No JSON envelope is sent. |
| `400` | `40000`-`40099` | Malformed JSON, invalid parameters, or invalid cursor. |
| `401` | `40100`-`40199` | Missing/expired admin session or bad admin password. |
| `403` | `40300`-`40399` | Authenticated admin lacks permission for a local-only/bootstrap action. |
| `404` | `40400`-`40499` | Resource not found. |
| `409` | `40900`-`40999` | Duplicate resource or stale state transition. |
| `422` | `42200`-`42299` | Well-formed request failed domain validation. |
| `429` | `42900`-`42999` | Login or operation rate limit. |
| `500` | `50000`-`50099` | Internal service error. |

Recommended initial body codes:

| Body code | HTTP status | Meaning |
| --- | --- | --- |
| `40001` | `400` | Validation failed. |
| `40002` | `400` | Invalid cursor. |
| `40101` | `401` | Admin session required. |
| `40102` | `401` | Admin password invalid. |
| `40301` | `403` | Bootstrap action denied. |
| `40401` | `404` | Resource not found. |
| `40901` | `409` | Duplicate resource. |
| `42201` | `422` | Domain validation failed. |
| `42901` | `429` | Login rate limited. |
| `50001` | `500` | Internal service error. |

## OpenAI-Compatible `/v1/*`

| Status | Meaning |
| --- | --- |
| `200` | Successful response or SSE stream accepted. |
| `400` | Invalid OpenAI/Responses request body. |
| `401` | Missing or invalid client API key. |
| `404` | Requested model is not a supported Codex model. |
| `413` | Request payload too large for safe replay or forwarding. |
| `429` | Codex quota or rate limit surfaced to client. |
| `499` | Client aborted; log internally only. |
| `502` | Upstream Codex transport or protocol failure. |
| `503` | No usable Codex account is available. |
| `504` | Upstream timeout. |
````

- [x] **Step 3: Create dependency policy document**

Create `/home/zyy/桌面/Codes/codex-proxy-rs/docs/dependency-policy.md`:

````markdown
# Dependency Policy

- Use current stable crate releases for normal Rust dependencies.
- Commit `Cargo.lock` because this is an application.
- Run `cargo update` in dedicated dependency-refresh changes.
- Keep `reqwest = 0.12.28` and `rustls = 0.23.36` pinned until a TLS fingerprint review proves a newer pair matches real Codex Desktop behavior.
- Do not add unmaintained crates when a maintained community-standard crate exists.
- Every dependency change must run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test
```
````

- [x] **Step 4: Create implementation status document**

Create `/home/zyy/桌面/Codes/codex-proxy-rs/docs/implementation-status.md`:

````markdown
# Implementation Status

Update this file before each feature commit.

| Area | Status | Commit | Verification | Notes |
| --- | --- | --- | --- | --- |
| Scaffold | Planned |  |  |  |
| API contract docs | Planned |  |  |  |
| Configuration | Planned |  |  |  |
| SQLite storage | Planned |  |  |  |
| Admin auth and client API keys | Planned |  |  |  |
| Secret encryption | Planned |  |  |  |
| Logging and pagination | Planned |  |  |  |
| TLS headers and fingerprint | Planned |  |  |  |
| Cookie persistence | Planned |  |  |  |
| Account pool and refresh | Planned |  |  |  |
| Translation | Planned |  |  |  |
| HTTP routes | Planned |  |  |  |
| Upstream lifecycle | Planned |  |  |  |
| Fingerprint updates | Planned |  |  |  |
| Runtime docs and packaging | Planned |  |  |  |
````

- [x] **Step 5: Commit API contract docs**

```bash
git add docs/api.md docs/status-codes.md docs/dependency-policy.md docs/implementation-status.md docs/superpowers/plans/2026-06-11-codex-proxy-rs.md
git commit -m "docs: define api contracts and status codes"
```

---

### Task 2: Configuration and Application State

**Files:**
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/config/default.yaml`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/config.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/state.rs`
- Modify: `/home/zyy/桌面/Codes/codex-proxy-rs/src/lib.rs`
- Test: `/home/zyy/桌面/Codes/codex-proxy-rs/tests/config_test.rs`

- [x] **Step 1: Write failing config test**

Create `/home/zyy/桌面/Codes/codex-proxy-rs/tests/config_test.rs`:

```rust
use codex_proxy_rs::config::AppConfig;

#[test]
fn default_config_keeps_only_codex_backend() {
    let yaml = r#"
server:
  host: 127.0.0.1
  port: 8080
api:
  base_url: https://chatgpt.com/backend-api
auth:
  refresh_margin_seconds: 300
  refresh_enabled: true
  refresh_concurrency: 2
database:
  url: sqlite://data/codex-proxy-rs.sqlite
security:
  master_key_file: data/master.key
  api_key_pepper_file: data/api-key-pepper.key
tls:
  force_http11: false
admin:
  session_ttl_minutes: 1440
logging:
  directory: logs
  max_file_bytes: 10485760
  retention_days: 14
"#;
    let cfg: AppConfig = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(cfg.api.base_url, "https://chatgpt.com/backend-api");
    assert_eq!(cfg.auth.refresh_margin_seconds, 300);
    assert_eq!(cfg.database.url, "sqlite://data/codex-proxy-rs.sqlite");
    assert_eq!(cfg.security.master_key_file, "data/master.key");
}
```

- [x] **Step 2: Run test and verify RED**

Run:

```bash
cargo test default_config_keeps_only_codex_backend
```

Expected: compile failure because `codex_proxy_rs::config` does not exist.

- [x] **Step 3: Implement config types**

Modify `/home/zyy/桌面/Codes/codex-proxy-rs/src/lib.rs`:

```rust
pub mod config;
pub mod error;
pub mod state;
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/config.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub api: ApiConfig,
    pub auth: AuthConfig,
    pub database: DatabaseConfig,
    pub security: SecurityConfig,
    pub tls: TlsConfig,
    pub admin: AdminConfig,
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ApiConfig {
    pub base_url: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct AuthConfig {
    pub refresh_margin_seconds: u64,
    pub refresh_enabled: bool,
    pub refresh_concurrency: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct DatabaseConfig {
    pub url: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct SecurityConfig {
    pub master_key_file: String,
    pub api_key_pepper_file: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct TlsConfig {
    pub force_http11: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct AdminConfig {
    pub session_ttl_minutes: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct LoggingConfig {
    pub directory: String,
    pub max_file_bytes: u64,
    pub retention_days: u64,
}
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/state.rs`:

```rust
use std::sync::Arc;

use crate::config::AppConfig;

#[derive(Clone)]
pub struct AppState {
    pub services: Arc<AppServices>,
}

pub struct AppServices {
    pub config: AppConfig,
}

impl AppState {
    pub fn new(config: AppConfig) -> Self {
        Self {
            services: Arc::new(AppServices { config }),
        }
    }

    pub fn config(&self) -> &AppConfig {
        &self.services.config
    }
}
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/config/default.yaml`:

```yaml
server:
  host: 127.0.0.1
  port: 8080
api:
  base_url: https://chatgpt.com/backend-api
auth:
  refresh_margin_seconds: 300
  refresh_enabled: true
  refresh_concurrency: 2
database:
  url: sqlite://data/codex-proxy-rs.sqlite
security:
  master_key_file: data/master.key
  api_key_pepper_file: data/api-key-pepper.key
tls:
  force_http11: false
admin:
  session_ttl_minutes: 1440
logging:
  directory: logs
  max_file_bytes: 10485760
  retention_days: 14
```

- [x] **Step 4: Verify GREEN**

Run:

```bash
cargo test default_config_keeps_only_codex_backend
```

Expected: pass.

- [x] **Step 5: Commit config**

```bash
git add config src tests/config_test.rs
git commit -m "feat: add codex-only configuration"
```

---

### Task 3: SQLite Storage with sqlx-Managed Schema

**Files:**
- Create schema version file: `/home/zyy/桌面/Codes/codex-proxy-rs/migrations/0001_initial.sql`
- Create schema version file: `/home/zyy/桌面/Codes/codex-proxy-rs/migrations/0002_events_indexes.sql`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/storage/mod.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/storage/db.rs`
- Modify: `/home/zyy/桌面/Codes/codex-proxy-rs/src/lib.rs`
- Test: `/home/zyy/桌面/Codes/codex-proxy-rs/tests/storage_schema_test.rs`

- [x] **Step 1: Write failing schema test**

Create `/home/zyy/桌面/Codes/codex-proxy-rs/tests/storage_schema_test.rs`:

```rust
use codex_proxy_rs::storage::db::connect_sqlite;

#[tokio::test]
async fn sqlite_schema_creates_accounts_and_event_tables() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("test.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();

    let row: (i64,) = sqlx::query_as("select count(*) from sqlite_master where type = 'table' and name in ('accounts', 'client_api_keys', 'event_logs')")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.0, 3);
}
```

- [x] **Step 2: Run test and verify RED**

Run:

```bash
cargo test sqlite_schema_creates_accounts_and_event_tables
```

Expected: compile failure because `storage::db` does not exist.

- [x] **Step 3: Create sqlx schema version files**

Create `/home/zyy/桌面/Codes/codex-proxy-rs/migrations/0001_initial.sql`:

```sql
pragma foreign_keys = on;

create table if not exists admin_users (
  id text primary key,
  password_hash text not null,
  created_at text not null,
  updated_at text not null
);

create table if not exists admin_sessions (
  id text primary key,
  user_id text not null references admin_users(id) on delete cascade,
  expires_at text not null,
  created_at text not null
);

create table if not exists client_api_keys (
  id text primary key,
  name text not null,
  prefix text not null,
  key_hash text not null,
  enabled integer not null default 1,
  created_at text not null,
  last_used_at text
);

create table if not exists accounts (
  id text primary key,
  email text,
  account_id text,
  user_id text,
  label text,
  plan_type text,
  access_token_cipher text not null,
  refresh_token_cipher text,
  status text not null,
  quota_json text,
  quota_fetched_at text,
  added_at text not null,
  updated_at text not null
);

create table if not exists account_usage (
  account_id text primary key references accounts(id) on delete cascade,
  request_count integer not null default 0,
  input_tokens integer not null default 0,
  output_tokens integer not null default 0,
  cached_tokens integer not null default 0,
  last_used_at text
);

create table if not exists account_cookies (
  id text primary key,
  account_id text not null references accounts(id) on delete cascade,
  domain text not null,
  name text not null,
  value_cipher text not null,
  path text not null default '/',
  expires_at text,
  updated_at text not null,
  unique(account_id, domain, name, path)
);

create table if not exists fingerprints (
  id text primary key,
  app_version text not null,
  build_number text not null,
  platform text not null,
  arch text not null,
  chromium_version text not null,
  user_agent_template text not null,
  source text not null,
  created_at text not null
);

create table if not exists event_logs (
  id text primary key,
  request_id text,
  kind text not null,
  level text not null,
  account_id text,
  route text,
  model text,
  status_code integer,
  latency_ms integer,
  message text not null,
  metadata_json text not null,
  created_at text not null
);
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/migrations/0002_events_indexes.sql`:

```sql
create index if not exists idx_event_logs_created_id on event_logs(created_at desc, id desc);
create index if not exists idx_event_logs_kind_created on event_logs(kind, created_at desc);
create index if not exists idx_event_logs_request_id on event_logs(request_id);
create index if not exists idx_accounts_status on accounts(status);
create index if not exists idx_client_api_keys_prefix on client_api_keys(prefix);
create index if not exists idx_account_cookies_account on account_cookies(account_id);
```

- [x] **Step 4: Implement database connector**

Modify `/home/zyy/桌面/Codes/codex-proxy-rs/src/lib.rs`:

```rust
pub mod config;
pub mod error;
pub mod state;
pub mod storage;
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/storage/mod.rs`:

```rust
pub mod db;
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/storage/db.rs`:

```rust
use sqlx::{sqlite::SqliteConnectOptions, SqlitePool};
use std::{str::FromStr, time::Duration};

pub async fn connect_sqlite(database_url: &str) -> Result<SqlitePool, sqlx::Error> {
    let options = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePool::connect_with(options).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}
```

- [x] **Step 5: Verify schema**

Run:

```bash
cargo test sqlite_schema_creates_accounts_and_event_tables
```

Expected: pass.

- [x] **Step 6: Commit storage**

```bash
git add Cargo.toml migrations src/storage src/lib.rs tests/storage_schema_test.rs
git commit -m "feat: add sqlite storage schema"
```

---

### Task 4: Split Admin Login, Client API Keys, and Upstream Accounts

**Files:**
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/auth/mod.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/auth/error.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/auth/admin_session.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/auth/api_key.rs`
- Modify: `/home/zyy/桌面/Codes/codex-proxy-rs/src/lib.rs`
- Test: `/home/zyy/桌面/Codes/codex-proxy-rs/tests/admin_auth_test.rs`
- Test: `/home/zyy/桌面/Codes/codex-proxy-rs/tests/api_key_auth_test.rs`

- [x] **Step 1: Write failing tests for separate secret types**

Create `/home/zyy/桌面/Codes/codex-proxy-rs/tests/api_key_auth_test.rs`:

```rust
use codex_proxy_rs::auth::api_key::ApiKeyHasher;

#[test]
fn client_api_key_has_proxy_prefix_and_verifies_against_hash() {
    let hasher = ApiKeyHasher::new([9u8; 32]);
    let generated = hasher.generate_client_api_key("cursor");
    assert!(generated.plaintext.starts_with("cpr_"));
    assert_eq!(generated.prefix.len(), 12);
    assert!(hasher.verify_client_api_key(&generated.plaintext, &generated.key_hash).unwrap());
    assert!(!hasher.verify_client_api_key("wrong", &generated.key_hash).unwrap());
}
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/tests/admin_auth_test.rs`:

```rust
use codex_proxy_rs::auth::admin_session::{hash_admin_password, verify_admin_password};

#[test]
fn admin_password_hash_is_not_a_client_api_key() {
    let hash = hash_admin_password("correct horse battery staple").unwrap();
    assert!(verify_admin_password("correct horse battery staple", &hash).unwrap());
    assert!(!verify_admin_password("cpr_fake_client_key", &hash).unwrap());
}
```

- [x] **Step 2: Run tests and verify RED**

Run:

```bash
cargo test client_api_key_has_proxy_prefix_and_verifies_against_hash admin_password_hash_is_not_a_client_api_key
```

Expected: compile failure because auth modules do not exist.

- [x] **Step 3: Implement hashing helpers**

Modify `/home/zyy/桌面/Codes/codex-proxy-rs/src/lib.rs`:

```rust
pub mod auth;
pub mod config;
pub mod crypto;
pub mod error;
pub mod state;
pub mod storage;
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/auth/mod.rs`:

```rust
pub mod admin_session;
pub mod api_key;
pub mod error;
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/auth/error.rs`:

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("password hash error: {0}")]
    PasswordHash(#[from] argon2::password_hash::Error),
    #[error("invalid api key encoding: {0}")]
    ApiKeyEncoding(#[from] base64::DecodeError),
    #[error("invalid api key pepper length")]
    InvalidPepperLength,
}

pub type AuthResult<T> = Result<T, AuthError>;
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/auth/admin_session.rs`:

```rust
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};

use crate::auth::error::AuthResult;

pub fn hash_admin_password(password: &str) -> AuthResult<String> {
    let salt = SaltString::generate(&mut OsRng);
    Ok(Argon2::default().hash_password(password.as_bytes(), &salt)?.to_string())
}

pub fn verify_admin_password(password: &str, hash: &str) -> AuthResult<bool> {
    let parsed = PasswordHash::new(hash)?;
    Ok(Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
}
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/auth/api_key.rs`:

```rust
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;

use crate::auth::error::{AuthError, AuthResult};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone)]
pub struct GeneratedClientApiKey {
    pub plaintext: String,
    pub prefix: String,
    pub key_hash: String,
}

#[derive(Debug, Clone)]
pub struct ApiKeyHasher {
    pepper: [u8; 32],
}

impl ApiKeyHasher {
    pub fn new(pepper: [u8; 32]) -> Self {
        Self { pepper }
    }

    pub fn try_from_slice(pepper: &[u8]) -> AuthResult<Self> {
        let pepper: [u8; 32] = pepper.try_into().map_err(|_| AuthError::InvalidPepperLength)?;
        Ok(Self::new(pepper))
    }

    pub fn generate_client_api_key(&self, _name: &str) -> GeneratedClientApiKey {
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        let plaintext = format!("cpr_{}", URL_SAFE_NO_PAD.encode(bytes));
        let prefix = plaintext.chars().take(12).collect::<String>();
        let key_hash = self.hash_client_api_key(&plaintext);
        GeneratedClientApiKey { plaintext, prefix, key_hash }
    }

    pub fn hash_client_api_key(&self, plaintext: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(&self.pepper).expect("HMAC accepts any key size");
        mac.update(plaintext.as_bytes());
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    }

    pub fn verify_client_api_key(&self, plaintext: &str, key_hash: &str) -> AuthResult<bool> {
        if !plaintext.starts_with("cpr_") {
            return Ok(false);
        }
        let suffix = plaintext.strip_prefix("cpr_").unwrap_or_default();
        let decoded = URL_SAFE_NO_PAD.decode(suffix)?;
        let candidate = format!("cpr_{}", URL_SAFE_NO_PAD.encode(decoded));
        Ok(self.hash_client_api_key(&candidate) == key_hash)
    }
}
```

- [x] **Step 4: Verify auth split tests**

Run:

```bash
cargo test client_api_key_has_proxy_prefix_and_verifies_against_hash admin_password_hash_is_not_a_client_api_key
```

Expected: pass.

- [x] **Step 5: Commit auth split**

```bash
git add Cargo.toml src/auth src/lib.rs tests/admin_auth_test.rs tests/api_key_auth_test.rs
git commit -m "feat: split admin login and client api keys"
```

---

### Task 4A: Encrypt Stored Upstream Secrets

**Files:**
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/crypto.rs`
- Modify: `/home/zyy/桌面/Codes/codex-proxy-rs/src/lib.rs`
- Test: `/home/zyy/桌面/Codes/codex-proxy-rs/tests/crypto_test.rs`

- [x] **Step 1: Write failing encryption test**

Create `/home/zyy/桌面/Codes/codex-proxy-rs/tests/crypto_test.rs`:

```rust
use codex_proxy_rs::crypto::SecretBox;
use secrecy::{ExposeSecret, SecretString};

#[test]
fn secret_box_encrypts_and_decrypts_without_storing_plaintext() {
    let secret_box = SecretBox::new([7u8; 32]);
    let plaintext = SecretString::new("rt_example_refresh_token".to_string());
    let ciphertext = secret_box.encrypt(&plaintext).unwrap();

    assert!(ciphertext.starts_with("v1:"));
    assert!(!ciphertext.contains("rt_example_refresh_token"));
    assert_eq!(secret_box.decrypt(&ciphertext).unwrap().expose_secret(), "rt_example_refresh_token");
}
```

- [x] **Step 2: Run test and verify RED**

Run:

```bash
cargo test secret_box_encrypts_and_decrypts_without_storing_plaintext
```

Expected: compile failure because `codex_proxy_rs::crypto::SecretBox` does not exist.

- [x] **Step 3: Implement AES-GCM secret encryption**

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/crypto.rs`:

```rust
use aes_gcm::{
    aead::{Aead, OsRng, rand_core::RngCore},
    Aes256Gcm, KeyInit, Nonce,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("invalid secret key length")]
    InvalidKeyLength,
    #[error("secret encryption failed")]
    Encrypt,
    #[error("secret decryption failed")]
    Decrypt,
    #[error("invalid secret encoding: {0}")]
    Decode(#[from] base64::DecodeError),
    #[error("unsupported secret version")]
    UnsupportedVersion,
    #[error("secret is not valid utf-8: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
}

pub type CryptoResult<T> = Result<T, CryptoError>;

#[derive(Clone)]
pub struct SecretBox {
    key: [u8; 32],
}

impl SecretBox {
    pub fn new(key: [u8; 32]) -> Self {
        Self { key }
    }

    pub fn encrypt(&self, plaintext: &SecretString) -> CryptoResult<String> {
        let cipher = Aes256Gcm::new_from_slice(&self.key)
            .map_err(|_| CryptoError::InvalidKeyLength)?;
        let mut nonce_bytes = [0u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce_bytes), plaintext.expose_secret().as_bytes())
            .map_err(|_| CryptoError::Encrypt)?;
        Ok(format!(
            "v1:{}:{}",
            URL_SAFE_NO_PAD.encode(nonce_bytes),
            URL_SAFE_NO_PAD.encode(ciphertext)
        ))
    }

    pub fn decrypt(&self, encoded: &str) -> CryptoResult<SecretString> {
        let mut parts = encoded.split(':');
        let version = parts.next().unwrap_or_default();
        let nonce = parts.next().unwrap_or_default();
        let ciphertext = parts.next().unwrap_or_default();
        if version != "v1" {
            return Err(CryptoError::UnsupportedVersion);
        }
        let nonce = URL_SAFE_NO_PAD.decode(nonce)?;
        let ciphertext = URL_SAFE_NO_PAD.decode(ciphertext)?;
        let cipher = Aes256Gcm::new_from_slice(&self.key)
            .map_err(|_| CryptoError::InvalidKeyLength)?;
        let plaintext = cipher
            .decrypt(Nonce::from_slice(&nonce), ciphertext.as_ref())
            .map_err(|_| CryptoError::Decrypt)?;
        Ok(SecretString::new(String::from_utf8(plaintext)?))
    }
}
```

Modify `/home/zyy/桌面/Codes/codex-proxy-rs/src/lib.rs`:

```rust
pub mod auth;
pub mod config;
pub mod crypto;
pub mod error;
pub mod state;
pub mod storage;
```

- [x] **Step 4: Verify encryption**

Run:

```bash
cargo test secret_box_encrypts_and_decrypts_without_storing_plaintext
```

Expected: pass.

- [x] **Step 5: Commit secret encryption**

```bash
git add Cargo.toml src/crypto.rs src/lib.rs tests/crypto_test.rs
git commit -m "feat: encrypt stored upstream secrets"
```

---

### Task 5: Structured Logging, Event Store, Rotation, and Pagination

**Files:**
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/pagination.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/logs/mod.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/logs/event.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/logs/repository.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/logs/rotation.rs`
- Modify: `/home/zyy/桌面/Codes/codex-proxy-rs/src/lib.rs`
- Test: `/home/zyy/桌面/Codes/codex-proxy-rs/tests/logs_pagination_test.rs`

- [x] **Step 1: Write failing pagination test**

Create `/home/zyy/桌面/Codes/codex-proxy-rs/tests/logs_pagination_test.rs`:

```rust
use codex_proxy_rs::{
    logs::{event::{EventLevel, EventLog}, repository::EventLogRepository},
    storage::db::connect_sqlite,
};

#[tokio::test]
async fn event_logs_are_cursor_paginated() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("logs.sqlite");
    let url = format!("sqlite://{}", db.display());
    let pool = connect_sqlite(&url).await.unwrap();
    let repo = EventLogRepository::new(pool);

    for idx in 0..3 {
        repo.insert(EventLog::new("request", EventLevel::Info, format!("event {idx}"))).await.unwrap();
    }

    let first = repo.list(None, 2).await.unwrap();
    assert_eq!(first.items.len(), 2);
    assert!(first.next_cursor.is_some());

    let second = repo.list(first.next_cursor, 2).await.unwrap();
    assert_eq!(second.items.len(), 1);
    assert!(second.next_cursor.is_none());
}
```

- [x] **Step 2: Run test and verify RED**

Run:

```bash
cargo test event_logs_are_cursor_paginated
```

Expected: compile failure because logs modules do not exist.

- [x] **Step 3: Implement pagination and event models**

Modify `/home/zyy/桌面/Codes/codex-proxy-rs/src/lib.rs`:

```rust
pub mod auth;
pub mod config;
pub mod crypto;
pub mod error;
pub mod logs;
pub mod pagination;
pub mod state;
pub mod storage;
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/pagination.rs`:

```rust
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

pub fn encode_cursor(created_at: &str, id: &str) -> String {
    URL_SAFE_NO_PAD.encode(format!("{created_at}|{id}"))
}

pub fn decode_cursor(cursor: &str) -> Option<(String, String)> {
    let raw = URL_SAFE_NO_PAD.decode(cursor).ok()?;
    let text = String::from_utf8(raw).ok()?;
    let (created_at, id) = text.split_once('|')?;
    Some((created_at.to_string(), id.to_string()))
}

pub fn clamp_limit(limit: u32) -> u32 {
    limit.clamp(1, 200)
}
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/logs/mod.rs`:

```rust
pub mod event;
pub mod repository;
pub mod rotation;
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/logs/event.rs`:

```rust
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum EventLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl EventLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            EventLevel::Debug => "debug",
            EventLevel::Info => "info",
            EventLevel::Warn => "warn",
            EventLevel::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventLog {
    pub id: String,
    pub kind: String,
    pub level: EventLevel,
    pub message: String,
    pub created_at: String,
}

impl EventLog {
    pub fn new(kind: impl Into<String>, level: EventLevel, message: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            kind: kind.into(),
            level,
            message: message.into(),
            created_at: Utc::now().to_rfc3339(),
        }
    }
}
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/logs/repository.rs`:

```rust
use sqlx::{Row, SqlitePool};

use crate::{
    logs::event::{EventLevel, EventLog},
    pagination::{clamp_limit, decode_cursor, encode_cursor, Page},
};

#[derive(Clone)]
pub struct EventLogRepository {
    pool: SqlitePool,
}

impl EventLogRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn insert(&self, event: EventLog) -> Result<(), sqlx::Error> {
        sqlx::query(
            "insert into event_logs (id, kind, level, message, metadata_json, created_at) values (?, ?, ?, ?, '{}', ?)",
        )
        .bind(event.id)
        .bind(event.kind)
        .bind(event.level.as_str())
        .bind(event.message)
        .bind(event.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list(&self, cursor: Option<String>, limit: u32) -> Result<Page<EventLog>, sqlx::Error> {
        let limit = clamp_limit(limit);
        let mut rows = if let Some(cursor) = cursor.and_then(|c| decode_cursor(&c)) {
            sqlx::query(
                "select id, kind, level, message, created_at from event_logs where (created_at < ? or (created_at = ? and id < ?)) order by created_at desc, id desc limit ?",
            )
            .bind(&cursor.0)
            .bind(&cursor.0)
            .bind(&cursor.1)
            .bind((limit + 1) as i64)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query("select id, kind, level, message, created_at from event_logs order by created_at desc, id desc limit ?")
                .bind((limit + 1) as i64)
                .fetch_all(&self.pool)
                .await?
        };

        let has_next = rows.len() > limit as usize;
        if has_next {
            rows.truncate(limit as usize);
        }
        let items = rows
            .into_iter()
            .map(|row| EventLog {
                id: row.get("id"),
                kind: row.get("kind"),
                level: match row.get::<String, _>("level").as_str() {
                    "debug" => EventLevel::Debug,
                    "warn" => EventLevel::Warn,
                    "error" => EventLevel::Error,
                    _ => EventLevel::Info,
                },
                message: row.get("message"),
                created_at: row.get("created_at"),
            })
            .collect::<Vec<_>>();
        let next_cursor = if has_next {
            items.last().map(|e| encode_cursor(&e.created_at, &e.id))
        } else {
            None
        };
        Ok(Page { items, next_cursor })
    }
}
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/logs/rotation.rs`:

```rust
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct RotationConfig {
    pub directory: PathBuf,
    pub max_file_bytes: u64,
    pub retention_days: u64,
}

impl RotationConfig {
    pub fn new(directory: impl AsRef<Path>, max_file_bytes: u64, retention_days: u64) -> Self {
        Self {
            directory: directory.as_ref().to_path_buf(),
            max_file_bytes,
            retention_days,
        }
    }
}
```

- [x] **Step 4: Verify event pagination**

Run:

```bash
cargo test event_logs_are_cursor_paginated
```

Expected: pass.

- [x] **Step 5: Commit logging foundation**

```bash
git add src/logs src/pagination.rs src/lib.rs tests/logs_pagination_test.rs
git commit -m "feat: add paginated event logs"
```

---

### Task 6: Codex Fingerprint, Header Generation, and TLS Client

**Files:**
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/fingerprint/mod.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/fingerprint/model.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/codex/mod.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/codex/headers.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/codex/client.rs`
- Modify: `/home/zyy/桌面/Codes/codex-proxy-rs/src/lib.rs`
- Test: `/home/zyy/桌面/Codes/codex-proxy-rs/tests/codex_headers_test.rs`

- [x] **Step 1: Write failing header test**

Create `/home/zyy/桌面/Codes/codex-proxy-rs/tests/codex_headers_test.rs`:

```rust
use codex_proxy_rs::{
    codex::headers::build_codex_headers,
    fingerprint::model::Fingerprint,
};

#[test]
fn codex_headers_include_desktop_identity_and_turn_state() {
    let fp = Fingerprint::default_for_tests();
    let headers = build_codex_headers(&fp, "access-token", Some("acct_123"), Some("turn-state"), "rid_1");

    assert_eq!(headers.get("originator").unwrap(), "Codex Desktop");
    assert!(headers.get("user-agent").unwrap().contains("Codex"));
    assert_eq!(headers.get("authorization").unwrap(), "Bearer access-token");
    assert_eq!(headers.get("chatgpt-account-id").unwrap(), "acct_123");
    assert_eq!(headers.get("x-codex-turn-state").unwrap(), "turn-state");
    assert_eq!(headers.get("x-client-request-id").unwrap(), "rid_1");
}
```

- [x] **Step 2: Run test and verify RED**

Run:

```bash
cargo test codex_headers_include_desktop_identity_and_turn_state
```

Expected: compile failure because `codex` and `fingerprint` modules do not exist.

- [x] **Step 3: Implement fingerprint and headers**

Modify `/home/zyy/桌面/Codes/codex-proxy-rs/src/lib.rs`:

```rust
pub mod auth;
pub mod codex;
pub mod config;
pub mod crypto;
pub mod error;
pub mod fingerprint;
pub mod logs;
pub mod pagination;
pub mod state;
pub mod storage;
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/fingerprint/mod.rs`:

```rust
pub mod model;
pub mod updater;
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/fingerprint/model.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Fingerprint {
    pub originator: String,
    pub app_version: String,
    pub build_number: String,
    pub platform: String,
    pub arch: String,
    pub chromium_version: String,
    pub user_agent_template: String,
}

impl Fingerprint {
    pub fn default_for_tests() -> Self {
        Self {
            originator: "Codex Desktop".to_string(),
            app_version: "26.519.81530".to_string(),
            build_number: "3178".to_string(),
            platform: "darwin".to_string(),
            arch: "arm64".to_string(),
            chromium_version: "146".to_string(),
            user_agent_template: "Codex/{app_version} ({platform}; {arch}) Chromium/{chromium_version}".to_string(),
        }
    }

    pub fn user_agent(&self) -> String {
        self.user_agent_template
            .replace("{app_version}", &self.app_version)
            .replace("{platform}", &self.platform)
            .replace("{arch}", &self.arch)
            .replace("{chromium_version}", &self.chromium_version)
    }
}
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/codex/mod.rs`:

```rust
pub mod client;
pub mod headers;
pub mod sse;
pub mod types;
pub mod usage;
pub mod websocket;
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/codex/headers.rs`:

```rust
use std::collections::BTreeMap;

use crate::fingerprint::model::Fingerprint;

pub fn build_codex_headers(
    fp: &Fingerprint,
    access_token: &str,
    account_id: Option<&str>,
    turn_state: Option<&str>,
    request_id: &str,
) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    headers.insert("accept".to_string(), "text/event-stream".to_string());
    headers.insert("authorization".to_string(), format!("Bearer {access_token}"));
    headers.insert("originator".to_string(), fp.originator.clone());
    headers.insert("user-agent".to_string(), fp.user_agent());
    headers.insert("x-client-request-id".to_string(), request_id.to_string());
    headers.insert("x-openai-internal-codex-residency".to_string(), "global".to_string());
    if let Some(account_id) = account_id {
        headers.insert("chatgpt-account-id".to_string(), account_id.to_string());
    }
    if let Some(turn_state) = turn_state {
        headers.insert("x-codex-turn-state".to_string(), turn_state.to_string());
    }
    headers
}
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/codex/client.rs`:

```rust
use reqwest::Client;

pub fn build_reqwest_client(force_http11: bool) -> Result<Client, reqwest::Error> {
    let builder = Client::builder()
        .use_rustls_tls()
        .no_proxy()
        .gzip(true)
        .brotli(true)
        .zstd(true)
        .deflate(true);
    let builder = if force_http11 { builder.http1_only() } else { builder };
    builder.build()
}
```

Create empty module files:

```bash
touch src/fingerprint/updater.rs src/codex/sse.rs src/codex/types.rs src/codex/usage.rs src/codex/websocket.rs
```

- [x] **Step 4: Verify headers**

Run:

```bash
cargo test codex_headers_include_desktop_identity_and_turn_state
cargo tree | rg 'reqwest|rustls'
```

Expected: test passes; `cargo tree` shows `reqwest v0.12.28` and `rustls v0.23.36`.

- [x] **Step 5: Commit TLS and headers foundation**

```bash
git add Cargo.toml src/codex src/fingerprint src/lib.rs tests/codex_headers_test.rs
git commit -m "feat: add codex tls client and headers"
```

---

### Task 7: Account Cookie Persistence

**Files:**
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/cookies/mod.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/cookies/jar.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/cookies/repository.rs`
- Modify: `/home/zyy/桌面/Codes/codex-proxy-rs/src/lib.rs`
- Test: `/home/zyy/桌面/Codes/codex-proxy-rs/tests/cookie_store_test.rs`

- [x] **Step 1: Write failing cookie replay test**

Create `/home/zyy/桌面/Codes/codex-proxy-rs/tests/cookie_store_test.rs`:

```rust
use codex_proxy_rs::cookies::jar::CookieJar;

#[test]
fn cookie_jar_captures_and_replays_account_scoped_cookies() {
    let mut jar = CookieJar::default();
    jar.capture_set_cookie("acct_a", "cf_clearance=abc; Domain=chatgpt.com; Path=/; HttpOnly");
    jar.capture_set_cookie("acct_b", "cf_clearance=def; Domain=chatgpt.com; Path=/; HttpOnly");

    assert_eq!(jar.cookie_header("acct_a", "chatgpt.com"), Some("cf_clearance=abc".to_string()));
    assert_eq!(jar.cookie_header("acct_b", "chatgpt.com"), Some("cf_clearance=def".to_string()));
}
```

- [x] **Step 2: Run test and verify RED**

Run:

```bash
cargo test cookie_jar_captures_and_replays_account_scoped_cookies
```

Expected: compile failure because cookies module does not exist.

- [x] **Step 3: Implement in-memory cookie jar**

Modify `/home/zyy/桌面/Codes/codex-proxy-rs/src/lib.rs`:

```rust
pub mod auth;
pub mod codex;
pub mod config;
pub mod cookies;
pub mod crypto;
pub mod error;
pub mod fingerprint;
pub mod logs;
pub mod pagination;
pub mod state;
pub mod storage;
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/cookies/mod.rs`:

```rust
pub mod jar;
pub mod repository;
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/cookies/jar.rs`:

```rust
use std::collections::HashMap;

#[derive(Debug, Clone)]
struct StoredCookie {
    domain: String,
    name: String,
    value: String,
}

#[derive(Debug, Default, Clone)]
pub struct CookieJar {
    by_account: HashMap<String, Vec<StoredCookie>>,
}

impl CookieJar {
    pub fn capture_set_cookie(&mut self, account_id: &str, raw: &str) {
        let mut parts = raw.split(';').map(str::trim);
        let Some(name_value) = parts.next() else { return };
        let Some((name, value)) = name_value.split_once('=') else { return };
        let mut domain = "chatgpt.com".to_string();
        for part in parts {
            if let Some(value) = part.strip_prefix("Domain=") {
                domain = value.trim_start_matches('.').to_string();
            }
        }
        let account = self.by_account.entry(account_id.to_string()).or_default();
        account.retain(|cookie| !(cookie.domain == domain && cookie.name == name));
        account.push(StoredCookie {
            domain,
            name: name.to_string(),
            value: value.to_string(),
        });
    }

    pub fn cookie_header(&self, account_id: &str, domain: &str) -> Option<String> {
        let cookies = self.by_account.get(account_id)?;
        let pairs = cookies
            .iter()
            .filter(|cookie| domain == cookie.domain || domain.ends_with(&format!(".{}", cookie.domain)))
            .map(|cookie| format!("{}={}", cookie.name, cookie.value))
            .collect::<Vec<_>>();
        if pairs.is_empty() { None } else { Some(pairs.join("; ")) }
    }
}
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/cookies/repository.rs`:

```rust
#[derive(Debug, Clone)]
pub struct CookieRepository;
```

- [x] **Step 4: Verify cookie capture**

Run:

```bash
cargo test cookie_jar_captures_and_replays_account_scoped_cookies
```

Expected: pass.

- [x] **Step 5: Commit cookies**

```bash
git add src/cookies src/lib.rs tests/cookie_store_test.rs
git commit -m "feat: add account scoped cookie jar"
```

---

### Task 8: Account Model, Pool, and Refresh Scheduler Compatibility

**Files:**
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/accounts/mod.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/accounts/model.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/accounts/pool.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/accounts/repository.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/accounts/lifecycle.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/auth/refresh.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/auth/token.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/auth/oauth.rs`
- Modify: `/home/zyy/桌面/Codes/codex-proxy-rs/src/lib.rs`
- Test: `/home/zyy/桌面/Codes/codex-proxy-rs/tests/refresh_scheduler_test.rs`

- [x] **Step 1: Write failing account acquisition test**

Create `/home/zyy/桌面/Codes/codex-proxy-rs/tests/refresh_scheduler_test.rs`:

```rust
use codex_proxy_rs::accounts::{
    model::{Account, AccountStatus},
    pool::AccountPool,
};

#[test]
fn account_pool_skips_expired_disabled_banned_and_quota_exhausted_accounts() {
    let mut pool = AccountPool::default();
    pool.insert(Account::test("active", AccountStatus::Active));
    pool.insert(Account::test("expired", AccountStatus::Expired));
    pool.insert(Account::test("disabled", AccountStatus::Disabled));
    pool.insert(Account::test("banned", AccountStatus::Banned));
    pool.insert(Account::test("quota", AccountStatus::QuotaExhausted));

    let acquired = pool.acquire("gpt-5.4").unwrap();
    assert_eq!(acquired.id, "active");
}
```

- [x] **Step 2: Run test and verify RED**

Run:

```bash
cargo test account_pool_skips_expired_disabled_banned_and_quota_exhausted_accounts
```

Expected: compile failure because accounts module does not exist.

- [x] **Step 3: Implement account status and pool**

Modify `/home/zyy/桌面/Codes/codex-proxy-rs/src/lib.rs`:

```rust
pub mod accounts;
pub mod auth;
pub mod codex;
pub mod config;
pub mod cookies;
pub mod crypto;
pub mod error;
pub mod fingerprint;
pub mod logs;
pub mod pagination;
pub mod state;
pub mod storage;
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/accounts/mod.rs`:

```rust
pub mod lifecycle;
pub mod model;
pub mod pool;
pub mod repository;
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/accounts/model.rs`:

```rust
use chrono::Utc;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AccountStatus {
    Active,
    Expired,
    QuotaExhausted,
    Refreshing,
    Disabled,
    Banned,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub email: Option<String>,
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    pub label: Option<String>,
    pub plan_type: Option<String>,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub status: AccountStatus,
    pub added_at: String,
    pub last_used_at: Option<String>,
}

impl Account {
    pub fn test(id: &str, status: AccountStatus) -> Self {
        Self {
            id: id.to_string(),
            email: None,
            account_id: None,
            user_id: None,
            label: None,
            plan_type: None,
            access_token: format!("token-{id}"),
            refresh_token: Some(format!("refresh-{id}")),
            status,
            added_at: Utc::now().to_rfc3339(),
            last_used_at: None,
        }
    }
}
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/accounts/pool.rs`:

```rust
use std::collections::BTreeMap;

use crate::accounts::model::{Account, AccountStatus};

#[derive(Debug, Default)]
pub struct AccountPool {
    accounts: BTreeMap<String, Account>,
}

impl AccountPool {
    pub fn insert(&mut self, account: Account) {
        self.accounts.insert(account.id.clone(), account);
    }

    pub fn acquire(&self, _model: &str) -> Option<Account> {
        self.accounts
            .values()
            .filter(|account| account.status == AccountStatus::Active)
            .min_by_key(|account| account.last_used_at.clone())
            .cloned()
    }
}
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/accounts/repository.rs`:

```rust
#[derive(Debug, Clone)]
pub struct AccountRepository;
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/accounts/lifecycle.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountLifecycleEvent {
    Added,
    Refreshed,
    Expired,
    QuotaExhausted,
    Banned,
    Disabled,
}
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/auth/refresh.rs`:

```rust
#[derive(Debug, Clone, Copy)]
pub struct RefreshPolicy {
    pub refresh_margin_seconds: u64,
    pub refresh_concurrency: u32,
}
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/auth/token.rs`:

```rust
#[derive(Debug, Clone)]
pub struct TokenPair {
    pub access_token: String,
    pub refresh_token: Option<String>,
}
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/auth/oauth.rs`:

```rust
#[derive(Debug, Clone)]
pub struct OAuthConfig {
    pub client_id: String,
    pub auth_endpoint: String,
    pub token_endpoint: String,
}
```

Modify `/home/zyy/桌面/Codes/codex-proxy-rs/src/auth/mod.rs`:

```rust
pub mod admin_session;
pub mod api_key;
pub mod oauth;
pub mod refresh;
pub mod token;
```

- [x] **Step 4: Verify account pool behavior**

Run:

```bash
cargo test account_pool_skips_expired_disabled_banned_and_quota_exhausted_accounts
```

Expected: pass.

- [x] **Step 5: Commit account foundation**

```bash
git add src/accounts src/auth src/lib.rs tests/refresh_scheduler_test.rs
git commit -m "feat: add account pool foundation"
```

---

### Task 9: Translation Layer for OpenAI Chat and Codex Responses

**Files:**
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/translation/mod.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/translation/schema.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/translation/openai_to_codex.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/translation/codex_to_openai.rs`
- Modify: `/home/zyy/桌面/Codes/codex-proxy-rs/src/codex/types.rs`
- Modify: `/home/zyy/桌面/Codes/codex-proxy-rs/src/lib.rs`
- Test: `/home/zyy/桌面/Codes/codex-proxy-rs/tests/routes_chat_test.rs`

- [x] **Step 1: Write failing translation test**

Create `/home/zyy/桌面/Codes/codex-proxy-rs/tests/routes_chat_test.rs`:

```rust
use codex_proxy_rs::translation::openai_to_codex::{translate_chat_to_codex, ChatCompletionRequest, ChatMessage};

#[test]
fn chat_completion_translates_to_codex_response_request() {
    let req = ChatCompletionRequest {
        model: "gpt-5.4".to_string(),
        stream: true,
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: "hello".to_string(),
        }],
    };

    let codex = translate_chat_to_codex(req).unwrap();
    assert_eq!(codex.model, "gpt-5.4");
    assert!(codex.stream);
    assert_eq!(codex.input[0]["role"], "user");
}
```

- [x] **Step 2: Run test and verify RED**

Run:

```bash
cargo test chat_completion_translates_to_codex_response_request
```

Expected: compile failure because translation module does not exist.

- [x] **Step 3: Implement minimal translation types**

Modify `/home/zyy/桌面/Codes/codex-proxy-rs/src/lib.rs`:

```rust
pub mod accounts;
pub mod auth;
pub mod codex;
pub mod config;
pub mod cookies;
pub mod crypto;
pub mod error;
pub mod fingerprint;
pub mod logs;
pub mod pagination;
pub mod state;
pub mod storage;
pub mod translation;
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/translation/mod.rs`:

```rust
pub mod codex_to_openai;
pub mod openai_to_codex;
pub mod schema;
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/codex/types.rs`:

```rust
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexResponsesRequest {
    pub model: String,
    pub instructions: String,
    pub input: Vec<Value>,
    pub stream: bool,
    pub store: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
}
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/translation/openai_to_codex.rs`:

```rust
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{codex::types::CodexResponsesRequest, error::{AppError, AppResult}};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    #[serde(default)]
    pub stream: bool,
    pub messages: Vec<ChatMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

pub fn translate_chat_to_codex(req: ChatCompletionRequest) -> AppResult<CodexResponsesRequest> {
    if req.messages.is_empty() {
        return Err(AppError::BadRequest("messages must not be empty".to_string()));
    }
    let input = req.messages
        .into_iter()
        .map(|message| json!({ "role": message.role, "content": message.content }))
        .collect();
    Ok(CodexResponsesRequest {
        model: req.model,
        instructions: String::new(),
        input,
        stream: req.stream,
        store: false,
        reasoning: None,
        tools: None,
    })
}
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/translation/codex_to_openai.rs`:

```rust
use serde_json::{json, Value};

pub fn openai_error(message: &str, code: &str) -> Value {
    json!({
        "error": {
            "message": message,
            "type": "server_error",
            "param": null,
            "code": code
        }
    })
}
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/translation/schema.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseFormat {
    Text,
    JsonObject,
    JsonSchema,
}
```

- [x] **Step 4: Verify translation**

Run:

```bash
cargo test chat_completion_translates_to_codex_response_request
```

Expected: pass.

- [x] **Step 5: Commit translation layer**

```bash
git add src/translation src/codex/types.rs src/lib.rs tests/routes_chat_test.rs
git commit -m "feat: add openai to codex translation"
```

---

### Task 10: HTTP Router and Auth Gates

**Files:**
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/app.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/http/mod.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/http/middleware.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/http/admin.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/http/auth.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/http/v1.rs`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/src/http/health.rs`
- Modify: `/home/zyy/桌面/Codes/codex-proxy-rs/src/lib.rs`
- Modify: `/home/zyy/桌面/Codes/codex-proxy-rs/src/main.rs`
- Test: `/home/zyy/桌面/Codes/codex-proxy-rs/tests/routes_responses_test.rs`

- [x] **Step 1: Write failing route boundary test**

Create `/home/zyy/桌面/Codes/codex-proxy-rs/tests/routes_responses_test.rs`:

```rust
use axum::{body::Body, http::{Request, StatusCode}};
use tower::ServiceExt;

use codex_proxy_rs::{app::build_router, config::{AdminConfig, ApiConfig, AppConfig, AuthConfig, DatabaseConfig, LoggingConfig, SecurityConfig, ServerConfig, TlsConfig}, state::AppState};

fn test_config() -> AppConfig {
    AppConfig {
        server: ServerConfig { host: "127.0.0.1".to_string(), port: 0 },
        api: ApiConfig { base_url: "https://chatgpt.com/backend-api".to_string() },
        auth: AuthConfig { refresh_margin_seconds: 300, refresh_enabled: true, refresh_concurrency: 2 },
        database: DatabaseConfig { url: "sqlite://:memory:".to_string() },
        security: SecurityConfig {
            master_key_file: "data/master.key".to_string(),
            api_key_pepper_file: "data/api-key-pepper.key".to_string(),
        },
        tls: TlsConfig { force_http11: false },
        admin: AdminConfig { session_ttl_minutes: 1440 },
        logging: LoggingConfig { directory: "logs".to_string(), max_file_bytes: 10_485_760, retention_days: 14 },
    }
}

#[tokio::test]
async fn v1_requires_client_api_key_not_admin_cookie() {
    let app = build_router(AppState::new(test_config()));
    let response = app
        .oneshot(Request::builder().method("POST").uri("/v1/responses").body(Body::from("{}")).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
```

- [x] **Step 2: Run test and verify RED**

Run:

```bash
cargo test v1_requires_client_api_key_not_admin_cookie
```

Expected: compile failure because app/http modules do not exist.

- [x] **Step 3: Implement minimal router**

Modify `/home/zyy/桌面/Codes/codex-proxy-rs/src/lib.rs`:

```rust
pub mod accounts;
pub mod app;
pub mod auth;
pub mod codex;
pub mod config;
pub mod cookies;
pub mod crypto;
pub mod error;
pub mod fingerprint;
pub mod http;
pub mod logs;
pub mod pagination;
pub mod state;
pub mod storage;
pub mod translation;
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/app.rs`:

```rust
use axum::{routing::{get, post}, Router};

use crate::{http::{health::health, v1::responses}, state::AppState};

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/responses", post(responses))
        .route("/v1/chat/completions", post(responses))
        .with_state(state)
}
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/http/mod.rs`:

```rust
pub mod admin;
pub mod auth;
pub mod health;
pub mod middleware;
pub mod v1;
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/http/health.rs`:

```rust
use axum::Json;
use serde_json::{json, Value};

pub async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}
```

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/http/v1.rs`:

```rust
use axum::{http::StatusCode, response::IntoResponse, Json};

use crate::translation::codex_to_openai::openai_error;

pub async fn responses() -> impl IntoResponse {
    (
        StatusCode::UNAUTHORIZED,
        Json(openai_error("Missing client API key", "invalid_api_key")),
    )
}
```

Create empty route module files:

```bash
touch src/http/admin.rs src/http/auth.rs src/http/middleware.rs
```

Modify `/home/zyy/桌面/Codes/codex-proxy-rs/src/main.rs`:

```rust
use codex_proxy_rs::{app::build_router, config::{AdminConfig, ApiConfig, AppConfig, AuthConfig, DatabaseConfig, LoggingConfig, SecurityConfig, ServerConfig, TlsConfig}, state::AppState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = AppConfig {
        server: ServerConfig { host: "127.0.0.1".to_string(), port: 8080 },
        api: ApiConfig { base_url: "https://chatgpt.com/backend-api".to_string() },
        auth: AuthConfig { refresh_margin_seconds: 300, refresh_enabled: true, refresh_concurrency: 2 },
        database: DatabaseConfig { url: "sqlite://data/codex-proxy-rs.sqlite".to_string() },
        security: SecurityConfig {
            master_key_file: "data/master.key".to_string(),
            api_key_pepper_file: "data/api-key-pepper.key".to_string(),
        },
        tls: TlsConfig { force_http11: false },
        admin: AdminConfig { session_ttl_minutes: 1440 },
        logging: LoggingConfig { directory: "logs".to_string(), max_file_bytes: 10_485_760, retention_days: 14 },
    };
    let app = build_router(AppState::new(config.clone()));
    let listener = tokio::net::TcpListener::bind((config.server.host.as_str(), config.server.port)).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
```

- [x] **Step 4: Verify route boundary**

Run:

```bash
cargo test v1_requires_client_api_key_not_admin_cookie
```

Expected: pass.

- [x] **Step 5: Commit router**

```bash
git add src/app.rs src/http src/main.rs src/lib.rs tests/routes_responses_test.rs
git commit -m "feat: add http router auth boundaries"
```

---

### Task 11: Codex Upstream Request Lifecycle

**Files:**
- Modify: `/home/zyy/桌面/Codes/codex-proxy-rs/src/codex/client.rs`
- Modify: `/home/zyy/桌面/Codes/codex-proxy-rs/src/codex/sse.rs`
- Modify: `/home/zyy/桌面/Codes/codex-proxy-rs/src/http/v1.rs`
- Modify: `/home/zyy/桌面/Codes/codex-proxy-rs/src/state.rs`
- Test: `/home/zyy/桌面/Codes/codex-proxy-rs/tests/routes_chat_test.rs`
- Test: `/home/zyy/桌面/Codes/codex-proxy-rs/tests/routes_responses_test.rs`

- [x] **Step 1: Add failing upstream mock test**

Append to `/home/zyy/桌面/Codes/codex-proxy-rs/tests/routes_responses_test.rs`:

```rust
#[tokio::test]
async fn responses_route_rejects_non_codex_provider_models() {
    let app = build_router(AppState::new(test_config()));
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("authorization", "Bearer cpr_test")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"model":"claude-3","input":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
```

- [x] **Step 2: Run test and verify RED**

Run:

```bash
cargo test responses_route_rejects_non_codex_provider_models
```

Expected: fails with `401 Unauthorized`, because client API key validation and model validation are not implemented.

- [x] **Step 3: Implement model allow rule**

Modify `/home/zyy/桌面/Codes/codex-proxy-rs/src/http/v1.rs`:

```rust
use axum::{http::{HeaderMap, StatusCode}, response::IntoResponse, Json};
use serde::Deserialize;

use crate::translation::codex_to_openai::openai_error;

#[derive(Deserialize)]
struct ResponsesBody {
    model: Option<String>,
}

pub async fn responses(headers: HeaderMap, Json(body): Json<ResponsesBody>) -> impl IntoResponse {
    let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or_default();
    if !auth.starts_with("Bearer cpr_") {
        return (
            StatusCode::UNAUTHORIZED,
            Json(openai_error("Missing client API key", "invalid_api_key")),
        );
    }
    let model = body.model.unwrap_or_else(|| "gpt-5.4".to_string());
    if !(model.starts_with("gpt") || model.starts_with("codex") || model.starts_with("o")) {
        return (
            StatusCode::NOT_FOUND,
            Json(openai_error("Model not found", "model_not_found")),
        );
    }
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(openai_error("No available Codex accounts", "no_available_accounts")),
    )
}
```

- [x] **Step 4: Verify model rejection**

Run:

```bash
cargo test responses_route_rejects_non_codex_provider_models
```

Expected: pass.

- [x] **Step 5: Commit upstream lifecycle boundary**

```bash
git add src/codex src/http/v1.rs src/state.rs tests/routes_chat_test.rs tests/routes_responses_test.rs
git commit -m "feat: enforce codex-only upstream routing"
```

---

### Task 12: Fingerprint Auto-Update

**Files:**
- Modify: `/home/zyy/桌面/Codes/codex-proxy-rs/src/fingerprint/updater.rs`
- Modify: `/home/zyy/桌面/Codes/codex-proxy-rs/src/fingerprint/model.rs`
- Test: `/home/zyy/桌面/Codes/codex-proxy-rs/tests/codex_headers_test.rs`

- [x] **Step 1: Add failing updater parse test**

Append to `/home/zyy/桌面/Codes/codex-proxy-rs/tests/codex_headers_test.rs`:

```rust
use codex_proxy_rs::fingerprint::updater::parse_update_manifest;

#[test]
fn update_manifest_updates_app_version_and_build_number() {
    let manifest = r#"{"version":"26.600.12345","build_number":"4001"}"#;
    let update = parse_update_manifest(manifest).unwrap();
    assert_eq!(update.app_version, "26.600.12345");
    assert_eq!(update.build_number, "4001");
}
```

- [x] **Step 2: Run test and verify RED**

Run:

```bash
cargo test update_manifest_updates_app_version_and_build_number
```

Expected: compile failure because `parse_update_manifest` does not exist.

- [x] **Step 3: Implement updater parser**

Create `/home/zyy/桌面/Codes/codex-proxy-rs/src/fingerprint/updater.rs`:

```rust
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FingerprintUpdate {
    pub app_version: String,
    pub build_number: String,
}

#[derive(Debug, Error)]
pub enum FingerprintError {
    #[error("invalid update manifest: {0}")]
    InvalidManifest(#[from] serde_json::Error),
}

#[derive(Deserialize)]
struct Manifest {
    version: String,
    build_number: String,
}

pub fn parse_update_manifest(input: &str) -> Result<FingerprintUpdate, FingerprintError> {
    let manifest: Manifest = serde_json::from_str(input)?;
    Ok(FingerprintUpdate {
        app_version: manifest.version,
        build_number: manifest.build_number,
    })
}
```

- [x] **Step 4: Verify updater parser**

Run:

```bash
cargo test update_manifest_updates_app_version_and_build_number
```

Expected: pass.

- [x] **Step 5: Commit fingerprint updater parser**

```bash
git add src/fingerprint/updater.rs tests/codex_headers_test.rs
git commit -m "feat: parse codex desktop fingerprint updates"
```

---

### Task 13: Documentation and Runtime Verification

**Files:**
- Modify: `/home/zyy/桌面/Codes/codex-proxy-rs/README.md`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/.env.example`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/Dockerfile`
- Create: `/home/zyy/桌面/Codes/codex-proxy-rs/docker-compose.yml`

- [x] **Step 1: Write README with exact scope**

Create `/home/zyy/桌面/Codes/codex-proxy-rs/README.md`:

```markdown
# codex-proxy-rs

Rust rewrite of Codex Proxy focused only on ChatGPT/Codex accounts and the Codex backend.

## Included

- `/v1/chat/completions`
- `/v1/responses`
- `/v1/models`
- ChatGPT/Codex OAuth and refresh-token based account pool
- Codex Desktop-style TLS, headers, Cookies, and fingerprint updates
- SQLite storage with sqlx-managed schema
- Structured logs with pagination and file rotation

## Excluded

- OpenAI official API Key upstream
- Anthropic, Gemini, custom providers, OpenRouter-style routing
- Ollama bridge
- Per-account proxy assignment
- Electron app
- Legacy compatibility layers and old-project data migration scripts

## Auth Model

- Admin login uses admin password and HttpOnly session Cookie.
- Client API keys call `/v1/*` and start with `cpr_`.
- Codex account tokens are internal upstream credentials.
```

- [x] **Step 2: Add environment example**

Create `/home/zyy/桌面/Codes/codex-proxy-rs/.env.example`:

```dotenv
CPRS_HOST=127.0.0.1
CPRS_PORT=8080
CPRS_DATABASE_URL=sqlite://data/codex-proxy-rs.sqlite
CPRS_MASTER_KEY_FILE=data/master.key
CPRS_API_KEY_PEPPER_FILE=data/api-key-pepper.key
RUST_LOG=codex_proxy_rs=info,tower_http=info
```

- [x] **Step 3: Verify full test suite and lint**

Run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test
```

Expected: all commands pass.

- [x] **Step 4: Run local server smoke test**

Run:

```bash
cargo run
curl -s http://127.0.0.1:8080/health
```

Expected response:

```json
{"status":"ok"}
```

- [x] **Step 5: Commit docs and runtime files**

```bash
git add README.md .env.example Dockerfile docker-compose.yml
git commit -m "docs: document codex-proxy-rs scope"
```

---

## Audit Follow-ups

- [x] Add `POST /admin/login` so admin login is a password-backed session flow, not a client API key flow. The route issues an HttpOnly `cpr_admin_session` cookie, returns the admin envelope with lower camelCase `expiresAt`, and rejects `Bearer cpr_...` as admin credentials.
- [x] Replace the refresh policy shell with a native refresh scheduler. It refreshes before expiry, refreshes immediately after an upstream 401 trigger, preserves the existing refresh token when the refresh response omits one, maps refresh failures to account statuses, and enforces configured concurrency with a semaphore.
- [x] Fill the previously empty `src/codex/sse.rs` and `src/codex/usage.rs` modules with tested SSE frame parsing/encoding and token usage extraction helpers.
- [x] Add a tested SQLite account repository foundation. It encrypts access/refresh tokens, decrypts account reads, cursor-pages accounts, updates status/labels without rewriting token ciphertext, persists usage counters, and preserves the existing refresh token when token refresh responses omit a new one.
- [x] Add a real HTTP auth boundary. `src/http/auth.rs` parses client API keys and admin session cookies as separate credential types, `/v1/*` uses only `Bearer cpr_...`, and admin session parsing ignores client API keys.
- [x] Add a tested Codex WebSocket boundary. `src/codex/websocket.rs` classifies HTTP SSE versus WebSocket-only requests, adds `previous_response_id`/`use_websocket` request fields, and returns typed errors instead of silently downgrading WebSocket-required traffic.

## 2026-06-11 Original Project Gap Audit

The Rust project has no legacy compatibility requirement, but the in-scope behavior must still be rebuilt natively. This audit compares the TypeScript project under `/home/zyy/Codes/codex-proxy` with the Rust rewrite and corrects earlier over-broad completion claims.

### Keep and Rebuild Natively

- [ ] **HTTP auth boundary:** restore a real `src/http/auth.rs` module instead of scattered header checks. It must validate `Bearer cpr_...` client API keys for `/v1/*`, validate admin session cookies for `/admin/*`, reject cross-use in both directions, and return the documented OpenAI/admin response families.
- [ ] **Account SQLite repository:** implement `src/accounts/repository.rs` for encrypted access/refresh tokens, account metadata, quota JSON, usage counters, labels, status transitions, pagination, and batch-safe mutations. Do not add TypeScript JSON migration or legacy import shims.
- [ ] **Admin account routes:** rebuild `/auth/status`, `/auth/accounts`, import/export, batch delete/status, health-check, per-account refresh, label, delete, reset usage, quota, Cookie CRUD, and quota warnings as `/admin/*` endpoints with admin envelopes and cursor pagination where a list is returned.
- [ ] **Login routes:** implement OAuth PKCE login-start/code-relay/callback, device-login/device-poll, CLI auth import, manual access-token import, and logout as account-login flows. Keep this separate from admin password login and client API key creation.
- [ ] **Client API key management:** implement admin CRUD for local client API keys: list/create/delete/batch-delete/label/status/export/import. Remove provider/model binding semantics from the TypeScript version; Rust client API keys authorize only local `/v1/*` access.
- [ ] **Settings:** implement admin settings for server/runtime fields that remain in scope: default model, reasoning effort, service tier, model aliases, refresh enabled/margin/concurrency, max concurrent per account, request interval, rotation strategy, tier priority, quota refresh/thresholds/skip exhausted, logs state/capacity/body capture, and usage history retention. Remove proxy URL, Ollama, third-party provider, OpenAI official key, and Electron/self-update settings.
- [ ] **Diagnostics:** expand health/admin diagnostics to include authenticated state, pool summary, capacity summary, transport/fingerprint status, paths, runtime metadata, and test-connection checks. Keep production-local gating for sensitive debug endpoints.
- [ ] **Logs and usage stats:** add `/admin/logs/state`, clear/detail, error-log equivalents if retained, and `/admin/usage-stats/summary` plus paginated or bounded history queries. Keep log file rotation and SQLite event pagination.
- [ ] **Model catalog:** replace the hard-coded `/v1/models` list with a model store that supports static defaults, configured aliases, `-low/-medium/-high/-xhigh/-fast/-flex` suffix parsing, backend model fetching per plan, cache persistence, `/v1/models/catalog`, `/v1/models/{id}`, `/v1/models/{id}/info`, `/admin/refresh-models`, and `/debug/models`.
- [ ] **Account scheduling parity:** extend account acquisition beyond `active + least last_used`: max concurrent slots per account, stale slot cleanup, least-used/round-robin/sticky strategies, quota-cache skip, tier priority, model-plan filtering, exclude IDs, preferred account from session affinity, Cloudflare cooldown, request staggering, and release-time usage accounting.
- [ ] **Upstream Codex lifecycle:** implement account acquire -> Codex request -> SSE/collect -> rate-limit capture -> usage release -> retry/fallback. Include Cookie replay/capture, Codex Desktop headers, `x-codex-turn-state`, `x-client-request-id`, upstream error classification, 401-triggered refresh, 429 quota cache, 402 quota exhausted, 403 ban/Cloudflare distinction, path-block Cookie clearing, and OpenAI-compatible error mapping.
- [ ] **Session affinity and previous response:** persist an in-memory response-id to account mapping with TTL, prompt-cache/conversation identity, turn-state replay, and preferred account selection. `src/codex/websocket.rs` must either implement the WebSocket path with verified TLS/header behavior or the API must reject WebSocket-only cases explicitly with documented limitations until fingerprint parity is proven.
- [ ] **Fingerprint auto-update parity:** poll the real Codex Desktop appcast/update source, parse app version/build number, persist update state, select the latest stored fingerprint for runtime headers, and optionally extract Chromium version from a local Codex.app path when configured.

### Keep Removed

- [ ] Do not rebuild per-account proxy assignment, proxy pool, proxy health checks, or proxy import/export.
- [ ] Do not rebuild Ollama bridge/settings.
- [ ] Do not rebuild Anthropic, Gemini, OpenRouter/custom provider routing, or OpenAI official API key direct upstream.
- [ ] Do not rebuild Electron packaging, large dashboard static UI, or proxy self-update installer flows.
- [ ] Do not add old TypeScript data migration, deprecated API compatibility adapters, or dual-mode legacy behavior.

### Original Bugs or Risky Behavior to Correct During Rebuild

- [ ] Old `/admin/*` settings mutations used the proxy API key as a write gate. Rust must use admin sessions only.
- [ ] Old refresh fallback could be dangerous for one-time refresh tokens if retried after a mid-flight failure. Rust must keep the one-time RT preservation rule and retry only when the request definitely did not reach the server.
- [ ] Old settings exposed proxy and provider knobs that are out of scope. Rust config and API must not keep dead settings as no-op compatibility fields.
- [ ] Old route responses mix multiple body shapes. Rust must keep `/v1/*` OpenAI-compatible and `/admin/*` lower camelCase `code/message/data/requestId`.
- [ ] Empty placeholder modules are not acceptable. A module either owns tested behavior or is removed from the target structure with a documented reason.

---

## Acceptance Criteria

- `codex-proxy-rs` builds as a standalone Rust service.
- `cargo test`, `cargo clippy --all-targets --all-features --locked -- -D warnings`, and `cargo fmt --check` pass.
- `Cargo.toml` contains lint settings for Rust and Clippy; task commits do not silence lint warnings with broad `allow` attributes.
- Non-fingerprint dependencies are resolved from current stable crate releases at implementation time and `Cargo.lock` is committed.
- `reqwest` and `rustls` remain explicitly pinned until a separate TLS fingerprint review approves newer versions.
- Library modules use `thiserror` error enums. `anyhow` is limited to `src/main.rs`, tests, and setup utilities.
- Runtime config loads from `config/default.yaml`, optional local config, and `CPRS_*` environment variables.
- In-memory upstream secrets use `secrecy` types, and persisted access tokens, refresh tokens, Cookies, master key material, and API key pepper material are encrypted or stored outside plaintext database fields.
- `/v1/*` and `/admin/*` response envelopes and status codes match `docs/api.md` and `docs/status-codes.md`.
- `/v1/*` never uses the admin/frontend `code/message/data/requestId` envelope; it keeps OpenAI-compatible response and error bodies.
- `/admin/*` uses accurate HTTP status codes plus body-level frontend codes, with lower camelCase JSON fields and no PascalCase/snake_case JSON keys.
- All responses include `X-Request-Id`; admin/frontend JSON bodies include `requestId` except `204 No Content`.
- Each completed task updates both the plan checkbox state and `docs/implementation-status.md` before commit.
- Any API/status-code change updates `docs/api.md` or `docs/status-codes.md` in the same commit.
- Any dependency change updates `docs/dependency-policy.md` in the same commit.
- No OpenAI official API Key direct upstream exists in code, config, routes, docs, or tests.
- No Anthropic, Gemini, custom provider, Ollama, Electron, or per-account proxy assignment modules exist.
- `/v1/*` accepts only client API keys with `cpr_` shape and never accepts admin session cookies.
- `/admin/*` accepts only admin session cookies and never accepts client API keys.
- Codex upstream request code uses pinned `reqwest = 0.12.28` and `rustls = 0.23.36`.
- Codex headers include Codex Desktop identity, authorization, account id when available, turn-state when available, and request id.
- Cloudflare Cookies are captured from upstream `Set-Cookie`, encrypted at rest, stored per account, and replayed only for the same account.
- Fingerprint update logic can parse a Codex Desktop update manifest and persist app version/build number history.
- SQLite is the only database. The Rust service's own sqlx schema version files create accounts, client API keys, admin users, sessions, cookies, fingerprints, and event logs; no old-project data migration code is included.
- Log files rotate by date/size policy, and event query APIs are cursor paginated with a max limit of 200.
- Account refresh behavior is implemented natively with the required refresh semantics: refresh before expiry, immediate refresh after 401, refresh-token preservation, status transitions for expired/quota/banned/disabled, and bounded concurrency.
- Important protocol, security, and business invariants have sparse Chinese comments; obvious code is left uncommented.

## Self-Review Notes

- Spec coverage: the plan covers Rust rewrite, removed channels, TLS fingerprint, headers, Cookies, fingerprint update, auth split, logging, pagination, SQLite storage, API/status-code contracts, frontend body codes, JSON casing, request IDs, Chinese comment policy, dependency policy, documentation completion tracking, account refresh behavior, and the ban on legacy compatibility/data-migration code.
- Rust best-practices coverage: the revised plan adds lint gates, `thiserror` library errors, binary-only `anyhow`, `secrecy` secret handling, HMAC API key hashing, SQLite URL configuration, and an `Arc<AppServices>` runtime state boundary.
- Open item scan: no open-ended implementation gaps are intentionally left in task steps.
- Type consistency: the early scaffold uses `AppConfig`, `AppState`, `AccountStatus`, `EventLog`, `Fingerprint`, and `CodexResponsesRequest` consistently across later tasks.
