# 数据库存储审计与优化计划

## 结论

当前数据库设计的主体边界是清晰的：账号密钥、cookie、quota 缓存、事件日志、管理会话和模型快照都有独立表承载。账号调度依赖的窗口用量和 session affinity 已从纯内存状态收敛到 SQLite，重启后的短期调度退化已经被消除。事件日志写入也已统一到 `LogService`，请求链路会遵守运行时日志策略。

最优先要修复的是账号用量窗口状态。以下字段属于 `account_usage`，不是 `accounts`：

- `window_request_count`
- `window_input_tokens`
- `window_output_tokens`
- `window_cached_tokens`
- `window_started_at`
- `window_reset_at`
- `limit_window_seconds`

原因是它们描述的是账号在当前 rate-limit window 内的用量快照，而不是账号身份、密钥或生命周期属性。放在 `account_usage` 可以让累计统计和窗口统计共享同一个存储边界，也能让 `least_used` 调度在重启后恢复更多上下文。

## 已验证问题

`Account` 和 `AccountPool` 已经有窗口字段，调度逻辑也会使用 `window_reset_at`、窗口内请求数和 token 计数。审计时 `account_usage` 表没有这些列，`AccountRepository::list_pool_accounts()` 也把窗口字段固定恢复为 0 或 `None`。当前实现已将这些字段落到 `account_usage`，并在旧库启动时自动补列。

这不会损坏 access token、refresh token 或 cookie，也不会造成永久性数据异常。真实影响是：

- 重启后窗口计数归零，`least_used` 会丢失短期负载信息。
- `window_reset_at` 丢失后，调度无法按上游 rate-limit window 做更精确排序。
- 窗口内 token 消耗无法跨进程保留，统计和排障信息不完整。

Claude 审计中“窗口字段未持久化”的结论成立，但“配额控制失效”的表述偏重。项目已经持久化 `quota_json`、`quota_limit_reached`、`quota_cooldown_until` 和 Cloudflare cooldown；缺失的是窗口统计快照，而不是所有配额状态。

## 目标存储边界

`accounts` 继续只承载账号身份、密钥密文、状态和上游 quota/cooldown 摘要。

`account_usage` 承载所有本地统计：

- 累计请求和 token 计数。
- 空响应计数。
- 当前窗口请求和 token 计数。
- 当前窗口开始、重置时间和窗口长度。

`session_affinities` 承载 `previous_response_id` 到账号和会话上下文的映射。当前实现已补 `SessionAffinityRepository`，完成响应时同步写库，启动时清理过期记录并恢复未过期映射。表结构补充 `function_call_ids_json`，避免工具调用分支信息只存在于内存。

事件日志通过统一日志服务入口写入，避免请求链路绕过日志容量、开关和 body capture 策略。

## 优化阶段

### P0：持久化 account_usage 窗口状态（已落地）

- 扩展 `account_usage` schema。
- 对旧库启动时自动补列。
- `record_usage` 同时更新累计计数和窗口计数。
- `list_pool_accounts` 从数据库恢复窗口字段。
- 收到 rate-limit header 时持久化 `window_reset_at` 和 `limit_window_seconds`。

### P1：持久化 session affinity（已落地）

- 增加 `SessionAffinityRepository`。
- 将内存 affinity map 的写入同步到 `session_affinities`。
- 启动时清理过期记录并恢复未过期映射。
- 补充 `function_call_ids_json`，恢复 `function_call_ids`。

### P1：统一事件日志入口（已落地）

- v1 response 和 401 后账户刷新失败事件通过 `LogService::record` 写入。
- 在一个入口执行 `enabled`、`capacity`、`capture_body` 和 metadata 规范。
- 管理端 `/api/admin/logs/state` 和请求链路共享同一个 `LogService` 实例，运行时开关立即生效。

### P2：收敛 schema 约束（已落地）

- 为账号状态、事件日志级别增加枚举 check constraints。
- 为 enabled、quota_limit_reached 增加 0/1 check constraints。
- 为 usage 计数、窗口计数、latency、session input_tokens 增加非负 check constraints。
- 为 `limit_window_seconds` 增加正数约束。

### P2：收敛直接 SQL（已落地）

- `admin_users` 和 `admin_sessions` 已收敛到 `AdminAuthRepository`。
- 管理登录、session 校验、默认管理员初始化和过期 session 清理都通过 repository 访问数据库。
- fingerprint 更新已从 updater 内部 SQL 迁移到 `FingerprintRepository`。
- `fingerprints` 同时支持历史记录写入和 `auto_updated` 当前记录 upsert。

### P2：规范 cookie `expires_at` 读取语义（已落地）

- `CookieRepository::cookie_header` 读取时过滤已过期 cookie。
- 支持解析标准 `Expires` 时间和 RFC3339 时间。
- 无法解析的历史值保守保留，避免误删仍可能有效的 cookie。
