# 后台调度器实现

## 当前边界

后台任务由 `src/runtime/tasks/coordinator.rs` 统一启动和关闭。运行时只保存任务句柄，具体业务逻辑留在各自领域模块：

- `src/codex/tasks/refresh.rs`：OAuth access token 刷新调度。
- `src/codex/tasks/quota.rs`：quota 锁定账户的主动刷新。
- `src/codex/tasks/model.rs`：Codex 后端模型列表刷新。
- `src/admin/tasks/session_cleanup.rs`：管理员 session 过期清理。
- `src/codex/gateway/fingerprint/update_checker.rs`：Codex Desktop fingerprint 版本轮询。

调度器共享 `SchedulerHandle`，应用退出时由 `BackgroundTaskCoordinator::shutdown()` 逐个关闭。

## RefreshScheduler

`RefreshScheduler` 对齐原版刷新调度的关键行为：

- 按 JWT `exp - refresh_margin_seconds` 调度刷新。
- 使用指数退避，最多 5 次：5s、15s、45s、135s、300s。
- 区分封禁类错误和 refresh token 失效类错误。
- 进程重启后恢复 `refreshing` 和带 refresh token 的 `expired` 账户。
- 用 per-account `in_flight` 抑制重复刷新。
- 用 `auth.refresh_concurrency` 通过 `Semaphore` 限制全局刷新并发。

这部分的行为测试集中在 `tests/codex_accounts/refresh.rs`，包括刷新成功、401 触发刷新、错误状态映射和并发上限。

## Account Scheduling

账号池调度逻辑在 `src/codex/accounts/pool.rs`：

- `least_used` 先降权 quota-limited 账号。
- 再按 `window_reset_at` 排序，优先选择更早进入下一窗口的账号。
- 再比较 `request_count`。
- 最后在并列账号之间轮换，避免固定选择同一个账号。

调度所需的 `request_count`、窗口 token 计数、`window_started_at`、`window_reset_at` 和 `limit_window_seconds` 已持久化到 `account_usage`。重启后 `AccountRepository::list_pool_accounts()` 会恢复这些字段，避免短期调度状态丢失。

## Storage Boundaries

后台任务不直接散落业务表 SQL：

- 默认管理员创建、管理员登录 session、session 校验和过期清理由 `AdminAuthRepository` 访问 `admin_users` / `admin_sessions`。
- fingerprint 历史写入和 `auto_updated` 当前记录由 `FingerprintRepository` 访问 `fingerprints`。
- session affinity 由 `SessionAffinityRepository` 访问 `session_affinities`。
- 请求事件日志由 `LogService::record` 统一执行开关、容量和 body capture 策略。

## Verification

当前实现已通过：

- `cargo fmt --check`
- `git diff --check`
- `cargo test`
- `cargo clippy --all-targets --all-features --locked -- -D warnings`

本文件只描述当前 Rust 实现，不再保留旧 `src/scheduler/*` 路径或早期直接 SQL 方案。
