# Logging Refactor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the custom file logging path with the Rust community `tracing-appender` stack and standardize source log messages in Chinese.

**Architecture:** Keep two separate logging concerns. Runtime process logs go through `tracing` JSON output and daily rolling files. Admin queryable business events stay in SQLite through `EventLogRepository` and `LogService`.

**Tech Stack:** Rust, `tracing`, `tracing-subscriber`, `tracing-appender`, `tower-http::trace`, `axum`, `serde_yml`, `thiserror`.

---

### Task 1: Remove Size-Based Logging Configuration

**Files:**
- Modify: `src/config/types.rs`
- Modify: `config.yaml`
- Modify: `tests/config.rs`
- Modify: test support configs under `tests/`

- [x] Write a failing test that rejects `logging.max_file_bytes`.
- [x] Remove `max_file_bytes` from `LoggingConfig` and default config.
- [x] Update all test fixtures that construct `LoggingConfig`.
- [x] Run `cargo test --test config`.

### Task 2: Replace Custom Rotating Writer

**Files:**
- Modify: `src/codex/logs/rotation.rs`
- Modify: `tests/codex_operations/log_rotation.rs`
- Modify: `src/main.rs`

- [x] Replace the old size/date writer with `RollingFileAppender::builder()`.
- [x] Return a guard that keeps the non-blocking writer alive.
- [x] Configure JSON subscriber output with target, level, file, line, current span, and span list.
- [x] Run `cargo test --test codex_operations`.

### Task 3: Improve HTTP Trace Spans

**Files:**
- Modify: `src/runtime/router.rs`
- Test: `tests/runtime/http_trace.rs`

- [x] Add a test that verifies request logs include `request_id`, method, uri, status, and latency.
- [x] Build a custom `TraceLayer` that logs Chinese lifecycle messages and avoids headers/bodies.
- [x] Keep request id middleware in the correct layer order.
- [x] Run the HTTP trace test.

### Task 4: Standardize Structured Logs

**Files:**
- Modify: `src/**/*.rs`

- [x] Convert string-formatted logs to structured fields.
- [x] Convert all source log messages to Chinese, preserving technical English terms where clearer.
- [x] Change expected WebSocket fallback from `warn` to `info`.
- [x] Add focused `#[tracing::instrument]` spans around upstream response transport paths without logging secrets.

### Task 5: Verify and Commit

**Files:**
- All changed files

- [x] Run `cargo fmt --check`.
- [x] Run `git diff --check`.
- [x] Run `cargo test`.
- [x] Run `cargo clippy --all-targets --all-features --locked -- -D warnings`.
- [x] Commit with a short message and the `Co-authored-by: Codex <noreply@openai.com>` trailer.
