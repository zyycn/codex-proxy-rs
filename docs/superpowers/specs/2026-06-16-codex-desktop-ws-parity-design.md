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
- 2026-06-17: Resumed implementation work from the durable design doc. CodeGraph reports the index is up to date with 285 files, 3,933 nodes, and 12,834 edges. Next step is to write the implementation plan before changing production code.
- 2026-06-17: Implementation plan written at `docs/superpowers/plans/2026-06-17-codex-desktop-ws-parity.md`. `git diff --check` passed for the documentation checkpoint.
- 2026-06-17: Documentation checkpoint committed as `0dbcbc9 docs: plan codex desktop websocket parity`. Task 2 is starting with a failing test for redacted WebSocket opening audit snapshots.
- 2026-06-17: Task 2 implementation completed locally. Added redacted opening audit snapshot API, preserved existing opening byte behavior, and passed focused WebSocket tests plus targeted Clippy.
- 2026-06-17: Task 2 committed as `4bc1b6d feat: add websocket opening audit snapshot`. Task 3 is starting with CodeGraph inspection of the `response.create` payload path.
- 2026-06-17: Task 3 implementation completed locally. Added redacted payload audit snapshot API and passed focused payload/WebSocket tests plus targeted Clippy.
- 2026-06-17: Task 3 committed as `0e56cb1 feat: add websocket payload audit snapshot`. Task 4 is starting with inspection of existing diagnostics/config patterns for an explicit audit artifact gate.
- 2026-06-17: Task 4 implementation completed locally. Added `CODEX_PROXY_WS_AUDIT_DIR` gated JSON artifact writing for redacted rs WebSocket opening/payload attempts and ignored `.codex-ws-audit/`.
- 2026-06-17: Task 4 committed as `e869260 feat: write redacted websocket audit artifacts`. Task 5 is starting with a local capture harness for actual rs opening bytes and first WebSocket frame.
- 2026-06-17: Task 5 implementation completed locally. Added a local WebSocket capture harness test and generated redacted rs sample artifact at `.codex-ws-audit/rs-current-capture.json`.
- 2026-06-17: Task 5 committed as `95221cf test: add websocket capture harness`. Task 6 is starting with official bundled `codex` binary existence/help/config inspection.
- 2026-06-17: Task 6 inspection completed locally. Official bundled binary exists but is macOS arm64 Mach-O and cannot be executed on this Linux host; static evidence was extracted with `strings` instead.
- 2026-06-17: Task 6 committed as `55ec710 docs: record official codex websocket sampling`. Task 7 is starting with a structured parity diff helper for redacted audit artifacts.
- 2026-06-17: Task 7 implementation completed locally. Added structured `websocket_parity_diff` support for redacted audit artifacts and classified current rs versus official static evidence.
- 2026-06-17: Task 7 committed as `4fdbd45 feat: add websocket parity diff report`. Task 8 is starting to enable `reqwest` `http2` for HTTP SSE fallback parity.
- 2026-06-17: Task 8 committed as `e96cab2 fix: enable http2 for codex fallback client`. Task 9 is starting with CodeGraph-assisted review of WebSocket opening/payload evidence.
- 2026-06-17: Task 9 committed as `e4ffdd5 fix: align codex desktop websocket opening`. Task 10 is starting with official-source downgrade evidence review.

## Analysis Journal

### 2026-06-16 Initial Chain Assessment

The earlier Electron `net.fetch` fingerprint remains useful for Desktop HTTP fetches but is not the target for model Responses parity. The official bundled `codex` binary includes `responses_websocket`, `tokio-tungstenite`, and `rustls`, which makes it the correct target for `/v1/responses` WebSocket parity.

Current `codex-proxy-rs` already follows the same broad transport choice: Responses defaults to WebSocket, `previous_response_id` forces WebSocket, and only preferred WebSocket requests may fall back to HTTP SSE. The remaining work is not to change the high-level direction; it is to prove and align the low-level opening, TLS, payload, and downgrade details.

### 2026-06-16 Current Known Gaps

The HTTP fallback path lacks `reqwest` `http2`, so fallback does not yet match the bundled Rust HTTP stack closely enough. The WebSocket path uses `rustls` with the `ring` provider, while the official binary contains both `aws-lc-rs` and `ring` symbols; the active provider must be verified by observation or stronger binary evidence. Header and payload order are currently modeled from local assumptions and must be compared against official samples.

### 2026-06-17 Resume Notes

Command/context touched: loaded the Rust best-practices guidance, read this design doc, checked an existing implementation-plan format, and ran `codegraph status .`.

Observed result: no production implementation exists yet for the audit harness, official sampling, rs sampling, or parity diff. The repository status was clean at resume time.

Next decision: create `docs/superpowers/plans/2026-06-17-codex-desktop-ws-parity.md` with test-first implementation tasks and use it as the execution checklist.

Verification status: documentation-only checkpoint; no Rust tests required yet.

### 2026-06-17 Implementation Plan Checkpoint

Command/code area touched: created the implementation plan and ran `git diff --check`.

Observed result: the plan now defines the audit-first sequence, including redacted WebSocket opening snapshots, payload snapshots, local rs capture, official bundled binary sampling, structured parity diffing, and evidence-backed behavior changes. Whitespace verification passed.

Next decision: commit the documentation checkpoint, then start Task 2 by writing a failing test for redacted WebSocket opening audit snapshots.

Verification status: `git diff --check` passed; no production Rust code has changed.

### 2026-06-17 Task 2 Start

Command/code area touched: will edit `src/codex/gateway/transport/websocket/opening.rs` and focused WebSocket tests to expose an audit-only snapshot of the opening request.

Observed result: current opening logic serializes request bytes privately through `opening_request_bytes`, with fixed WebSocket headers, `Sec-WebSocket-Key`, and ordered business headers. No redacted structured audit representation exists yet.

Next decision: add a failing test first for header order preservation and sensitive header redaction, then implement the smallest audit helper without changing emitted opening bytes.

Verification status: pending focused test failure.

### 2026-06-17 Task 2 Failing Test

Command/code area touched: added `websocket_opening_audit_snapshot_should_redact_sensitive_headers` to `tests/codex_gateway/websocket.rs` and ran `cargo test --test codex_gateway websocket_opening_audit_snapshot_should_redact_sensitive_headers`.

Observed result: the test failed to compile with unresolved import `websocket_opening_audit_snapshot`, which is the expected red state for the new audit API.

Next decision: implement `OpeningAuditSnapshot` and `websocket_opening_audit_snapshot` near `opening_request_bytes`, sharing the same ordered header source used by actual opening byte serialization.

Verification status: expected failing test captured.

### 2026-06-17 Task 7 Implementation Result

Command/code area touched: modified `src/codex/gateway/transport/websocket/audit.rs`, `src/codex/gateway/transport/websocket/mod.rs`, and `tests/codex_gateway/websocket.rs`.

Observed result: `websocket_parity_diff` now compares redacted audit artifacts structurally and reports differences as JSON values with stable paths. Covered paths include `transport_mode`, `fallback_allowed`, `opening.request_line`, `opening.header_order`, `opening.sec_websocket_extensions`, `payload.top_level_keys`, and `error.classification`. The focused test verifies that opening header order changes are reported without string-only artifact comparison.

Current classification from available evidence:

- `already compatible`: current rs and official static evidence both target the ChatGPT/Codex Responses WebSocket path. Official strings include `https://chatgpt.com/backend-api/codex`, `responses_websocket`, `responses_websockets=2026-02-06`, and `permessage-deflate`; current rs sample uses `GET /codex/responses HTTP/1.1`, `OpenAI-Beta: responses_websockets=2026-02-06`, and `Sec-WebSocket-Extensions: permessage-deflate; client_max_window_bits`.
- `already compatible`: current rs WebSocket stack dependency versions match the official static evidence for the critical Rust layer: `tokio-rustls-0.26.4`, `rustls-0.23.36`, and `tokio-tungstenite`.
- `observe more`: exact official opening header order/casing, dynamic `Sec-WebSocket-Key` placement, first-frame JSON key order, TLS ClientHello/ALPN/cipher ordering, and terminal/error/fallback edge cases still require an official live artifact or a macOS execution environment.
- `must change`: HTTP SSE fallback still lacks the `reqwest` `http2` feature in current rs, while the official binary contains `hyper-rustls-0.27.7` and `reqwest-0.12.28`/`0.13.4`; Task 8 will make the minimal fallback dependency parity change and verify focused fallback tests.

Next decision: commit Task 7, then start Task 8 by enabling `reqwest` `http2` for the HTTP SSE fallback path with tests first or focused fallback verification.

Verification status: `cargo test --test codex_gateway websocket_parity_diff_should_report_header_order_changes`, `cargo test --test codex_gateway websocket`, `cargo clippy --test codex_gateway --locked -- -D warnings`, and `cargo fmt --all --check` passed. Full official artifact comparison remains unavailable on this Linux host because the official target is macOS arm64 Mach-O.

### 2026-06-17 Task 8 Start

Command/code area touched: will inspect `Cargo.toml`, `src/codex/gateway/transport/http_client.rs`, and focused HTTP/fallback tests before changing dependency features.

Observed result: Task 7 classified fallback dependency parity as `must change`: current rs uses `reqwest =0.12.28` without the `http2` feature, while official static evidence includes `hyper-rustls-0.27.7` and `reqwest-0.12.28`/`0.13.4`.

Next decision: enable the `reqwest` `http2` feature with minimal dependency churn, then run focused HTTP client and serving fallback/SSE tests.

Verification status: pending dependency inspection and focused tests.

### 2026-06-17 Task 8 TDD Setup

Command/code area touched: inspected `Cargo.toml`, `src/codex/gateway/transport/http_client.rs`, and existing focused tests under `tests/codex_gateway/http_client.rs`, `tests/codex_serving/responses_http_sse.rs`, and `tests/codex_serving/upstream_fallback.rs`.

Observed result: `build_reqwest_client(false)` currently leaves protocol negotiation to `reqwest`, and `build_reqwest_client(true)` remains explicitly `http1_only()` for tests that capture raw HTTP/1.1 header order. The missing parity item is the disabled `reqwest` `http2` feature, not a client-construction option change.

Next decision: add a focused compile-time test that references a `reqwest` HTTP/2-only builder API before enabling the feature. The expected red state is a compile error showing that the method is unavailable without `reqwest/http2`.

Verification status: red test confirmed with `cargo test --test codex_gateway reqwest_http2_feature_should_be_enabled_for_fallback_parity`; compile failed with `E0599` because `ClientBuilder::http2_prior_knowledge` is unavailable.

### 2026-06-17 Task 8 Feature Change

Command/code area touched: will update the `reqwest` dependency feature list in `Cargo.toml`.

Observed result: the red test proves current dependency features do not expose HTTP/2-specific builder APIs. Existing fallback request behavior is covered by `tests/codex_gateway/http_client.rs`, `tests/codex_serving/responses_http_sse.rs`, and `tests/codex_serving/upstream_fallback.rs`.

Next decision: add `"http2"` to `reqwest` features only; do not force HTTP/2 in `build_reqwest_client(false)` and do not alter `build_reqwest_client(true)` because raw header-order tests intentionally require HTTP/1.1.

Verification status: `cargo test --test codex_gateway reqwest_http2_feature_should_be_enabled_for_fallback_parity` passed after the feature change; `Cargo.lock` only added `h2` to the `reqwest` package dependencies; `cargo check --locked` passed.

### 2026-06-17 Task 8 Focused Verification Start

Command/code area touched: will run focused HTTP client and serving fallback suites after enabling `reqwest/http2`.

Observed result: the parity change is currently limited to `Cargo.toml`, `Cargo.lock`, and the new feature exposure test in `tests/codex_gateway/http_client.rs`.

Next decision: run `cargo test --test codex_gateway http_client`, `cargo test --test codex_serving responses_http_sse`, and `cargo test --test codex_serving upstream_fallback`, followed by formatting/lint checks.

Verification status: focused suites passed:
- `cargo test --test codex_gateway http_client`: 8 passed.
- `cargo test --test codex_serving responses_http_sse`: 28 passed.
- `cargo test --test codex_serving upstream_fallback`: 27 passed.

Next decision: run `cargo fmt --all --check`, `cargo clippy --test codex_gateway --locked -- -D warnings`, and `git diff --check`, then commit Task 8 if clean.

### 2026-06-17 Task 8 Final Verification

Command/code area touched: finalized `Cargo.toml`, `Cargo.lock`, `tests/codex_gateway/http_client.rs`, and Task 8 documentation.

Observed result: `reqwest =0.12.28` now enables `"http2"` with `default-features = false`. `Cargo.lock` only records the resulting `h2` dependency under `reqwest`. The fallback client construction remains unchanged: normal fallback can negotiate protocol through `reqwest`, while explicit `force_http11` test paths still call `http1_only()`.

Next decision: commit Task 8 as `fix: enable http2 for codex fallback client`, then continue Task 9 without changing WebSocket opening/payload behavior unless stronger official evidence is available.

Verification status: passed `cargo test --test codex_gateway reqwest_http2_feature_should_be_enabled_for_fallback_parity`, `cargo check --locked`, `cargo test --test codex_gateway http_client`, `cargo test --test codex_serving responses_http_sse`, `cargo test --test codex_serving upstream_fallback`, `cargo fmt --all --check`, `cargo clippy --test codex_gateway --locked -- -D warnings`, and `git diff --check`.

### 2026-06-17 Task 9 Start

Command/code area touched: will initialize/sync CodeGraph and inspect WebSocket opening/payload code plus current redacted audit artifact before making any behavior change.

Observed result: Task 7's parity classification left WebSocket opening and payload diffs in `observe more`, not `must change`, because official live opening bytes and first-frame payload are unavailable on this Linux host. Task 8 handled the only current `must change` item: HTTP SSE fallback `reqwest/http2`.

Next decision: use CodeGraph to map the WebSocket opening/payload call path, re-run the local audit capture, and only modify `src/codex/gateway/transport/websocket/*` if a supported `must change` diff is found. If no such diff exists, close Task 9 with a documented no-op decision instead of inventing parity changes.

Verification status: pending CodeGraph index and local audit refresh.

### 2026-06-17 Task 9 Payload Evidence

Command/code area touched: ran CodeGraph exploration for `websocket_request_body`, `connect_with_original_opening_handshake`, and `websocket_parity_diff`; refreshed `.codex-ws-audit/rs-current-capture.json` with `cargo test --test codex_gateway websocket_capture_harness_should_record_opening_bytes`; fetched official `openai/codex` source files `codex-api/src/endpoint/responses_websocket.rs`, `codex-api/src/common.rs`, and `core/src/client.rs`.

Observed result: official source path and symbols match the bundled binary evidence (`codex-api/src/endpoint/responses_websocket.rs`, `responses_websocket`, `response_create_client_metadata`, `build_ws_client_metadata`). Official `ResponsesWebsocketConnection::stream_request` serializes the first frame with `serde_json::to_string(ResponsesWsRequest)`, where `ResponsesWsRequest` is `#[serde(tag = "type")]` and `ResponseCreateWsRequest` field order is `model`, `instructions`, `previous_response_id`, `input`, `tools`, `tool_choice`, `parallel_tool_calls`, `reasoning`, `store`, `stream`, `include`, `service_tier`, `prompt_cache_key`, `text`, `generate`, `client_metadata`. Current rs builds a `serde_json::Value` object and sends `payload.to_string()`, which sorts object keys under the current `serde_json` configuration and omits default empty/null fields such as `tools`, `reasoning`, and `include`.

Next decision: classify payload wire serialization as `must change`; add a failing test that asserts official first-frame key order and default field presence before changing `src/codex/gateway/transport/websocket/codec.rs`.

Verification status: red tests confirmed:
- `cargo test --test codex_gateway websocket_payload_audit_snapshot_should_redact_user_content` failed because current `top_level_keys` are sorted as `serde_json::Value` object keys instead of official struct order.
- `cargo test --test codex_gateway websocket_capture_harness_should_record_opening_bytes` failed because the current wire payload omits default `tools: []`, `reasoning: null`, and `include: []` fields.

### 2026-06-17 Task 9 Payload Implementation Start

Command/code area touched: will modify `src/codex/gateway/transport/websocket/codec.rs` and `src/codex/gateway/transport/websocket/mod.rs`.

Observed result: the opening handshake remains supported by existing source/static evidence, but first-frame payload serialization has a concrete official-source delta.

Next decision: add an internal serializable `ResponsesWsRequest`/`ResponseCreateWsRequest` shape matching official field order, send `serde_json::to_string(...)` output on the WebSocket, and keep audit redaction derived from the same shape.

Verification status: implementation completed locally. Added official-order WebSocket payload serialization, switched the send path to the serialized text, updated audit `top_level_keys`, and classified payload encoding failures as non-fallback local errors.

### 2026-06-17 Task 9 Payload Verification

Command/code area touched: modified `src/codex/gateway/transport/websocket/codec.rs`, `src/codex/gateway/transport/websocket/mod.rs`, `src/codex/gateway/transport/websocket/audit.rs`, `src/codex/gateway/transport/http_client.rs`, `tests/codex_gateway/websocket.rs`, and `tests/fixtures/responses/golden/reasoning_replay_request.json`.

Observed result: refreshed `.codex-ws-audit/rs-current-capture.json` now reports first-frame `top_level_keys` in official order and includes default `tools`, `reasoning`, and `include` fields. Opening bytes remain unchanged from the previous local capture.

Next decision: run final formatting/lint checks and commit Task 9 as `fix: align codex desktop websocket opening`, with the note that the actual code change is payload serialization parity; opening remains unchanged because no supported opening `must change` diff was found.

Verification status: passed `cargo test --test codex_gateway websocket_payload_audit_snapshot_should_redact_user_content`, `cargo test --test codex_gateway websocket_capture_harness_should_record_opening_bytes`, `cargo test --test codex_gateway websocket`, `cargo test --test codex_serving responses_websocket`, and `cargo test --test codex_serving upstream_fallback`.

### 2026-06-17 Task 9 Final Verification

Command/code area touched: finalized WebSocket payload serialization parity and Task 9 documentation.

Observed result: no opening-byte code change was made because official live opening order remains unavailable and no supported `must change` opening diff was found. The implemented delta is first-frame `response.create` wire serialization parity against official source and bundled-binary symbols.

Next decision: commit Task 9, then continue to Task 10 downgrade semantics review.

Verification status: passed `cargo fmt --all --check`, `cargo clippy --test codex_gateway --locked -- -D warnings`, `cargo test --test codex_gateway http_client`, and `git diff --check` in addition to the focused WebSocket/serving suites above.

### 2026-06-17 Task 10 Start

Command/code area touched: inspected current gateway and serving WebSocket fallback classifiers, then fetched official `openai/codex` `core/src/client.rs` downgrade path.

Observed result: official `ModelClientSession::stream_responses_websocket` returns `WebsocketStreamOutcome::FallbackToHttp` only when WebSocket connection setup returns `ApiError::Transport(TransportError::Http { status: StatusCode::UPGRADE_REQUIRED, .. })`. HTTP 401 follows auth recovery, and other connect/stream errors return errors rather than switching to HTTP. This supersedes the earlier plan assumption that generic network/opening failure may fall back.

Current rs difference: `websocket_error_allows_http_sse_fallback` and `websocket_stream_error_allows_http_sse_fallback` allow transport errors, open timeout, empty response, and HTTP 404/405/501 to downgrade to HTTP SSE. That is broader than the official source-backed behavior.

Next decision: add/update failing tests so ordinary WS transport failure does not fall back, HTTP 426 still falls back for preferred non-history requests, `previous_response_id` remains WebSocket-only, and business/quota/auth errors do not trigger HTTP SSE fallback.

Verification status: red/guard tests confirmed:
- `cargo test --test codex_gateway ordinary_response_should_not_fallback_to_http_sse_when_websocket_transport_fails` failed because current rs attempted an HTTP SSE request after a WebSocket transport close, then returned a reqwest connection-refused error.
- `cargo test --test codex_gateway ordinary_response_should_fallback_to_http_sse_when_websocket_upgrade_required` passed, confirming current HTTP 426 downgrade behavior already exists.

### 2026-06-17 Task 10 Fallback Implementation Start

Command/code area touched: will update fallback classifiers in `src/codex/gateway/transport/http_client.rs` and `src/codex/serving/dispatch/mod.rs`.

Observed result: both classifiers currently allow more downgrade cases than official source supports.

Next decision: make HTTP SSE downgrade return true only for upstream HTTP 426 `Upgrade Required`; preserve account-level fallback/retry handling for quota/auth/business errors.

Verification status: implementation completed locally. Gateway and serving streaming fallback classifiers now allow HTTP SSE downgrade only for upstream HTTP 426. HTTP SSE account fallback tests were updated to set `use_websocket: false` explicitly instead of relying on mock WebSocket 404 fallback.

### 2026-06-17 Task 10 Verification

Command/code area touched: modified `src/codex/gateway/transport/http_client.rs`, `src/codex/serving/dispatch/mod.rs`, `tests/codex_gateway/websocket.rs`, and `tests/codex_serving/upstream_fallback.rs`.

Observed result: generic WebSocket transport failure now surfaces as a WebSocket transport error without an HTTP SSE retry; HTTP 426 `Upgrade Required` still downgrades to HTTP SSE for preferred non-history requests. Existing WebSocket business/quota/auth error tests continue to avoid HTTP SSE fallback and use account-level fallback/error handling where applicable.

Next decision: run final formatting/lint checks and commit Task 10 as `fix: align codex desktop websocket fallback`.

Verification status: passed `cargo test --test codex_gateway ordinary_response_should_not_fallback_to_http_sse_when_websocket_transport_fails`, `cargo test --test codex_gateway ordinary_response_should_fallback_to_http_sse_when_websocket_upgrade_required`, `cargo test --test codex_gateway websocket`, `cargo test --test codex_serving responses_websocket`, and `cargo test --test codex_serving upstream_fallback`.

### 2026-06-17 Task 10 Final Verification

Command/code area touched: finalized WebSocket-to-HTTP fallback parity and Task 10 documentation.

Observed result: Task 10 leaves transport selection defaults intact but narrows actual downgrade execution to official-source HTTP 426 only. HTTP SSE account fallback tests now opt into HTTP transport explicitly with `use_websocket: false`.

Next decision: commit Task 10, then run Task 11 final verification and summary.

Verification status: passed `cargo fmt --all --check`, `cargo clippy --test codex_gateway --locked -- -D warnings`, and `git diff --check`.

### 2026-06-17 Task 5 Implementation Result

Command/code area touched: modified `tests/codex_gateway/websocket.rs` to add `capture_codex_websocket_exchange`, a raw local WebSocket capture harness, and `websocket_capture_harness_should_record_opening_bytes`.

Observed result: the harness starts a local TCP WebSocket endpoint, captures the actual HTTP/1.1 opening bytes from `codex-proxy-rs`, completes the WebSocket upgrade manually, decodes the masked first client text frame, sends a terminal `response.completed` frame, and writes `.codex-ws-audit/rs-current-capture.json`. The sample artifact records the request line, ordered headers, `Sec-WebSocket-Extensions`, `Sec-WebSocket-Key` offset, `Authorization` offset, redacted first-frame `response.create` payload shape, and a TLS note stating that local WSS capture is not used because the production client validates against native system roots.

Observed current rs sample: request line is `GET /codex/responses HTTP/1.1`; fixed opening headers appear as `Host`, `Connection`, `Upgrade`, `Sec-WebSocket-Version`, `Sec-WebSocket-Extensions`, `Sec-WebSocket-Key` before business headers; extension offer is `permessage-deflate; client_max_window_bits`; `Sec-WebSocket-Key` appeared before `Authorization`; first frame type is `response.create`.

Next decision: commit Task 5, then start Task 6 by checking the official bundled `codex` binary and looking for a practical local endpoint/proxy override path.

Verification status: `cargo test --test codex_gateway websocket_capture_harness_should_record_opening_bytes`, `cargo test --test codex_gateway websocket`, `cargo clippy --test codex_gateway --locked -- -D warnings`, and `cargo fmt --all --check` passed. The sample artifact was inspected and did not contain private prompt, private instructions, prompt cache key, or thread metadata values.

### 2026-06-17 Task 6 Start

Command/code area touched: will inspect `/tmp/codex-desktop-fingerprint/dmg/Codex Installer/Codex.app/Contents/Resources/codex` with non-mutating commands first.

Observed result: current rs artifact exists locally at `.codex-ws-audit/rs-current-capture.json`, so the next missing reference is the official bundled binary sample or a documented blocker.

Next decision: confirm the binary exists and run help/config discovery without live credentials. Search static strings for API base URL, proxy, and Responses WebSocket override hints before attempting any execution against a local harness.

Verification status: pending official binary inspection.

### 2026-06-17 Task 6 Official Binary Inspection Result

Command/code area touched: inspected `/tmp/codex-desktop-fingerprint/dmg/Codex Installer/Codex.app/Contents/Resources/codex` with `test -x`, `file`, direct `--help`, `sha256sum`, `stat`, and targeted `strings | rg` searches for Responses WebSocket, provider base URL, config keys, proxy, TLS, and dependency evidence.

Observed result: the target binary exists and has executable mode, but `file` reports `Mach-O 64-bit arm64 executable`. Running it on this Linux host fails with `cannot execute binary file: Exec format error`, so direct local harness sampling and local proxy/DNS interception cannot be performed from this environment.

Static official evidence extracted:

- SHA-256: `2bc151dfd04a12dcc13650cb0ed67fb5399ff5250ef8579fcdc482e8b3a6ea5b`.
- Size: `213429648` bytes.
- Target provider/base strings include `https://chatgpt.com/backend-api`, `https://chatgpt.com/backend-api/codex`, `chatgpt_base_url`, `openai_base_url`, and `supports_websockets`.
- Responses WebSocket strings include `codex-api/src/endpoint/responses_websocket.rs`, `responses_websocket`, `responses_websockets=2026-02-06`, `websocket request:`, `failed to build websocket URL:`, and `permessage-deflate`.
- HTTP fallback/dependency strings include `reqwest-0.12.28`, `reqwest-0.13.4`, and `hyper-rustls-0.27.7`.
- TLS/WebSocket dependency strings include `tokio-rustls-0.26.4`, `rustls-0.23.36`, `tokio-tungstenite`, and `tokio_tungstenite`.
- Network/proxy strings include `network-proxy`, `HTTP_PROXY`, `HTTPS_PROXY`, `WSS_PROXY`, `ALL_PROXY`, and `allow_upstream_proxy`, but live validation is blocked by the Mach-O format.

Next decision: commit this official sampling blocker and static evidence, then start Task 7 by adding a structured parity diff helper that can compare the current rs artifact against an official artifact when available, or against this documented static substitute for fields that static evidence can prove.

Verification status: official live sampling is blocked on this Linux host by executable format; static evidence extraction completed and the blocker is documented.

### 2026-06-17 Task 7 Start

Command/code area touched: will add a focused parity diff test and helper around existing `WebSocketAuditArtifact`, `OpeningAuditSnapshot`, and `PayloadAuditSnapshot`.

Observed result: current rs artifact capture and official static evidence are recorded, but there is no structured diff helper yet to classify stable differences. String-only artifact comparison would be too noisy because ports and dynamic WebSocket keys vary.

Next decision: write `websocket_parity_diff_should_report_header_order_changes` first, then implement a structured diff helper that compares transport mode, fallback eligibility, opening header order, extension value, and payload top-level key order.

Verification status: pending failing diff test.

### 2026-06-17 Task 7 Failing Test

Command/code area touched: added `websocket_parity_diff_should_report_header_order_changes` to `tests/codex_gateway/websocket.rs` and ran `cargo test --test codex_gateway websocket_parity_diff_should_report_header_order_changes`.

Observed result: the test failed to compile because `websocket_parity_diff` does not exist yet. This is the expected red state for the structured diff helper.

Next decision: implement `WebSocketParityDiff` and `WebSocketParityDifference` in the WebSocket audit module, comparing transport mode, fallback flag, opening request line/header order/extensions, payload top-level keys, and optional error classification.

Verification status: expected failing test captured.

### 2026-06-17 Task 4 Implementation Result

Command/code area touched: added `src/codex/gateway/transport/websocket/audit.rs`; modified `.gitignore`, `src/codex/gateway/transport/websocket/codec.rs`, `src/codex/gateway/transport/websocket/mod.rs`, `src/codex/gateway/transport/websocket/opening.rs`, `src/codex/gateway/transport/websocket/pool.rs`, and `tests/codex_gateway/websocket.rs`.

Observed result: WebSocket audit artifact writing is disabled unless `CODEX_PROXY_WS_AUDIT_DIR` is set. When enabled, the runtime writes a pretty JSON artifact before the first `response.create` frame is sent. The artifact includes `transport_mode`, `fallback_allowed`, redacted opening snapshot, redacted payload snapshot, and optional error snapshot fields. Local default audit output under `.codex-ws-audit/` is ignored by git.

Next decision: commit Task 4, then start Task 5 by building a local capture harness that records actual opening bytes and first-frame payload without real OpenAI credentials.

Verification status: `cargo test --test codex_gateway websocket_audit_artifact_should_require_explicit_gate`, `cargo test --test codex_gateway websocket`, `cargo clippy --test codex_gateway --locked -- -D warnings`, and `cargo fmt --all --check` passed. The first format check reported rustfmt ordering/line wrapping; `cargo fmt --all` corrected it and the follow-up check passed.

### 2026-06-17 Task 5 Start

Command/code area touched: read Rust testing/error-handling guidance and existing `tests/codex_gateway/websocket.rs` helpers: `read_http_upgrade_request`, `read_client_websocket_frame`, `websocket_accept_key`, and `assert_headers_appear_in_order`.

Observed result: existing test helpers can already read raw HTTP/1.1 WebSocket opening bytes and decode the masked client first frame. A stable local WSS/TLS integration test is not practical with the current production client because `connect_stream` builds a rustls client from native system roots only; a local self-signed certificate would not validate without adding test-only trust injection to production code. The first Task 5 artifact will therefore capture HTTP/WS bytes and record this TLS limitation explicitly; TLS ClientHello capture remains a follow-up observation path rather than a unit-test prerequisite.

Next decision: write a failing integration test named `websocket_capture_harness_should_record_opening_bytes` around a not-yet-implemented local capture helper, then implement the helper and produce a redacted sample artifact under `.codex-ws-audit/`.

Verification status: pending failing capture harness test.

### 2026-06-17 Task 5 Failing Test

Command/code area touched: added `websocket_capture_harness_should_record_opening_bytes` to `tests/codex_gateway/websocket.rs` and ran `cargo test --test codex_gateway websocket_capture_harness_should_record_opening_bytes`.

Observed result: the test failed to compile because `capture_codex_websocket_exchange` does not exist yet. This is the expected red state for the local capture harness.

Next decision: implement a small test harness that starts a local raw WebSocket endpoint, captures the actual HTTP/1.1 upgrade bytes and first client text frame, returns offsets for `Sec-WebSocket-Key` and `Authorization`, and writes a redacted JSON sample to `.codex-ws-audit/rs-current-capture.json`.

Verification status: expected failing test captured.

### 2026-06-17 Task 3 Implementation Result

Command/code area touched: modified `src/codex/gateway/transport/websocket/codec.rs`, `src/codex/gateway/transport/websocket/mod.rs`, and `tests/codex_gateway/websocket.rs`.

Observed result: `PayloadAuditSnapshot` records `top_level_keys` from the same `serde_json::Value` object that becomes the first `response.create` text frame. The redacted audit body preserves non-sensitive transport structure and redacts `instructions`, `input`, `previous_response_id`, `prompt_cache_key`, `client_metadata`, and `tools`.

Next decision: commit Task 3, then start Task 4 by inspecting existing diagnostic/config patterns before choosing an explicit audit artifact gate.

Verification status: `cargo test --test codex_gateway websocket_payload_audit_snapshot_should_redact_user_content`, `cargo test --test codex_gateway websocket`, `cargo clippy --test codex_gateway --locked -- -D warnings`, and `cargo fmt --all --check` passed. The first format check reported only re-export ordering; `cargo fmt --all` corrected it and the follow-up check passed.

### 2026-06-17 Task 4 Start

Command/code area touched: will inspect `src/config`, runtime diagnostics, and existing environment/config flag patterns before adding any audit artifact writer.

Observed result: opening and payload audit snapshots now exist as in-memory redacted structures, but there is no opt-in artifact writer and no durable local sample for current rs behavior.

Next decision: choose the smallest explicit gate, likely an environment variable if that matches existing local diagnostics better than persistent config, then write a failing test proving the writer is disabled by default and redacts output when enabled.

Verification status: pending config/diagnostics inspection.

### 2026-06-17 Task 4 Config/Diagnostics Inspection

Command/code area touched: searched config, diagnostics, environment-variable usage, runtime trace tests, and existing stream audit code with `rg`; read `src/config/types.rs`, `src/config/loader.rs`, `src/codex/serving/dispatch/stream_audit.rs`, and WebSocket connection metadata paths.

Observed result: app configuration is YAML-first and strict for several sections. Existing `stream_audit` is business-event/usage auditing, not a local artifact writer. Environment variables are used sparingly for local/runtime concerns such as `CODEX_HOME`, making an explicit local-only audit directory environment variable a narrower fit than adding persistent config.

Next decision: use `CODEX_PROXY_WS_AUDIT_DIR` as the explicit production gate and add `.codex-ws-audit/` to `.gitignore`. Tests will exercise a pure `None`/`Some(dir)` gate helper instead of mutating process environment.

Verification status: inspection complete; failing artifact writer test pending.

### 2026-06-17 Task 4 Failing Test

Command/code area touched: added `websocket_audit_artifact_should_require_explicit_gate` to `tests/codex_gateway/websocket.rs` and ran `cargo test --test codex_gateway websocket_audit_artifact_should_require_explicit_gate`.

Observed result: the test failed to compile because `write_websocket_audit_artifact_for_dir`, `WebSocketAuditArtifact`, and `WebSocketAuditErrorSnapshot` do not exist yet. This is the expected red state for the artifact writer.

Next decision: add a WebSocket audit module with `CODEX_PROXY_WS_AUDIT_DIR` as the production gate, a pure directory-gated writer for tests, and runtime wiring that writes opening/payload artifacts only when the gate is present.

Verification status: expected failing test captured.

### 2026-06-17 Task 2 Implementation Result

Command/code area touched: modified `src/codex/gateway/transport/websocket/opening.rs`, `src/codex/gateway/transport/websocket/mod.rs`, and `tests/codex_gateway/websocket.rs`.

Observed result: `OpeningAuditSnapshot` now records `request_line` and ordered headers. `opening_request_bytes` and audit snapshot generation share the same `opening_request_head` source, so the audit view follows the actual request-line/header order. The snapshot redacts `authorization`, `chatgpt-account-id`, `cookie`, `x-client-request-id`, `x-codex-installation-id`, `session_id`, `x-codex-window-id`, `x-codex-turn-state`, `x-codex-turn-metadata`, and `x-codex-parent-thread-id`.

Next decision: commit Task 2, then start Task 3 by tracing the `response.create` payload construction path with CodeGraph and writing a failing payload audit snapshot test.

Verification status: `cargo test --test codex_gateway websocket_opening_audit_snapshot_should_redact_sensitive_headers`, `cargo test --test codex_gateway websocket_handshake_should_offer_original_permessage_deflate_extension`, `cargo fmt --all --check`, `cargo test --test codex_gateway websocket`, and `cargo clippy --test codex_gateway --locked -- -D warnings` all passed.

### 2026-06-17 Task 3 Start

Command/code area touched: will inspect `websocket_request_body` and the call site that sends the first `response.create` WebSocket text frame.

Observed result: Task 2 gives a redacted opening snapshot but there is still no structured audit view for the first WebSocket payload.

Next decision: use CodeGraph to locate payload construction, then write a failing test that records top-level JSON key order and redacts user prompt/input content.

Verification status: pending CodeGraph inspection and failing payload audit test.

### 2026-06-17 Task 3 CodeGraph Inspection

Command/code area touched: ran `codegraph node websocket_request_body` and `codegraph explore websocket_request_body create_response_via_websocket_stream_inner`.

Observed result: the first WebSocket text frame is sent from `create_response_via_websocket_stream_inner` as `Message::Text(websocket_request_body(request).to_string().into())`. Payload construction is centralized in `src/codex/gateway/transport/websocket/codec.rs`, where `websocket_request_body` builds the `response.create` JSON `Value`.

Next decision: add the audit snapshot beside `websocket_request_body` so the runtime payload and audit view share the same source value. The test should assert top-level key order from the actual JSON object iteration and redact user prompt/input content.

Verification status: CodeGraph inspection completed; failing test pending.

### 2026-06-17 Task 3 Failing Test

Command/code area touched: added `websocket_payload_audit_snapshot_should_redact_user_content` to `tests/codex_gateway/websocket.rs` and ran `cargo test --test codex_gateway websocket_payload_audit_snapshot_should_redact_user_content`.

Observed result: the test failed to compile with unresolved import `websocket_payload_audit_snapshot`, which is the expected red state for the new payload audit API.

Next decision: implement `PayloadAuditSnapshot` beside `websocket_request_body`, expose it through the WebSocket module, and redact prompt/input/metadata/id-bearing fields while preserving top-level key order.

Verification status: expected failing test captured.

## Update Discipline

This document is the durable context for the task. Before starting any substantial analysis or code edit, append a short entry to either Progress Log or Analysis Journal. After each implementation step, append:

- command or code area touched,
- observed result,
- next decision,
- verification status.

If context is compacted, resume from this document before continuing.

### 2026-06-17 Task 11 Verification Continuation

Command/code area touched: resumed from the Task 11 handoff, inspected `git status`, the focused serving/gateway tests, and the HTTP SSE request bodies in `tests/codex_serving/responses_http_sse.rs` and `tests/codex_serving/upstream_errors.rs`.

Observed result: focused WebSocket checks had already passed, while the remaining full-suite failures are legacy HTTP SSE tests that relied on mock WebSocket 404/404-like transport failure downgrading to HTTP SSE. After Task 10, that downgrade path is intentionally narrowed to official-source HTTP 426 only, so HTTP SSE behavior tests must opt into HTTP transport with `use_websocket: false`.

Next decision: update only those HTTP SSE test request bodies to make transport selection explicit. Do not loosen runtime fallback classification.

Verification status: pending focused `codex_serving` rerun and then full verification commands.

### 2026-06-17 Task 11 HTTP SSE Test Fix Result

Command/code area touched: updated HTTP SSE-focused request bodies in `tests/codex_serving/responses_http_sse.rs` and `tests/codex_serving/upstream_errors.rs` to include `use_websocket: false`. This keeps the tests on the HTTP SSE transport they are intended to exercise after Task 10 narrowed WebSocket fallback to HTTP 426.

Observed result: `cargo test --test codex_serving responses_http_sse` passed 28 tests. `cargo test --test codex_serving upstream_errors` passed 5 tests.

Next decision: run full `cargo test`, formatting, Clippy, `git diff --check`, and final status.

Verification status: focused serving tests passing; full verification pending.

### 2026-06-17 Task 11 Final Verification

Command/code area touched: ran the final verification set after the HTTP SSE test transport fixes:
`cargo test --test codex_serving responses_http_sse`,
`cargo test --test codex_serving upstream_errors`,
`cargo test`,
`cargo fmt --all --check`,
`cargo clippy --all-targets --all-features --locked -- -D warnings`,
`git diff --check`,
`cargo test --test codex_gateway websocket`, and
`cargo test --test codex_serving responses_websocket`.

Observed result: all commands passed. Full `cargo test` passed the library/unit tests, all integration suites, and doc tests. WebSocket-focused coverage passed 30 gateway tests and 26 serving tests. HTTP SSE-focused coverage passed 28 serving behavior tests and 5 upstream error tests after those tests explicitly selected `use_websocket: false`.

Next decision: commit the remaining documentation and test updates. Local audit artifacts remain under ignored `.codex-ws-audit/`; `.gitignore` contains `/.codex-ws-audit/`, so environment-specific capture output is not committed.

Verification status: final local verification complete. The official bundled `Codex.app` binary is still a macOS arm64 Mach-O and cannot be executed on this Linux host, so official live TLS/opening sampling remains unavailable here; parity decisions are based on local redacted capture plus official `openai/codex` source/static evidence documented above.

### 2026-06-17 Task 12 Ping/Pong Source Parity Start

Command/code area touched: resumed after Task 11 with a clean worktree, confirmed CodeGraph index is up to date, and re-fetched official `openai/codex` `codex-api/src/endpoint/responses_websocket.rs`, `codex-api/src/common.rs`, and `core/src/client.rs`.

Observed result: official `ResponsesWebsocketConnection` wraps `WebSocketStream` in a `WsStream` pump. In that pump, `Ok(Message::Ping(payload))` immediately sends `Message::Pong(payload)`, `Pong` is ignored, and only Text/Binary/Close/Frame are forwarded to the response consumer. Current rs response loops ignore non-text messages through `websocket_message_text(message) == None`; this does not explicitly send the Pong from our code path and is weaker than official source-backed behavior.

Next decision: add a gateway WebSocket test where the mock upstream sends Ping and waits for matching Pong before sending `response.completed`. The expected red state is a timeout/failure before implementing explicit Pong handling.

Verification status: source-backed difference identified; failing test pending.

### 2026-06-17 Task 12 Ping/Pong Observation and Binary Frame Gap

Command/code area touched: added and ran `cargo test --test codex_gateway websocket_should_reply_to_server_ping_before_completed_event`.

Observed result: the test passed without code changes. Although current rs does not explicitly match official `Ok(Message::Ping(payload)) => send(Pong(payload))` source, `tokio-tungstenite` currently flushes the Pong in this path, so no observable Ping/Pong parity change is justified from this test.

Next decision: keep the Ping/Pong regression test and target a stronger source-backed delta in the same official loop: official `run_websocket_response_stream` returns an error for `Message::Binary(_)` with `unexpected binary websocket event`, while current rs tries to UTF-8 decode binary messages in `websocket_message_text` and may treat binary JSON as valid response events.

Verification status: Ping/Pong observable parity covered; binary-frame failing test pending.

### 2026-06-17 Task 12 Binary Frame Implementation Result

Command/code area touched: added `websocket_binary_response_frame_should_error_like_official_client` to `tests/codex_gateway/websocket.rs`, then modified `src/codex/gateway/transport/websocket/mod.rs`, `src/codex/gateway/transport/websocket/codec.rs`, and `src/codex/gateway/transport/http_client.rs`.

Observed result: the failing test first failed to compile because `CodexWebSocketError::UnexpectedBinaryEvent` did not exist. Implementation added that error, changed `websocket_message_text` to return `Err(UnexpectedBinaryEvent)` for `Message::Binary(_)`, discards the active socket on binary frames, forwards the error on streaming paths, and marks the error as ineligible for HTTP SSE downgrade. This matches official source behavior where binary websocket events are rejected instead of being parsed as UTF-8 response events.

Next decision: run focused WebSocket gateway/serving checks, formatting, Clippy, and whitespace checks before committing Task 12.

Verification status: `cargo test --test codex_gateway websocket_` passed 31 tests, including Ping/Pong and binary-frame parity; `cargo test --test codex_serving responses_websocket` passed 26 tests.

### 2026-06-17 Task 12 Final Verification

Command/code area touched: ran `cargo fmt --all`, `cargo clippy --all-targets --all-features --locked -- -D warnings`, `cargo test --test codex_gateway websocket_`, `cargo test --test codex_serving responses_websocket`, `cargo fmt --all --check`, `git diff --check`, and full `cargo test`.

Observed result: all verification commands passed. Full `cargo test` passed unit tests, all integration suites, and doc tests. The gateway WebSocket suite now includes 31 filtered tests and the full gateway suite includes 65 tests after adding Ping/Pong and binary-frame parity coverage.

Next decision: commit Task 12 as a source-backed observable parity increment. Remaining large uncertainty is still official live TLS/opening sampling, blocked locally by the macOS arm64 binary format.

Verification status: ready to commit.

### 2026-06-17 Task 31 Response Failed Retry-After Message Audit Start

Command/code area touched: starting comparison of official `try_parse_retry_after` for `response.failed` `rate_limit_exceeded` errors against local HTTP/WebSocket upstream retry-after extraction.

Observed result: pending. Official `process_responses_event` parses retry delays embedded in `response.error.message`, including strings like `Please try again in 11.054s`, `Try again in 35 seconds`, and `try again in 28ms`, but only when the error code is `rate_limit_exceeded`. Local `retry_after_seconds_from_body` currently reads structured `resets_in_seconds` or `resets_at` only, so WebSocket `response.failed` rate-limit frames without those fields lose the upstream delay and fall back to generic account retry defaults.

Next decision: add focused gateway tests for WebSocket `response.failed` `rate_limit_exceeded` messages with official retry-after wording, then implement message parsing in the shared upstream body retry-after extraction path without broadening parsing to non-rate-limit codes.

Verification status: red test pending.

### 2026-06-17 Task 31 Response Failed Retry-After Message Implementation Result

Command/code area touched: added gateway WebSocket coverage in `tests/codex_gateway/websocket.rs`; introduced shared body retry-after parsing in `src/codex/gateway/transport/retry_after.rs`; switched `http_client.rs` and `websocket/{codec.rs,mod.rs}` to the shared helper.

Observed result: the focused WebSocket test first failed with `retry_after_seconds: None` for a `response.failed` frame carrying `{"code":"rate_limit_exceeded","message":"Rate limit reached. Please try again in 11.054s."}`. The implementation now preserves existing structured `resets_in_seconds` / `resets_at` parsing and then parses official-shaped message delays only for `rate_limit_exceeded` (`code`, with local `type` fallback). Because the proxy's public retry-after field is `Option<u64>` seconds rather than official `Duration`, positive fractional seconds and millisecond delays are rounded up to at least one second; e.g. `11.054s` becomes `12`, and `28ms` becomes `1`.

Focused verification passed:
- `cargo test retry_after_seconds_from_body`
- `cargo test --test codex_gateway websocket_response_failed_`

Next decision: run gateway WebSocket and serving WebSocket suites, formatting, diff check, Clippy, and full `cargo test`.

Verification status: focused tests passed; broader verification pending.

### 2026-06-17 Task 31 Final Verification

Command/code area touched: ran Task 31 verification after formatting and shared retry-after parser changes.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 47 tests, including `response.failed` rate-limit message retry-after parsing and the non-rate-limit guard. Serving WebSocket coverage remained at 28 passing tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo fmt --all`
- `cargo test retry_after_seconds_from_body`
- `cargo test --test codex_gateway websocket_response_failed_`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 31 as `fix: parse websocket rate limit retry delays`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 32 Malformed Completed Frame Audit Start

Command/code area touched: starting comparison of official `process_responses_event` `response.completed` parsing against local WebSocket terminal-frame handling.

Observed result: pending. Official `process_responses_event` deserializes `response.completed.response` into `ResponseCompleted`, where `id` is required. If deserialization fails, official returns `ApiError::Stream("failed to parse ResponseCompleted: ...")`; it does not emit a successful completed event. Local WebSocket handling currently treats any `type: "response.completed"` frame as terminal and encodes it as downstream SSE without validating the present `response` object, so a malformed completed frame can be returned as success.

Next decision: add a focused gateway WebSocket test with `response.completed` missing `response.id`, then implement minimal validation for present completed response objects before treating the frame as terminal success. To avoid broad fixture churn, this task will validate the required `id` field rather than fully validating every optional usage subfield.

Verification status: red test pending.

### 2026-06-17 Task 32 Malformed Completed Frame Implementation Result

Command/code area touched: added `websocket_malformed_completed_response_should_error_like_official_client` in `tests/codex_gateway/websocket.rs`; added `response_completed_parse_error` in `src/codex/gateway/transport/websocket/codec.rs`; wired it into both first-frame and forwarding loops in `websocket/mod.rs`; updated HTTP SSE fallback eligibility in `http_client.rs`.

Observed result: the focused test first failed because local returned successful SSE for a `response.completed` frame whose `response` object lacked the required `id`; the body was `event: response.completed` and local usage extraction still produced token usage. Implementation now rejects a present `response.completed.response` without a non-empty string `id`, returns `failed to parse ResponseCompleted: missing field \`id\``, discards the WebSocket, and does not fall back to HTTP SSE. This matches the official source-backed requirement for the required `ResponseCompleted.id` field while intentionally leaving optional usage-field strictness unchanged to avoid broad mock churn.

Focused verification passed:
- `cargo test --test codex_gateway websocket_malformed_completed_response_should_error_like_official_client`

Next decision: run gateway WebSocket and serving WebSocket suites, formatting, diff check, Clippy, and full `cargo test`.

Verification status: focused test passed; broader verification pending.

### 2026-06-17 Task 32 Final Verification

Command/code area touched: ran Task 32 verification after formatting and malformed completed-frame validation changes.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 48 tests, including malformed `response.completed` rejection. Serving WebSocket coverage remained at 28 passing tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo fmt --all`
- `cargo test --test codex_gateway websocket_malformed_completed_response_should_error_like_official_client`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 32 as `fix: reject malformed websocket completed frames`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 33 Completed Usage Shape Audit Start

Command/code area touched: continuing the Task 32 `response.completed` validation audit by comparing official `ResponseCompletedUsage` shape against local WebSocket completed-frame validation and local test fixtures.

Observed result: pending. Official `ResponseCompleted` allows `usage` to be absent, but when `usage` is present its `ResponseCompletedUsage` requires `input_tokens`, `output_tokens`, and `total_tokens`; nested `input_tokens_details.cached_tokens` and `output_tokens_details.reasoning_tokens` are also required when their detail objects are present. Task 32 only rejected missing `response.id`, while local `extract_sse_usage` still accepts incomplete usage by deriving `total_tokens` from input plus output. That leaves a concrete mismatch: a WebSocket `response.completed` with `usage` missing `total_tokens` can still be returned as success locally, while official would emit `failed to parse ResponseCompleted: ...`.

Next decision: add a focused gateway WebSocket test for `response.completed` with `usage` missing `total_tokens`, then replace the minimal `id` check with official-shaped completed-response deserialization. Local upstream mocks that represent successful official frames should include `total_tokens`.

Verification status: red test pending.

### 2026-06-17 Task 33 Completed Usage Shape Implementation Result

Command/code area touched: added `websocket_completed_response_with_incomplete_usage_should_error_like_official_client` in `tests/codex_gateway/websocket.rs`; replaced local `response.completed` minimal `id` validation in `src/codex/gateway/transport/websocket/codec.rs` with official-shaped `ResponseCompleted` / `ResponseCompletedUsage` deserialization; updated WebSocket success fixtures and helpers to include official `total_tokens`.

Observed result: the focused test first failed because local returned successful SSE and synthesized usage total for a WebSocket `response.completed` frame whose `usage` lacked `total_tokens`. Implementation now rejects such frames with `failed to parse ResponseCompleted: ...`, while successful WebSocket fixtures now carry `total_tokens` so they match official completed-response shape. The existing missing-`id` malformed completed test remains covered by the same deserialization path.

Focused verification passed:
- `cargo test --test codex_gateway completed_response`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`

Next decision: run formatting, diff check, Clippy, and full `cargo test`, then commit Task 33 if clean.

Verification status: focused and WebSocket suite tests passed; broader verification pending.

### 2026-06-17 Task 33 Final Verification

Command/code area touched: ran Task 33 verification after formatting, official-shaped completed-response deserialization, and fixture updates for successful WebSocket completed frames.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 49 tests, including malformed completed `id` and incomplete `usage` rejection. Serving WebSocket coverage remained at 28 passing tests. Full `cargo test` passed unit tests, integration suites, and doc tests. One runtime startup fixture initially failed because its successful WebSocket completed mock omitted `total_tokens`; the fixture now includes official-shaped usage and the focused runtime test passes.

Commands:
- `cargo fmt --all`
- `cargo test --test codex_gateway completed_response`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo test --test runtime app_state_should_restore_session_affinity_from_sqlite`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 33 as `fix: validate websocket completed usage shape`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 34 Response-Less Completed Frame Audit Start

Command/code area touched: comparing official `process_responses_event` `response.completed` branch against local WebSocket terminal event handling.

Observed result: pending. Official only returns `ResponseEvent::Completed` inside `if let Some(resp_val) = event.response`; a `response.completed` event without a `response` object falls through to `Ok(None)` and is not considered completion. Local WebSocket handling currently treats any `type: "response.completed"` frame as terminal and returns it as successful SSE, because `response_completed_parse_error` returns `None` when `response` is absent and `is_terminal_websocket_event` then marks it complete.

Next decision: add a focused gateway WebSocket test where upstream sends `{"type":"response.completed"}` then closes. The expected local parity behavior is that this frame is ignored as non-completing and the close surfaces as a WebSocket before-completed error, not a successful response.

Verification status: red test pending.

### 2026-06-17 Task 34 Response-Less Completed Frame Implementation Result

Command/code area touched: added `websocket_completed_without_response_should_not_finish_like_official_client` in `tests/codex_gateway/websocket.rs`; added `response_completed_missing_response` in `src/codex/gateway/transport/websocket/codec.rs`; wired first-frame and forwarding loops in `websocket/mod.rs` to skip response-less completed frames.

Observed result: the focused test first failed because local returned successful SSE for `{"type":"response.completed"}`. Implementation now ignores that frame as non-completing, so the subsequent WebSocket close surfaces as `ClosedByServerBeforeCompleted`, matching the official branch where `response.completed` without `event.response` falls through to `Ok(None)` rather than `ResponseEvent::Completed`.

Focused verification passed:
- `cargo test --test codex_gateway websocket_completed_without_response_should_not_finish_like_official_client`

Next decision: run gateway WebSocket and serving WebSocket suites, formatting, diff check, Clippy, and full `cargo test`.

Verification status: focused test passed; broader verification pending.

### 2026-06-17 Task 34 Final Verification

Command/code area touched: ran Task 34 verification after formatting and response-less completed-frame skip changes.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 50 tests, including response-less completed-frame rejection by ignoring the frame and surfacing the following close as `ClosedByServerBeforeCompleted`. Serving WebSocket coverage remained at 28 passing tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo fmt --all`
- `cargo test --test codex_gateway websocket_completed_without_response_should_not_finish_like_official_client`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 34 as `fix: ignore websocket completed frames without response`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 25 Completed Event Side-Channel Audit Start

Command/code area touched: starting comparison of official `run_websocket_response_stream` handling for `response.completed` side-channel fields (`openai_model`, `model_verification_status`, and turn moderation metadata) against local WebSocket forwarding, usage extraction, and session-affinity metadata handling.

Observed result: pending. Official WebSocket code emits several Desktop-internal `ResponseEvent` variants before passing the parsed event through `process_responses_event`; local proxy currently forwards raw SSE-compatible frames and separately extracts completed-response metadata for usage/session affinity. This task will determine whether any missing completed-event side channel is observable in the proxy's API contract or should remain documented as Desktop-internal only.

Next decision: inspect official `ResponsesStreamEvent` parsing and local completed-response metadata extraction paths, then add a failing test only if the difference affects local downstream behavior without inventing new external API fields.

Verification status: analysis in progress.

### 2026-06-17 Task 25 Completed Event Side-Channel Audit Result

Command/code area touched: inspected official `ResponsesStreamEvent::response_model`, `model_verifications`, and `turn_moderation_metadata`; official WebSocket event loop side-channel emission; official core consumers for `ResponseEvent::ServerModel`, `ModelVerifications`, `TurnModerationMetadata`, and `ServerReasoningIncluded`; local WebSocket forwarding and serving completed-response metadata extraction.

Observed result: no local code change is justified for this task. Official `openai-model` / `x-openai-model`, `openai_verification_recommendation`, and `openai_chatgpt_moderation_metadata` are emitted as Desktop-internal `ResponseEvent` side channels and consumed by the official core for model mismatch warnings, account verification UI, turn moderation presentation, and related state. They are not emitted as raw SSE frames. Local proxy has no corresponding internal Desktop UI/session consumer and its public contract is OpenAI-compatible SSE passthrough plus local usage/session-affinity extraction. Adding synthetic SSE frames or response fields for these side channels would change the proxy's external API rather than improve 1:1 behavior inside the current boundary.

Next decision: leave these completed-event side channels documented as Desktop-internal and not actionable for the proxy API surface. Continue with the next source-backed audit target that can affect local behavior.

Verification status: no code change; documentation-only audit result pending commit.

### 2026-06-17 Task 26 WebSocket Receive Idle Timeout Audit Start

Command/code area touched: starting comparison of official per-event WebSocket receive timeout in `run_websocket_response_stream` against local `create_response_via_websocket_stream_inner` and `forward_websocket_as_sse` loops.

Observed result: official wraps every `ws_stream.next()` with `tokio::time::timeout(idle_timeout, ...)` and returns `ApiError::Stream("idle timeout waiting for websocket")` when the server stops sending events before `response.completed`. Local WebSocket loops currently await `.next()` directly after a successful handshake/request send, so an upstream that accepts the request and then stays silent can hang the local caller indefinitely.

Next decision: add a focused gateway test using Tokio virtual time for a silent upstream WebSocket, then add a local receive idle timeout that returns a WebSocket stream error instead of hanging. Keep the timeout scoped to WebSocket event receive behavior and avoid changing opening-byte parity.

Verification status: red test pending.

### 2026-06-17 Task 26 WebSocket Receive Idle Timeout Implementation Result

Command/code area touched: added `websocket_silent_upstream_should_timeout_like_official_client` in `tests/codex_gateway/websocket.rs`; updated local WebSocket receive loops in `src/codex/gateway/transport/websocket/mod.rs`; updated WebSocket fallback classification in `src/codex/gateway/transport/http_client.rs`.

Observed result: the focused test first failed to compile because `CodexWebSocketError::ReceiveIdleTimeout` did not exist. Implementation adds a local receive idle timeout around both first-frame and forwarding-loop WebSocket reads. A silent upstream now returns `CodexWebSocketError::ReceiveIdleTimeout { timeout: 20s }` with display text `idle timeout waiting for websocket`, matching the official error string for this condition. The timeout error is not eligible for HTTP SSE downgrade, so a stuck WebSocket request surfaces as an upstream transport failure instead of silently changing transport.

Verification status: `cargo test --test codex_gateway websocket_silent_upstream_should_timeout_like_official_client` passed. Broader verification pending.

### 2026-06-17 Task 26 WebSocket Receive Idle Timeout Verification Result

Command/code area touched: verified receive idle timeout handling with focused gateway coverage, the gateway WebSocket filtered suite, serving WebSocket suite, formatting, diff whitespace checks, Clippy, and the full test suite.

Observed result: all verification commands passed. The gateway WebSocket filtered suite now covers 42 tests, including the silent-upstream receive timeout case. The serving `responses_websocket` suite remained at 27 passing tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo test --test codex_gateway websocket_silent_upstream_should_timeout_like_official_client`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 26 as a source-backed WebSocket receive timeout parity increment. Continue auditing official WebSocket behavior that can affect local callers; live macOS Desktop TLS/opening capture remains a separate unproven artifact.

Verification status: ready to commit.

### 2026-06-17 Task 27 WebSocket Error Connection Lifecycle Audit Start

Command/code area touched: starting comparison of official stream-error connection lifecycle against local pooled WebSocket handling for classified upstream error frames.

Observed result: official `ResponsesWebsocketConnection::stream_request` calls `run_websocket_response_stream`; if it returns any error, the spawned task takes the locked stream out of the connection and drops it so the connection is not reused after a terminal stream error. Local `classify_ws_error_frame` errors currently discard only `connection_fatal` cases and call `active.finish()` for most upstream error frames, returning the socket to the local pool. That can reuse a connection after `response.failed` / wrapped upstream errors, which diverges from official Desktop behavior.

Next decision: add a focused pooled WebSocket test where the first request receives a classified upstream error and the second request for the same conversation must open a new WebSocket instead of reusing the errored one. Then make all classified WebSocket upstream errors discard the active connection.

Verification status: red test pending.

### 2026-06-17 Task 27 WebSocket Error Connection Lifecycle Implementation Result

Command/code area touched: added `websocket_pooled_upstream_error_should_discard_connection_like_official_client` in `tests/codex_gateway/websocket.rs`; updated classified upstream error handling in `src/codex/gateway/transport/websocket/mod.rs`; removed now-unused `connection_fatal` state from `src/codex/gateway/transport/websocket/codec.rs`; updated serving WebSocket recovery tests in `tests/codex_serving/responses_websocket.rs` so recovery attempts after stream errors occur on fresh upstream WebSocket connections.

Observed result: the focused test first failed because a pooled WebSocket that returned `response.failed` with `rate_limit_exceeded` was returned to the pool; the next request for the same conversation reused that errored connection and eventually hit the receive idle timeout instead of opening a fresh WebSocket. Implementation now discards the active WebSocket for every classified upstream error frame in both first-frame and forwarding loops, matching official behavior where any `run_websocket_response_stream` error causes the connection stream to be taken and dropped. Existing serving recovery tests initially assumed same-connection retry after upstream recovery errors; their upstream mocks now accept a new WebSocket for the recovered request, preserving recovery coverage while matching official connection lifecycle.

Verification status: `cargo test --test codex_gateway websocket_pooled_upstream_error_should_discard_connection_like_official_client` and `cargo test --test codex_serving responses_websocket` passed. Broader verification pending.

### 2026-06-17 Task 27 WebSocket Error Connection Lifecycle Verification Result

Command/code area touched: verified WebSocket error connection discard behavior with focused gateway coverage, the gateway WebSocket filtered suite, serving WebSocket suite, formatting, diff whitespace checks, Clippy, and the full test suite.

Observed result: all verification commands passed. The gateway WebSocket filtered suite now covers 43 tests, including pooled upstream-error discard behavior. The serving `responses_websocket` suite remains at 27 passing tests, with recovery retries now modeled on fresh WebSocket connections after stream errors. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo test --test codex_gateway websocket_pooled_upstream_error_should_discard_connection_like_official_client`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 27 as a source-backed WebSocket lifecycle parity increment. Continue auditing remaining official WebSocket behavior, including send timeout parity if it can be tested without fragile socket backpressure assumptions.

Verification status: ready to commit.

### 2026-06-17 Task 28 WebSocket Send Idle Timeout Audit Start

Command/code area touched: starting comparison of official `send_websocket_request` timeout handling against local WebSocket request-frame send.

Observed result: official wraps `ws_stream.send(Message::Text(request_text.into()))` in `tokio::time::timeout(idle_timeout, ...)` and maps timeout to `ApiError::Stream("idle timeout sending websocket request")`. Local `create_response_via_websocket_stream_inner` currently awaits `active.websocket.send(Message::Text(...))` directly, so a stuck send future has no local timeout boundary. This is distinct from Task 26 receive idle timeout.

Next decision: add a focused virtual-time unit test for a pending send future, then route production WebSocket request sends through a helper that maps send timeout to a dedicated WebSocket error and keeps it ineligible for HTTP SSE fallback.

Verification status: red test pending.

### 2026-06-17 Task 28 WebSocket Send Idle Timeout Implementation Result

Command/code area touched: added `timeout_websocket_send_should_fail_when_send_future_stalls` in `src/codex/gateway/transport/websocket/mod.rs`; added `CodexWebSocketError::SendIdleTimeout`; routed production request-frame sends through `timeout_websocket_send`; updated WebSocket fallback classification in `src/codex/gateway/transport/http_client.rs`.

Observed result: the focused virtual-time test first failed because no send-timeout helper or `SendIdleTimeout` error variant existed. Implementation now wraps request-frame send futures in the same 20s WebSocket event timeout boundary used for receive polling and maps timeout to display text `idle timeout sending websocket request`, matching official `send_websocket_request`. The new timeout remains ineligible for HTTP SSE fallback.

Verification status: `cargo test timeout_websocket_send_should_fail_when_send_future_stalls` passed. Broader verification pending.

### 2026-06-17 Task 28 WebSocket Send Idle Timeout Verification Result

Command/code area touched: verified WebSocket request send timeout handling with focused virtual-time unit coverage, the gateway WebSocket filtered suite, serving WebSocket suite, formatting, diff whitespace checks, Clippy, and the full test suite.

Observed result: all verification commands passed. Unit coverage now includes pending send futures mapping to `SendIdleTimeout`. The gateway WebSocket filtered suite remains at 43 passing tests and the serving `responses_websocket` suite remains at 27 passing tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo test timeout_websocket_send_should_fail_when_send_future_stalls`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 28 as a source-backed WebSocket send timeout parity increment. Continue auditing remaining official WebSocket behavior and keep live macOS Desktop TLS/opening capture separate from source-backed parity work.

Verification status: ready to commit.

### 2026-06-17 Task 29 Unknown Response Failed Error Audit Start

Command/code area touched: starting comparison of official `process_responses_event` fallback handling for `response.failed` errors against local WebSocket error classification.

Observed result: official `process_responses_event` returns an `ApiError` for every `response.failed` event with a `response` object. If `response.error` parses but its code is not one of the special cases, official maps it to `ApiError::Retryable { message, delay }`; it is not emitted as a successful response event. Local `classify_ws_error_frame` only maps known error codes, so an unknown `response.failed` code currently falls through to terminal SSE encoding and can be returned as a successful `event: response.failed` body.

Next decision: add a focused gateway test for `response.failed` with an unknown error code, then classify unknown `response.failed` errors as the proxy's existing 503 upstream error equivalent for official retryable stream errors while preserving the original body.

Verification status: red test pending.

### 2026-06-17 Task 29 Unknown Response Failed Error Implementation Result

Command/code area touched: added `websocket_unknown_response_failed_should_surface_as_503_like_official_retryable` in `tests/codex_gateway/websocket.rs`; updated fallback classification in `src/codex/gateway/transport/websocket/codec.rs`.

Observed result: the focused test first failed because local returned an OK response body containing `event: response.failed` for an unknown `response.error.code`. Implementation now maps otherwise-unclassified `response.failed` frames to `StatusCode::SERVICE_UNAVAILABLE`, the proxy's existing upstream equivalent for official retryable/stream WebSocket failures, while preserving the original upstream JSON body.

Verification status: `cargo test --test codex_gateway websocket_unknown_response_failed_should_surface_as_503_like_official_retryable` and `cargo test --test codex_gateway response_failed` passed. Broader verification pending.

### 2026-06-17 Task 24 Response Failed Server Overload Audit Start

Command/code area touched: starting comparison of official `response.failed` server overload handling against local WebSocket error classification.

Observed result: official `process_responses_event` maps `response.failed` with error code `server_is_overloaded` or `slow_down` to `ApiError::ServerOverloaded`. Local `classify_ws_error_frame` currently does not classify those codes, so a WebSocket `response.failed` frame with `server_is_overloaded` can be encoded as a successful SSE `event: response.failed` body rather than surfacing as an upstream retryable/server-overloaded error.

Next decision: add a focused gateway test for a first-frame `response.failed` with `server_is_overloaded`, then map `server_is_overloaded` and `slow_down` to the proxy's existing 503 upstream error equivalent.

Verification status: red test pending.

### 2026-06-17 Task 24 Response Failed Server Overload Implementation Result

Command/code area touched: added `websocket_response_failed_server_overloaded_should_surface_as_503_like_official_client` in `tests/codex_gateway/websocket.rs`; updated `rotatable_error_status` in `src/codex/gateway/transport/websocket/codec.rs`.

Observed result: the focused test first failed because local returned an OK response body containing `event: response.failed` for `server_is_overloaded`. Implementation now maps `server_is_overloaded` and `slow_down` to `StatusCode::SERVICE_UNAVAILABLE`, the proxy's existing upstream equivalent for official `ApiError::ServerOverloaded`, while preserving the original upstream JSON body.

Verification status: `cargo test --test codex_gateway websocket_response_failed_server_overloaded_should_surface_as_503_like_official_client`, `cargo test --test codex_gateway response_failed`, and `cargo test --test codex_gateway connection_limit` passed. Broader verification pending.

### 2026-06-17 Task 24 Response Failed Server Overload Verification Result

Command/code area touched: verified server-overload `response.failed` handling with focused gateway coverage, related response-failed and connection-limit filters, the full gateway WebSocket filtered suite, serving WebSocket suite, Clippy, formatting, diff whitespace checks, and the full test suite.

Observed result: all verification commands passed. The gateway WebSocket filtered suite now covers 41 tests, including `server_is_overloaded` handling. The serving `responses_websocket` suite remained at 27 passing tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo test --test codex_gateway websocket_response_failed_server_overloaded_should_surface_as_503_like_official_client`
- `cargo test --test codex_gateway response_failed`
- `cargo test --test codex_gateway connection_limit`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo test`

Next decision: commit Task 24 as a source-backed server-overload error parity increment. Continue auditing official WebSocket behavior that is observable locally, while keeping live macOS Desktop TLS/opening capture marked as unproven on this Linux host.

Verification status: ready to commit.

### 2026-06-17 Task 23 Unmapped Wrapped Error Event Audit Start

Command/code area touched: starting comparison of official `type:error` frames that are parsed as wrapped WebSocket errors but do not map to an API error, especially success `status` values and missing status.

Observed result: official `parse_wrapped_websocket_error_event` returns an event for any text frame with `type == "error"`, but `map_wrapped_websocket_error_event` returns `None` when status is missing or `StatusCode::is_success()`. The frame is then parsed as `ResponsesStreamEvent` kind `error`; `process_responses_event` has no `error` branch and returns `Ok(None)`, so the official loop waits for more events and eventually errors if the socket closes before `response.completed`. Local currently treats any typed `error` frame as a terminal SSE event after classification misses, so a payload like `{"type":"error","status":200,...}` can become a successful response body containing `event: error`.

Next decision: add a focused gateway test for a success-status wrapped error frame followed by close, then skip unmapped `type:error` frames instead of forwarding them as successful terminal SSE.

Verification status: red test pending.

### 2026-06-17 Task 23 Unmapped Wrapped Error Event Implementation Result

Command/code area touched: added `websocket_unmapped_success_status_error_should_not_return_successful_error_event_like_official_client` in `tests/codex_gateway/websocket.rs`; updated first-frame and forwarding loops in `src/codex/gateway/transport/websocket/mod.rs`.

Observed result: the focused test first failed because local returned an OK response body containing `event: error` for a wrapped error with `status: 200`. Implementation now applies normal error classification first; if a typed `error` frame remains unmapped after classification and metadata/incomplete handling, it is skipped instead of encoded as SSE. The loop then waits for a later `response.completed` or returns the normal close-before-completed error, matching official `process_responses_event` behavior for unhandled kind `error`. During verification, the existing unanswered-function-call serving path exposed an important proxy recovery requirement: missing-status `type:error` frames with `code: invalid_request` must still classify as `400` so history-stripping recovery can run. The classifier now treats explicit success status as unmapped, while missing-status `invalid_request` remains a 400 upstream error.

Verification status: `cargo test --test codex_gateway websocket_unmapped_success_status_error_should_not_return_successful_error_event_like_official_client` and `cargo test --test codex_serving v1_responses_non_stream_unanswered_function_call_should_strip_history_and_retry_same_account` passed. Broader verification pending.

### 2026-06-17 Task 23 Unmapped Wrapped Error Event Verification Result

Command/code area touched: verified unmapped `type:error` handling with focused gateway coverage, the serving unanswered-function-call recovery path, gateway WebSocket filtered suite, serving WebSocket suite, Clippy, formatting, diff whitespace checks, and the full test suite.

Observed result: all verification commands passed. The gateway WebSocket filtered suite now covers 40 tests, including the success-status wrapped error case. The serving `responses_websocket` suite remained at 27 passing tests and specifically confirms missing-status `invalid_request` still triggers history stripping rather than hanging. The full `cargo test` run completed successfully.

Commands:
- `cargo test --test codex_gateway websocket_unmapped_success_status_error_should_not_return_successful_error_event_like_official_client`
- `cargo test --test codex_serving v1_responses_non_stream_unanswered_function_call_should_strip_history_and_retry_same_account`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo test`

Next decision: commit Task 23 as a source-backed unmapped error event parity increment. Continue auditing remaining official WebSocket behavior and preserve the documented distinction between proven source-backed parity and unproven live macOS Desktop TLS/opening capture.

Verification status: ready to commit.

### 2026-06-17 Task 22 Wrapped Connection Limit Error Audit Start

Command/code area touched: starting comparison of official wrapped WebSocket `websocket_connection_limit_reached` handling against local wrapped error status precedence.

Observed result: official `map_wrapped_websocket_error_event` checks `error.code == "websocket_connection_limit_reached"` before parsing `status` and returns `ApiError::Retryable` with the official fallback message. Local `classify_ws_error_frame` currently checks wrapped non-success `status` before error code classification; therefore a payload like `{"type":"error","status":400,"error":{"code":"websocket_connection_limit_reached"}}` is surfaced as upstream 400 instead of the proxy's existing 503/retryable connection-limit equivalent. The proxy already uses `StatusCode::SERVICE_UNAVAILABLE` for pooled `response.failed` connection-limit frames, so the wrapped error path should share that behavior and treat the connection as fatal.

Next decision: add a focused gateway test for wrapped `websocket_connection_limit_reached` with `status: 400`, then give that code priority over wrapped status in local classification.

Verification status: red test pending.

### 2026-06-17 Task 22 Wrapped Connection Limit Error Implementation Result

Command/code area touched: added `websocket_wrapped_connection_limit_should_use_retryable_503_precedence_like_official_client`; updated the existing one-shot connection-limit `response.failed` test to expect the same retryable-equivalent behavior; changed `classify_ws_error_frame` in `src/codex/gateway/transport/websocket/codec.rs`.

Observed result: the focused wrapped test first failed because local surfaced status 400 from the wrapped error instead of giving `websocket_connection_limit_reached` priority. After moving error-code extraction ahead of wrapped status classification, both wrapped `type:error` and `response.failed` connection-limit frames map to `StatusCode::SERVICE_UNAVAILABLE` and mark the connection fatal. This matches the proxy's existing 503 retryable equivalent for official Desktop's `ApiError::Retryable` connection-limit path and avoids treating connection exhaustion as a client invalid request.

Verification status: `cargo test --test codex_gateway connection_limit` passed the wrapped, one-shot, and pooled connection-limit coverage. Broader verification pending.

### 2026-06-17 Task 22 Wrapped Connection Limit Error Verification Result

Command/code area touched: verified connection-limit precedence with focused gateway coverage, the gateway WebSocket filtered suite, serving WebSocket suite, Clippy, formatting, diff whitespace checks, and the full test suite.

Observed result: all verification commands passed. The gateway WebSocket filtered suite now covers 39 tests, including wrapped connection-limit status precedence and one-shot/pool `response.failed` connection-limit handling. The serving `responses_websocket` suite remained at 27 passing tests. The full `cargo test` run completed successfully.

Commands:
- `cargo test --test codex_gateway connection_limit`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo test`

Next decision: commit Task 22 as a source-backed connection-limit retryability parity increment. Continue auditing remaining official WebSocket behavior, with live official TLS/opening parity still unproven without a macOS Desktop capture artifact.

Verification status: ready to commit.

### 2026-06-17 Task 21 Wrapped Error Headers Retry-After Audit Start

Command/code area touched: starting comparison of official wrapped WebSocket error `headers` handling against local `CodexWebSocketError::Upstream.retry_after_seconds`.

Observed result: official `WrappedWebsocketErrorEvent` accepts a top-level `headers` object and `map_wrapped_websocket_error_event` converts those JSON headers into `TransportError::Http { headers: Some(...) }` along with status/body. Local `classify_ws_error_frame` currently preserves status and original body but drops wrapped `headers`; callers only compute `retry_after_seconds` from the body. If upstream sends `{"type":"error","status":429,"headers":{"retry-after":"37"},...}` without body reset fields, local returns `retry_after_seconds: None`.

Next decision: add a focused gateway test for wrapped error `headers.retry-after`, then parse that header from wrapped error JSON and use it before body-derived retry-after.

Verification status: red test pending.

### 2026-06-17 Task 21 Wrapped Error Headers Retry-After Implementation Result

Command/code area touched: added `websocket_wrapped_error_retry_after_header_should_be_preserved_like_official_client` in `tests/codex_gateway/websocket.rs`; added wrapped error header parsing in `src/codex/gateway/transport/websocket/codec.rs`; updated first-frame and forwarding-loop `CodexWebSocketError::Upstream` construction in `src/codex/gateway/transport/websocket/mod.rs`.

Observed result: the focused test first failed with `retry_after_seconds: None` when upstream sent `{"type":"error","status":429,"headers":{"retry-after":"37"}}`. Implementation now parses top-level wrapped error `headers.retry-after` case-insensitively from string, number, or first array item, filters zero values, and uses that value before falling back to body-derived `resets_in_seconds` / `resets_at`.

Verification status: `cargo test --test codex_gateway websocket_wrapped_error_retry_after_header_should_be_preserved_like_official_client` passed. Broader verification pending.

### 2026-06-17 Task 21 Wrapped Error Headers Retry-After Verification Result

Command/code area touched: verified wrapped error header retry-after preservation with focused gateway coverage, gateway WebSocket filtered suite, serving WebSocket suite, Clippy, formatting, diff whitespace checks, and the full test suite.

Observed result: all verification commands passed. The gateway WebSocket filtered suite now covers 38 tests, including wrapped error status, mid-stream wrapped error status, and wrapped `headers.retry-after` handling. The serving `responses_websocket` suite remained at 27 passing tests. The full `cargo test` run completed successfully.

Commands:
- `cargo test --test codex_gateway websocket_wrapped_error_retry_after_header_should_be_preserved_like_official_client`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo test`

Next decision: commit Task 21 as a source-backed wrapped error header parity increment. Continue auditing remaining official WebSocket behavior, especially status alias coverage and Desktop-only internal events, without claiming live TLS parity until a macOS Desktop capture artifact exists.

Verification status: ready to commit.

### 2026-06-17 Task 20 Responses Event Filter Audit Start

Command/code area touched: starting an audit of official `process_responses_event` branches that return `Ok(None)` or transform stream events before Codex core consumes them, comparing those paths against local WebSocket-to-SSE forwarding.

Observed result: pending. Important constraint: this proxy intentionally exposes an OpenAI-compatible SSE surface, while official Desktop converts some events into internal `ResponseEvent` variants. The audit should only produce code changes when the difference is source-backed and externally observable as incorrect proxy behavior, not merely because Desktop suppresses an event internally.

Next decision: enumerate official `Ok(None)` / transformed branches, map them to local codec/forwarder behavior, and identify whether any should be suppressed, transformed, or left as external SSE pass-through.

Verification status: analysis in progress.

### 2026-06-17 Task 20 Mid-Stream Wrapped Error Audit Finding

Command/code area touched: compared official `run_websocket_response_stream` against local `create_response_via_websocket_stream_inner` and `forward_websocket_as_sse`.

Observed result: official checks `parse_wrapped_websocket_error_event` and maps non-success `status` / `status_code` before parsing every WebSocket text frame. Local checks `classify_ws_error_frame` only while searching for the first externally forwarded event. After the first event has been emitted and `forward_websocket_as_sse` is running, a later wrapped error frame is currently terminal only because `is_terminal_websocket_event("error")` is true, but it is forwarded as `event: error` instead of surfacing as a stream error/upstream HTTP error body. This is externally observable and source-backed.

Next decision: add a focused gateway streaming test where `response.created` is followed by `{"type":"error","status":400,...}`, confirm the current behavior is wrong, then reuse the existing classifier in the forwarding loop.

Verification status: red test pending.

### 2026-06-17 Task 20 Mid-Stream Wrapped Error Implementation Result

Command/code area touched: added `websocket_midstream_wrapped_error_status_should_surface_as_upstream_error_like_official_client` in `tests/codex_gateway/websocket.rs`; updated `forward_websocket_as_sse` in `src/codex/gateway/transport/websocket/mod.rs`.

Observed result: the focused test first failed because local returned an OK response body containing both `event: response.created` and `event: error` for a mid-stream wrapped error with `status: 400`. Implementation now applies `classify_ws_error_frame` to every forwarded WebSocket text frame after internal rate-limit capture and before SSE encoding. Classified errors are sent as `CodexWebSocketError::Upstream`, preserving the original JSON payload and `retry_after` extraction; connection-fatal classifications discard pooled connections while non-fatal errors finish the active turn.

Verification status: `cargo test --test codex_gateway websocket_midstream_wrapped_error_status_should_surface_as_upstream_error_like_official_client` passed. Broader verification pending.

### 2026-06-17 Task 20 Mid-Stream Wrapped Error Verification Result

Command/code area touched: verified the mid-stream wrapped error fix with focused gateway coverage, gateway WebSocket filtered suite, serving WebSocket suite, Clippy, formatting, diff whitespace checks, and the full test suite.

Observed result: all verification commands passed. The gateway WebSocket filtered suite now covers 37 tests, including first-frame and mid-stream wrapped error status handling. The serving `responses_websocket` suite remained at 27 passing tests. The full `cargo test` run completed successfully.

Commands:
- `cargo test --test codex_gateway websocket_midstream_wrapped_error_status_should_surface_as_upstream_error_like_official_client`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo test`

Next decision: commit Task 20 as a source-backed WebSocket mid-stream error parity increment. Continue remaining audit work with care around Desktop-internal `ResponseEvent` transforms versus this proxy's OpenAI-compatible SSE surface.

Verification status: ready to commit.

### 2026-06-17 Task 14 Invalid Text Event Source Parity Start

Command/code area touched: re-fetched official `codex-api/src/endpoint/responses_websocket.rs`, explored local `websocket_message_text`, `websocket_event_type`, `websocket_sse_chunk`, `create_response_via_websocket_stream_inner`, and `forward_websocket_as_sse` with CodeGraph.

Observed result: official `run_websocket_response_stream` handles `Message::Text(text)` by first mapping wrapped websocket error events, then `serde_json::from_str::<ResponsesStreamEvent>(&text)`. If parsing fails, it logs and `continue`s without forwarding anything to the consumer. Current rs extracts an optional `type` with `websocket_event_type`, then always calls `websocket_sse_chunk(&raw, event.as_deref())`; invalid text or text JSON without a type can therefore be emitted downstream as an unnamed SSE event.

Next decision: add a gateway WebSocket test where the upstream sends an invalid text frame before `response.completed`; the invalid frame must not appear in the collected SSE body. Then change the WS event loop to skip text frames that do not parse to a typed event.

Verification status: source-backed difference identified; failing test pending.

### 2026-06-17 Task 14 Invalid Text Implementation Result

Command/code area touched: added `websocket_invalid_text_frame_should_be_ignored_like_official_client` in `tests/codex_gateway/websocket.rs`, then updated `src/codex/gateway/transport/websocket/{mod.rs,codec.rs}`.

Observed result: the failing test first proved current rs forwarded `not-json-from-upstream` as an unnamed SSE event. Implementation now skips text frames that do not parse to a JSON object with a string `type`, before building an SSE chunk. `websocket_sse_chunk` now requires a concrete event name, removing the anonymous event path. Wrapped error classification and internal rate-limit event handling still run before the skip.

Next decision: run formatting, Clippy, focused WebSocket tests, and full suite before committing Task 14.

Verification status: `cargo test --test codex_gateway websocket_invalid_text_frame_should_be_ignored_like_official_client` passed; `cargo test --test codex_gateway websocket_` passed 33 tests; `cargo test --test codex_serving responses_websocket` passed 26 tests.

### 2026-06-17 Task 13 Close Frame Source Parity Start

Command/code area touched: rechecked official `responses_websocket.rs` control-flow handling and local `src/codex/gateway/transport/websocket/{mod.rs,codec.rs}` after Task 12.

Observed result: official `run_websocket_response_stream` treats `Message::Close(_)` as an immediate error with message `websocket closed by server before response.completed`; if the stream ends without a close frame it returns `stream closed before response.completed`. Current rs maps `Message::Close(_)` through `websocket_message_text(..) == Ok(None)`, so a first-frame close can become `EmptyResponse`, and a later close collapses into the generic `ClosedBeforeTerminal` path.

Next decision: add a gateway WebSocket test that sends a server Close frame before any visible response event and expects a WebSocket close-before-completed error instead of `EmptyResponse`. Then update event handling to distinguish Close frames from ignored Ping/Pong/Frame messages.

Verification status: source-backed difference identified; failing test pending.

### 2026-06-17 Task 13 Close Frame Implementation Result

Command/code area touched: added `websocket_close_frame_before_first_event_should_error_like_official_client` in `tests/codex_gateway/websocket.rs`, then updated `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}` and the WebSocket fallback classifier in `src/codex/gateway/transport/http_client.rs`.

Observed result: the failing test first failed to compile because `CodexWebSocketError::ClosedByServerBeforeCompleted` did not exist. Implementation now maps `Message::Close(_)` to `ClosedByServerBeforeCompleted` with official-source message `websocket closed by server before response.completed`, and maps stream termination without a close frame to `StreamClosedBeforeCompleted` with message `stream closed before response.completed`. Both errors are non-HTTP-SSE-downgrade errors. During verification, a serving test showed that a pooled connection may receive the server close from the previous terminal turn on first read during reuse; the first-frame semantic-error path now follows existing stale pooled connection behavior and retries once with a one-shot connection before surfacing the error.

Next decision: run formatting, Clippy, focused WebSocket tests, and full suite before committing Task 13.

Verification status: `cargo test --test codex_gateway websocket_` passed 32 tests; `cargo test --test codex_serving responses_websocket` passed 26 tests.

### 2026-06-17 Task 13 Final Verification

Command/code area touched: ran `cargo fmt --all --check`, `cargo clippy --all-targets --all-features --locked -- -D warnings`, `git diff --check`, `cargo test --test codex_gateway websocket_`, `cargo test --test codex_serving responses_websocket`, and full `cargo test`.

Observed result: all commands passed. Full `cargo test` passed unit tests, all integration suites, and doc tests. The gateway WebSocket filtered suite now covers 32 tests, including binary-frame rejection, Ping/Pong observation, server Close frame before first visible event, and Close frame after partial streaming.

Next decision: commit Task 13. After this, remaining 1:1 work should continue from source-backed differences or a macOS environment that can execute the official bundled binary for live TLS/opening sampling.

Verification status: ready to commit.

### 2026-06-17 Task 14 Final Verification

Command/code area touched: ran `cargo fmt --all --check`, `cargo clippy --all-targets --all-features --locked -- -D warnings`, `git diff --check`, `cargo test --test codex_gateway websocket_invalid_text_frame_should_be_ignored_like_official_client`, `cargo test --test codex_gateway websocket_`, `cargo test --test codex_serving responses_websocket`, and full `cargo test`.

Observed result: all commands passed. The focused invalid-text test confirms upstream text that cannot be parsed into a typed `ResponsesStreamEvent` is ignored and not forwarded as anonymous SSE data. The gateway WebSocket filtered suite now covers 33 tests after adding invalid-text source parity.

Next decision: commit Task 14 as a small source-backed WebSocket stream-loop parity increment, then continue with the next official-source-backed delta. Live official TLS/opening sampling is still unavailable on this Linux host because the bundled Desktop `codex` binary is macOS arm64 Mach-O.

Verification status: ready to commit.

### 2026-06-17 Task 15 ResponseEvent Side-Channel Audit Start

Command/code area touched: starting a source-backed comparison of official `ResponseEvent` side-channel handling in `codex-api/src/endpoint/responses_websocket.rs` against local gateway WebSocket forwarding and serving behavior.

Observed result: pending. The current hypothesis is that official Desktop consumes some typed WebSocket events internally before exposing stream events to the caller; if local proxy forwards those same side-channel events downstream as SSE, it may differ from Desktop behavior. This task will not change behavior unless the official source and local tests show an observable delta.

Next decision: inspect official event loop and local event classification/forwarding paths, then add a failing test only if a concrete mismatch exists.

Verification status: analysis in progress.

### 2026-06-17 Task 15 Metadata Turn State Red Test

Command/code area touched: inspected official `codex-api/src/sse/responses.rs` and `codex-api/src/endpoint/responses_websocket.rs`; added `websocket_metadata_event_should_update_turn_state_without_forwarding` to `tests/codex_gateway/websocket.rs`.

Observed result: official WebSocket handling parses `response.metadata`, extracts `x-codex-turn-state` from its `headers`, and then `process_responses_event` returns `Ok(None)` for the metadata frame unless side-channel fields are emitted internally. Local gateway previously forwarded any typed event as SSE and only captured turn state from the WebSocket upgrade response headers. The new focused test fails with `left: None, right: Some("turn-from-metadata")`, proving local metadata turn state is dropped.

Next decision: add a local metadata helper that captures `response.metadata.headers.x-codex-turn-state`, skip raw metadata forwarding, and carry the updated turn state through non-streaming and streaming completion paths.

Verification status: red test confirmed.

### 2026-06-17 Task 15 Metadata Turn State Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}`, `src/codex/gateway/transport/http_client.rs`, `src/codex/serving/dispatch/{mod.rs,stream_audit.rs}`, `tests/codex_gateway/websocket.rs`, and `tests/codex_serving/responses_websocket.rs`.

Observed result: local WebSocket handling now parses `response.metadata.headers.x-codex-turn-state` using the same string-or-first-array shape as official `json_value_as_string`, stores it in a shared turn-state update slot, and skips forwarding raw `response.metadata` frames as SSE. Non-streaming collection reads the final shared turn state after the body stream completes. Streaming serving audit also reads the final shared turn state at completion before recording response affinity, so later `previous_response_id` continuations can send the metadata-derived `x-codex-turn-state` on a new WebSocket opening.

Verification status: `cargo test --test codex_gateway websocket_metadata_event_should_update_turn_state_without_forwarding` passed; `cargo test --test codex_serving v1_responses_websocket_stream_should_record_metadata_turn_state_for_continuation` passed.

### 2026-06-17 Task 15 Final Verification

Command/code area touched: ran `cargo fmt --all`, `cargo fmt --all --check`, `git diff --check`, `cargo clippy --all-targets --all-features --locked -- -D warnings`, `cargo test --test codex_gateway websocket_`, `cargo test --test codex_serving responses_websocket`, and full `cargo test`.

Observed result: all commands passed. The gateway WebSocket filtered suite now covers 34 tests, and the serving `responses_websocket` suite now covers 27 tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Next decision: commit Task 15 metadata turn-state parity. Remaining source-backed audit items include custom CA / Rustls connector parity and any other official event-loop side channels that can be represented in this proxy without changing the external API contract.

Verification status: ready to commit.

### 2026-06-17 Task 16 Custom CA / Rustls Connector Audit Start

Command/code area touched: starting comparison of official `maybe_build_rustls_client_config_with_custom_ca()` WebSocket TLS path against local `src/codex/gateway/transport/websocket/opening.rs` and HTTP client TLS configuration.

Observed result: pending. Official `responses_websocket.rs` builds an explicit `Connector::Rustls` only when Codex custom CA configuration exists; otherwise it leaves tungstenite to use its default connector. The local opening path has a custom byte-level handshake implementation, so parity depends on whether it can honor the same configured CA source without changing the already-matched opening bytes.

Next decision: inspect local TLS connector construction and global config surface. Add a failing test only if the local code exposes custom CA config that the WebSocket path ignores, or document as not actionable if no such configuration path exists.

Verification status: analysis in progress.

### 2026-06-17 Task 16 Custom CA Implementation Result

Command/code area touched: added `src/codex/gateway/transport/custom_ca.rs` and `tests/fixtures/test-ca.pem`; updated `src/codex/gateway/transport/{mod.rs,http_client.rs,websocket/opening.rs}` and `src/codex/accounts/service/health.rs`.

Observed result: official `codex-client` custom CA support uses `CODEX_CA_CERTIFICATE` first, falls back to `SSL_CERT_FILE`, treats empty values as unset, parses PEM `CERTIFICATE` bundles, starts WebSocket rustls from native roots, and adds the custom CA certificates. Local HTTP and WebSocket paths previously used rustls/native roots only. Local now has a shared custom CA helper with the same env precedence and no-empty-value behavior. The HTTP client builder uses the helper and caches by `(force_http11, selected custom CA env/path)` so connection-pool reuse is preserved without ignoring CA env changes. The byte-for-byte WebSocket opening path still writes its own HTTP/1.1 handshake, but its TLS connector now uses the shared custom CA rustls config when configured and otherwise keeps the native-root behavior.

Verification status: `cargo test custom_ca` passed 5 custom CA unit tests. Full verification pending.

### 2026-06-17 Task 16 Final Verification

Command/code area touched: ran `cargo fmt --all`, `cargo fmt --all --check`, `git diff --check`, `cargo test custom_ca`, `cargo test --test codex_gateway http_client`, `cargo test --test codex_gateway websocket_`, `cargo test --test codex_serving responses_websocket`, `cargo clippy --all-targets --all-features --locked -- -D warnings`, and full `cargo test`.

Observed result: all commands passed after changing the reqwest client cache key to include the selected custom CA env/path. Full `cargo test` passed unit tests, integration suites, and doc tests. The custom CA unit coverage includes env precedence, fallback, empty-value handling, valid PEM rustls config construction, and invalid PEM error mapping.

Next decision: commit Task 16 as official-source-backed TLS connector parity. Remaining 1:1 uncertainty is still live TLS ClientHello/opening sampling from the official macOS Desktop binary, which cannot be executed on this Linux host.

Verification status: ready to commit.

### 2026-06-17 Task 17 Response Error Event Audit Start

Command/code area touched: starting comparison of official `process_responses_event` error-like event branches against local WebSocket terminal/error handling.

Observed result: pending. Official `process_responses_event` turns `response.failed` into an `ApiError` and turns `response.incomplete` into an `ApiError::Stream("Incomplete response returned, reason: ...")`; these are not normal downstream response events in the Desktop API stream. Local gateway currently classifies `response.completed`, `response.failed`, and `error` as terminal, but `response.incomplete` is not terminal and may be forwarded before the connection eventually closes. This task will verify whether `response.incomplete` is a concrete parity gap that can be fixed without changing unrelated serving error semantics.

Next decision: inspect local `is_terminal_websocket_event`, upstream failure parsing, and serving tests around `response.failed` / premature close; add a focused gateway test for `response.incomplete` if the mismatch is confirmed.

Verification status: analysis in progress.

### 2026-06-17 Task 17 Response Incomplete Implementation Result

Command/code area touched: added `websocket_incomplete_event_should_error_like_official_client` in `tests/codex_gateway/websocket.rs`; updated `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}` and `src/codex/gateway/transport/http_client.rs`.

Observed result: official `process_responses_event` maps `response.incomplete` to `ApiError::Stream("Incomplete response returned, reason: ...")` immediately, without exposing the raw event to the Desktop stream consumer. Local previously treated `response.incomplete` as a regular typed SSE event and would only fail later when the socket closed before `response.completed`. The implementation now parses `/response/incomplete_details/reason` with an `unknown` fallback, returns `CodexWebSocketError::IncompleteResponse`, discards the active WebSocket, and keeps the error ineligible for HTTP SSE fallback.

Verification status: `cargo test --test codex_gateway websocket_incomplete_event_should_error_like_official_client` passed. Full verification pending.

### 2026-06-17 Task 17 Final Verification

Command/code area touched: ran `cargo fmt --all`, `cargo fmt --all --check`, `git diff --check`, `cargo test --test codex_gateway websocket_incomplete_event_should_error_like_official_client`, `cargo test --test codex_gateway websocket_`, `cargo test --test codex_serving responses_websocket`, `cargo clippy --all-targets --all-features --locked -- -D warnings`, and full `cargo test`.

Observed result: all commands passed. The gateway WebSocket filtered suite now covers 35 tests after adding `response.incomplete` parity. Full `cargo test` passed unit tests, integration suites, and doc tests.

Next decision: commit Task 17 as a source-backed error-event parity increment. Continue auditing official `process_responses_event` and WebSocket stream side channels for any remaining behavior that is observable locally and aligned with the proxy's external API contract.

Verification status: ready to commit.

### 2026-06-17 Task 18 Upgrade Header Side-Channel Audit Start

Command/code area touched: starting comparison of official WebSocket upgrade response headers `openai-model`, `x-models-etag`, and `x-reasoning-included` against local WebSocket connection metadata and serving response handling.

Observed result: pending. Official `ResponsesWebsocketConnection::stream_request` emits `ResponseEvent::ServerModel`, `ResponseEvent::ModelsEtag`, and `ResponseEvent::ServerReasoningIncluded(true)` before reading per-turn WebSocket frames when those values were present on the upgrade response. Local currently has visible handling for `x-codex-turn-state`, `set-cookie`, and rate-limit headers on the WebSocket handshake; this task will determine whether the missing side channels are observable or useful in this proxy's API surface.

Next decision: inspect `CodexWebSocketConnectionMetadata`, gateway response structs, serving affinity/audit usage, and any existing model-etag/reasoning-included paths in HTTP SSE.

Verification status: analysis in progress.

### 2026-06-17 Task 18 Upgrade Header Side-Channel Audit Result

Command/code area touched: inspected official `responses_websocket.rs`, official `sse/responses.rs`, local `CodexWebSocketConnectionMetadata`, and project-wide references to `openai-model`, `x-models-etag`, `x-reasoning-included`, `ServerModel`, `ModelsEtag`, and `ServerReasoningIncluded`.

Observed result: official Desktop emits these upgrade headers as internal `ResponseEvent` variants consumed by core session/compact logic, not as raw SSE frames. Local proxy has no external response fields or serving-side consumer for these Desktop-only internal events; adding them as SSE would change the OpenAI-compatible surface rather than making the current proxy behavior more faithful. Existing local WebSocket metadata capture remains limited to externally useful values: `x-codex-turn-state`, `set-cookie`, and rate-limit headers.

Next decision: do not add unsupported external fields for Task 18. Continue auditing for differences that are both official-source-backed and observable within the proxy's current API contract.

Verification status: no code change justified.

### 2026-06-17 Task 19 Wrapped Error Status Audit Start

Command/code area touched: starting comparison of official wrapped WebSocket error handling (`{"type":"error","status":...}` and `status_code`) against local `classify_ws_error_frame`.

Observed result: official `responses_websocket.rs` checks wrapped error events before normal `ResponsesStreamEvent` parsing and maps any non-success `status` / `status_code` to a transport HTTP error with the original payload as body. Local classification currently only maps a small set of error codes from `type:error` / `response.failed`; if a wrapped error has a status but no recognized code, it can be emitted downstream as `event: error` instead of surfacing as an upstream error.

Next decision: add a focused gateway WebSocket test for a first text frame `{"type":"error","status":400,...}` and then extend local error classification to honor wrapped non-success status values.

Verification status: red test pending.

### 2026-06-17 Task 19 Wrapped Error Status Implementation Result

Command/code area touched: added `websocket_wrapped_error_status_should_surface_as_upstream_error_like_official_client` in `tests/codex_gateway/websocket.rs`; updated `src/codex/gateway/transport/websocket/codec.rs`.

Observed result: the focused test first failed because local returned an OK SSE body containing `event: error` for a wrapped error with `status: 400`. Implementation now checks `type == "error"` for `status` or `status_code`, maps any non-success status to `ClassifiedWebSocketError`, and keeps the original payload as the upstream error body through existing `CodexWebSocketError::Upstream` handling.

Verification status: `cargo test --test codex_gateway websocket_wrapped_error_status_should_surface_as_upstream_error_like_official_client` passed. Full verification pending.

### 2026-06-17 Task 19 Wrapped Error Status Verification Result

Command/code area touched: verified Task 19 with focused gateway coverage, the gateway WebSocket filtered suite, serving WebSocket suite, Clippy, formatting, diff whitespace checks, and the full test suite.

Observed result: all verification commands passed. The gateway WebSocket filtered suite now covers 36 tests, including the wrapped status error case. The serving `responses_websocket` suite remained at 27 passing tests. The full `cargo test` run completed successfully.

Commands:
- `cargo test --test codex_gateway websocket_wrapped_error_status_should_surface_as_upstream_error_like_official_client`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo test`

Next decision: commit Task 19 as a source-backed WebSocket error parity increment, then continue auditing remaining official Desktop WebSocket stream behavior that is observable through this proxy's API contract.

Verification status: ready to commit.

### 2026-06-17 Task 29 Invalid Reasoning Replay Recovery Follow-up

Command/code area touched: inspected local serving recovery paths after unknown `response.failed` classification changed WebSocket `invalid_encrypted_content` from a successful SSE terminal event into `CodexClientError::Upstream`; updated `src/codex/serving/{responses.rs,dispatch/{stream.rs,fallback.rs,mod.rs}}` and `tests/codex_serving/responses_websocket.rs`.

Observed result: classifying all unknown `response.failed` frames as upstream errors exposed a serving gap. `invalid_encrypted_content` was previously handled only through `CollectedResponse::Failed`, where `ResponsesSseFailure::invalid_reasoning_replay()` evicted cached encrypted reasoning replay. After WebSocket classification, the same frame arrived as `CodexClientError::Upstream`, so the replay cache was not evicted before recovery. A second issue was that the proxy's generic upstream 5xx retry helper treated the new 503 as retryable and resent the same bad replay before the service layer could inspect the error.

Implementation result: `contains_invalid_encrypted_content_signal` is now reusable by the serving entrypoint and fallback classifier. Non-stream `/v1/responses` evicts reasoning replay when a `CodexClientError::Upstream` body carries the invalid encrypted content signal. The fallback classifier now treats this as request recovery with `invalidReasoningReplay`, reusing the existing `StripPreviousResponse` transition so the retry removes implicit resume state. Generic upstream 5xx retries now exclude invalid reasoning replay so the service-level eviction/recovery path runs first.

Focused verification passed:
- `cargo test --test codex_serving v1_responses_websocket_should_evict_reasoning_replay_after_invalid_encrypted_content`
- `cargo test classify_upstream_recovery_action_should_strip_history_after_invalid_reasoning_replay`
- `cargo test is_retryable_upstream_5xx_should_exclude_invalid_reasoning_replay`

Next decision: run the Task 29 broader verification set: gateway WebSocket filtered tests, serving WebSocket suite, formatting, diff check, Clippy, and full `cargo test`.

Verification status: focused tests passed; broader verification pending.

### 2026-06-17 Task 29 Final Verification

Command/code area touched: ran the Task 29 verification suite after formatting and the invalid reasoning replay recovery follow-up.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 44 tests, including unknown `response.failed` classification. Serving WebSocket coverage passed with 27 tests, including same-client-request recovery from `invalid_encrypted_content` after replay eviction and implicit resume stripping. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo fmt --all`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 29 as `fix: classify unknown websocket response failures`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 30 Response Failed Special Error Classification Audit Start

Command/code area touched: starting comparison of official `process_responses_event` special-case handling for `response.failed` errors against local WebSocket error classification and serving fallback semantics.

Observed result: pending. Task 29 aligned the broad unknown `response.failed` fallback with official retryable behavior, but official also has explicit special cases before the retryable fallback, including context-window, quota, cyber policy, invalid prompt, and server overload classifications. Local WebSocket classification already handles some status-like/rotatable codes, but this task must verify whether any official special-case error is still misclassified in a way that affects this proxy's API surface or account/request recovery behavior.

Next decision: inspect official `codex-api/src/sse/responses.rs` special-case predicates and compare them with local `rotatable_error_status`, HTTP SSE failure classification, and serving fallback tests. Add focused red tests only for source-backed observable differences.

Verification status: analysis in progress.

### 2026-06-17 Task 30 Response Failed Special Error Classification Implementation Result

Command/code area touched: inspected official `codex-api/src/sse/responses.rs` `response.failed` special predicates and local `src/codex/gateway/transport/websocket/codec.rs`; added `websocket_response_failed_special_codes_should_use_official_status_classes` in `tests/codex_gateway/websocket.rs` and `v1_responses_websocket_response_failed_quota_should_retry_fallback_account` in `tests/codex_serving/responses_websocket.rs`.

Observed result: official maps `context_length_exceeded`, `insufficient_quota`, `usage_not_included`, `cyber_policy`, `invalid_prompt`, `server_is_overloaded`, and `slow_down` before the generic retryable fallback. Local WebSocket classification already handled overload, but `insufficient_quota`, `quota_exceeded`, `context_length_exceeded`, `invalid_prompt`, `cyber_policy`, and `usage_not_included` fell through to the Task 29 generic 503 mapping. This was observable: a WebSocket `response.failed` with `insufficient_quota` retried the same account as a generic 5xx instead of using the proxy's existing 402 quota-exhausted fallback behavior.

Implementation result: local WebSocket error classification now maps quota codes (`insufficient_quota`, `quota_exceeded`, `quota_exhausted`, `payment_required`) to 402, `usage_not_included` to 429, and fatal request/content-policy style codes (`context_length_exceeded`, `invalid_prompt`, `cyber_policy`, `invalid_request`) to 400. Unknown `response.failed` remains mapped to 503 per Task 29.

Focused verification passed:
- `cargo test --test codex_gateway websocket_response_failed_special_codes_should_use_official_status_classes`
- `cargo test --test codex_serving v1_responses_websocket_response_failed_quota_should_retry_fallback_account`

Next decision: run gateway WebSocket and serving WebSocket suites, formatting, diff check, Clippy, and full `cargo test`.

Verification status: focused tests passed; broader verification pending.

### 2026-06-17 Task 30 Final Verification

Command/code area touched: ran Task 30 verification after formatting the WebSocket special error classification changes.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 45 tests, including official special-code status mapping for `response.failed`. Serving WebSocket coverage passed with 28 tests, including fallback-account retry after a WebSocket `insufficient_quota` frame. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo fmt --all`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 30 as `fix: align websocket response failure special codes`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 35 Response Created Missing Response Audit Start

Command/code area touched: inspected official `codex-api/src/sse/responses.rs`, official `codex-api/src/endpoint/responses_websocket.rs`, local `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}`, and `tests/codex_gateway/websocket.rs`.

Observed result: official WebSocket handling parses each text frame as `ResponsesStreamEvent` before `process_responses_event`. In `process_responses_event`, the `response.created` branch returns `ResponseEvent::Created` only when `event.response.is_some()`; otherwise it returns `Ok(None)` and the WebSocket loop keeps waiting. Local gateway currently converts any typed frame that is not internal metadata/error/completed validation into an SSE chunk, so `{"type":"response.created"}` can become a successful downstream `event: response.created`.

Red test result: added `websocket_created_without_response_should_be_ignored_like_official_client` and ran `cargo test --test codex_gateway websocket_created_without_response_should_be_ignored_like_official_client`. It failed as expected because `response.body` contained `event: response.created`.

Next decision: add a small response-less-created predicate in the WebSocket codec and skip those frames in both first-frame and forwarding-loop paths, matching the official `Ok(None)` behavior.

Verification status: expected red test captured.

### 2026-06-17 Task 35 Response Created Missing Response Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}` and `tests/codex_gateway/websocket.rs`.

Observed result: local WebSocket handling now detects `{"type":"response.created"}` frames with no `response` field and skips them in both the first-frame path and the spawned forwarding loop. This mirrors official `process_responses_event`, where the `response.created` branch only returns `ResponseEvent::Created` when `event.response.is_some()` and otherwise falls through to `Ok(None)`.

Focused verification passed: `cargo test --test codex_gateway websocket_created_without_response_should_be_ignored_like_official_client`.

Next decision: run broader WebSocket gateway and serving WebSocket suites, then formatting, diff check, Clippy, and full `cargo test`.

### 2026-06-17 Task 35 Final Verification

Command/code area touched: ran Task 35 verification after formatting the response-less `response.created` filtering change.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 51 tests, including the new response-less created frame test. Serving WebSocket coverage passed with 28 tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo test --test codex_gateway websocket_created_without_response_should_be_ignored_like_official_client`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 35 as `fix: ignore websocket created frames without response`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 36 Output Text Delta Missing Delta Audit Start

Command/code area touched: inspected official `codex-api/src/sse/responses.rs` `process_responses_event` branches and local `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}`.

Observed result: official `response.output_text.delta` returns `ResponseEvent::OutputTextDelta(delta)` only when the parsed `ResponsesStreamEvent` has a top-level `delta` field. If `delta` is missing, the branch falls through to `Ok(None)` and the WebSocket loop keeps waiting. Local gateway currently forwards any typed non-internal frame as SSE, so `{"type":"response.output_text.delta"}` can be exposed downstream as a successful event.

Red test result: added `websocket_output_text_delta_without_delta_should_be_ignored_like_official_client` and ran `cargo test --test codex_gateway websocket_output_text_delta_without_delta_should_be_ignored_like_official_client`. It failed as expected because `response.body` contained `event: response.output_text.delta`.

Next decision: add a small delta-less output-text predicate in the WebSocket codec and skip those frames in both first-frame and forwarding-loop paths, matching the official `Ok(None)` behavior.

Verification status: expected red test captured.

### 2026-06-17 Task 36 Output Text Delta Missing Delta Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}` and `tests/codex_gateway/websocket.rs`.

Observed result: local WebSocket handling now detects `{"type":"response.output_text.delta"}` frames with no `delta` field and skips them in both the first-frame path and the spawned forwarding loop. This mirrors official `process_responses_event`, where `response.output_text.delta` only returns `ResponseEvent::OutputTextDelta` when `event.delta` exists and otherwise falls through to `Ok(None)`.

Focused verification passed: `cargo test --test codex_gateway websocket_output_text_delta_without_delta_should_be_ignored_like_official_client`.

Next decision: run broader WebSocket gateway and serving WebSocket suites, then formatting, diff check, Clippy, and full `cargo test`.

Verification status: focused test passed; broader verification pending.

### 2026-06-17 Task 36 Final Verification

Command/code area touched: ran Task 36 verification after formatting the delta-less `response.output_text.delta` filtering change.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 52 tests, including the new delta-less output-text frame test. Serving WebSocket coverage passed with 28 tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo test --test codex_gateway websocket_output_text_delta_without_delta_should_be_ignored_like_official_client`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 36 as `fix: ignore websocket text deltas without delta`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 37 Required Delta Fields Audit Start

Command/code area touched: inspected official `codex-api/src/sse/responses.rs` `process_responses_event` delta branches and local `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}`.

Observed result: official `response.custom_tool_call_input.delta` only emits when both `delta` and either `item_id` or `call_id` exist. Official `response.reasoning_summary_text.delta` only emits when both `delta` and `summary_index` exist. Official `response.reasoning_text.delta` only emits when both `delta` and `content_index` exist. Missing any required field falls through to `Ok(None)`. Local gateway currently forwards any typed non-internal frame as SSE after the existing error/completed/created/output-text filters, so these malformed delta events can be exposed downstream.

Red test result: added `websocket_delta_events_missing_required_fields_should_be_ignored_like_official_client` and ran `cargo test --test codex_gateway websocket_delta_events_missing_required_fields_should_be_ignored_like_official_client`. It failed as expected because `response.body` contained `event: response.custom_tool_call_input.delta`.

Next decision: add a shared predicate that skips only these official-required-field failures in both first-frame and forwarding-loop paths.

Verification status: expected red test captured.

### 2026-06-17 Task 37 Required Delta Fields Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}` and `tests/codex_gateway/websocket.rs`.

Observed result: local WebSocket handling now detects malformed `response.custom_tool_call_input.delta`, `response.reasoning_summary_text.delta`, and `response.reasoning_text.delta` frames that lack the official required fields and skips them in both the first-frame path and the spawned forwarding loop. Normal frames with the required fields remain eligible for existing raw SSE forwarding.

Focused verification passed: `cargo test --test codex_gateway websocket_delta_events_missing_required_fields_should_be_ignored_like_official_client`.

Next decision: run broader WebSocket gateway and serving WebSocket suites, then formatting, diff check, Clippy, and full `cargo test`.

Verification status: focused test passed; broader verification pending.

### 2026-06-17 Task 37 Final Verification

Command/code area touched: ran Task 37 verification after formatting the official-required-field delta filtering change.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 53 tests, including the new required-delta-fields test. Serving WebSocket coverage passed with 28 tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo test --test codex_gateway websocket_delta_events_missing_required_fields_should_be_ignored_like_official_client`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 37 as `fix: ignore malformed websocket delta events`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 38 Output Item Missing Item Audit Start

Command/code area touched: inspected official `codex-api/src/sse/responses.rs` `process_responses_event` branches for `response.output_item.done` and `response.output_item.added`, plus local `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}`.

Observed result: official `response.output_item.done` and `response.output_item.added` only emit internal `ResponseEvent` values when `event.item` exists and `serde_json::from_value::<ResponseItem>(item_val)` succeeds. If `item` is missing, both branches fall through to `Ok(None)`. Local gateway currently forwards typed non-internal frames as raw SSE after the existing malformed-event filters, so item-less `output_item` frames can be exposed downstream.

Red test result: added `websocket_output_item_events_without_item_should_be_ignored_like_official_client` and ran `cargo test --test codex_gateway websocket_output_item_events_without_item_should_be_ignored_like_official_client`. It failed as expected because `response.body` contained `event: response.output_item.done`.

Next decision: add a small predicate that skips item-less `response.output_item.done` and `response.output_item.added` in both first-frame and forwarding-loop paths.

Verification status: expected red test captured.

### 2026-06-17 Task 38 Output Item Missing Item Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}` and `tests/codex_gateway/websocket.rs`.

Observed result: local WebSocket handling now detects item-less `response.output_item.done` and `response.output_item.added` frames and skips them in both the first-frame path and the spawned forwarding loop. This matches the official `Ok(None)` behavior when `event.item` is absent. Full `ResponseItem` parse-equivalence remains a separate audit item because this repository does not currently depend on the official `codex_protocol` model crate.

Focused verification passed: `cargo test --test codex_gateway websocket_output_item_events_without_item_should_be_ignored_like_official_client`.

Next decision: run broader WebSocket gateway and serving WebSocket suites, then formatting, diff check, Clippy, and full `cargo test`.

Verification status: focused test passed; broader verification pending.

### 2026-06-17 Task 38 Final Verification

Command/code area touched: ran Task 38 verification after formatting the item-less `output_item` filtering change.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 54 tests, including the new item-less output-item frame test. Serving WebSocket coverage passed with 28 tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo test --test codex_gateway websocket_output_item_events_without_item_should_be_ignored_like_official_client`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 38 as `fix: ignore websocket output item frames without item`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 39 Reasoning Summary Part Missing Summary Index Audit Start

Command/code area touched: inspected official `codex-api/src/sse/responses.rs` `process_responses_event` branch for `response.reasoning_summary_part.added`, plus local `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}`.

Observed result: official `response.reasoning_summary_part.added` only emits `ResponseEvent::ReasoningSummaryPartAdded` when `event.summary_index` exists. If `summary_index` is missing, the branch falls through to `Ok(None)` and the WebSocket loop keeps waiting. Local gateway currently forwards typed non-internal frames as raw SSE after the existing malformed-event filters, so `{"type":"response.reasoning_summary_part.added"}` can be exposed downstream.

Red test result: added `websocket_reasoning_summary_part_added_without_summary_index_should_be_ignored_like_official_client` and ran `cargo test --test codex_gateway websocket_reasoning_summary_part_added_without_summary_index_should_be_ignored_like_official_client`. It failed as expected because `response.body` contained `event: response.reasoning_summary_part.added`.

Next decision: add a narrow predicate that skips summary-index-less `response.reasoning_summary_part.added` frames in both first-frame and forwarding-loop paths.

Verification status: expected red test captured.

### 2026-06-17 Task 39 Reasoning Summary Part Missing Summary Index Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}` and `tests/codex_gateway/websocket.rs`.

Observed result: local WebSocket handling now detects `{"type":"response.reasoning_summary_part.added"}` frames with no integer `summary_index` and skips them in both the first-frame path and the spawned forwarding loop. This mirrors official `process_responses_event`, where `response.reasoning_summary_part.added` only returns `ResponseEvent::ReasoningSummaryPartAdded` when `event.summary_index` exists.

Focused verification passed: `cargo test --test codex_gateway websocket_reasoning_summary_part_added_without_summary_index_should_be_ignored_like_official_client`.

Next decision: run broader WebSocket gateway and serving WebSocket suites, then formatting, diff check, Clippy, and full `cargo test`.

Verification status: focused test passed; broader verification pending.

### 2026-06-17 Task 39 Final Verification

Command/code area touched: ran Task 39 verification after formatting the summary-index-less `response.reasoning_summary_part.added` filtering change.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 55 tests, including the new summary-index-less reasoning-summary-part test. Serving WebSocket coverage passed with 28 tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo test --test codex_gateway websocket_reasoning_summary_part_added_without_summary_index_should_be_ignored_like_official_client`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 39 as `fix: ignore websocket reasoning summary parts without index`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 40 Nullable Option Fields Audit Start

Command/code area touched: inspected official `codex-api/src/sse/responses.rs` `ResponsesStreamEvent` and `process_responses_event`, official `codex-api/src/endpoint/responses_websocket.rs` frame loop, plus local `src/codex/gateway/transport/websocket/codec.rs`.

Observed result: official `ResponsesStreamEvent` declares `response`, `item`, `delta`, `summary_index`, and related optional fields as `Option`, and the WebSocket loop deserializes text frames into that struct before calling `process_responses_event`. JSON `null` for these fields therefore becomes `None` and follows the same `Ok(None)` ignore path as a missing field. Local skip predicates still use field-presence checks for several already-audited cases, so `response.created` with `response: null`, `response.output_text.delta` with `delta: null`, `output_item` events with `item: null`, and `response.completed` with `response: null` can diverge from the official missing-field behavior.

Red test result: added `websocket_null_option_fields_should_be_ignored_like_missing_fields_in_official_client` and ran `cargo test --test codex_gateway websocket_null_option_fields_should_be_ignored_like_missing_fields_in_official_client`. It failed as expected because local returned `CodexWebSocketError::InvalidCompletedResponse` for `response.completed` with `response: null` instead of ignoring it like official `Option<Value>::None`.

Next decision: update the existing official-shaped skip predicates to treat null as absent only for these source-backed fields.

Verification status: expected red test captured.

### 2026-06-17 Task 40 Nullable Option Fields Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/codec.rs` and `tests/codex_gateway/websocket.rs`.

Observed result: local WebSocket skip predicates now treat JSON `null` as absent for the already source-backed optional fields `response`, `delta`, and `item`. This covers `response.created`, `response.completed`, `response.output_text.delta`, `response.output_item.done`, and `response.output_item.added` without expanding into wrong-type parse behavior, which remains a separate audit item.

Focused verification passed: `cargo test --test codex_gateway websocket_null_option_fields_should_be_ignored_like_missing_fields_in_official_client`.

Next decision: run broader WebSocket gateway and serving WebSocket suites, then formatting, diff check, Clippy, and full `cargo test`.

Verification status: focused test passed; broader verification pending.

### 2026-06-17 Task 40 Final Verification

Command/code area touched: ran Task 40 verification after formatting the nullable official `Option` field filtering change.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 56 tests, including the new null-valued option field test. Serving WebSocket coverage passed with 28 tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo test --test codex_gateway websocket_null_option_fields_should_be_ignored_like_missing_fields_in_official_client`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 40 as `fix: treat null websocket option fields as missing`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 41 Official Event Shape Parse Audit Start

Command/code area touched: inspected official `codex-api/src/endpoint/responses_websocket.rs` WebSocket text-frame loop, official `codex-api/src/sse/responses.rs` `ResponsesStreamEvent`, plus local `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}`.

Observed result: official WebSocket handling runs `serde_json::from_str::<ResponsesStreamEvent>(&text)` before `process_responses_event`; if deserialization fails, it logs and `continue`s. This means typed frames with invalid official top-level field types, such as `{"type":"response.output_text.delta","delta":123}`, are ignored rather than forwarded. Local handling currently extracts the string `type` from raw JSON and can forward such typed frames as SSE unless an existing predicate catches them.

Red test result: added `websocket_frames_with_invalid_official_event_shape_should_be_ignored_like_official_client` and ran `cargo test --test codex_gateway websocket_frames_with_invalid_official_event_shape_should_be_ignored_like_official_client`. It failed as expected because `response.body` contained `event: response.output_text.delta`.

Next decision: add a local official-shape parse guard that skips only frames which fail the same top-level `ResponsesStreamEvent` deserialization.

Verification status: expected red test captured.

### 2026-06-17 Task 41 Official Event Shape Parse Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}` and `tests/codex_gateway/websocket.rs`.

Observed result: local WebSocket handling now validates typed text frames against a local `ResponsesStreamEvent` top-level shape before raw SSE forwarding. Frames that fail the same official top-level deserialization, such as `response.output_text.delta` with numeric `delta`, are skipped in both first-frame and forwarding-loop paths. Downstream official per-event parsing remains separate; for example, `response.completed.response` is still validated by the existing `ResponseCompleted` parse check.

Focused verification passed: `cargo test --test codex_gateway websocket_frames_with_invalid_official_event_shape_should_be_ignored_like_official_client`.

Next decision: run broader WebSocket gateway and serving WebSocket suites, then formatting, diff check, Clippy, and full `cargo test`.

Verification status: focused test passed; broader verification pending.

### 2026-06-17 Task 41 Final Verification

Command/code area touched: ran Task 41 verification after formatting the official event-shape parse guard.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 57 tests, including the new invalid official event shape test. Serving WebSocket coverage passed with 28 tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo test --test codex_gateway websocket_frames_with_invalid_official_event_shape_should_be_ignored_like_official_client`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 41 as `fix: ignore malformed websocket event shapes`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 42 Output Item Non-Object Item Audit Start

Command/code area touched: inspected official `codex-api/src/sse/responses.rs` `process_responses_event` branches for `response.output_item.done` and `response.output_item.added`, official output-item tests in `codex-api/src/sse/responses.rs`, and local `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}`.

Observed result: official `output_item` branches only emit when `event.item` exists and `serde_json::from_value::<ResponseItem>(item_val)` succeeds. Official examples parse object-shaped items such as message and tool-search-call records. A primitive `item` value cannot satisfy the official `ResponseItem` enum shape, so official handling falls through to `Ok(None)`. Local handling now validates only top-level `ResponsesStreamEvent`, where `item: Value` accepts primitives, then forwards typed frames unless the item is missing/null.

Scope note: this task only aligns the clear non-object `item` parse failure. Full official `ResponseItem` parse-equivalence remains separate because this repository still does not depend on the official `codex_protocol` model crate.

Red test result: added `websocket_output_item_events_with_non_object_item_should_be_ignored_like_official_client` and ran `cargo test --test codex_gateway websocket_output_item_events_with_non_object_item_should_be_ignored_like_official_client`. It failed as expected because `response.body` contained `event: response.output_item.done`.

Next decision: skip present non-object `item` values in both WebSocket forwarding paths.

Verification status: expected red test captured.

### 2026-06-17 Task 42 Output Item Non-Object Item Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}` and `tests/codex_gateway/websocket.rs`.

Observed result: local WebSocket handling now skips `response.output_item.done` and `response.output_item.added` frames whose `item` field is present but not an object, in both first-frame and forwarding-loop paths. This matches the official behavior for primitive `item` values because official `serde_json::from_value::<ResponseItem>` cannot parse them and falls through to `Ok(None)`.

Focused verification passed: `cargo test --test codex_gateway websocket_output_item_events_with_non_object_item_should_be_ignored_like_official_client`.

Next decision: run broader WebSocket gateway and serving WebSocket suites, then formatting, diff check, Clippy, and full `cargo test`.

Verification status: focused test passed; broader verification pending.

### 2026-06-17 Task 42 Final Verification

Command/code area touched: ran Task 42 verification after formatting the non-object `output_item.item` filtering change.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 58 tests, including the new non-object output-item test. Serving WebSocket coverage passed with 28 tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo test --test codex_gateway websocket_output_item_events_with_non_object_item_should_be_ignored_like_official_client`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 42 as `fix: ignore websocket output item frames with non-object items`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 43 Output Item Missing Type Tag Audit Start

Command/code area touched: inspected official `codex-api/src/sse/responses.rs` `output_item` branches and official `codex-rs/protocol/src/models.rs` from git tree via `git show HEAD:codex-rs/protocol/src/models.rs`, plus local `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}`.

Observed result: official `process_responses_event` only emits `OutputItemDone`/`OutputItemAdded` when `serde_json::from_value::<ResponseItem>(item_val)` succeeds. Official `ResponseItem` is declared as `#[serde(tag = "type", rename_all = "snake_case")]`; its `Other` variant is `#[serde(other)]`, so unknown string `type` values can still parse as `Other`, but missing `type` or non-string `type` cannot satisfy the tagged enum shape. Local handling currently forwards object-shaped `item` values even when `item.type` is absent or numeric.

Red test result: added `websocket_output_item_events_with_invalid_item_type_tag_should_be_ignored_like_official_client` and ran `cargo test --test codex_gateway websocket_output_item_events_with_invalid_item_type_tag_should_be_ignored_like_official_client`. It failed as expected because `response.body` contained `event: response.output_item.done`.

Next decision: skip `output_item` frames whose object `item.type` is absent or non-string in both WebSocket forwarding paths, without filtering unknown string `type` values.

Verification status: expected red test captured.

### 2026-06-17 Task 43 Output Item Missing Type Tag Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}` and `tests/codex_gateway/websocket.rs`.

Observed result: local WebSocket handling now skips `response.output_item.done` and `response.output_item.added` frames whose object-shaped `item` has no string `type` tag, in both first-frame and forwarding-loop paths. Unknown string `type` values remain eligible for forwarding because official `ResponseItem` includes `#[serde(other)] Other`.

Focused verification passed: `cargo test --test codex_gateway websocket_output_item_events_with_invalid_item_type_tag_should_be_ignored_like_official_client`.

Next decision: run broader WebSocket gateway and serving WebSocket suites, then formatting, diff check, Clippy, and full `cargo test`.

Verification status: focused test passed; broader verification pending.

### 2026-06-17 Task 43 Final Verification

Command/code area touched: ran Task 43 verification after formatting the invalid `output_item.item.type` tag filtering change.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 59 tests, including the new invalid output-item type tag test. Serving WebSocket coverage passed with 28 tests. Full `cargo test` initially exposed an unrelated transient failure in `codex::tasks::token_refresh::tests::do_refresh_inner_should_restore_refreshing_account_after_transient_failures`; that test passed when rerun directly, and the full suite passed on rerun.

Commands:
- `cargo test --test codex_gateway websocket_output_item_events_with_invalid_item_type_tag_should_be_ignored_like_official_client`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`
- `cargo test codex::tasks::token_refresh::tests::do_refresh_inner_should_restore_refreshing_account_after_transient_failures`
- `cargo test`

Next decision: commit Task 43 as `fix: ignore websocket output item frames with invalid type tags`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 44 Message Output Item Shape Audit Start

Command/code area touched: inspected official `codex-rs/protocol/src/models.rs` from git tree and local `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}`.

Observed result: official `process_responses_event` still emits `OutputItemDone`/`OutputItemAdded` only after `serde_json::from_value::<ResponseItem>(item_val)` succeeds. For known `type: "message"` items, official `ResponseItem::Message` requires `role: String` and `content: Vec<ContentItem>`; missing or non-string `role`, and missing or non-array `content`, fail official parsing. Local filtering now validates the top-level event shape and item type tag, but still forwards known `message` items whose required fields are malformed.

Next decision: add a focused gateway WebSocket red test using malformed `message` output items, then add a scoped local predicate for the required `message` fields.

Red test result: added `websocket_message_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client` and ran `cargo test --test codex_gateway websocket_message_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`. It failed as expected because `response.body` contained `event: response.output_item.done`.

Next decision: skip known `message` output-item frames whose object `item.role` is not a string or whose `item.content` is not an array, in both WebSocket forwarding paths.

Verification status: expected red test captured.

### 2026-06-17 Task 44 Message Output Item Shape Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}` and `tests/codex_gateway/websocket.rs`.

Observed result: local WebSocket handling now skips `response.output_item.done` and `response.output_item.added` frames whose known `item.type == "message"` item lacks a string `role` or array `content`, in both first-frame and forwarding-loop paths. Unknown string item types remain eligible for forwarding because official `ResponseItem` maps unknown tags to `Other`.

Focused verification passed: `cargo test --test codex_gateway websocket_message_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`.

Next decision: run broader WebSocket gateway and serving WebSocket suites, then formatting, diff check, Clippy, and full `cargo test`.

Verification status: focused test passed; broader verification pending.

### 2026-06-17 Task 44 Final Verification

Command/code area touched: ran Task 44 verification after formatting the malformed `message` output-item filtering change.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 60 tests, including the new malformed `message` output-item test. Serving WebSocket coverage passed with 28 tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo test --test codex_gateway websocket_message_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 44 as `fix: ignore malformed websocket message output items`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 45 Function Call Output Item Shape Audit Start

Command/code area touched: inspected official `codex-rs/protocol/src/models.rs` from git tree and local `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}`.

Observed result: official `ResponseItem::FunctionCall` is a known tagged variant and requires `name: String`, `arguments: String`, and `call_id: String`; `id`, `namespace`, and `metadata` are optional. Because official WebSocket event processing emits output-item events only after `serde_json::from_value::<ResponseItem>(item_val)` succeeds, malformed `function_call` items with missing or non-string required fields are ignored. Local filtering currently validates only the item tag and the `message` required fields, so malformed known `function_call` items can still be forwarded.

Next decision: add a focused gateway WebSocket red test using malformed `function_call` output items, then add a scoped local predicate for the required `function_call` fields.

Red test result: added `websocket_function_call_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client` and ran `cargo test --test codex_gateway websocket_function_call_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`. It failed as expected because `response.body` contained `event: response.output_item.done`.

Next decision: skip known `function_call` output-item frames whose object `item.name`, `item.arguments`, or `item.call_id` is not a string, in both WebSocket forwarding paths.

Verification status: expected red test captured.

### 2026-06-17 Task 45 Function Call Output Item Shape Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}` and `tests/codex_gateway/websocket.rs`.

Observed result: local WebSocket handling now skips `response.output_item.done` and `response.output_item.added` frames whose known `item.type == "function_call"` item lacks string `name`, `arguments`, or `call_id`, in both first-frame and forwarding-loop paths. Unknown string item types remain eligible for forwarding because official `ResponseItem` maps unknown tags to `Other`.

Focused verification passed: `cargo test --test codex_gateway websocket_function_call_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`.

Next decision: run broader WebSocket gateway and serving WebSocket suites, then formatting, diff check, Clippy, and full `cargo test`.

Verification status: focused test passed; broader verification pending.

### 2026-06-17 Task 45 Final Verification

Command/code area touched: ran Task 45 verification after formatting the malformed `function_call` output-item filtering change.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 61 tests, including the new malformed `function_call` output-item test. Serving WebSocket coverage passed with 28 tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo test --test codex_gateway websocket_function_call_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 45 as `fix: ignore malformed websocket function call output items`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 46 Tool Search Call Output Item Shape Audit Start

Command/code area touched: inspected official `codex-rs/protocol/src/models.rs` from git tree and local `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}`.

Observed result: official `ResponseItem::ToolSearchCall` is a known tagged variant with optional `id`, optional `call_id`, optional `status`, required `execution: String`, and required `arguments: serde_json::Value`. Because `arguments` is `Value`, a present `null`, object, array, string, number, or bool can parse, but a missing `arguments` field cannot. Local filtering currently validates only `message` and `function_call` known-item required fields, so malformed known `tool_search_call` items with missing/non-string `execution` or missing `arguments` can still be forwarded.

Next decision: add a focused gateway WebSocket red test using malformed `tool_search_call` output items, then add a scoped local predicate for the required `tool_search_call` fields.

Red test result: added `websocket_tool_search_call_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client` and ran `cargo test --test codex_gateway websocket_tool_search_call_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`. It failed as expected because `response.body` contained `event: response.output_item.done`.

Next decision: skip known `tool_search_call` output-item frames whose object `item.execution` is not a string or whose `item.arguments` field is absent, in both WebSocket forwarding paths. Do not skip present `arguments: null` because official `serde_json::Value` accepts it.

Verification status: expected red test captured.

### 2026-06-17 Task 46 Tool Search Call Output Item Shape Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}` and `tests/codex_gateway/websocket.rs`.

Observed result: local WebSocket handling now skips `response.output_item.done` and `response.output_item.added` frames whose known `item.type == "tool_search_call"` item lacks string `execution` or lacks the required `arguments` field, in both first-frame and forwarding-loop paths. Present `arguments: null` remains eligible because official `serde_json::Value` accepts null values.

Focused verification passed: `cargo test --test codex_gateway websocket_tool_search_call_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`.

Next decision: run broader WebSocket gateway and serving WebSocket suites, then formatting, diff check, Clippy, and full `cargo test`.

Verification status: focused test passed; broader verification pending.

### 2026-06-17 Task 46 Final Verification

Command/code area touched: ran Task 46 verification after formatting the malformed `tool_search_call` output-item filtering change.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 62 tests, including the new malformed `tool_search_call` output-item test. Serving WebSocket coverage passed with 28 tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo test --test codex_gateway websocket_tool_search_call_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 46 as `fix: ignore malformed websocket tool search output items`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 47 Function Call Output Item Shape Audit Start

Command/code area touched: inspected official `codex-rs/protocol/src/models.rs` from git tree and local `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}`.

Observed result: official `ResponseItem::FunctionCallOutput` is a known tagged variant with required `call_id: String` and required `output: FunctionCallOutputPayload`; `metadata` is optional. `FunctionCallOutputPayload` serializes/deserializes its body as either plain text (`String`) or structured content items (`Vec<FunctionCallOutputContentItem>`), so a missing `output` field or a primitive/object output that is not string/array cannot parse. Local filtering currently validates `message`, `function_call`, and `tool_search_call` known-item fields, but still forwards malformed known `function_call_output` items.

Next decision: add a focused gateway WebSocket red test using malformed `function_call_output` output items, then add a scoped local predicate for the required `function_call_output` fields and output wire shape.

Red test result: added `websocket_function_call_output_payload_item_events_with_invalid_required_fields_should_be_ignored_like_official_client` and ran `cargo test --test codex_gateway websocket_function_call_output_payload_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`. It failed as expected because `response.body` contained `event: response.output_item.done`.

Next decision: skip known `function_call_output` output-item frames whose object `item.call_id` is not a string, whose `item.output` field is absent, or whose `item.output` is neither a string nor an array, in both WebSocket forwarding paths.

Verification status: expected red test captured.

### 2026-06-17 Task 47 Function Call Output Item Shape Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}` and `tests/codex_gateway/websocket.rs`.

Observed result: local WebSocket handling now skips `response.output_item.done` and `response.output_item.added` frames whose known `item.type == "function_call_output"` item lacks string `call_id`, lacks `output`, or has an `output` value that is neither string nor array, in both first-frame and forwarding-loop paths. String output and array output remain eligible because they match official `FunctionCallOutputPayload` wire encoding.

Focused verification passed: `cargo test --test codex_gateway websocket_function_call_output_payload_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`.

Next decision: run broader WebSocket gateway and serving WebSocket suites, then formatting, diff check, Clippy, and full `cargo test`.

Verification status: focused test passed; broader verification pending.

### 2026-06-17 Task 47 Final Verification

Command/code area touched: ran Task 47 verification after formatting the malformed `function_call_output` output-item filtering change.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 63 tests, including the new malformed `function_call_output` output-item test. Serving WebSocket coverage passed with 28 tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo test --test codex_gateway websocket_function_call_output_payload_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 47 as `fix: ignore malformed websocket function output items`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 48 Custom Tool Call Output Item Shape Audit Start

Command/code area touched: inspected official `codex-rs/protocol/src/models.rs` from git tree, official `codex-rs/codex-api/src/sse/responses.rs`, local `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}`, and `tests/codex_gateway/websocket.rs`; used `codegraph query` to confirm local predicate/import placement.

Observed result: official `ResponseItem::CustomToolCall` is a known tagged variant with optional `id`, optional `status`, and required `call_id: String`, `name: String`, and `input: String`. Official WebSocket event processing emits `response.output_item.done` / `response.output_item.added` only when `serde_json::from_value::<ResponseItem>(item_val)` succeeds, so malformed known `custom_tool_call` items with missing or non-string required fields are ignored. Local filtering currently validates other known output-item variants but still forwards malformed known `custom_tool_call` items.

Next decision: add a focused gateway WebSocket red test using malformed `custom_tool_call` output items, then add a scoped local predicate for the required `custom_tool_call` fields and wire it into both forwarding paths.

Red test result: added `websocket_custom_tool_call_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client` and ran `cargo test --test codex_gateway websocket_custom_tool_call_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`. It failed as expected because `response.body` contained `event: response.output_item.done`.

Next decision: skip known `custom_tool_call` output-item frames whose object `item.call_id`, `item.name`, or `item.input` is not a string, in both WebSocket forwarding paths.

### 2026-06-17 Task 48 Custom Tool Call Output Item Shape Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}` and `tests/codex_gateway/websocket.rs`.

Observed result: local WebSocket handling now skips `response.output_item.done` and `response.output_item.added` frames whose known `item.type == "custom_tool_call"` item lacks string `call_id`, `name`, or `input`, in both first-frame and forwarding-loop paths. Optional official fields (`id`, `status`, `metadata`) remain unconstrained.

Focused verification passed: `cargo test --test codex_gateway websocket_custom_tool_call_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`.

Next decision: run broader WebSocket gateway and serving WebSocket suites, then formatting, diff check, Clippy, and full `cargo test`.

### 2026-06-17 Task 48 Final Verification

Command/code area touched: ran Task 48 verification after formatting the malformed `custom_tool_call` output-item filtering change.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 64 tests, including the new malformed `custom_tool_call` output-item test. Serving WebSocket coverage passed with 28 tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo test --test codex_gateway websocket_custom_tool_call_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 48 as `fix: ignore malformed websocket custom tool calls`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 49 Custom Tool Call Output Item Shape Audit Start

Command/code area touched: inspected official `codex-rs/protocol/src/models.rs` from git tree and local `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}`.

Observed result: official `ResponseItem::CustomToolCallOutput` is a known tagged variant with required `call_id: String` and required `output: FunctionCallOutputPayload`; optional `name` and `metadata` are not part of this required-field pass. `FunctionCallOutputPayload` uses the same wire encoding as `function_call_output.output`, so `output` must parse as either a plain string or an array of structured content items. Local filtering already validates `function_call_output` payload shape and `custom_tool_call` required fields, but still forwards malformed known `custom_tool_call_output` items.

Next decision: add a focused gateway WebSocket red test using malformed `custom_tool_call_output` output items, then add a scoped local predicate for required `call_id` and `output` wire shape in both forwarding paths.

Red test result: added `websocket_custom_tool_call_output_payload_item_events_with_invalid_required_fields_should_be_ignored_like_official_client` and ran `cargo test --test codex_gateway websocket_custom_tool_call_output_payload_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`. It failed as expected because `response.body` contained `event: response.output_item.done`.

Next decision: skip known `custom_tool_call_output` output-item frames whose object `item.call_id` is not a string, whose `item.output` field is absent, or whose `item.output` is neither a string nor an array, in both WebSocket forwarding paths.

### 2026-06-17 Task 49 Custom Tool Call Output Item Shape Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}` and `tests/codex_gateway/websocket.rs`.

Observed result: local WebSocket handling now skips `response.output_item.done` and `response.output_item.added` frames whose known `item.type == "custom_tool_call_output"` item lacks string `call_id`, lacks `output`, or has an `output` value that is neither string nor array, in both first-frame and forwarding-loop paths. This matches the required-field and payload wire-shape behavior of official `ResponseItem::CustomToolCallOutput`.

Focused verification passed: `cargo test --test codex_gateway websocket_custom_tool_call_output_payload_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`.

Next decision: run broader WebSocket gateway and serving WebSocket suites, then formatting, diff check, Clippy, and full `cargo test`.

### 2026-06-17 Task 49 Final Verification

Command/code area touched: ran Task 49 verification after formatting the malformed `custom_tool_call_output` output-item filtering change.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 65 tests, including the new malformed `custom_tool_call_output` output-item test. Serving WebSocket coverage passed with 28 tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo test --test codex_gateway websocket_custom_tool_call_output_payload_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 49 as `fix: ignore malformed websocket custom tool outputs`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 50 Tool Search Output Item Shape Audit Start

Command/code area touched: inspected official `codex-rs/protocol/src/models.rs` from git tree and local `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}`.

Observed result: official `ResponseItem::ToolSearchOutput` is a known tagged variant with optional `call_id`, required `status: String`, required `execution: String`, and required `tools: Vec<serde_json::Value>`. Because `tools` is a vector, the field must be present as a JSON array; array elements may be any JSON value. Local filtering validates malformed `tool_search_call` items but still forwards malformed known `tool_search_output` items.

Next decision: add a focused gateway WebSocket red test using malformed `tool_search_output` output items, then add a scoped local predicate for required `status`, `execution`, and array-shaped `tools` in both forwarding paths.

Red test result: added `websocket_tool_search_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client` and ran `cargo test --test codex_gateway websocket_tool_search_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`. It failed as expected because `response.body` contained `event: response.output_item.done`.

Next decision: skip known `tool_search_output` output-item frames whose object `item.status` or `item.execution` is not a string, or whose `item.tools` is not an array, in both WebSocket forwarding paths.

### 2026-06-17 Task 50 Tool Search Output Item Shape Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}` and `tests/codex_gateway/websocket.rs`.

Observed result: local WebSocket handling now skips `response.output_item.done` and `response.output_item.added` frames whose known `item.type == "tool_search_output"` item lacks string `status`, lacks string `execution`, or lacks array-shaped `tools`, in both first-frame and forwarding-loop paths. `call_id` remains optional for this required-field pass.

Focused verification passed: `cargo test --test codex_gateway websocket_tool_search_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`.

Next decision: run broader WebSocket gateway and serving WebSocket suites, then formatting, diff check, Clippy, and full `cargo test`.

### 2026-06-17 Task 50 Final Verification

Command/code area touched: ran Task 50 verification after formatting the malformed `tool_search_output` output-item filtering change.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 66 tests, including the new malformed `tool_search_output` output-item test. Serving WebSocket coverage passed with 28 tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo test --test codex_gateway websocket_tool_search_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 50 as `fix: ignore malformed websocket tool search outputs`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 51 Image Generation Call Output Item Shape Audit Start

Command/code area touched: inspected official `codex-rs/protocol/src/models.rs` from git tree and local `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}`.

Observed result: official `ResponseItem::ImageGenerationCall` is a known tagged variant with required `id: String`, required `status: String`, and required `result: String`; `revised_prompt` and `metadata` are optional. Official WebSocket event processing emits output-item events only after `serde_json::from_value::<ResponseItem>(item_val)` succeeds, so malformed known `image_generation_call` items with missing or non-string required fields are ignored. Local filtering currently does not validate this known item variant.

Next decision: add a focused gateway WebSocket red test using malformed `image_generation_call` output items, then add a scoped local predicate for required `id`, `status`, and `result` in both forwarding paths.

Red test result: added `websocket_image_generation_call_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client` and ran `cargo test --test codex_gateway websocket_image_generation_call_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`. It failed as expected because `response.body` contained `event: response.output_item.done`.

Next decision: skip known `image_generation_call` output-item frames whose object `item.id`, `item.status`, or `item.result` is not a string, in both WebSocket forwarding paths.

### 2026-06-17 Task 51 Image Generation Call Output Item Shape Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}` and `tests/codex_gateway/websocket.rs`.

Observed result: local WebSocket handling now skips `response.output_item.done` and `response.output_item.added` frames whose known `item.type == "image_generation_call"` item lacks string `id`, string `status`, or string `result`, in both first-frame and forwarding-loop paths. Optional `revised_prompt` and `metadata` remain unconstrained for this required-field pass.

Focused verification passed: `cargo test --test codex_gateway websocket_image_generation_call_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`.

Next decision: run broader WebSocket gateway and serving WebSocket suites, then formatting, diff check, Clippy, and full `cargo test`.

### 2026-06-17 Task 51 Final Verification

Command/code area touched: ran Task 51 verification after formatting the malformed `image_generation_call` output-item filtering change.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 67 tests, including the new malformed `image_generation_call` output-item test. Serving WebSocket coverage passed with 28 tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo test --test codex_gateway websocket_image_generation_call_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 51 as `fix: ignore malformed websocket image generation calls`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 52 Compaction Output Item Shape Audit Start

Command/code area touched: inspected official `codex-rs/protocol/src/models.rs` from git tree and local `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}`.

Observed result: official `ResponseItem::Compaction` is a known tagged variant with alias `compaction_summary` and required `encrypted_content: String`; `metadata` is optional. Official WebSocket event processing emits output-item events only after `serde_json::from_value::<ResponseItem>(item_val)` succeeds, so known `compaction` or `compaction_summary` items with missing or non-string `encrypted_content` are ignored. Local filtering currently does not validate this known item variant.

Next decision: add a focused gateway WebSocket red test using malformed `compaction` and `compaction_summary` output items, then add a scoped local predicate for required `encrypted_content` in both forwarding paths.

Red test result: added `websocket_compaction_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client` and ran `cargo test --test codex_gateway websocket_compaction_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`. It failed as expected because `response.body` contained `event: response.output_item.done`.

Next decision: skip known `compaction` and `compaction_summary` output-item frames whose object `item.encrypted_content` is not a string, in both WebSocket forwarding paths.

### 2026-06-17 Task 52 Compaction Output Item Shape Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}` and `tests/codex_gateway/websocket.rs`.

Observed result: local WebSocket handling now skips `response.output_item.done` and `response.output_item.added` frames whose known `item.type` is `compaction` or `compaction_summary` and whose `item.encrypted_content` is not a string, in both first-frame and forwarding-loop paths. Optional `metadata` remains unconstrained for this required-field pass.

Focused verification passed: `cargo test --test codex_gateway websocket_compaction_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`.

Next decision: run broader WebSocket gateway and serving WebSocket suites, then formatting, diff check, Clippy, and full `cargo test`.

### 2026-06-17 Task 52 Final Verification

Command/code area touched: ran Task 52 verification after formatting the malformed `compaction` output-item filtering change.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 68 tests, including the new malformed `compaction` output-item test. Serving WebSocket coverage passed with 28 tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo test --test codex_gateway websocket_compaction_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 52 as `fix: ignore malformed websocket compaction items`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 53 Agent Message Output Item Shape Audit Start

Command/code area touched: inspected official `codex-rs/protocol/src/models.rs` from git tree and local `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}`.

Observed result: official `ResponseItem::AgentMessage` is a known tagged variant with required `author: String`, required `recipient: String`, and required `content: Vec<AgentMessageInputContent>`; `metadata` is optional. `AgentMessageInputContent` is itself a tagged enum with `input_text { text: String }` and `encrypted_content { encrypted_content: String }`, but this pass is scoped to the outer required fields already used by local output-item filtering. Official WebSocket event processing emits output-item events only after `serde_json::from_value::<ResponseItem>(item_val)` succeeds, so malformed known `agent_message` items with missing or non-string `author` / `recipient`, or non-array `content`, are ignored. Local filtering currently does not validate this known item variant.

Next decision: add a focused gateway WebSocket red test using malformed `agent_message` output items, then add a scoped local predicate for required `author`, `recipient`, and array-shaped `content` in both forwarding paths.

Red test result: added `websocket_agent_message_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client` and ran `cargo test --test codex_gateway websocket_agent_message_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`. It failed as expected because `response.body` contained `event: response.output_item.done`.

Next decision: skip known `agent_message` output-item frames whose object `item.author` or `item.recipient` is not a string, or whose `item.content` is not an array, in both WebSocket forwarding paths.

### 2026-06-17 Task 53 Agent Message Output Item Shape Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}` and `tests/codex_gateway/websocket.rs`.

Observed result: local WebSocket handling now skips `response.output_item.done` and `response.output_item.added` frames whose known `item.type == "agent_message"` item lacks string `author`, lacks string `recipient`, or lacks array-shaped `content`, in both first-frame and forwarding-loop paths. Optional `metadata` remains unconstrained for this required-field pass.

Focused verification passed: `cargo test --test codex_gateway websocket_agent_message_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`.

Next decision: run broader WebSocket gateway and serving WebSocket suites, then formatting, diff check, Clippy, and full `cargo test`.

### 2026-06-17 Task 53 Final Verification

Command/code area touched: ran Task 53 verification after formatting the malformed `agent_message` output-item filtering change.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 69 tests, including the new malformed `agent_message` output-item test. Serving WebSocket coverage passed with 28 tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo test --test codex_gateway websocket_agent_message_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 53 as `fix: ignore malformed websocket agent messages`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 54 Reasoning Output Item Shape Audit Start

Command/code area touched: inspected official `codex-rs/protocol/src/models.rs` from git tree and local `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}`.

Observed result: official `ResponseItem::Reasoning` is a known tagged variant with `id: String` using `#[serde(default)]`, required `summary: Vec<ReasoningItemReasoningSummary>`, optional/defaulted `content: Option<Vec<ReasoningItemContent>>`, optional `encrypted_content: Option<String>`, and optional `metadata`. Missing `id` parses as an empty string, but present non-string/null `id` does not parse. Missing or null `content` / `encrypted_content` parse as `None`, but present non-array `content` or present non-string `encrypted_content` fails. Local output-item filtering currently does not validate the known `reasoning` variant at all.

Next decision: add a focused gateway WebSocket red test using malformed `reasoning` output items, then add a scoped local predicate for the outer official parse shape in both forwarding paths.

Red test result: added `websocket_reasoning_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client` and ran `cargo test --test codex_gateway websocket_reasoning_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`. It failed as expected because `response.body` contained `event: response.output_item.done`.

Next decision: skip known `reasoning` output-item frames whose object lacks array `summary`, has a present non-string `id`, has a present non-null non-array `content`, or has a present non-null non-string `encrypted_content`, in both WebSocket forwarding paths.

### 2026-06-17 Task 54 Reasoning Output Item Shape Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}` and `tests/codex_gateway/websocket.rs`.

Observed result: local WebSocket handling now skips `response.output_item.done` and `response.output_item.added` frames whose known `item.type == "reasoning"` item lacks array `summary`, has a present non-string `id`, has a present non-null non-array `content`, or has a present non-null non-string `encrypted_content`, in both first-frame and forwarding-loop paths. Missing `id`, missing/null `content`, and missing/null `encrypted_content` remain eligible because they match official serde behavior for this outer shape.

Focused verification passed: `cargo test --test codex_gateway websocket_reasoning_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`.

Next decision: run broader WebSocket gateway and serving WebSocket suites, then formatting, diff check, Clippy, and full `cargo test`.

### 2026-06-17 Task 54 Final Verification

Command/code area touched: ran Task 54 verification after formatting the malformed `reasoning` output-item filtering change.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 70 tests, including the new malformed `reasoning` output-item test. Serving WebSocket coverage passed with 28 tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo test --test codex_gateway websocket_reasoning_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 54 as `fix: ignore malformed websocket reasoning items`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 55 Message Content Item Shape Audit Start

Command/code area touched: inspected official `codex-rs/protocol/src/models.rs` from git tree and local `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}`.

Observed result: official `ResponseItem::Message.content` parses as `Vec<ContentItem>`. `ContentItem` is a `#[serde(tag = "type", rename_all = "snake_case")]` enum with no `serde(other)`: `input_text` requires string `text`, `output_text` requires string `text`, and `input_image` requires string `image_url` with optional `detail: ImageDetail`. `ImageDetail` accepts only lowercase `auto`, `low`, `high`, or `original`. Local filtering currently validates only that `message.content` is an array, so malformed nested content items can still be forwarded.

Next decision: add a focused gateway WebSocket red test using malformed nested `message.content` entries, then extend the existing `message` output-item predicate to reject nested content items that cannot parse as official `ContentItem`.

Red test result: added `websocket_message_output_item_events_with_invalid_content_items_should_be_ignored_like_official_client` and ran `cargo test --test codex_gateway websocket_message_output_item_events_with_invalid_content_items_should_be_ignored_like_official_client`. It failed as expected because `response.body` contained `event: response.output_item.done`.

Next decision: extend the existing `message` output-item predicate to skip known `message` items when any nested `content` entry is not an object, lacks a supported string `type`, lacks required string fields for `input_text`, `output_text`, or `input_image`, or has a present invalid `detail` value.

### 2026-06-17 Task 55 Message Content Item Shape Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/codec.rs` and `tests/codex_gateway/websocket.rs`.

Observed result: local WebSocket handling now skips `response.output_item.done` and `response.output_item.added` frames whose known `item.type == "message"` item has malformed nested `content` entries. The predicate now rejects non-object content entries, missing/unsupported content `type`, missing string `text` for `input_text` / `output_text`, missing string `image_url` for `input_image`, and present invalid `detail` values. Extra fields remain unconstrained, matching serde's default unknown-field behavior.

Focused verification passed: `cargo test --test codex_gateway websocket_message_output_item_events_with_invalid_content_items_should_be_ignored_like_official_client`.

Next decision: run broader WebSocket gateway and serving WebSocket suites, then formatting, diff check, Clippy, and full `cargo test`.

### 2026-06-17 Task 55 Final Verification

Command/code area touched: ran Task 55 verification after formatting the malformed nested `message.content` output-item filtering change.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 71 tests, including the new malformed nested `message.content` output-item test. Serving WebSocket coverage passed with 28 tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo test --test codex_gateway websocket_message_output_item_events_with_invalid_content_items_should_be_ignored_like_official_client`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 55 as `fix: ignore malformed websocket message content items`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 56 Agent Message Content Item Shape Audit Start

Command/code area touched: inspected official `codex-rs/protocol/src/models.rs` from git tree and local `src/codex/gateway/transport/websocket/codec.rs`.

Observed result: official `ResponseItem::AgentMessage.content` parses as `Vec<AgentMessageInputContent>`. `AgentMessageInputContent` is a `#[serde(tag = "type", rename_all = "snake_case")]` enum with no `serde(other)`: `input_text` requires string `text`, and `encrypted_content` requires string `encrypted_content`. Local filtering currently validates only that `agent_message.content` is an array, so malformed nested content items can still be forwarded.

Next decision: add a focused gateway WebSocket red test using malformed nested `agent_message.content` entries, then extend the existing `agent_message` output-item predicate to reject nested content items that cannot parse as official `AgentMessageInputContent`.

Red test result: added `websocket_agent_message_output_item_events_with_invalid_content_items_should_be_ignored_like_official_client` and ran `cargo test --test codex_gateway websocket_agent_message_output_item_events_with_invalid_content_items_should_be_ignored_like_official_client`. It failed as expected because `response.body` contained `event: response.output_item.done`.

Next decision: extend the existing `agent_message` output-item predicate to skip known `agent_message` items when any nested `content` entry is not an object, lacks a supported string `type`, lacks string `text` for `input_text`, or lacks string `encrypted_content` for `encrypted_content`.

### 2026-06-17 Task 56 Agent Message Content Item Shape Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/codec.rs` and `tests/codex_gateway/websocket.rs`.

Observed result: local WebSocket handling now skips `response.output_item.done` and `response.output_item.added` frames whose known `item.type == "agent_message"` item has malformed nested `content` entries. The predicate now rejects non-object content entries, missing/unsupported content `type`, missing string `text` for `input_text`, and missing string `encrypted_content` for `encrypted_content`. Extra fields remain unconstrained, matching serde's default unknown-field behavior.

Focused verification passed: `cargo test --test codex_gateway websocket_agent_message_output_item_events_with_invalid_content_items_should_be_ignored_like_official_client`.

Next decision: run broader WebSocket gateway and serving WebSocket suites, then formatting, diff check, Clippy, and full `cargo test`.

### 2026-06-17 Task 56 Final Verification

Command/code area touched: ran Task 56 verification after adding malformed nested `agent_message.content` output-item filtering. The verification also covers the test-only token refresh scheduler timing stabilization added after a transient full-suite failure exposed virtual-time advancement racing the spawned refresh task.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 72 tests, including the new malformed nested `agent_message.content` output-item test. Serving WebSocket coverage passed with 28 tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo test --test codex_gateway websocket_agent_message_output_item_events_with_invalid_content_items_should_be_ignored_like_official_client`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 56 as `fix: ignore malformed websocket agent message content`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 57 Reasoning Nested Item Shape Audit Start

Command/code area touched: inspected official `codex-rs/protocol/src/models.rs` from git tree via `/tmp/openai-codex-src` and local `src/codex/gateway/transport/websocket/codec.rs`.

Observed result: official `ResponseItem::Reasoning.summary` parses as `Vec<ReasoningItemReasoningSummary>`, where the only tagged variant is `summary_text { text: String }`. Official `ResponseItem::Reasoning.content` parses as `Option<Vec<ReasoningItemContent>>`, where present array entries must be tagged objects with `reasoning_text { text: String }` or `text { text: String }`. Both nested enums have no `serde(other)`, so non-object entries, missing/unsupported `type`, or missing/non-string `text` fail `serde_json::from_value::<ResponseItem>`. Local filtering currently validates that `summary` is an array and present `content` is an array, but it does not validate the nested entries, so malformed nested reasoning items can still be forwarded.

Next decision: add a focused gateway WebSocket red test using malformed nested `reasoning.summary` and `reasoning.content` entries, then extend the existing `reasoning` output-item predicate to reject nested items that cannot parse as the official tagged enums.

Red test result: added `websocket_reasoning_output_item_events_with_invalid_nested_items_should_be_ignored_like_official_client` and ran `cargo test --test codex_gateway websocket_reasoning_output_item_events_with_invalid_nested_items_should_be_ignored_like_official_client`. It failed as expected because `response.body` contained `event: response.output_item.done`.

Next decision: extend the existing `reasoning` output-item predicate to skip known `reasoning` items when any nested `summary` entry is not `summary_text` with string `text`, or any present `content` entry is not `reasoning_text` / `text` with string `text`.

### 2026-06-17 Task 57 Reasoning Nested Item Shape Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/codec.rs` and `tests/codex_gateway/websocket.rs`.

Observed result: local WebSocket handling now skips `response.output_item.done` and `response.output_item.added` frames whose known `item.type == "reasoning"` item has malformed nested `summary` or `content` entries. The predicate now rejects non-object nested entries, missing/unsupported nested `type`, missing string `text` for `summary_text`, and missing string `text` for `reasoning_text` / `text`. Missing/null `content` remains eligible because it matches official `Option<Vec<_>>` serde behavior; extra fields remain unconstrained.

Focused verification passed: `cargo test --test codex_gateway websocket_reasoning_output_item_events_with_invalid_nested_items_should_be_ignored_like_official_client`.

Next decision: run broader WebSocket gateway and serving WebSocket suites, then formatting, diff check, Clippy, and full `cargo test`.

### 2026-06-17 Task 57 Final Verification

Command/code area touched: ran Task 57 verification after formatting the malformed nested `reasoning.summary` / `reasoning.content` output-item filtering change.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 73 tests, including the new malformed nested reasoning output-item test. Serving WebSocket coverage passed with 28 tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo test --test codex_gateway websocket_reasoning_output_item_events_with_invalid_nested_items_should_be_ignored_like_official_client`
- `cargo fmt --all`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 57 as `fix: ignore malformed websocket reasoning content`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 58 Function Output Content Item Shape Audit Start

Command/code area touched: inspected official `codex-rs/protocol/src/models.rs` from git tree via `/tmp/openai-codex-src` and local `src/codex/gateway/transport/websocket/codec.rs`.

Observed result: official `function_call_output.output` and `custom_tool_call_output.output` both parse as `FunctionCallOutputPayload`, whose array body is `Vec<FunctionCallOutputContentItem>`. `FunctionCallOutputContentItem` is a `#[serde(tag = "type", rename_all = "snake_case")]` enum with no `serde(other)`: `input_text` requires string `text`, `input_image` requires string `image_url` and optional `detail: ImageDetail`, and `encrypted_content` requires string `encrypted_content`. Local filtering currently validates only that `output` is a string or array, so malformed structured array entries can still be forwarded.

Next decision: add a focused gateway WebSocket red test using malformed structured `function_call_output.output[]` and `custom_tool_call_output.output[]` entries, then extend the shared payload predicate to reject entries that cannot parse as official `FunctionCallOutputContentItem`.

Red test result: added `websocket_function_output_payload_item_events_with_invalid_content_items_should_be_ignored_like_official_client` and ran `cargo test --test codex_gateway websocket_function_output_payload_item_events_with_invalid_content_items_should_be_ignored_like_official_client`. It failed as expected because `response.body` contained `event: response.output_item.done`.

Next decision: extend both `function_call_output` and `custom_tool_call_output` payload predicates to reject array `output` entries that are non-object, have missing/unsupported `type`, lack string fields required by `input_text`, `input_image`, or `encrypted_content`, or have an invalid present image `detail`.

### 2026-06-17 Task 58 Function Output Content Item Shape Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/codec.rs` and `tests/codex_gateway/websocket.rs`.

Observed result: local WebSocket handling now skips `response.output_item.done` and `response.output_item.added` frames whose known `item.type == "function_call_output"` or `item.type == "custom_tool_call_output"` item uses an array `output` with malformed structured content entries. String `output` remains eligible. Array entries now reject non-object items, missing/unsupported content `type`, missing string `text` for `input_text`, missing string `image_url` for `input_image`, invalid present `detail`, and missing string `encrypted_content` for `encrypted_content`.

Focused verification passed: `cargo test --test codex_gateway websocket_function_output_payload_item_events_with_invalid_content_items_should_be_ignored_like_official_client`.

Next decision: run broader WebSocket gateway and serving WebSocket suites, then formatting, diff check, Clippy, and full `cargo test`.

### 2026-06-17 Task 58 Final Verification

Command/code area touched: ran Task 58 verification after formatting the malformed structured function/custom-tool output content item filtering change.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 74 tests, including the new malformed structured function/custom-tool output content item test. Serving WebSocket coverage passed with 28 tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo test --test codex_gateway websocket_function_output_payload_item_events_with_invalid_content_items_should_be_ignored_like_official_client`
- `cargo fmt --all`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 58 as `fix: ignore malformed websocket function output content`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 59 Local Shell Call Output Item Shape Audit Start

Command/code area touched: inspected official `codex-rs/protocol/src/models.rs` from git tree via `/tmp/openai-codex-src`, plus local `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}` and `tests/codex_gateway/websocket.rs`.

Observed result: official `ResponseItem::LocalShellCall` is a known tagged variant with optional/defaulted `id`, optional `call_id`, required `status: LocalShellStatus`, and required `action: LocalShellAction`. `LocalShellStatus` accepts only `completed`, `in_progress`, or `incomplete`. `LocalShellAction` is a tagged enum whose supported action is `exec`, with required `command: Vec<String>` and optional `timeout_ms`, `working_directory`, `env`, and `user` that still fail parsing when present with the wrong type. Local filtering currently does not validate `local_shell_call`, so malformed known items can still be forwarded even though official `serde_json::from_value::<ResponseItem>` would fall through to `Ok(None)`.

Next decision: add a focused gateway WebSocket red test for malformed `local_shell_call` output items, then add a scoped local predicate for official `LocalShellCall` shape in both forwarding paths.

Red test result: added `websocket_local_shell_call_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client` and ran `cargo test --test codex_gateway websocket_local_shell_call_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`. It failed as expected because `response.body` contained `event: response.output_item.done`.

Next decision: skip known `local_shell_call` output-item frames whose `status` is missing or outside the official enum, whose `action` is not an object tagged as `exec`, whose `action.command` is missing/non-array/contains non-string entries, or whose present optional `id`, `call_id`, `timeout_ms`, `working_directory`, `env`, or `user` fields have non-serde-compatible types.

### 2026-06-17 Task 59 Local Shell Call Output Item Shape Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}` and `tests/codex_gateway/websocket.rs`.

Observed result: local WebSocket handling now skips `response.output_item.done` and `response.output_item.added` frames whose known `item.type == "local_shell_call"` item cannot parse as official `ResponseItem::LocalShellCall`. The predicate rejects missing/invalid `status`, missing/non-object `action`, unsupported `action.type`, missing/non-array/non-string `action.command` entries, and present optional `id`, `call_id`, `timeout_ms`, `working_directory`, `env`, or `user` fields with serde-incompatible types. Optional `metadata` remains unconstrained, matching the scope of the existing known-item passes.

Focused verification passed: `cargo test --test codex_gateway websocket_local_shell_call_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`.

Next decision: run broader WebSocket gateway and serving WebSocket suites, then formatting, diff check, Clippy, and full `cargo test`.

### 2026-06-17 Task 59 Final Verification

Command/code area touched: ran Task 59 verification after formatting the malformed `local_shell_call` output-item filtering change.

Observed result: all verification commands passed. Gateway WebSocket filtered coverage passed with 75 tests, including the new malformed `local_shell_call` output-item test. Serving WebSocket coverage passed with 28 tests. Full `cargo test` passed unit tests, integration suites, and doc tests.

Commands:
- `cargo test --test codex_gateway websocket_local_shell_call_output_item_events_with_invalid_required_fields_should_be_ignored_like_official_client`
- `cargo fmt --all`
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 59 as `fix: ignore malformed websocket local shell calls`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 60 Web Search Call Output Item Shape Audit Start

Command/code area touched: inspected official `codex-rs/protocol/src/models.rs` from git tree via `/tmp/openai-codex-src`, plus local `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}` and `tests/codex_gateway/websocket.rs`.

Observed result: official `ResponseItem::WebSearchCall` is a known tagged variant with optional/defaulted `id`, optional `status`, optional `action: WebSearchAction`, and optional `metadata`. Missing or null `id`, `status`, and `action` parse as `None`, but present non-string `id` / `status` fail. Present `action` must be null or an object with a string tag; known `search` actions accept optional string `query` and optional array-of-string `queries`, `open_page` accepts optional string `url`, and `find_in_page` accepts optional string `url` / `pattern`. `WebSearchAction` has `#[serde(other)] Other`, so unknown string action tags parse successfully. Local filtering currently does not validate `web_search_call`, so malformed known items can still be forwarded even though official `serde_json::from_value::<ResponseItem>` would fall through to `Ok(None)`.

Next decision: add a focused gateway WebSocket red test for malformed `web_search_call` output items, then add a scoped local predicate for official `WebSearchCall` optional-field and action shape in both forwarding paths.

### 2026-06-17 Task 60 Failing Test

Command/code area touched: added `websocket_web_search_call_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client` in `tests/codex_gateway/websocket.rs` and ran the focused test.

Observed result: `cargo test --test codex_gateway websocket_web_search_call_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client` fails as expected. Local still forwards malformed `web_search_call` `response.output_item.done`, proving the current WebSocket forwarding path has no official-shaped `WebSearchCall` optional/action field validation.

Next decision: add a `web_search_call` output-item predicate in `src/codex/gateway/transport/websocket/codec.rs`, reuse existing optional-field helpers where possible, and wire it into both forwarding loops in `mod.rs`.

### 2026-06-17 Task 60 Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/codec.rs`, `src/codex/gateway/transport/websocket/mod.rs`, and the focused gateway WebSocket test.

Observed result: added `web_search_call_output_item_event_invalid_required_fields` for `response.output_item.done|added` frames. It rejects present non-string `id` / `status`, invalid non-null/non-object `action`, missing/non-string action tags, and wrong typed known action fields for `search`, `open_page`, and `find_in_page`; unknown string action tags are still accepted to match official `#[serde(other)] Other`. The predicate is wired into both forwarding paths, and the focused test now passes.

Verification status: focused Task 60 red test is green; broader WebSocket, formatting, Clippy, and full-suite checks still pending.

### 2026-06-17 Task 60 Final Verification

Command/code area touched: ran Task 60 verification after formatting the malformed `web_search_call` output-item filtering change.

Observed result: all requested checks passed.

Commands:
- `cargo test --test codex_gateway websocket_web_search_call_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client`
- `cargo fmt --all`
- `cargo test --test codex_gateway websocket_` (76 passed)
- `cargo test --test codex_serving responses_websocket` (28 passed)
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 60 as `fix: ignore malformed websocket web search calls`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 61 Context Compaction Output Item Shape Audit Start

Command/code area touched: inspected official `codex-rs/protocol/src/models.rs` from git tree via `/tmp/openai-codex-src`, local `src/codex/gateway/transport/websocket/codec.rs`, `tests/codex_gateway/websocket.rs`, and CodeGraph query help after the Task 60 codegraph sync.

Observed result: official `ResponseItem::ContextCompaction` is a known tagged variant with optional/defaulted `encrypted_content: Option<String>` and optional `metadata`. Missing or null `encrypted_content` parses as `None`, but a present non-string value fails official deserialization. Current local compaction filtering only checks `compaction` / `compaction_summary` required `encrypted_content`, so malformed known `context_compaction` output items can still be forwarded as successful SSE even though the official `ResponseItem` parse would fail and be ignored.

Next decision: add a focused red test for malformed `context_compaction` output items, then extend the existing compaction output-item predicate to reject present non-string `encrypted_content` for `context_compaction`.

### 2026-06-17 Task 61 Failing Test

Command/code area touched: added `websocket_context_compaction_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client` in `tests/codex_gateway/websocket.rs` and ran the focused test.

Observed result: `cargo test --test codex_gateway websocket_context_compaction_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client` fails as expected. Local still forwards malformed `context_compaction` `response.output_item.done`, proving the existing compaction predicate only covers `compaction` / `compaction_summary` and not the official `ContextCompaction` optional field shape.

Next decision: extend `compaction_output_item_event_invalid_required_fields` so `context_compaction.encrypted_content` is accepted when missing/null/string and rejected when present with any other type.

### 2026-06-17 Task 61 Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/codec.rs` and `tests/codex_gateway/websocket.rs`.

Observed result: the existing compaction output-item predicate now also handles `item.type == "context_compaction"` and rejects present non-string `encrypted_content` while preserving the official missing/null/string behavior for `Option<String>`. The focused Task 61 test now passes.

Verification status: focused Task 61 red test is green; broader WebSocket, formatting, Clippy, and full-suite checks still pending.

### 2026-06-17 Task 61 Final Verification

Command/code area touched: ran Task 61 verification after formatting the malformed `context_compaction` output-item filtering change.

Observed result: all requested checks passed.

Commands:
- `cargo test --test codex_gateway websocket_context_compaction_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client`
- `cargo fmt --all`
- `cargo test --test codex_gateway websocket_` (77 passed)
- `cargo test --test codex_serving responses_websocket` (28 passed)
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 61 as `fix: ignore malformed websocket context compaction`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 62 Output Item Metadata Shape Audit Start

Command/code area touched: inspected official `codex-rs/protocol/src/models.rs` from git tree via `/tmp/openai-codex-src`, local `src/codex/gateway/transport/websocket/codec.rs`, and current output-item filtering order in `src/codex/gateway/transport/websocket/mod.rs`.

Observed result: official known `ResponseItem` variants share `metadata: Option<ResponseItemMetadata>`, and `ResponseItemMetadata` has optional/defaulted `turn_id: Option<String>`. Missing or null `metadata` parses as `None`; present object metadata parses when `turn_id` is missing/null/string and ignores unknown fields; present non-object metadata or non-string `metadata.turn_id` fails official deserialization. Local output-item filtering validates many variant-specific fields but does not currently validate the shared metadata shape, so an otherwise valid known output item with malformed `metadata` can still be forwarded even though official `serde_json::from_value::<ResponseItem>` would fail and be ignored.

Next decision: add a focused red test for malformed output-item `metadata`, then add a shared output-item metadata predicate before variant-specific filters in both WebSocket forwarding paths.

### 2026-06-17 Task 62 Failing Test

Command/code area touched: added `websocket_output_item_events_with_invalid_metadata_should_be_ignored_like_official_client` in `tests/codex_gateway/websocket.rs` and ran the focused test.

Observed result: `cargo test --test codex_gateway websocket_output_item_events_with_invalid_metadata_should_be_ignored_like_official_client` fails as expected. Local still forwards malformed output-item `metadata` on `response.output_item.done`, proving no shared official metadata-shape check exists before variant-specific forwarding.

Next decision: add a shared `output_item_event_invalid_metadata` predicate that accepts missing/null metadata and object metadata with missing/null/string `turn_id`, rejects non-object metadata and non-string `turn_id`, then wire it into both forwarding paths.

### 2026-06-17 Task 62 Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/{codec.rs,mod.rs}` and `tests/codex_gateway/websocket.rs`.

Observed result: added `output_item_event_invalid_metadata` for `response.output_item.done|added` frames. It rejects present non-object `metadata` and object metadata whose present `turn_id` is non-string, while preserving official missing/null metadata and missing/null/string `turn_id` behavior. The predicate is wired into both forwarding paths immediately after output-item tag validation, and the focused Task 62 test now passes.

Verification status: focused Task 62 red test is green; broader WebSocket, formatting, Clippy, and full-suite checks still pending.

### 2026-06-17 Task 62 Final Verification

Command/code area touched: ran Task 62 verification after formatting the malformed output-item metadata filtering change.

Observed result: all requested checks passed.

Commands:
- `cargo test --test codex_gateway websocket_output_item_events_with_invalid_metadata_should_be_ignored_like_official_client`
- `cargo fmt --all`
- `cargo test --test codex_gateway websocket_` (78 passed)
- `cargo test --test codex_serving responses_websocket` (28 passed)
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 62 as `fix: ignore malformed websocket output metadata`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 63 Message Optional Field Shape Audit Start

Command/code area touched: inspected official `codex-rs/protocol/src/models.rs` from git tree via `/tmp/openai-codex-src`, local `src/codex/gateway/transport/websocket/codec.rs`, and CodeGraph results for `message_output_item_event_invalid_required_fields`.

Observed result: official `ResponseItem::Message` has optional/defaulted `id: Option<String>` and optional/defaulted `phase: Option<MessagePhase>`. Missing or null `id` / `phase` parse as `None`; present non-string `id`, non-string `phase`, or unknown string `phase` fail official deserialization because `MessagePhase` only accepts `commentary` and `final_answer`. Current local message filtering validates required `role`, `content`, nested content items, and shared metadata, but does not validate these optional message fields, so malformed known `message` items can still be forwarded as successful SSE.

Next decision: add a focused red test for malformed `message.id` and `message.phase`, then extend the message output-item predicate to reject present non-string `id` and present `phase` values outside official `MessagePhase`.

### 2026-06-17 Task 63 Failing Test

Command/code area touched: added `websocket_message_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client` in `tests/codex_gateway/websocket.rs` and ran the focused test.

Observed result: `cargo test --test codex_gateway websocket_message_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client` fails as expected. Local still forwards malformed `message` `response.output_item.done`, proving the existing message predicate does not cover official optional `id` / `phase` parse failures.

Next decision: extend `message_output_item_event_invalid_required_fields` to reject present non-string `id`, present non-string `phase`, and present string `phase` values outside official `commentary` / `final_answer`.

### 2026-06-17 Task 63 Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/codec.rs` and `tests/codex_gateway/websocket.rs`.

Observed result: `message_output_item_event_invalid_required_fields` now rejects `message` output items with present non-string `id`, present non-string `phase`, or string `phase` outside official `MessagePhase` values `commentary` / `final_answer`, while preserving missing/null optional field behavior. The focused Task 63 test now passes.

Verification status: focused Task 63 red test is green; broader WebSocket, formatting, Clippy, and full-suite checks still pending.

### 2026-06-17 Task 63 Final Verification

Command/code area touched: ran Task 63 verification after formatting the malformed `message.id` / `message.phase` output-item filtering change.

Observed result: all requested checks passed.

Commands:
- `cargo test --test codex_gateway websocket_message_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client`
- `cargo fmt --all`
- `cargo test --test codex_gateway websocket_` (79 passed)
- `cargo test --test codex_serving responses_websocket` (28 passed)
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 63 as `fix: ignore malformed websocket message optional fields`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 64 Function Call Optional Field Shape Audit Start

Command/code area touched: inspected official `codex-rs/protocol/src/models.rs` from git tree via `/tmp/openai-codex-src`, local `src/codex/gateway/transport/websocket/codec.rs`, and current `function_call` output-item tests.

Observed result: official `ResponseItem::FunctionCall` has optional/defaulted `id: Option<String>` and optional/defaulted `namespace: Option<String>` in addition to required `name`, `arguments`, and `call_id`. Missing or null `id` / `namespace` parse as `None`, but present non-string values fail official deserialization. Current local `function_call` filtering validates only required `name`, `arguments`, and `call_id`, so malformed optional fields can still be forwarded as successful SSE.

Next decision: add a focused red test for malformed `function_call.id` and `function_call.namespace`, then extend the function-call output-item predicate to reject present non-string optional fields.

### 2026-06-17 Task 64 Failing Test

Command/code area touched: added `websocket_function_call_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client` in `tests/codex_gateway/websocket.rs` and ran the focused test.

Observed result: `cargo test --test codex_gateway websocket_function_call_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client` fails as expected. Local still forwards malformed `function_call` `response.output_item.done`, proving the existing function-call predicate does not cover official optional `id` / `namespace` parse failures.

Next decision: extend `function_call_output_item_event_invalid_required_fields` to reject present non-string `id` and `namespace` while preserving missing/null optional field behavior.

### 2026-06-17 Task 64 Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/codec.rs` and `tests/codex_gateway/websocket.rs`.

Observed result: `function_call_output_item_event_invalid_required_fields` now rejects known `function_call` output items with present non-string `id` or `namespace`, while preserving official missing/null optional field behavior. Required `name`, `arguments`, and `call_id` validation remains unchanged. The focused Task 64 test now passes.

Verification status: focused Task 64 red test is green; broader WebSocket, formatting, Clippy, and full-suite checks still pending.

### 2026-06-17 Task 64 Final Verification

Command/code area touched: ran Task 64 verification after formatting the malformed `function_call.id` / `function_call.namespace` output-item filtering change.

Observed result: all requested checks passed.

Commands:
- `cargo test --test codex_gateway websocket_function_call_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client`
- `cargo fmt --all`
- `cargo test --test codex_gateway websocket_` (80 passed)
- `cargo test --test codex_serving responses_websocket` (28 passed)
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 64 as `fix: ignore malformed websocket function call optional fields`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 65 Tool Search Call Optional Field Shape Audit Start

Command/code area touched: inspected official `codex-rs/protocol/src/models.rs` from git tree via `/tmp/openai-codex-src`, local `src/codex/gateway/transport/websocket/codec.rs`, current `tool_search_call` output-item tests, and `codegraph status .`.

Observed result: official `ResponseItem::ToolSearchCall` has optional/defaulted `id: Option<String>`, optional `call_id: Option<String>`, optional/defaulted `status: Option<String>`, required `execution: String`, and required present `arguments: serde_json::Value`. Missing or null `id` / `call_id` / `status` parse as `None`, but present non-string values fail official deserialization. Current local `tool_search_call` filtering validates only required `execution` and present `arguments`, so malformed optional fields can still be forwarded as successful SSE.

Next decision: add a focused red test for malformed `tool_search_call.id`, `call_id`, and `status`, then extend the tool-search-call output-item predicate to reject present non-string optional fields.

### 2026-06-17 Task 65 Failing Test

Command/code area touched: added `websocket_tool_search_call_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client` in `tests/codex_gateway/websocket.rs` and ran the focused test.

Observed result: `cargo test --test codex_gateway websocket_tool_search_call_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client` fails as expected. Local still forwards malformed `tool_search_call` `response.output_item.done`, proving the existing tool-search-call predicate does not cover official optional `id` / `call_id` / `status` parse failures.

Next decision: extend `tool_search_call_output_item_event_invalid_required_fields` to reject present non-string `id`, `call_id`, and `status` while preserving missing/null optional field behavior.

### 2026-06-17 Task 65 Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/codec.rs` and `tests/codex_gateway/websocket.rs`.

Observed result: `tool_search_call_output_item_event_invalid_required_fields` now rejects known `tool_search_call` output items with present non-string `id`, `call_id`, or `status`, while preserving official missing/null optional field behavior. Required `execution` validation and present `arguments` validation remain unchanged. The focused Task 65 test now passes.

Verification status: focused Task 65 red test is green; broader WebSocket, formatting, Clippy, and full-suite checks still pending.

### 2026-06-17 Task 65 Final Verification

Command/code area touched: ran Task 65 verification after formatting the malformed `tool_search_call.id` / `call_id` / `status` output-item filtering change.

Observed result: all requested checks passed.

Commands:
- `cargo test --test codex_gateway websocket_tool_search_call_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client`
- `cargo fmt --all`
- `cargo test --test codex_gateway websocket_` (81 passed)
- `cargo test --test codex_serving responses_websocket` (28 passed)
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 65 as `fix: ignore malformed websocket tool search call optional fields`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 66 Custom Tool Call Optional Field Shape Audit Start

Command/code area touched: inspected official `codex-rs/protocol/src/models.rs` from git tree via `/tmp/openai-codex-src`, local `src/codex/gateway/transport/websocket/codec.rs`, and current `custom_tool_call` output-item tests.

Observed result: official `ResponseItem::CustomToolCall` has optional/defaulted `id: Option<String>`, optional/defaulted `status: Option<String>`, required `call_id: String`, required `name: String`, and required `input: String`. Missing or null `id` / `status` parse as `None`, but present non-string values fail official deserialization. Current local `custom_tool_call` filtering validates only required `call_id`, `name`, and `input`, so malformed optional fields can still be forwarded as successful SSE.

Next decision: add a focused red test for malformed `custom_tool_call.id` and `status`, then extend the custom-tool-call output-item predicate to reject present non-string optional fields.

### 2026-06-17 Task 66 Failing Test

Command/code area touched: added `websocket_custom_tool_call_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client` in `tests/codex_gateway/websocket.rs` and ran the focused test.

Observed result: `cargo test --test codex_gateway websocket_custom_tool_call_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client` fails as expected. Local still forwards malformed `custom_tool_call` `response.output_item.done`, proving the existing custom-tool-call predicate does not cover official optional `id` / `status` parse failures.

Next decision: extend `custom_tool_call_output_item_event_invalid_required_fields` to reject present non-string `id` and `status` while preserving missing/null optional field behavior.

### 2026-06-17 Task 66 Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/codec.rs` and `tests/codex_gateway/websocket.rs`.

Observed result: `custom_tool_call_output_item_event_invalid_required_fields` now rejects known `custom_tool_call` output items with present non-string `id` or `status`, while preserving official missing/null optional field behavior. Required `call_id`, `name`, and `input` validation remains unchanged. The focused Task 66 test now passes.

Verification status: focused Task 66 red test is green; broader WebSocket, formatting, Clippy, and full-suite checks still pending.

### 2026-06-17 Task 66 Final Verification

Command/code area touched: ran Task 66 verification after formatting the malformed `custom_tool_call.id` / `status` output-item filtering change.

Observed result: all requested checks passed.

Commands:
- `cargo test --test codex_gateway websocket_custom_tool_call_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client`
- `cargo fmt --all`
- `cargo test --test codex_gateway websocket_` (82 passed)
- `cargo test --test codex_serving responses_websocket` (28 passed)
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 66 as `fix: ignore malformed websocket custom tool optional fields`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 67 Custom Tool Call Output Optional Field Shape Audit Start

Command/code area touched: inspected official `codex-rs/protocol/src/models.rs` from git tree via `/tmp/openai-codex-src`, local `src/codex/gateway/transport/websocket/codec.rs`, current `custom_tool_call_output` output-item tests, and `codegraph status .`.

Observed result: official `ResponseItem::CustomToolCallOutput` has required `call_id: String`, optional/defaulted `name: Option<String>`, required `output: FunctionCallOutputPayload`, and optional metadata. Missing or null `name` parses as `None`, but a present non-string `name` fails official deserialization. Current local `custom_tool_call_output` filtering validates only required `call_id` and `output` payload shape, so malformed optional `name` can still be forwarded as successful SSE.

Next decision: add a focused red test for malformed `custom_tool_call_output.name`, then extend the custom-tool-call-output predicate to reject present non-string `name`.

### 2026-06-17 Task 67 Failing Test

Command/code area touched: added `websocket_custom_tool_call_output_result_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client` in `tests/codex_gateway/websocket.rs` and ran the focused test.

Observed result: `cargo test --test codex_gateway websocket_custom_tool_call_output_result_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client` fails as expected. Local still forwards malformed `custom_tool_call_output` `response.output_item.done`, proving the existing custom-tool-call-output predicate does not cover official optional `name` parse failures.

Next decision: extend `custom_tool_call_output_payload_item_event_invalid_required_fields` to reject present non-string `name` while preserving missing/null optional field behavior.

### 2026-06-17 Task 67 Implementation Result

Command/code area touched: updated `src/codex/gateway/transport/websocket/codec.rs` and `tests/codex_gateway/websocket.rs`.

Observed result: `custom_tool_call_output_payload_item_event_invalid_required_fields` now rejects known `custom_tool_call_output` output items with present non-string `name`, while preserving official missing/null optional field behavior. Required `call_id` validation and `output` payload validation remain unchanged. The focused Task 67 test now passes.

Verification status: focused Task 67 red test is green; broader WebSocket, formatting, Clippy, and full-suite checks still pending.

### 2026-06-17 Task 67 Final Verification

Command/code area touched: ran Task 67 verification after formatting the malformed `custom_tool_call_output.name` output-item filtering change. During full-suite verification, `do_refresh_inner_should_restore_refreshing_account_after_transient_failures` repeatedly failed because the test helper could return before the paused-time retry loop reached the expected call count; the helper now advances virtual time while waiting and asserts that the expected count was reached.

Observed result: all requested checks passed after the test helper stabilization.

Commands:
- `cargo test --test codex_gateway websocket_custom_tool_call_output_result_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client`
- `cargo test do_refresh_inner_should_restore_refreshing_account_after_transient_failures`
- `cargo fmt --all`
- `cargo test --test codex_gateway websocket_` (83 passed)
- `cargo test --test codex_serving responses_websocket` (28 passed)
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Next decision: commit Task 67 as `fix: ignore malformed websocket custom tool output optional fields`, then mark the plan commit checkbox in a docs-only sync commit.

Verification status: ready to commit.

### 2026-06-17 Task 68 Full Remaining Difference Audit

Command/code area touched: inspected official `codex-api/src/sse/responses.rs`, official `codex-api/src/endpoint/responses_websocket.rs`, official `codex-rs/protocol/src/models.rs` from git tree via `/tmp/openai-codex-src`, local `src/codex/gateway/transport/websocket/codec.rs`, local `tests/codex_gateway/websocket.rs`, and current plan/task history.

Observed result: official `ResponsesStreamEvent` and local `ResponsesStreamEventShape` currently have the same top-level parse-sensitive fields: `type`, `headers`, `metadata`, `response`, `item`, `item_id`, `call_id`, `delta`, `summary_index`, and `content_index`. The remaining source-backed local parse gaps are therefore inside known `ResponseItem` variants, not in the top-level event shape.

Remaining source-backed `must change` items:

| Area | Official shape | Current local behavior | Action |
| --- | --- | --- | --- |
| `ResponseItem::ToolSearchOutput.call_id` | `Option<String>`; missing/null accepted, present non-string fails `ResponseItem` deserialization and official WebSocket skips the output-item event | Local `tool_search_output_item_event_invalid_required_fields` validates only required `status`, `execution`, and `tools`, so present non-string `call_id` can still be forwarded | Add focused red test and reject present non-string `call_id` |
| `ResponseItem::ImageGenerationCall.revised_prompt` | `Option<String>`; missing/null accepted, present non-string fails `ResponseItem` deserialization and official WebSocket skips the output-item event | Local `image_generation_call_output_item_event_invalid_required_fields` validates only required `id`, `status`, and `result`, so present non-string `revised_prompt` can still be forwarded | Add focused red test and reject present non-string `revised_prompt` |

Already source-backed and covered locally:

| Area | Coverage state |
| --- | --- |
| Top-level malformed `ResponsesStreamEvent` fields | `responses_stream_event_shape_parse_error` covers type mismatches for the official top-level fields before raw SSE forwarding |
| Missing/null top-level optional fields | Tasks 34-40 cover missing/null `response`, `delta`, `item`, and summary-index option behavior |
| Required delta fields | Task 37 covers `custom_tool_call_input.delta`, `reasoning_summary_text.delta`, and `reasoning_text.delta` required-field combinations |
| `response.reasoning_summary_part.added` | Task 39 covers required `summary_index` |
| `response.completed` parse shape | Tasks 32-34 cover missing/invalid `response`, required `id`, and required `usage.total_tokens` when `usage` is present |
| `response.failed`, `response.incomplete`, wrapped error frames | Tasks 17, 19-24, 29-31 cover official error classifications, retry-after handling, connection-limit precedence, unknown failures, and overload cases |
| Output-item `item` envelope | Tasks 38, 42, and 43 cover missing/null, non-object, and missing/non-string type tags |
| Shared output-item `metadata.turn_id` | Task 62 covers malformed metadata shape |
| `message` | Tasks 44, 55, and 63 cover required fields, nested content items, optional `id`, and optional `phase` enum shape |
| `agent_message` | Tasks 53 and 56 cover required fields and nested content items |
| `reasoning` | Tasks 54 and 57 cover defaulted `id`, required `summary`, optional `content`, optional `encrypted_content`, and nested items |
| `local_shell_call` | Task 59 covers optional `id`/`call_id`, enum `status`, `action.type == exec`, required `exec.command`, and typed optional `timeout_ms`, `working_directory`, `env`, and `user` |
| `function_call` | Tasks 45 and 64 cover required `name`, `arguments`, `call_id`, optional `id`, and optional `namespace` |
| `tool_search_call` | Tasks 46 and 65 cover required `execution`, present `arguments`, optional `id`, optional `call_id`, and optional `status` |
| `function_call_output` | Tasks 47 and 58 cover required `call_id`, string/array output payload, and structured output content items |
| `custom_tool_call` | Tasks 48 and 66 cover required `call_id`, `name`, `input`, optional `id`, and optional `status` |
| `custom_tool_call_output` | Tasks 49, 58, and 67 cover required `call_id`, optional `name`, string/array output payload, and structured output content items |
| `tool_search_output` required fields | Task 50 covers required `status`, `execution`, and `tools`; only optional `call_id` remains |
| `web_search_call` | Task 60 covers optional `id`, optional `status`, optional `action`, and known action optional fields |
| `image_generation_call` required fields | Task 51 covers required `id`, `status`, and `result`; only optional `revised_prompt` remains |
| `compaction` / `compaction_summary` / `context_compaction` | Tasks 52 and 61 cover required `encrypted_content` for compaction variants and optional `encrypted_content` for context compaction |
| `compaction_trigger` | Official known variant has no fields except optional metadata; shared metadata filtering is sufficient and no extra local predicate is needed |

Not a proxy API surface / do not synthesize public SSE changes:

| Area | Classification |
| --- | --- |
| Official `ResponseEvent::ServerModel`, `ModelsEtag`, `ServerReasoningIncluded`, `ModelVerifications`, and `TurnModerationMetadata` side channels | Desktop-internal `ResponseEvent` values. Local proxy has no matching Desktop UI/session consumer, and adding synthetic public SSE fields would change the OpenAI-compatible proxy surface. Prior Tasks 18 and 25 documented this as no-code. |
| Raw `response.metadata` forwarding | Official extracts side channels and returns `Ok(None)` for metadata frames. Local now captures `x-codex-turn-state` and skips raw metadata forwarding; remaining Desktop-only metadata side channels stay documented rather than exposed. |

Still unproven without a macOS Desktop live capture:

| Area | Current evidence |
| --- | --- |
| TLS ClientHello / ALPN / cipher / extension ordering | Official binary is macOS arm64 Mach-O and cannot run on this Linux host. Source/static evidence confirms rustls/tokio-tungstenite family and custom CA behavior, but live fingerprint parity is not proven. |
| Exact official opening header order/casing and dynamic `Sec-WebSocket-Key` placement | Local capture and audit artifacts exist; official live opening bytes are unavailable on this host. |
| Proxy/environment behavior of the shipped Desktop binary | Static strings show proxy env support, but live validation remains blocked by the Mach-O binary format. |

Next decision: implement the two remaining source-backed `must change` items together or in immediate sequence: `tool_search_output.call_id` and `image_generation_call.revised_prompt`. Keep live TLS/opening parity explicitly unclaimed until a macOS capture artifact exists.

Verification status: documentation-only full audit; no code changes in Task 68.

### 2026-06-17 Task 69 Remaining Optional Output Item Fields

Command/code area touched: inspected official `/tmp/openai-codex-src` `codex-rs/protocol/src/models.rs`, then prepared local WebSocket output-item filter tests for `tool_search_output.call_id` and `image_generation_call.revised_prompt`.

Observed result: official `ResponseItem::ToolSearchOutput` has `call_id: Option<String>`, `status: String`, `execution: String`, and `tools: Vec<serde_json::Value>`. Official `ResponseItem::ImageGenerationCall` has `id: String`, `status: String`, `revised_prompt: Option<String>`, and `result: String`. Because these variants deserialize through Serde before official Desktop forwards output-item events, missing/null optional fields are accepted but present non-string values fail the variant parse and should be skipped.

Expected local changes: add focused red tests for present non-string `tool_search_output.call_id` and `image_generation_call.revised_prompt`, then add the corresponding `optional_string_field_invalid` checks in the existing output-item predicates.

Red test result:
- `cargo test --test codex_gateway websocket_tool_search_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client` failed at `tests/codex_gateway/websocket.rs` because the response still contained `event: response.output_item.done`.
- `cargo test --test codex_gateway websocket_image_generation_call_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client` failed at `tests/codex_gateway/websocket.rs` because the response still contained `event: response.output_item.done`.

Verification status: red tests confirmed; implementation pending.

Implementation result: local `tool_search_output_item_event_invalid_required_fields` now rejects present non-string `call_id`, and local `image_generation_call_output_item_event_invalid_required_fields` now rejects present non-string `revised_prompt`. Missing/null remains accepted because both use the existing `optional_string_field_invalid` helper.

Focused green checks:
- `cargo test --test codex_gateway websocket_tool_search_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client`
- `cargo test --test codex_gateway websocket_image_generation_call_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client`

Verification status: focused tests passed; broader verification pending.

Broader verification commands:
- `cargo fmt --all`
- `cargo test --test codex_gateway websocket_` (85 passed)
- `cargo test --test codex_serving responses_websocket` (28 passed)
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Remaining source-backed WebSocket parse/filter delta: none found in the Task 68 matrix after this implementation. Remaining unclaimed areas are live TLS ClientHello/opening-byte parity and shipped Desktop proxy/env behavior, which still require a macOS Desktop capture artifact.

Verification status: ready to commit.

### 2026-06-17 Task 70 Linux Desktop Repack Live Chain Capture

Command/code area touched: ran the Linux Desktop repack from `/home/zyy/桌面/Codes/codex-desktop-linux` (same real tree as `/home/zyy/Codes/codex-desktop-linux`) with `CODEX_CLI_PATH` pointed to a local wrapper in `.codex-ws-audit/task70-desktop/desktop-appserver-wrapper.sh`. The wrapper preserved the Desktop-launched `app-server --analytics-default-enabled` invocation, spawned the real Codex app-server, overrode only the model provider base URL to a local capture endpoint, and injected `thread/start` plus `turn/start` over stdio. Artifacts are ignored under `.codex-ws-audit/task70-desktop/`.

Attribution result: `.codex-ws-audit/task70-desktop/desktop-wrapper.ndjson` records the wrapper parent as `/home/zyy/Codes/codex-desktop-linux/codex-app/electron ... --user-data-dir=/home/zyy/.local/state/codex-desktop/instances/port-5186/electron-user-data ...`, original argv `["app-server","--analytics-default-enabled"]`, real child `/home/zyy/.volta/bin/codex`, and successful injected `thread/start` / `turn/start` responses. This proves the sampled WebSocket request chain is `codex-desktop-linux` Electron -> `CODEX_CLI_PATH` wrapper -> official Codex app-server -> Responses WebSocket client.

Captured WebSocket opening artifacts:
- `.codex-ws-audit/task70-desktop/desktop-ws-capture-001.json`
- `.codex-ws-audit/task70-desktop/desktop-ws-capture-002.json`

Official Desktop-launched app-server opening request:
- Request line: `GET /backend-api/codex/responses HTTP/1.1`
- Header order/casing: `Host`, `Connection`, `Upgrade`, `Sec-WebSocket-Version`, `Sec-WebSocket-Key`, `chatgpt-account-id`, `authorization`, `user-agent`, `originator`, `openai-beta`, `x-codex-beta-features`, `x-client-request-id`, `session-id`, `thread-id`, `x-codex-window-id`, `x-codex-turn-metadata`, `sec-websocket-extensions`
- `Sec-WebSocket-Key` header index: `4`
- `sec-websocket-extensions`: `permessage-deflate; client_max_window_bits`
- The local capture endpoint responded with `HTTP/1.1 101 Switching Protocols`, `Upgrade: websocket`, `Connection: Upgrade`, and `Sec-WebSocket-Accept`. No upstream OpenAI response body was decrypted; mihomo does not expose decrypted HTTP/WebSocket payloads.

Official Desktop-launched first WebSocket text frames:
- Capture 001 keys: `type`, `model`, `instructions`, `input`, `tools`, `tool_choice`, `parallel_tool_calls`, `reasoning`, `store`, `stream`, `include`, `prompt_cache_key`, `generate`, `client_metadata`; `generate` is `false`.
- Capture 002 keys: `type`, `model`, `instructions`, `input`, `tools`, `tool_choice`, `parallel_tool_calls`, `reasoning`, `store`, `stream`, `include`, `prompt_cache_key`, `client_metadata`.
- Redaction preserved top-level key order while replacing prompt/tool/client metadata payloads and account/auth/session identifiers.

mihomo attribution result: controller `127.0.0.1:9090` with bearer key `123456` is reachable; `/configs` reported `mode=rule`, `mixed-port=7897`, and TUN enabled. A short direct Desktop launch wrote `.codex-ws-audit/task70-desktop/mihomo-desktop-connections.ndjson` and showed live `electron` connections from `/home/zyy/Codes/codex-desktop-linux/codex-app/electron` to `ab.chatgpt.com` and `chat.openai.com`, routed by `DomainSuffix` through the configured OpenAI rule chain. The same sample also showed official Codex child-process traffic to `ab.chatgpt.com`. This is process/route attribution only; it is not an HTTP header/body capture path.

TLS ClientHello result: added `.codex-ws-audit/task70-desktop/capture-tls-clienthello.mjs` and launched the Desktop repack with `TASK70_BASE_URL=https://127.0.0.1:18772/backend-api/codex`. The Desktop-launched app-server produced `.codex-ws-audit/task70-desktop/tls-clienthello-001.json`, `tls-clienthello-002.json`, and `tls-clienthello-003.json`. Wrapper logs show the first two failed at `wss://127.0.0.1:18772/backend-api/codex/responses` with `tls handshake eof`; after that the client logged `falling back to HTTP`, so `tls-clienthello-003.json` is classified as the HTTP fallback TLS path, not the WSS opening path.

WSS ClientHello stable fields from `tls-clienthello-001.json` / `tls-clienthello-002.json`:
- TLS record version `0x0301`; ClientHello legacy version `0x0303`
- Cipher suites: `0x1302`, `0x1301`, `0x1303`, `0xc02c`, `0xc02b`, `0xcca9`, `0xc030`, `0xc02f`, `0xcca8`, `0x00ff`
- Supported groups: `x25519`, `secp256r1`, `secp384r1`
- Signature algorithms: `0x0503`, `0x0403`, `0x0807`, `0x0806`, `0x0805`, `0x0804`, `0x0601`, `0x0501`, `0x0401`
- Supported versions: `TLS1.3`, `TLS1.2`
- Key share groups: `x25519`
- ALPN: empty in these WSS samples
- Extension set: `signature_algorithms`, `psk_key_exchange_modes`, `status_request`, `extended_master_secret`, `key_share`, `ec_point_formats`, `supported_groups`, `session_ticket`, `supported_versions`
- Extension ordering varied between WSS attempts, producing different JA3 MD5 values (`7ac5439bc736f74eb03aab08506671b6`, `bb8e1b0e5cc184cc85fff1d940590c19`) with the same cipher/group/version core.

Comparison against current rs audit artifact `.codex-ws-audit/rs-parity-check-capture.json`:
- Current rs request line is `GET /codex/responses HTTP/1.1`; official Desktop app-server uses `GET /backend-api/codex/responses HTTP/1.1` when the provider base URL is `.../backend-api/codex`.
- Current rs header order/casing is `Host`, `Connection`, `Upgrade`, `Sec-WebSocket-Version`, `Sec-WebSocket-Key`, `Authorization`, `ChatGPT-Account-Id`, `originator`, `User-Agent`, browser-like `sec-ch-*` / `sec-fetch-*`, `Accept-Encoding`, `Accept-Language`, `OpenAI-Beta`, `x-openai-internal-codex-residency`, `x-client-request-id`, `Sec-WebSocket-Extensions`.
- Official Desktop app-server omits the browser-like `sec-ch-*`, `sec-fetch-*`, `Accept-Encoding`, `Accept-Language`, and `x-openai-internal-codex-residency` headers in this chain.
- Official Desktop app-server uses lowercase business/extension header names for `chatgpt-account-id`, `authorization`, `user-agent`, `openai-beta`, and `sec-websocket-extensions`, and orders account before authorization.
- Official Desktop app-server adds `x-codex-beta-features`, `session-id`, `thread-id`, `x-codex-window-id`, and `x-codex-turn-metadata`.
- Current rs first-frame audit sample only contained `include`, `input`, `instructions`, `model`, `parallel_tool_calls`, `reasoning`, `store`, `stream`, `tool_choice`, `tools`, `type`; official Desktop app-server includes `prompt_cache_key` and `client_metadata`, with prewarm also including `generate: false`.

Next decision: treat Task 70 as evidence capture complete. Any code change should be a separate task that decides whether the proxy should mimic the Desktop app-server opening exactly or preserve the existing browser-style/header-synthesis behavior for its public proxy role. The highest-confidence deltas for a Desktop-app-server parity mode are path/base composition, header casing/order, removal of browser-like headers from this WS chain, and addition of the Codex session/window/turn metadata headers where the local data model has sources.

Verification status: Desktop-originated opening, first-frame payload shape, mihomo process attribution, and WSS ClientHello artifacts are captured and redacted. Full upstream OpenAI response bodies were not captured because TLS decryption was not configured and mihomo exposes connection metadata rather than decrypted WebSocket payloads.

### 2026-06-17 Task 71 live5 Complete Request Chain Alignment

Command/code area touched: inspected `/home/zyy/桌面/Codes/codex-proxy-rs/.codex-ws-audit/live5/flows.all.txt` and focused on model WebSocket flows `#41`, `#44`, `#48`, `#59`, and `#60`. Flow `#44` had no response captured; flows `#41`, `#48`, `#59`, and `#60` carried WebSocket text frames. The live5 capture confirms the Task 70 Desktop app-server opening shape with a real full request chain rather than only local wrapper artifacts.

Observed live5 request shape:
- Request line: `GET /backend-api/codex/responses`.
- Header order/casing: `Host`, `Connection`, `Upgrade`, `Sec-WebSocket-Version`, `Sec-WebSocket-Key`, `chatgpt-account-id`, `authorization`, `user-agent`, `originator`, `openai-beta`, optional `x-codex-beta-features`, `x-client-request-id`, `session-id`, `thread-id`, `x-codex-window-id`, `x-codex-turn-metadata`, `sec-websocket-extensions`.
- Browser-style headers (`sec-ch-*`, `sec-fetch-*`, `Accept-Encoding`, `Accept-Language`) and `x-openai-internal-codex-residency` were not present on the Desktop app-server WebSocket opening in the sampled flows.
- `sec-websocket-extensions` remained last with `permessage-deflate; client_max_window_bits`.
- First-frame payloads preserve official `response.create` struct ordering; prewarm-shaped requests can include `generate:false` before `client_metadata`.

Red tests added and confirmed:
- `cargo test --test codex_gateway websocket_opening_audit_snapshot_should_redact_sensitive_headers` failed because local still emitted `Authorization`, `ChatGPT-Account-Id`, `User-Agent`, `OpenAI-Beta`, `session_id`, and older order.
- `cargo test --test codex_gateway websocket_payload_audit_snapshot_should_redact_user_content` failed because the audit key list omitted `generate`.
- `cargo test --test codex_serving v1_responses_should_use_websocket_upstream_by_default_while_serving_sse` failed because request body `generate:false` was parsed away and upstream saw `Null`.
- `cargo test --test codex_gateway websocket_capture_harness_should_record_opening_bytes` failed because the actual rs opening still used the old browser-style/header-synthesis shape.

Implementation result: WebSocket opening serialization now follows the live Desktop app-server chain for the fields available in local context. It writes account before authorization, lowercases the business headers observed in live5, omits browser-style headers and `x-openai-internal-codex-residency` from this WebSocket opening path, maps local `session_id` to `session-id` and `thread-id`, keeps `x-codex-beta-features`, `x-codex-window-id`, and `x-codex-turn-metadata` when present, and writes lowercase `sec-websocket-extensions` last. The request body path now parses and forwards `generate` and includes it in payload audit key ordering.

Focused green checks:
- `cargo test --test codex_gateway websocket_opening_audit_snapshot_should_redact_sensitive_headers`
- `cargo test --test codex_gateway websocket_payload_audit_snapshot_should_redact_user_content`
- `cargo test --test codex_gateway websocket_capture_harness_should_record_opening_bytes`
- `cargo test --test codex_serving v1_responses_should_use_websocket_upstream_by_default_while_serving_sse`
- `cargo test --test codex_gateway websocket_handshake_should_offer_original_permessage_deflate_extension`

Next decision: Task 71 local request-shape alignment is complete. Remaining unclaimed item from live5 is full upstream response-body decryption; the available capture is enough for opening and first-frame request parity but not for decrypted production response payload comparison.

Verification status: broader verification completed:
- `cargo test --test codex_gateway websocket_`
- `cargo test --test codex_serving responses_websocket`
- `cargo fmt --all`
- `cargo fmt --all --check`
- `git diff --check`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`
- `cargo test`

Full-suite verification initially reproduced the known `token_refresh` virtual-time helper race, where the spawned retry path could finish the first probe while the helper exhausted virtual-time advances before SQLite/blocking-pool work registered the second retry. The test-only helper now advances paused time in smaller steps and yields a small amount of real scheduler time between polls; the final `cargo test` run passed afterward.
