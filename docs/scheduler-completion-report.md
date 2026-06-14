# 账号调度与数据库审计落地报告

## 结论

本轮把账号调度审计和数据库存储审计合并落地。当前实现已经覆盖审计中需要立即修复的调度、刷新、窗口统计、session affinity、日志入口、schema 约束、cookie 过期语义和直接 SQL 边界问题。

不在本报告范围内的事项：IP 代理/VPN、旧项目数据迁移、真实 Codex Desktop TLS 指纹完全复刻。这些不是当前 Rust 服务要移植的能力。

## 账号调度审计项

### least_used 策略

已实现完整优先级：

1. quota-limited 账号降权。
2. `window_reset_at` 更早的账号优先。
3. `request_count` 更少的账号优先。
4. 并列时轮换，避免固定账号长期被选中。

对应测试：

- `least_used_should_deprioritize_quota_limited_accounts_when_skip_is_disabled`
- `least_used_should_prefer_earlier_rate_limit_window_reset`
- `least_used_should_prefer_lower_runtime_request_count`
- `account_pool_should_rotate_tied_least_used_accounts`

### refresh_concurrency

已通过 `tokio::sync::Semaphore` 增加全局刷新并发限制。配置来源为 `auth.refresh_concurrency`，并对 0 做最小值保护。

对应测试：

- `refresh_scheduler_limits_refresh_concurrency`
- `refresh_concurrency_limit_should_never_be_zero`

### 请求计数和窗口统计

`Account`、`AccountPool`、`AccountRepository` 和 `account_usage` 现在共同维护：

- 累计 `request_count`
- 窗口内请求数
- 窗口内 input/output/cached token 计数
- `window_started_at`
- `window_reset_at`
- `limit_window_seconds`

这些字段会在请求完成时累加，在收到 rate-limit header 时同步窗口边界，并在启动恢复账号池时重新加载。

对应测试：

- `account_repository_should_accumulate_usage_window_counters`
- `account_repository_should_restore_window_usage_into_runtime_pool_accounts`
- `v1_responses_should_passively_cache_rate_limit_headers`
- `sqlite_schema_should_persist_account_usage_window_columns`

## 数据库存储审计项

### account_usage 窗口状态

已落地到 `account_usage`，并为旧库启动补列。窗口统计属于 usage 域，不放入 `accounts`。

### session affinity

已增加 `SessionAffinityRepository`，完成响应后写入 SQLite，启动时删除过期记录并恢复未过期映射。`function_call_ids_json` 已持久化，避免工具调用分支信息只存在于内存。

对应测试：

- `app_state_should_restore_session_affinity_from_sqlite`
- `v1_responses_should_route_previous_response_id_to_recorded_account`
- `v1_responses_websocket_should_reuse_connection_for_recorded_conversation`

### 事件日志入口

请求链路和管理端共享 `LogService`。写入事件日志时统一执行：

- `enabled`
- `capacity`
- `capture_body`
- metadata 清理

对应测试：

- `v1_responses_should_skip_event_log_when_logging_disabled`
- `log_service_should_trim_to_capacity_after_record`
- `log_service_should_remove_body_metadata_when_capture_body_disabled`

### schema 约束

SQLite schema 已补充状态枚举、日志级别、布尔字段、非负计数、HTTP 状态码和 `limit_window_seconds` 约束。

对应测试：

- `sqlite_schema_should_reject_invalid_account_status`
- `sqlite_schema_should_reject_invalid_event_log_level`
- `sqlite_schema_should_reject_non_boolean_flags`
- `sqlite_schema_should_reject_negative_account_usage_counts`

### cookie 过期语义

`CookieRepository::cookie_header` 读取时过滤已过期 cookie，支持标准 `Expires` 时间和 RFC3339。无法解析的历史值会保守保留，避免误删仍可能有效的 cookie。

对应测试：

- `cookie_repository_should_not_replay_expired_cookies`

### 直接 SQL 边界

直接访问业务表的 SQL 已收敛到 repository：

- `admin_users` / `admin_sessions`：`AdminAuthRepository`
- `fingerprints`：`FingerprintRepository`
- `session_affinities`：`SessionAffinityRepository`
- `event_logs`：`EventLogRepository`，由 `LogService` 统一入口调用

对应测试：

- `admin_auth_repository_should_create_and_load_default_admin_once`
- `admin_auth_repository_should_create_validate_and_cleanup_sessions`
- `fingerprint_repository_should_upsert_auto_update_record`

## 当前验证

最后一次完整验证命令：

- `cargo fmt --check`
- `git diff --check`
- `cargo test`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`

以上命令均已通过。
