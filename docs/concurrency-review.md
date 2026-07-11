# 并发与错误处理审查

> 审查分支：`feat/postgres-redis-migration`
> 最终复核：2026-07-11
> 覆盖范围：账号池、WebSocket 连接池、token 刷新、HTTP 停机、日志回灌、更新回滚及后台清理任务。

## 结论

本轮确认的取消泄漏、停机数据窗口、timer 竞态和错误吞没均已修复。当前并发设计遵守以下不变量：

1. 每次资源占用都有唯一 ID 和唯一 lease，释放只命中该 lease 持有的资源。
2. 请求 future 被取消时，lease 的 `Drop` 路径负责回收资源；正常完成路径消费 lease，二者不会重复释放其他并发请求。
3. token 刷新只有账号级 timer 一条生产执行路径，不保留旧扫描兼容入口。
4. HTTP 排水、后台任务关闭和 token 刷新收尾均有明确时间上限。
5. `backend/src` 不包含测试模块或测试函数；测试统一位于 `backend/tests`。

## 问题处理结果

### P0：WebSocket Busy reservation 取消泄漏

状态：已修复。

- `WebSocketPoolReservation` 使用 UUID 标识单次占用，并记录 `reserved_at`。
- `WebSocketPoolAcquire::Reused` 和 `FreshReserved` 都返回 `WebSocketPoolLease`。
- `put`、`discard` 只在 reservation ID 匹配时修改 slot，旧任务不能覆盖新 reservation。
- 请求在 connect、send 或首帧 prefetch 期间被取消时，lease 的 `Drop` 会条件删除自己的 Busy slot。
- 维护任务会清理超过 `max_age` 的异常 Busy reservation，作为进程内最终兜底。
- 流式转发任务接管 lease 后，连接归还权随任务生命周期移动。

回归测试：`backend/tests/upstream/openai/transport/websocket_pool.rs`，覆盖首帧 prefetch 取消、下游主动断开、复用和池容量限制。

### P1：账号池 slot 取消泄漏

状态：已修复。

- 每个 `AccountSlot` 使用 UUID 标识，不再按账号 ID 模糊释放队首 slot。
- `AccountPoolService::acquire_with` 返回 `AccountLease`；lease 在状态持久化 await 之前创建，覆盖 acquire 内部取消窗口。
- `AccountLease::complete` 精确释放 slot 并记录真实上游请求用量。
- `AccountLease::release_without_usage` 用于尚未发起上游请求的配额预检分支。
- lease 的 `Drop` 精确释放自己的 slot，不会释放同账号的其他并发请求。
- 模型刷新使用独立的 `DistinctPlanAccountLease`，不保留旧 `release(account_id)` 兼容 API。
- 流式任务持有完整 lease；下游断开、任务取消或错误退出都会回收 slot。

回归测试：`backend/tests/fleet/pool/`，覆盖并发 slot 精确释放、lease Drop 容量恢复和运行时用量持久化。

### P1：HTTP 和后台任务停机无界

状态：已修复。

- 第一次关闭信号触发 Axum graceful shutdown。
- HTTP 在途请求最多排水 20 秒；第二次关闭信号立即结束排水。
- 排水超时后先丢弃 serve future，取消剩余连接，再关闭后台服务。
- 后台任务并行关闭；单任务上限 5 秒，协调器总上限 6 秒。
- 总预算不超过 Compose 的 `stop_grace_period: 30s`。

### P1：token 刷新任务不可见及数据丢失窗口

状态：已修复。

- 所有 timer 和已开始的刷新任务统一注册到 `TaskTracker`。
- 到期账号不再在周期 tick 内同步执行，而是进入相同的 tracked timer 路径。
- `shutdown` 停止新调度、取消仍在睡眠的 timer，并等待已进入刷新流程的任务结束。
- Redis 刷新租约使用 owner 校验释放；取消清理任务也由同一个 `TaskTracker` 管理。
- `in_flight` 使用同步 RAII guard，panic 和取消都能移除账号 ID。
- 已删除无生产调用方的 `refresh_due_accounts_once_at` 和 `TokenRefreshSummary`，不存在第二套测试专用执行路径。

回归测试：`backend/tests/fleet/refresh/`，覆盖结构化关闭、重复 in-flight 跳过、租约竞争、重试、恢复时间和 timer 替换。

### P1：账号选择锁内开销随账号数放大

状态：已修复。

- `access_token_expires_at` 存在时直接比较时间；仅在该字段缺失时解析 JWT。
- 候选过滤返回账号借用，不再克隆全部候选。
- Smart、QuotaResetPriority、RoundRobin 和 Sticky 均在借用切片上选择，只克隆最终账号。
- 按订阅计划选择模型刷新账号时同样先借用分组，最终选择后才克隆。
- 保留账号池单锁，不引入会破坏跨账号调度一致性的分片锁。

回归测试：`backend/tests/fleet/pool/selection.rs` 和 `backend/tests/fleet/scheduler/`。

### P2：timer 调度竞态

状态：已修复。

- `schedule_account_timer` 的检查、旧 timer 取消、新任务创建和 map 写入在一次锁持有期间完成。
- 每个 timer 使用 UUID；任务醒来时仅能移除与自身 UUID 匹配的 entry。
- 旧 timer 不能删除或覆盖同账号的新 timer。

### P2：错误处理和边界问题

状态：已修复。

- 管理端 logout 删除 Redis 会话失败时记录 `warn`，仍清除客户端 cookie。
- API key `last_used_at` flush 失败回灌按时间取最大值，不覆盖并发写入的新时间戳。
- 自更新回滚失败不再静默；主错误会携带所有回滚失败上下文。
- `RuntimeRefreshPolicy` 仅在并发数变化时替换 semaphore，普通设置更新不会瞬时放大并发。
- WebSocket pump 入站队列改为容量 64；缓冲满时关闭异常连接，不阻塞 pump 命令处理。
- reqwest client cache miss 的 TLS 构建和 CA 文件读取移到同步 Mutex 之外。
- Cloudflare challenge cookie 的清理时间持久化到 `account_cookies.expires_at`；冷却期间保留，截止后读取失效并由周期任务删除，重启不会丢失清理计划。
- Cloudflare path-block 仍立即清除 cookie，因为该信号表示当前路径状态已不可继续使用。

## 锁与生命周期复核

- std `Mutex` / `RwLock` guard 不跨 `.await`。
- 需要跨 `.await` 的共享状态使用 Tokio 锁。
- 账号池锁内只处理内存状态，PostgreSQL 和 Redis 写入均在锁外。
- WebSocket pool 的连接关闭在锁外执行。
- 账号 slot、WebSocket reservation、token in-flight 和 Redis refresh lease 都有取消兜底。
- crate 已启用 `#![forbid(unsafe_code)]`，生产代码无 `unsafe` 路径。

## 验证

必须通过以下门禁后才可提交：

```bash
cargo fmt --manifest-path backend/Cargo.toml -- --check
cargo clippy --manifest-path backend/Cargo.toml --all-targets --all-features --locked -- -D warnings
CPR_TEST_REDIS_URL='redis://:codex_proxy@127.0.0.1:6379' \
  cargo test --manifest-path backend/Cargo.toml --all-features --locked
pnpm --dir frontend format:check
pnpm --dir frontend build
docker build -f deploy/Dockerfile --target runtime -t codex-proxy-rs:concurrency-review .
```

2026-07-11 最终验证结果：

- Rust format：通过。
- Rust Clippy：`--all-targets --all-features -D warnings` 通过，0 warning。
- 后端完整测试：630 passed，0 failed。
- 前端 Prettier、`vue-tsc`、Vite build：全部通过。
- RustSec `cargo audit`：扫描 382 个依赖，0 vulnerability。
- 前端生产依赖审计：0 known vulnerability。
- `backend/src` 测试属性扫描：0 处。
- runtime 镜像：`codex-proxy-rs:concurrency-review`，镜像 ID `sha256:96a9bb2a828b16ddae55af1c3923255d4167c6c33c341b7146b62c857c30f84b`。
- 容器健康检查：`/healthz` 返回 204，PostgreSQL 和 Redis 均为 healthy。
- 真实 SIGTERM 重启：0.319 秒完成，全部后台任务按关闭流程退出。
- 本次启动及重启日志：0 条 `WARN`，0 条 `ERROR`。
