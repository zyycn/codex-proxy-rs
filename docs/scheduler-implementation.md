# 后台调度器实现完成

## 概述

本次实现添加了两个关键的后台调度器，用于自动化账户令牌刷新和会话清理。这些调度器是 TypeScript 原版 `codex-proxy` 项目中核心功能的 Rust 原生重写。

## 已实现的功能

### 1. RefreshScheduler - OAuth 刷新调度器

**位置**: `src/scheduler/refresh.rs`

**功能**:
- ✅ 在 JWT 令牌 `exp - margin` 时刻自动调度刷新
- ✅ 指数退避重试策略（5次尝试：5s → 15s → 45s → 135s → 300s）
- ✅ 永久失败检测（识别 `invalid_grant`、`invalid_token`、账户封禁等错误）
- ✅ 临时失败的恢复调度（10分钟后重试）
- ✅ 崩溃恢复机制：
  - `refreshing` 状态 → 立即重试
  - `expired` + 有 refreshToken → 延迟重试（30秒起，每个账户间隔2秒）
- ✅ 并发控制（防止同一账户重复刷新）
- ✅ JWT 解析和过期时间计算
- ✅ 优雅关闭支持

**关键实现细节**:
```rust
// 永久错误检测
const BAN_ERRORS: &[&str] = &["account has been deactivated", "refresh_token_reused"];
const EXPIRED_ERRORS: &[&str] = &[
    "invalid_grant",
    "invalid_token",
    "access_denied",
    "refresh_token_expired",
];

// 指数退避算法
let backoff = BASE_DELAY_MS.saturating_mul(3_u64.pow(attempt - 1));
let backoff = backoff.min(300_000); // 最大5分钟
```

**使用方式**:
```rust
let scheduler = RefreshScheduler::new(account_service, config);
let handle = scheduler.start().await;

// 优雅关闭
handle.shutdown().await;
```

### 2. SessionCleanupScheduler - 会话清理调度器

**位置**: `src/scheduler/session_cleanup.rs`

**功能**:
- ✅ 定期删除过期的管理员会话
- ✅ 可配置的清理间隔
- ✅ 优雅关闭支持
- ✅ 直接操作 SQLite 数据库（高效、轻量级）

**实现**:
```rust
async fn cleanup_expired_sessions(&self) -> Result<u64, sqlx::Error> {
    let now = chrono::Utc::now().to_rfc3339();
    let result = sqlx::query("DELETE FROM admin_sessions WHERE expires_at < ?")
        .bind(&now)
        .execute(&self.db)
        .await?;
    Ok(result.rows_affected())
}
```

### 3. 调度器基础设施

**位置**: `src/scheduler/types.rs`, `src/scheduler/mod.rs`

**提供**:
- ✅ `SchedulerHandle` - 统一的调度器句柄，支持优雅关闭
- ✅ `SchedulerError` - 类型化的错误处理
- ✅ `SchedulerResult<T>` - 统一的结果类型

## 与原 TypeScript 版本的对比

### TypeScript 版本（参考）
- 使用 `setTimeout` 和 `setInterval`
- 回调风格的异步处理
- 内存锁文件用于跨进程同步

### Rust 版本（新实现）
- 使用 `tokio::spawn` 和 `tokio::time`
- `async/await` 风格
- `RwLock` 用于线程安全的状态管理
- 类型安全的错误处理

## 架构设计决策

### 1. 避免异步递归
问题：`schedule_one` 调用 `do_refresh`，`do_refresh` 又可能调用 `schedule_one`，导致无限递归。

解决方案：
- `schedule_one()` - 初始调度，直接在定时器中执行刷新
- `schedule_next_refresh()` - 刷新成功后调度下次刷新，避免递归

### 2. 并发控制
使用 `in_flight: Arc<RwLock<HashMap<String, Instant>>>` 跟踪正在刷新的账户，防止重复刷新。

### 3. 优雅关闭
所有调度器返回 `SchedulerHandle`，通过 `mpsc::channel` 实现优雅关闭：
```rust
tokio::select! {
    _ = ticker.tick() => { /* 执行任务 */ }
    _ = shutdown_rx.recv() => { /* 退出循环 */ }
}
```

## 集成到 AccountService

新增方法：
```rust
impl AccountService {
    /// 列出所有账户用于刷新调度器
    pub async fn list_all_for_refresh(&self) -> Result<Vec<StoredAccount>, AccountServiceError> {
        self.repository()?
            .list_all()
            .await
            .map_err(|_| AccountServiceError::List)
    }
}

// 为 RefreshAccountError 实现 Display trait
impl std::fmt::Display for RefreshAccountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // ... 格式化输出
    }
}
```

## 测试状态

✅ **编译通过**: `cargo check --lib`
✅ **Clippy 通过**: `cargo clippy --all-targets --all-features --locked -- -D warnings`
✅ **格式化**: `cargo fmt`
✅ **现有测试通过**: 所有 152 个集成测试保持通过

## 下一步

根据计划文档 (line 3002, 3005)，仍需实现：

1. **启动集成**:
   - [ ] 在 `main.rs` 启动时初始化调度器
   - [ ] 在优雅关闭时停止调度器

2. **高级功能**（可选）:
   - [ ] 跨进程刷新锁（用于多实例部署）
   - [ ] 磁盘刷新令牌同步（防止一次性 RT 被多次消费）
   - [ ] 配额刷新调度器
   - [ ] 模型刷新调度器
   - [ ] 指纹更新调度器

3. **监控和可观测性**:
   - [ ] 调度器健康检查端点
   - [ ] 刷新成功/失败指标
   - [ ] 活跃定时器数量监控

## 提交信息

```
feat: add background refresh and session cleanup schedulers

- 实现 RefreshScheduler 用于在 JWT 过期前自动刷新访问令牌
- 在 exp - margin 时刻调度刷新
- 指数退避重试（5次尝试：5s → 15s → 45s → 135s → 300s）
- 永久失败检测（invalid_grant / invalid_token）
- 临时失败的恢复调度（10分钟）
- 崩溃恢复：refreshing → 立即重试，expired + refreshToken → 延迟重试
- 实现 SessionCleanupScheduler 用于定期清理过期的管理员会话
- 所有调度器支持优雅关闭

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
```

## 文件清单

新增文件：
- `src/scheduler/mod.rs` - 模块导出
- `src/scheduler/types.rs` - 调度器类型和错误定义
- `src/scheduler/refresh.rs` - OAuth 刷新调度器（385 行）
- `src/scheduler/session_cleanup.rs` - 会话清理调度器（66 行）

修改文件：
- `src/lib.rs` - 添加 `pub mod scheduler;`
- `src/codex/accounts/service/mod.rs` - 添加 `list_all_for_refresh()` 和 `Display` impl

总计：554 行新代码
