# Codex Desktop WebSocket Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build an auditable path to compare `codex-proxy-rs` against the official Codex Desktop bundled Rust `responses_websocket` chain, then make minimal 1:1 WebSocket and fallback parity changes only after evidence is recorded.

**Architecture:** Keep runtime behavior unchanged until audit artifacts exist. Add small, redacted audit primitives near the existing WebSocket transport, use local harnesses to capture current and official behavior, generate a parity diff, then align TLS/dependency, opening handshake, payload, and downgrade behavior in focused changes.

**Tech Stack:** Rust, Tokio, tokio-rustls, rustls 0.23, tokio-tungstenite, reqwest 0.12, serde_json, axum test support, CodeGraph, Cargo tests, Clippy.

---

### Task 1: Preserve the Current Baseline in Docs

**Files:**
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`
- Add: `docs/superpowers/plans/2026-06-17-codex-desktop-ws-parity.md`

- [x] Append a resume entry to the design doc before any code edits.
- [x] Check `codegraph status .` and record the result in the design doc.
- [x] Write this implementation plan with test-first implementation steps.
- [x] Run `git diff --check`.
- [x] Commit the documentation checkpoint with message `docs: plan codex desktop websocket parity`.

**Verification command:**
```bash
git diff --check
```

**Expected result:** no whitespace errors.

### Task 2: Add Redacted WebSocket Opening Audit Snapshot

**Files:**
- Modify: `src/codex/gateway/transport/websocket/opening.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Write a failing test that builds a WebSocket request with authorization, account, cookie, and ordinary headers, then asserts the audit snapshot preserves request line/header order while redacting secret values.
- [x] Add a small `OpeningAuditSnapshot` type with ordered headers and redacted values.
- [x] Extract snapshot generation from `opening_request_bytes` without changing the emitted opening bytes.
- [x] Keep tokens, cookies, account ids, session ids, request ids, installation ids, turn state, and turn metadata redacted.
- [x] Append an Analysis Journal entry with the code area touched, observed test failure, implementation result, next decision, and verification status.
- [x] Run the focused WebSocket test.
- [x] Commit with message `feat: add websocket opening audit snapshot`.

**Expected failing command before implementation:**
```bash
cargo test --test codex_gateway websocket_opening_audit_snapshot_should_redact_sensitive_headers
```

**Expected initial result:** the named test fails to compile or fails because the audit API does not exist.

**Expected passing command after implementation:**
```bash
cargo test --test codex_gateway websocket_opening_audit_snapshot_should_redact_sensitive_headers
```

**Expected final result:** the named test passes and no runtime WebSocket behavior changes are included.

### Task 3: Add Redacted `response.create` Payload Audit Snapshot

**Files:**
- Modify: `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Use CodeGraph to follow the `response.create` payload construction path before editing.
- [x] Write a failing test that asserts a payload audit snapshot records top-level JSON key order and structural fields while redacting user prompt/body content.
- [x] Add the smallest helper needed to produce the redacted payload snapshot before the first WebSocket text frame is sent.
- [x] Ensure the helper borrows data where practical and does not clone the full request body except for the audit representation.
- [x] Append an Analysis Journal entry with observed current payload shape and verification status.
- [x] Run the focused WebSocket test.
- [x] Commit with message `feat: add websocket payload audit snapshot`.

**Expected failing command before implementation:**
```bash
cargo test --test codex_gateway websocket_payload_audit_snapshot_should_redact_user_content
```

**Expected initial result:** the named test fails to compile or fails because the payload audit API does not exist.

**Expected passing command after implementation:**
```bash
cargo test --test codex_gateway websocket_payload_audit_snapshot_should_redact_user_content
```

**Expected final result:** the named test passes and emitted WebSocket payload bytes remain unchanged outside audit serialization.

### Task 4: Add Local `codex-proxy-rs` Audit Artifact Writer

**Files:**
- Modify: `src/config/types.rs` or existing environment/config loader if it already owns local diagnostic flags
- Modify: `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `src/codex/gateway/transport/websocket/opening.rs`
- Add or modify: focused tests under `tests/codex_gateway/`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Inspect existing diagnostics/config patterns before choosing the gate name.
- [x] Write a failing test for an opt-in audit path that writes a redacted JSON artifact to a local directory.
- [x] Gate the writer behind an explicit environment variable or config field, with auditing disabled by default.
- [x] Include transport mode, fallback eligibility, opening snapshot, payload snapshot, and error classification when present.
- [x] Avoid logging bearer tokens, cookies, account ids, user prompt content, and response body content.
- [x] Append an Analysis Journal entry that names the gate and artifact shape.
- [x] Run focused audit tests.
- [x] Commit with message `feat: write redacted websocket audit artifacts`.

**Expected failing command before implementation:**
```bash
cargo test --test codex_gateway websocket_audit_artifact_should_require_explicit_gate
```

**Expected initial result:** the named test fails because no artifact writer/gate exists.

**Expected passing command after implementation:**
```bash
cargo test --test codex_gateway websocket_audit_artifact_should_require_explicit_gate
```

**Expected final result:** audit artifacts are only produced when explicitly enabled.

### Task 5: Build a Local WebSocket/TLS Capture Harness

**Files:**
- Add or modify: test support under `tests/support/` if integration-test based
- Add or modify: `tests/codex_gateway/websocket.rs`
- Optionally add: a small binary under `src/bin/` only if tests cannot cover the capture path cleanly
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Write a failing integration test that starts a local WebSocket endpoint and captures opening bytes through the current client path.
- [x] Record request line, header order/casing, `Sec-WebSocket-Extensions`, `Sec-WebSocket-Key` position, and first text frame.
- [x] Capture TLS ClientHello summary when practical; if test TLS interception is not stable, record the blocker in the Analysis Journal and keep HTTP/WS bytes capture as the first artifact.
- [x] Run the current `codex-proxy-rs` request against the harness and write a redacted sample artifact under an ignored local audit directory.
- [x] Append an Analysis Journal entry with artifact path, command, observed current behavior, and verification status.
- [x] Commit with message `test: add websocket capture harness`.

**Expected failing command before implementation:**
```bash
cargo test --test codex_gateway websocket_capture_harness_should_record_opening_bytes
```

**Expected initial result:** the named test fails because no capture harness exists.

**Expected passing command after implementation:**
```bash
cargo test --test codex_gateway websocket_capture_harness_should_record_opening_bytes
```

**Expected final result:** the local harness captures current rs opening bytes and first-frame payload without real OpenAI credentials.

### Task 6: Sample the Official Codex Desktop Bundled Binary

**Files:**
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`
- Add ignored local artifacts under `.codex-ws-audit/` or another ignored path
- Optionally add harness invocation docs under `docs/superpowers/specs/` if the command is repeatable

- [x] Confirm the target binary exists at `/tmp/codex-desktop-fingerprint/dmg/Codex Installer/Codex.app/Contents/Resources/codex`.
- [x] Inspect official binary help/config output for API base URL or proxy override flags without using live credentials.
- [x] Try the smallest invocation path that points the official binary to the local capture harness.
- [x] If direct override fails, try a local proxy/DNS interception path that preserves the target URL shape.
- [x] If live execution remains blocked, extract static evidence with `strings`, `otool`/`readelf` as available, and record the blocker explicitly.
- [x] Append an Analysis Journal entry with command, result, official artifact path or blocker, and next decision.
- [x] Commit documentation/artifact index updates with message `docs: record official codex websocket sampling`.

**Verification commands:**
```bash
test -x "/tmp/codex-desktop-fingerprint/dmg/Codex Installer/Codex.app/Contents/Resources/codex"
"/tmp/codex-desktop-fingerprint/dmg/Codex Installer/Codex.app/Contents/Resources/codex" --help
```

**Expected result:** either an official sample artifact is produced by the harness, or the design doc explains the exact blocker and the static evidence used instead.

### Task 7: Generate a Parity Diff Report

**Files:**
- Add or modify: small comparison helper if needed
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Write a failing test for a diff helper that compares two redacted audit artifacts and reports stable differences in opening headers, payload fields, TLS summary, and fallback decisions.
- [x] Implement the diff helper with structured JSON parsing instead of string-only comparison.
- [x] Run the helper against a focused redacted fixture and classify current rs artifact versus official static evidence in the Analysis Journal because official live artifact is unavailable on Linux.
- [x] Append an Analysis Journal entry listing each difference and classifying it as `must change`, `observe more`, or `already compatible`.
- [x] Commit with message `feat: add websocket parity diff report`.

**Expected failing command before implementation:**
```bash
cargo test --test codex_gateway websocket_parity_diff_should_report_header_order_changes
```

**Expected passing command after implementation:**
```bash
cargo test --test codex_gateway websocket_parity_diff_should_report_header_order_changes
```

**Expected final result:** every proposed behavior change has a documented diff source.

### Task 8: Align HTTP SSE Fallback Dependency Parity

**Files:**
- Modify: `Cargo.toml`
- Modify: `Cargo.lock` only through Cargo
- Modify: `src/codex/gateway/transport/http_client.rs` if client construction needs an explicit HTTP/2 setting
- Modify: `tests/codex_gateway/http_client.rs` or focused fallback tests
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record the diff evidence that justifies changing fallback dependency behavior.
- [x] Write or update a test that proves fallback requests still work after enabling `reqwest` `http2`.
- [x] Enable the `reqwest` `http2` feature for fallback parity.
- [x] Run focused HTTP client/fallback tests.
- [x] Append an Analysis Journal entry with the exact feature change and result.
- [x] Commit with message `fix: enable http2 for codex fallback client`.

**Expected commands:**
```bash
cargo test --test codex_gateway http_client
cargo test --test codex_serving responses_http_sse
```

**Expected result:** fallback tests pass and `Cargo.toml` shows `reqwest` with `http2` enabled.

### Task 9: Align WebSocket Opening and Payload Behavior from Evidence

**Files:**
- Modify: `src/codex/gateway/transport/websocket/opening.rs`
- Modify: `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] For each `must change` opening diff, write or update a focused failing test before changing code.
- [x] Apply minimal changes for request line, fixed opening headers, business header order/casing, extension negotiation, and payload field shape as supported by the diff.
- [x] Do not change fallback semantics in this task.
- [x] Re-run local audit capture and update the parity diff result.
- [x] Append an Analysis Journal entry with before/after diff status.
- [x] Commit with message `fix: align codex desktop websocket opening`.

**Expected command:**
```bash
cargo test --test codex_gateway websocket
```

**Expected result:** focused WebSocket tests pass and the diff report no longer lists handled opening/payload differences.

### Task 10: Align WebSocket-to-HTTP Downgrade Semantics

**Files:**
- Modify: `src/codex/gateway/transport/http_client.rs`
- Modify: `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `tests/codex_serving/responses_websocket.rs` if serving behavior changes
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record the official or strongest available evidence for downgrade behavior.
- [x] Write failing tests for the exact downgrade cases: preferred WS HTTP 426 may fall back, generic network/opening failure does not fall back, `previous_response_id` does not fall back, quota/auth/business errors do not fall back, and early terminal/closed-before-terminal behavior matches the sample.
- [x] Apply minimal fallback classification changes.
- [x] Append an Analysis Journal entry with each downgrade case and verification status.
- [x] Commit with message `fix: align codex desktop websocket fallback`.

**Expected commands:**
```bash
cargo test --test codex_gateway websocket
cargo test --test codex_serving responses_websocket
cargo test --test codex_serving upstream_fallback
```

**Expected result:** downgrade behavior is covered by tests and matches the documented evidence.

### Task 11: Final Verification and Summary

**Files:**
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Run formatting, focused tests, full test suite if practical, and Clippy.
- [x] Append final Progress Log and Analysis Journal entries with all commands and results.
- [x] Ensure local audit artifacts containing sensitive or environment-specific data are ignored and not committed.
- [x] Commit final documentation updates with message `docs: summarize codex desktop websocket parity`.

**Verification commands:**
```bash
cargo fmt --all --check
cargo test --test codex_gateway websocket
cargo test --test codex_serving responses_websocket
cargo test
cargo clippy --all-targets --all-features --locked -- -D warnings
git status --short
```

**Expected result:** all feasible checks pass; any skipped full-suite or official-binary sampling limitation is explicitly documented in the design doc.

### Task 12: Align Binary Frame Handling

**Files:**
- Modify: `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `src/codex/gateway/transport/websocket/codec.rs`
- Modify: `src/codex/gateway/transport/http_client.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `Message::Binary(_)` returns `unexpected binary websocket event`.
- [x] Add a focused gateway test proving binary JSON is not accepted as a response event.
- [x] Reject binary WebSocket frames instead of UTF-8 decoding them.
- [x] Keep binary-frame errors ineligible for HTTP SSE downgrade.
- [x] Run focused WebSocket gateway/serving checks, Clippy, formatting, diff check, and full test suite.
- [x] Commit with message `fix: align websocket binary frame handling`.

### Task 13: Align Close Frame Handling

**Files:**
- Modify: `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `src/codex/gateway/transport/websocket/codec.rs`
- Modify: `src/codex/gateway/transport/http_client.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence for `Message::Close(_)` and stream-end-without-close errors.
- [x] Add gateway coverage for server Close before the first visible event.
- [x] Split local close-before-completed and stream-closed-before-completed errors.
- [x] Preserve stale pooled connection one-shot retry behavior.
- [x] Run focused WebSocket gateway/serving checks, Clippy, formatting, diff check, and full test suite.
- [x] Commit with message `fix: align websocket close frame handling`.

### Task 14: Ignore Invalid Text Events

**Files:**
- Modify: `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `src/codex/gateway/transport/websocket/codec.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that unparseable text frames are logged and skipped.
- [x] Add a focused gateway test proving invalid text is not forwarded as anonymous SSE.
- [x] Require a parsed typed event before building an SSE chunk.
- [x] Run focused invalid-text test, filtered WebSocket gateway tests, WebSocket serving tests, Clippy, formatting, diff check, and full test suite.
- [x] Commit with message `fix: ignore invalid websocket text events`.

### Task 15: Continue Source-Backed Parity Audit

**Files:**
- Inspect: official `codex-rs/codex-api/src/endpoint/responses_websocket.rs`
- Inspect: official `codex-rs/codex-api/src/common.rs`
- Inspect/modify as needed: local WebSocket transport, serving, and gateway tests
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Re-check official `ResponseEvent` side-channel handling against local gateway behavior.
- [x] Capture `response.metadata.headers.x-codex-turn-state` from WebSocket text frames and skip raw metadata forwarding.
- [x] Preserve metadata-derived turn state through non-streaming collection and streaming affinity recording.
- [x] Re-check custom CA / Rustls connector behavior against local WebSocket opening configuration.
- [x] Implement `CODEX_CA_CERTIFICATE` / `SSL_CERT_FILE` custom CA handling for shared HTTP and WebSocket TLS paths.
- [ ] Add failing tests only for differences that are observable locally and backed by official source.
- [ ] Document any remaining unprovable live TLS/opening items separately from implemented parity.

### Task 17: Align `response.incomplete` Handling

**Files:**
- Modify: `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `src/codex/gateway/transport/websocket/codec.rs`
- Modify: `src/codex/gateway/transport/http_client.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `response.incomplete` returns `Incomplete response returned, reason: ...`.
- [x] Add a focused gateway test proving `response.incomplete` is a WebSocket stream error, not a forwarded SSE event.
- [x] Parse `incomplete_details.reason` with official `unknown` fallback.
- [x] Keep incomplete response errors ineligible for HTTP SSE fallback.
- [x] Run focused WebSocket gateway/serving checks, Clippy, formatting, diff check, and full test suite.
- [x] Commit with message `fix: align websocket incomplete response handling`.

### Task 18: Audit Upgrade Header Side Channels

**Files:**
- Inspect: official `codex-rs/codex-api/src/endpoint/responses_websocket.rs`
- Inspect: local `src/codex/gateway/transport/websocket/pool.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Compare official `openai-model`, `x-models-etag`, and `x-reasoning-included` handling with local WebSocket metadata.
- [x] Document that these are Desktop-internal `ResponseEvent` side channels without a matching proxy API surface.
- [x] Avoid adding unsupported SSE fields that would change the OpenAI-compatible response surface.

### Task 19: Align Wrapped WebSocket Error Status Handling

**Files:**
- Modify: `src/codex/gateway/transport/websocket/codec.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `type:error` with non-success `status` / `status_code` maps to transport HTTP error.
- [x] Add a focused gateway test proving wrapped `status: 400` is not forwarded as `event: error`.
- [x] Extend local WebSocket error classification to honor wrapped non-success status values.
- [x] Run focused WebSocket gateway/serving checks, Clippy, formatting, diff check, and full test suite.
- [x] Commit with message `fix: align websocket wrapped error status handling`.

### Task 20: Audit Responses Event Filtering And Transforms

**Files:**
- Inspect: official `codex-rs/codex-api/src/sse/responses.rs`
- Inspect: official `codex-rs/codex-api/src/endpoint/responses_websocket.rs`
- Inspect: local `src/codex/gateway/transport/websocket/codec.rs`
- Inspect/modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Enumerate official `process_responses_event` branches and official `run_websocket_response_stream` pre-parse transforms.
- [x] Compare wrapped error handling with local first-frame and forwarding-loop behavior.
- [x] Decide that mid-stream wrapped error forwarding is externally incorrect for this proxy.
- [x] Add a focused red test and code fix for source-backed observable mismatch.
- [x] Record result and verification commands in the design doc.
- [x] Commit with message `fix: align websocket midstream wrapped error handling`.

### Task 21: Preserve Wrapped Error Retry-After Header

**Files:**
- Inspect: official `codex-rs/codex-api/src/endpoint/responses_websocket.rs`
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that wrapped WebSocket errors carry JSON `headers`.
- [x] Add a focused gateway test for `headers.retry-after`.
- [x] Parse wrapped error retry-after before falling back to body-derived reset fields.
- [x] Run focused WebSocket checks, Clippy, formatting, diff check, and full test suite.
- [x] Commit with message `fix: preserve websocket wrapped error retry-after`.

### Task 22: Align Wrapped Connection Limit Error Precedence

**Files:**
- Inspect: official `codex-rs/codex-api/src/endpoint/responses_websocket.rs`
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that wrapped `websocket_connection_limit_reached` is handled before status.
- [x] Add a focused gateway test proving wrapped connection-limit status 400 does not surface as upstream 400.
- [x] Prioritize connection-limit code over wrapped status and map it to the proxy's existing 503 retryable equivalent.
- [x] Run focused WebSocket checks, Clippy, formatting, diff check, and full test suite.
- [x] Commit with message `fix: align websocket wrapped connection limit handling`.

### Task 23: Ignore Unmapped Wrapped Error Events

**Files:**
- Inspect: official `codex-rs/codex-api/src/endpoint/responses_websocket.rs`
- Inspect: official `codex-rs/codex-api/src/sse/responses.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `type:error` with missing/success status maps to `Ok(None)`, not a terminal SSE event.
- [x] Add a focused gateway test proving success-status wrapped error is not returned as successful `event: error`.
- [x] Skip unmapped `type:error` frames in first-frame and forwarding loops.
- [x] Run focused WebSocket checks, Clippy, formatting, diff check, and full test suite.
- [x] Commit with message `fix: ignore unmapped websocket error events`.

### Task 24: Align `response.failed` Server Overload Handling

**Files:**
- Inspect: official `codex-rs/codex-api/src/sse/responses.rs`
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `server_is_overloaded` and `slow_down` map to `ServerOverloaded`.
- [x] Add a focused gateway test proving overloaded `response.failed` is not returned as successful SSE.
- [x] Map overload codes to the proxy's existing 503 upstream error equivalent.
- [x] Run focused WebSocket checks, Clippy, formatting, diff check, and full test suite.
- [x] Commit with message `fix: align websocket server overload failures`.

### Task 25: Audit Completed Event Side Channels

**Files:**
- Inspect: official `codex-rs/codex-api/src/endpoint/responses_websocket.rs`
- Inspect: official `codex-rs/codex-api/src/sse/responses.rs`
- Inspect: local `src/codex/gateway/transport/websocket/codec.rs`
- Inspect: local `src/codex/serving/dispatch/stream.rs`
- Inspect/modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Compare official `response.completed` side-channel extraction with local metadata extraction.
- [x] Decide whether any missing behavior is observable in the proxy API contract.
- [x] Add failing tests only for source-backed observable differences.
- [x] Record result and verification commands in the design doc.

### Task 26: Add WebSocket Receive Idle Timeout

**Files:**
- Inspect: official `codex-rs/codex-api/src/endpoint/responses_websocket.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that receive polling uses `idle_timeout`.
- [x] Add a focused gateway test proving a silent upstream WebSocket times out instead of hanging.
- [x] Add a local receive idle timeout for first-frame and forwarding-loop reads.
- [x] Run focused WebSocket checks, Clippy, formatting, diff check, and full test suite.
- [x] Commit with message `fix: add websocket receive idle timeout`.

### Task 27: Discard WebSocket Connections After Stream Errors

**Files:**
- Inspect: official `codex-rs/codex-api/src/endpoint/responses_websocket.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that terminal stream errors drop the WebSocket connection.
- [x] Add a focused pooled WebSocket test proving classified upstream errors are not returned to the pool.
- [x] Discard active WebSocket connections for all classified upstream error frames.
- [x] Run focused WebSocket checks, Clippy, formatting, diff check, and full test suite.
- [x] Commit with message `fix: discard websocket connections after stream errors`.

### Task 28: Add WebSocket Send Idle Timeout

**Files:**
- Inspect: official `codex-rs/codex-api/src/endpoint/responses_websocket.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: local `src/codex/gateway/transport/http_client.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that request-frame send uses `idle_timeout`.
- [x] Add a focused virtual-time test for pending WebSocket send timeout.
- [x] Route production request-frame sends through the timeout helper.
- [x] Run focused WebSocket checks, Clippy, formatting, diff check, and full test suite.
- [x] Commit with message `fix: add websocket send idle timeout`.

### Task 29: Classify Unknown `response.failed` Errors

**Files:**
- Inspect: official `codex-rs/codex-api/src/sse/responses.rs`
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: local `src/codex/serving/responses.rs`
- Modify: local `src/codex/serving/dispatch/{stream.rs,fallback.rs,mod.rs}`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `tests/codex_serving/responses_websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that unknown `response.failed` errors map to `ApiError::Retryable`.
- [x] Add a focused gateway test proving unknown `response.failed` is not returned as successful SSE.
- [x] Map unknown `response.failed` errors to the proxy's 503 upstream retryable equivalent.
- [x] Preserve invalid encrypted reasoning replay recovery after WebSocket `response.failed` classification.
- [x] Run focused WebSocket checks, Clippy, formatting, diff check, and full test suite.
- [x] Commit with message `fix: classify unknown websocket response failures`.

### Task 30: Audit `response.failed` Special Error Classification

**Files:**
- Inspect: official `codex-rs/codex-api/src/sse/responses.rs`
- Inspect: local `src/codex/gateway/transport/websocket/codec.rs`
- Inspect: local `src/codex/serving/dispatch/{stream.rs,fallback.rs}`
- Inspect/modify: `tests/codex_gateway/websocket.rs`
- Inspect/modify: `tests/codex_serving/{responses_websocket.rs,upstream_fallback.rs}`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record audit start and official/source comparison scope.
- [x] Compare official special-case predicates with local WebSocket and serving classification.
- [x] Add failing tests and code changes only for observable parity gaps.
- [x] Run focused checks and update the design doc with results.
- [x] Commit Task 30 if code/docs changes are justified.

### Task 31: Parse `response.failed` Retry-After Message Delay

**Files:**
- Inspect: official `codex-rs/codex-api/src/sse/responses.rs`
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Inspect/modify: local `src/codex/gateway/transport/http_client.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that rate-limit messages can carry retry delay text.
- [x] Add focused red tests for WebSocket body-derived retry-after from `rate_limit_exceeded` message text. Red result: `retry_after_seconds` was `None` for official `Please try again in 11.054s` wording.
- [x] Implement official-shaped message parsing without applying it to unrelated error codes.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 31 if code/docs changes are justified. Code/docs commit: `f210ae4 fix: parse websocket rate limit retry delays`.

### Task 32: Reject Malformed `response.completed` Frames

**Files:**
- Inspect: official `codex-rs/codex-api/src/sse/responses.rs`
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that malformed `response.completed.response` returns a stream error.
- [x] Add a focused red test proving WebSocket `response.completed` without `response.id` is not treated as success. Red result: local returned successful `event: response.completed` SSE.
- [x] Implement minimal official-shaped validation for completed frames with a present response object.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 32 if code/docs changes are justified. Code/docs commit: `7c9761d fix: reject malformed websocket completed frames`.

### Task 33: Enforce Official `response.completed.usage` Shape

**Files:**
- Inspect: official `codex-rs/codex-api/src/sse/responses.rs`
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `ResponseCompletedUsage.total_tokens` is required when `usage` is present.
- [x] Add a focused red test proving WebSocket `response.completed` with incomplete `usage` is not treated as success. Red result: local returned successful SSE and synthesized `total_tokens`.
- [x] Replace the Task 32 minimal `id` check with official-shaped completed-response deserialization.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 33 if code/docs changes are justified. Code/docs commit: `14c0e5c fix: validate websocket completed usage shape`.

### Task 34: Ignore `response.completed` Without `response`

**Files:**
- Inspect: official `codex-rs/codex-api/src/sse/responses.rs`
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `response.completed` only completes when `event.response` exists.
- [x] Add a focused red test proving WebSocket `response.completed` without `response` is not returned as success. Red result: local returned successful `event: response.completed` SSE.
- [x] Skip response-less completed frames so a subsequent close surfaces as incomplete stream.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 34 if code/docs changes are justified. Code/docs commit: `3e113e6 fix: ignore websocket completed frames without response`.

### Task 35: Ignore `response.created` Without `response`

**Files:**
- Inspect: official `codex-rs/codex-api/src/sse/responses.rs`
- Inspect: official `codex-rs/codex-api/src/endpoint/responses_websocket.rs`
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `response.created` only emits when `event.response` exists.
- [x] Add a focused red test proving WebSocket `response.created` without `response` is not forwarded as a successful SSE event. Red result: local response body contained `event: response.created`.
- [x] Skip response-less created frames in both first-frame and forwarding loops.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 35 if code/docs changes are justified. Code/docs commit: `3d10f99 fix: ignore websocket created frames without response`.

### Task 36: Ignore `response.output_text.delta` Without `delta`

**Files:**
- Inspect: official `codex-rs/codex-api/src/sse/responses.rs`
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `response.output_text.delta` only emits when `event.delta` exists.
- [x] Add a focused red test proving WebSocket `response.output_text.delta` without `delta` is not forwarded as a successful SSE event. Red result: local response body contained `event: response.output_text.delta`.
- [x] Skip delta-less output-text frames in both first-frame and forwarding loops.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 36 if code/docs changes are justified. Code/docs commit: `14a54a6 fix: ignore websocket text deltas without delta`.

### Task 37: Ignore Delta Events Missing Official Required Fields

**Files:**
- Inspect: official `codex-rs/codex-api/src/sse/responses.rs`
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence for required fields on custom-tool, reasoning-summary, and reasoning-text delta events.
- [x] Add focused red tests proving WebSocket delta events missing official required fields are not forwarded as successful SSE events. Red result: local response body contained `event: response.custom_tool_call_input.delta`.
- [x] Skip malformed delta frames in both first-frame and forwarding loops.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 37 if code/docs changes are justified. Code/docs commit: `c8d332d fix: ignore malformed websocket delta events`.

### Task 38: Ignore `output_item` Events Without `item`

**Files:**
- Inspect: official `codex-rs/codex-api/src/sse/responses.rs`
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `response.output_item.done` and `response.output_item.added` only emit when `event.item` exists and parses.
- [x] Add a focused red test proving WebSocket `output_item` events without `item` are not forwarded as successful SSE events. Red result: local response body contained `event: response.output_item.done`.
- [x] Skip item-less `output_item.done` and `output_item.added` frames in both first-frame and forwarding loops.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 38 if code/docs changes are justified. Code/docs commit: `3944b4e fix: ignore websocket output item frames without item`.

### Task 39: Ignore `response.reasoning_summary_part.added` Without `summary_index`

**Files:**
- Inspect: official `codex-rs/codex-api/src/sse/responses.rs`
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `response.reasoning_summary_part.added` only emits when `event.summary_index` exists.
- [x] Add a focused red test proving WebSocket `response.reasoning_summary_part.added` without `summary_index` is not forwarded as a successful SSE event. Red result: local response body contained `event: response.reasoning_summary_part.added`.
- [x] Skip summary-index-less `response.reasoning_summary_part.added` frames in both first-frame and forwarding loops.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 39 if code/docs changes are justified. Code/docs commit: `5f17f37 fix: ignore websocket reasoning summary parts without index`.

### Task 40: Treat JSON `null` Like Missing Official `Option` Fields

**Files:**
- Inspect: official `codex-rs/codex-api/src/sse/responses.rs`
- Inspect: official `codex-rs/codex-api/src/endpoint/responses_websocket.rs`
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that nullable `ResponsesStreamEvent` fields are `Option` and therefore `null` becomes `None`.
- [x] Add a focused red test proving WebSocket frames with null-valued optional fields follow the same ignore path as missing fields. Red result: local returned `InvalidCompletedResponse` for `response.completed` with `response: null`.
- [x] Treat null `response`, `delta`, and `item` values like missing fields in the existing official-shaped skip predicates.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 40 if code/docs changes are justified. Code/docs commit: `4bbfebc fix: treat null websocket option fields as missing`.

### Task 41: Ignore Frames That Fail Official `ResponsesStreamEvent` Shape Parsing

**Files:**
- Inspect: official `codex-rs/codex-api/src/sse/responses.rs`
- Inspect: official `codex-rs/codex-api/src/endpoint/responses_websocket.rs`
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that WebSocket text frames failing `ResponsesStreamEvent` deserialization are ignored before `process_responses_event`.
- [x] Add a focused red test proving a typed frame with an invalid official field type is not forwarded as successful SSE. Red result: local response body contained `event: response.output_text.delta`.
- [x] Add a local official-shape parse guard and skip failing frames before raw SSE forwarding.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 41 if code/docs changes are justified. Code/docs commit: `ffd5ca5 fix: ignore malformed websocket event shapes`.

### Task 42: Ignore `output_item` Events With Non-Object `item`

**Files:**
- Inspect: official `codex-rs/codex-api/src/sse/responses.rs`
- Inspect: official `codex_protocol::models::ResponseItem` usage evidence from official tests
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `output_item` events only emit when `item` parses as `ResponseItem`, and official examples use object-shaped items.
- [x] Add a focused red test proving non-object `item` values are not forwarded as successful SSE. Red result: local response body contained `event: response.output_item.done`.
- [x] Skip `response.output_item.done` and `response.output_item.added` frames when `item` is present but not an object.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 42 if code/docs changes are justified. Code/docs commit: `45827d0 fix: ignore websocket output item frames with non-object items`.

### Task 43: Ignore `output_item` Events With Missing or Non-String `item.type`

**Files:**
- Inspect: official `codex-rs/codex-api/src/sse/responses.rs`
- Inspect: official `codex-rs/protocol/src/models.rs` from git tree
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `ResponseItem` is a `type`-tagged enum and non-string/missing tags cannot parse, while unknown string tags map to `Other`.
- [x] Add a focused red test proving object `item` values with missing or non-string `type` are not forwarded as successful SSE. Red result: local response body contained `event: response.output_item.done`.
- [x] Skip `response.output_item.done` and `response.output_item.added` frames when object `item.type` is absent or not a string.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 43 if code/docs changes are justified. Code/docs commit: `20bf171 fix: ignore websocket output item frames with invalid type tags`.

### Task 44: Ignore Invalid `message` Output Items

**Files:**
- Inspect: official `codex-rs/protocol/src/models.rs` from git tree
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `ResponseItem::Message` requires string `role` and array `content`.
- [x] Add a focused red test proving malformed `message` output items are not forwarded as successful SSE. Red result: local response body contained `event: response.output_item.done`.
- [x] Skip malformed `message` output-item frames in both WebSocket forwarding paths.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 44 if code/docs changes are justified. Code/docs commit: `3bbf14c fix: ignore malformed websocket message output items`.

### Task 45: Ignore Invalid `function_call` Output Items

**Files:**
- Inspect: official `codex-rs/protocol/src/models.rs` from git tree
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `ResponseItem::FunctionCall` requires string `name`, `arguments`, and `call_id`.
- [x] Add a focused red test proving malformed `function_call` output items are not forwarded as successful SSE. Red result: local response body contained `event: response.output_item.done`.
- [x] Skip malformed `function_call` output-item frames in both WebSocket forwarding paths.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 45 if code/docs changes are justified. Code/docs commit: `5c46b54 fix: ignore malformed websocket function call output items`.

### Task 46: Ignore Invalid `tool_search_call` Output Items

**Files:**
- Inspect: official `codex-rs/protocol/src/models.rs` from git tree
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `ResponseItem::ToolSearchCall` requires string `execution` and a present `arguments` field, while `arguments` may be any JSON value.
- [x] Add a focused red test proving malformed `tool_search_call` output items are not forwarded as successful SSE. Red result: local response body contained `event: response.output_item.done`.
- [x] Skip malformed `tool_search_call` output-item frames in both WebSocket forwarding paths.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 46 if code/docs changes are justified. Code/docs commit: `73ce6de fix: ignore malformed websocket tool search output items`.

### Task 47: Ignore Invalid `function_call_output` Output Items

**Files:**
- Inspect: official `codex-rs/protocol/src/models.rs` from git tree
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `ResponseItem::FunctionCallOutput` requires string `call_id` and an `output` value that parses as `FunctionCallOutputPayload`, whose wire body is a string or array.
- [x] Add a focused red test proving malformed `function_call_output` output items are not forwarded as successful SSE. Red result: local response body contained `event: response.output_item.done`.
- [x] Skip malformed `function_call_output` output-item frames in both WebSocket forwarding paths.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 47 if code/docs changes are justified. Code/docs commit: `eb48085 fix: ignore malformed websocket function output items`.

### Task 48: Ignore Invalid `custom_tool_call` Output Items

**Files:**
- Inspect: official `codex-rs/protocol/src/models.rs` from git tree
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `ResponseItem::CustomToolCall` requires string `call_id`, `name`, and `input`.
- [x] Add a focused red test proving malformed `custom_tool_call` output items are not forwarded as successful SSE. Red result: local response body contained `event: response.output_item.done`.
- [x] Skip malformed `custom_tool_call` output-item frames in both WebSocket forwarding paths.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 48 if code/docs changes are justified. Code/docs commit: `26e94a7 fix: ignore malformed websocket custom tool calls`.

### Task 49: Ignore Invalid `custom_tool_call_output` Output Items

**Files:**
- Inspect: official `codex-rs/protocol/src/models.rs` from git tree
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `ResponseItem::CustomToolCallOutput` requires string `call_id` and an `output` value that parses as `FunctionCallOutputPayload`, whose wire body is a string or array.
- [x] Add a focused red test proving malformed `custom_tool_call_output` output items are not forwarded as successful SSE. Red result: local response body contained `event: response.output_item.done`.
- [x] Skip malformed `custom_tool_call_output` output-item frames in both WebSocket forwarding paths.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 49 if code/docs changes are justified. Code/docs commit: `5ac1e56 fix: ignore malformed websocket custom tool outputs`.

### Task 50: Ignore Invalid `tool_search_output` Output Items

**Files:**
- Inspect: official `codex-rs/protocol/src/models.rs` from git tree
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `ResponseItem::ToolSearchOutput` requires string `status`, string `execution`, and array `tools`.
- [x] Add a focused red test proving malformed `tool_search_output` output items are not forwarded as successful SSE. Red result: local response body contained `event: response.output_item.done`.
- [x] Skip malformed `tool_search_output` output-item frames in both WebSocket forwarding paths.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 50 if code/docs changes are justified. Code/docs commit: `89d6fb2 fix: ignore malformed websocket tool search outputs`.

### Task 51: Ignore Invalid `image_generation_call` Output Items

**Files:**
- Inspect: official `codex-rs/protocol/src/models.rs` from git tree
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `ResponseItem::ImageGenerationCall` requires string `id`, string `status`, and string `result`.
- [x] Add a focused red test proving malformed `image_generation_call` output items are not forwarded as successful SSE. Red result: local response body contained `event: response.output_item.done`.
- [x] Skip malformed `image_generation_call` output-item frames in both WebSocket forwarding paths.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 51 if code/docs changes are justified. Code/docs commit: `5aa33f7 fix: ignore malformed websocket image generation calls`.

### Task 52: Ignore Invalid `compaction` Output Items

**Files:**
- Inspect: official `codex-rs/protocol/src/models.rs` from git tree
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `ResponseItem::Compaction` accepts `compaction` / `compaction_summary` and requires string `encrypted_content`.
- [x] Add a focused red test proving malformed `compaction` output items are not forwarded as successful SSE. Red result: local response body contained `event: response.output_item.done`.
- [x] Skip malformed `compaction` output-item frames in both WebSocket forwarding paths.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 52 if code/docs changes are justified. Code/docs commit: `1b2480c fix: ignore malformed websocket compaction items`.

### Task 53: Ignore Invalid `agent_message` Output Items

**Files:**
- Inspect: official `codex-rs/protocol/src/models.rs` from git tree
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `ResponseItem::AgentMessage` requires string `author`, string `recipient`, and array `content`.
- [x] Add a focused red test proving malformed `agent_message` output items are not forwarded as successful SSE. Red result: local response body contained `event: response.output_item.done`.
- [x] Skip malformed `agent_message` output-item frames in both WebSocket forwarding paths.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 53 if code/docs changes are justified. Code/docs commit: `c1ab0f3 fix: ignore malformed websocket agent messages`.

### Task 54: Ignore Invalid `reasoning` Output Items

**Files:**
- Inspect: official `codex-rs/protocol/src/models.rs` from git tree
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `ResponseItem::Reasoning` requires array `summary`, defaults missing `id`, and accepts optional `content` / `encrypted_content` only when their present values parse.
- [x] Add a focused red test proving malformed `reasoning` output items are not forwarded as successful SSE. Red result: local response body contained `event: response.output_item.done`.
- [x] Skip malformed `reasoning` output-item frames in both WebSocket forwarding paths.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 54 if code/docs changes are justified. Code/docs commit: `8e6cb5d fix: ignore malformed websocket reasoning items`.

### Task 55: Ignore Invalid `message.content` Output Items

**Files:**
- Inspect: official `codex-rs/protocol/src/models.rs` from git tree
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs` only if a new predicate is needed
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `ResponseItem::Message.content` parses as `Vec<ContentItem>`, where known item variants require typed fields and `ImageDetail` values.
- [x] Add a focused red test proving malformed nested `message.content` items are not forwarded as successful SSE. Red result: local response body contained `event: response.output_item.done`.
- [x] Skip malformed nested `message.content` items in both WebSocket forwarding paths.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 55 if code/docs changes are justified. Code/docs commit: `d02d270 fix: ignore malformed websocket message content items`.

### Task 56: Ignore Invalid `agent_message.content` Output Items

**Files:**
- Inspect: official `codex-rs/protocol/src/models.rs` from git tree
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `src/codex/tasks/token_refresh.rs` test helper stability if full-suite virtual-time verification flakes
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `ResponseItem::AgentMessage.content` parses as `Vec<AgentMessageInputContent>`, whose known item variants require typed fields.
- [x] Add a focused red test proving malformed nested `agent_message.content` items are not forwarded as successful SSE. Red result: local response body contained `event: response.output_item.done`.
- [x] Skip malformed nested `agent_message.content` items in both WebSocket forwarding paths.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 56 if code/docs changes are justified. Code/docs commit: `b82c49a fix: ignore malformed websocket agent message content`.

### Task 57: Ignore Invalid `reasoning.summary` and `reasoning.content` Output Items

**Files:**
- Inspect: official `codex-rs/protocol/src/models.rs` from git tree
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `ResponseItem::Reasoning.summary` and `ResponseItem::Reasoning.content` parse as tagged nested enums with required string `text`.
- [x] Add a focused red test proving malformed nested `reasoning.summary` / `reasoning.content` items are not forwarded as successful SSE. Red result: local response body contained `event: response.output_item.done`.
- [x] Skip malformed nested `reasoning.summary` / `reasoning.content` items in both WebSocket forwarding paths.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 57 if code/docs changes are justified. Code/docs commit: `14ad08a fix: ignore malformed websocket reasoning content`.

### Task 58: Ignore Invalid Function Output Content Items

**Files:**
- Inspect: official `codex-rs/protocol/src/models.rs` from git tree
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `function_call_output.output[]` and `custom_tool_call_output.output[]` parse as `FunctionCallOutputContentItem` tagged items.
- [x] Add a focused red test proving malformed structured function/custom-tool output content items are not forwarded as successful SSE. Red result: local response body contained `event: response.output_item.done`.
- [x] Skip malformed structured function/custom-tool output content items in both WebSocket forwarding paths.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 58 if code/docs changes are justified. Code/docs commit: `1ffd263 fix: ignore malformed websocket function output content`.

### Task 59: Ignore Invalid `local_shell_call` Output Items

**Files:**
- Inspect: official `codex-rs/protocol/src/models.rs` from git tree
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `ResponseItem::LocalShellCall` requires `status: LocalShellStatus` and `action: LocalShellAction`, with `exec.command: Vec<String>`.
- [x] Add a focused red test proving malformed `local_shell_call` output items are not forwarded as successful SSE. Red result: local response body contained `event: response.output_item.done`.
- [x] Skip malformed `local_shell_call` output-item frames in both WebSocket forwarding paths.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 59 if code/docs changes are justified. Code/docs commit: `0dbe3ce fix: ignore malformed websocket local shell calls`.

### Task 60: Ignore Invalid `web_search_call` Output Items

**Files:**
- Inspect: official `codex-rs/protocol/src/models.rs` from git tree
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `ResponseItem::WebSearchCall` has optional typed `id`, `status`, and `action`, where known action variants have typed optional fields.
- [x] Add a focused red test proving malformed `web_search_call` output items are not forwarded as successful SSE. Red result: `cargo test --test codex_gateway websocket_web_search_call_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client` fails because local still forwards `event: response.output_item.done`.
- [x] Skip malformed `web_search_call` output-item frames in both WebSocket forwarding paths.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 60 if code/docs changes are justified. Code/docs commit: `9b0b799 fix: ignore malformed websocket web search calls`.

### Task 61: Ignore Invalid `context_compaction` Output Items

**Files:**
- Inspect: official `codex-rs/protocol/src/models.rs` from git tree
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `ResponseItem::ContextCompaction` has optional typed `encrypted_content`.
- [x] Add a focused red test proving malformed `context_compaction` output items are not forwarded as successful SSE. Red result: `cargo test --test codex_gateway websocket_context_compaction_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client` fails because local still forwards `event: response.output_item.done`.
- [x] Skip malformed `context_compaction` output-item frames in both WebSocket forwarding paths.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 61 if code/docs changes are justified. Code/docs commit: `87b3d22 fix: ignore malformed websocket context compaction`.

### Task 62: Ignore Invalid Output Item Metadata

**Files:**
- Inspect: official `codex-rs/protocol/src/models.rs` from git tree
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: local `src/codex/gateway/transport/websocket/mod.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that known `ResponseItem` variants share `metadata: Option<ResponseItemMetadata>` with optional typed `turn_id`.
- [x] Add a focused red test proving malformed output-item metadata is not forwarded as successful SSE. Red result: `cargo test --test codex_gateway websocket_output_item_events_with_invalid_metadata_should_be_ignored_like_official_client` fails because local still forwards `event: response.output_item.done`.
- [x] Skip output-item frames whose present `metadata` cannot parse as official `ResponseItemMetadata`.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 62 if code/docs changes are justified. Code/docs commit: `3e633c4 fix: ignore malformed websocket output metadata`.

### Task 63: Ignore Invalid `message` Optional Output Item Fields

**Files:**
- Inspect: official `codex-rs/protocol/src/models.rs` from git tree
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `ResponseItem::Message` has optional typed `id` and `phase: Option<MessagePhase>`.
- [x] Add a focused red test proving malformed `message.id` / `message.phase` output items are not forwarded as successful SSE. Red result: `cargo test --test codex_gateway websocket_message_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client` fails because local still forwards `event: response.output_item.done`.
- [x] Skip malformed `message` output-item frames whose optional `id` or `phase` fields cannot parse officially.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 63 if code/docs changes are justified. Code/docs commit: `a740cb4 fix: ignore malformed websocket message optional fields`.

### Task 64: Ignore Invalid `function_call` Optional Output Item Fields

**Files:**
- Inspect: official `codex-rs/protocol/src/models.rs` from git tree
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `ResponseItem::FunctionCall` has optional typed `id` and `namespace`.
- [x] Add a focused red test proving malformed `function_call.id` / `function_call.namespace` output items are not forwarded as successful SSE. Red result: `cargo test --test codex_gateway websocket_function_call_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client` fails because local still forwards `event: response.output_item.done`.
- [x] Skip malformed `function_call` output-item frames whose optional `id` or `namespace` fields cannot parse officially.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 64 if code/docs changes are justified. Code/docs commit: `9bbd461 fix: ignore malformed websocket function call optional fields`.

### Task 65: Ignore Invalid `tool_search_call` Optional Output Item Fields

**Files:**
- Inspect: official `codex-rs/protocol/src/models.rs` from git tree
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `ResponseItem::ToolSearchCall` has optional typed `id`, `call_id`, and `status`.
- [x] Add a focused red test proving malformed `tool_search_call.id` / `call_id` / `status` output items are not forwarded as successful SSE. Red result: `cargo test --test codex_gateway websocket_tool_search_call_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client` fails because local still forwards `event: response.output_item.done`.
- [x] Skip malformed `tool_search_call` output-item frames whose optional `id`, `call_id`, or `status` fields cannot parse officially.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 65 if code/docs changes are justified. Code/docs commit: `6c75142 fix: ignore malformed websocket tool search call optional fields`.

### Task 66: Ignore Invalid `custom_tool_call` Optional Output Item Fields

**Files:**
- Inspect: official `codex-rs/protocol/src/models.rs` from git tree
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `ResponseItem::CustomToolCall` has optional typed `id` and `status`.
- [x] Add a focused red test proving malformed `custom_tool_call.id` / `status` output items are not forwarded as successful SSE. Red result: `cargo test --test codex_gateway websocket_custom_tool_call_output_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client` fails because local still forwards `event: response.output_item.done`.
- [x] Skip malformed `custom_tool_call` output-item frames whose optional `id` or `status` fields cannot parse officially.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 66 if code/docs changes are justified. Code/docs commit: `f63cc60 fix: ignore malformed websocket custom tool optional fields`.

### Task 67: Ignore Invalid `custom_tool_call_output` Optional Output Item Fields

**Files:**
- Inspect: official `codex-rs/protocol/src/models.rs` from git tree
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `ResponseItem::CustomToolCallOutput` has optional typed `name`.
- [x] Add a focused red test proving malformed `custom_tool_call_output.name` output items are not forwarded as successful SSE. Red result: `cargo test --test codex_gateway websocket_custom_tool_call_output_result_item_events_with_invalid_optional_fields_should_be_ignored_like_official_client` fails because local still forwards `event: response.output_item.done`.
- [x] Skip malformed `custom_tool_call_output` output-item frames whose optional `name` field cannot parse officially.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 67 if code/docs changes are justified. Code/docs commit: `f24276a fix: ignore malformed websocket custom tool output optional fields`.

### Task 68: Confirm All Remaining WebSocket Parity Differences

**Files:**
- Inspect: official `codex-api/src/sse/responses.rs`
- Inspect: official `codex-api/src/endpoint/responses_websocket.rs`
- Inspect: official `codex-rs/protocol/src/models.rs` from git tree
- Inspect: local `src/codex/gateway/transport/websocket/codec.rs`
- Inspect: local `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Compare official `ResponsesStreamEvent` top-level shape with local `ResponsesStreamEventShape`.
- [x] Compare every known official `ResponseItem` variant field shape against local output-item predicates.
- [x] Separate remaining findings into `source-backed must change`, `already covered`, `not proxy API surface`, and `needs macOS live capture`.
- [x] Record the full remaining-difference matrix before doing more one-field implementation tasks.

### Task 69: Close Remaining Source-Backed Optional Output Item Field Gaps

**Files:**
- Inspect: official `codex-rs/protocol/src/models.rs` from git tree
- Modify: local `src/codex/gateway/transport/websocket/codec.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`

- [x] Record official source evidence that `ResponseItem::ToolSearchOutput.call_id` and `ResponseItem::ImageGenerationCall.revised_prompt` are typed `Option<String>`.
- [x] Add focused red tests proving present non-string `tool_search_output.call_id` and `image_generation_call.revised_prompt` frames are not forwarded by the official-compatible chain. Red result: both focused tests fail because local still forwards `event: response.output_item.done`.
- [x] Skip malformed output-item frames whose remaining optional string fields cannot parse officially.
- [x] Run focused WebSocket checks, formatting, diff check, Clippy, and full test suite.
- [x] Commit Task 69 if code/docs changes are justified. Code/docs commit: `24d9774 fix: ignore remaining malformed websocket optional output fields`.

### Task 70: Use Linux Repack To Sample Official Desktop Live WebSocket Chain

**Files/artifacts:**
- Inspect/run: `/home/zyy/桌面/Codes/codex-desktop-linux` (`/home/zyy/Codes/codex-desktop-linux` real path)
- Inspect/use: mihomo controller `127.0.0.1:9090`
- Create local ignored artifacts under `.codex-ws-audit/task70-desktop/`
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`
- Modify: `docs/superpowers/plans/2026-06-17-codex-desktop-ws-parity.md`

- [x] Launch the Linux Desktop repack and prove the sampled app-server process is parented by `codex-desktop-linux/codex-app/electron`.
- [x] Capture the official Desktop-launched app-server WebSocket opening request, ordered headers, local upgrade response, and first client text frame via a redacted local endpoint.
- [x] Sample mihomo process attribution for live Desktop network traffic without attempting to decrypt TLS payloads.
- [x] Capture Desktop-launched app-server TLS ClientHello fingerprints through a local TLS endpoint and separate WSS attempts from HTTP fallback.
- [x] Compare the official Desktop artifact against the current rs audit artifact.
- [x] Record findings, caveats, and artifact paths in the Analysis Journal.

### Task 71: Align Request Shape From Complete live5 Chain

**Files/artifacts:**
- Inspect: `.codex-ws-audit/live5/flows.all.txt`
- Modify: `src/codex/gateway/transport/websocket/opening.rs`
- Modify: `src/codex/gateway/transport/websocket/codec.rs`
- Modify: `src/codex/gateway/transport/types.rs`
- Modify: `src/codex/serving/responses.rs`
- Modify: `tests/codex_gateway/websocket.rs`
- Modify: `tests/codex_serving/responses_websocket.rs`
- Modify: `src/codex/tasks/token_refresh.rs` test helper stability if full-suite virtual-time verification flakes
- Modify: `docs/superpowers/specs/2026-06-16-codex-desktop-ws-parity-design.md`
- Modify: `docs/superpowers/plans/2026-06-17-codex-desktop-ws-parity.md`

- [x] Identify the complete live5 model WebSocket flows and compare them against the current rs capture.
- [x] Add focused red tests for live5 WebSocket opening header order/casing and omitted browser-style headers.
- [x] Add focused red tests for first-frame `generate:false` audit/serving forwarding.
- [x] Align WebSocket opening serialization to the Desktop app-server live chain where local request data exists.
- [x] Parse and forward `generate` through the serving request path.
- [x] Run focused gateway and serving tests for the changed request shape.
- [x] Run broader WebSocket gateway/serving checks, formatting, Clippy, and diff check.
