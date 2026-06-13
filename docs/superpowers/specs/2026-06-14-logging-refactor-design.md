# Logging Refactor Design

**Date:** 2026-06-14
**Status:** Approved

## Goal

Bring runtime logging in line with Rust community practice while the project is still in active refactor. The logging path should be simple, structured, safe to query, and free of compatibility code from the earlier custom rotation implementation.

## Decisions

- Use `tracing`, `tracing-subscriber`, `tracing-appender`, and `tower-http::trace::TraceLayer` as the runtime file logging stack.
- Remove the custom `RotatingLogWriter` and size-based `max_file_bytes` configuration. File logs rotate daily through `tracing-appender`, and retention is controlled by `retention_days`.
- Keep SQLite `EventLogRepository` and `LogService`. Those records power `/admin/logs` and business event queries; they are not the same concern as process file logs.
- HTTP tracing must create structured spans with `request_id`, `method`, and `uri`, then record completion with `status` and `latency_ms`.
- Log messages are written in Chinese. Technical terms such as `WebSocket`, `HTTP SSE`, `SQLite`, and `request_id` may remain in English when that is clearer.
- Structured field names remain English because they are machine-facing query keys for JSON logs and log systems such as Loki or Elasticsearch.
- Logs must not include access tokens, refresh tokens, cookies, full headers, or request/response bodies by default.

## Runtime Shape

`main.rs` initializes tracing once and keeps the non-blocking log guard alive for the whole process. The subscriber writes JSON lines to a daily rolling appender under `logging.directory`.

Each HTTP request receives an `x-request-id` through the existing middleware. `TraceLayer` then attaches that request id to the request span and emits Chinese lifecycle messages without logging sensitive headers or bodies.

Upstream Codex calls add focused spans around transport choices. Expected fallback, such as WebSocket being unavailable for a non-history request, is an `info` event. Recoverable persistence failures remain `warn`, and unrecoverable server failures remain `error`.

## Configuration

The retained logging configuration is:

```yaml
logging:
  directory: logs
  retention_days: 14
  enabled: false
  capacity: 2000
  capture_body: false
```

`retention_days` maps to the number of daily log files retained by `tracing-appender`. `enabled`, `capacity`, and `capture_body` continue to belong to the SQLite admin event log subsystem.

`max_file_bytes` is removed instead of deprecated. This avoids a configuration field that appears to work but no longer maps to the community appender behavior.

## Validation

- Config loading rejects the removed `max_file_bytes` field.
- File logging uses the `codex-proxy-rs.<date>.log` naming produced by `tracing-appender`.
- HTTP trace output includes `request_id`, `method`, `uri`, `status`, and `latency_ms`.
- All source `tracing` messages are Chinese, with professional English terms preserved where appropriate.
- `cargo fmt --check`, `cargo test`, and `cargo clippy --all-targets --all-features --locked -- -D warnings` must pass before committing.
