# 后台调度器使用指南

## 快速开始

### 1. 启动刷新调度器

```rust
use std::sync::Arc;
use codex_proxy_rs::{
    scheduler::refresh::RefreshScheduler,
    codex::accounts::service::AccountService,
    config::AppConfig,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 加载配置
    let config = AppConfig::load()?;

    // 创建 AccountService
    let account_service = Arc::new(AccountService::new(
        config.clone(),
        Some(account_repo),
        Some(usage_repo),
        Some(cookie_repo),
        Some(token_refresher),
        account_pool,
    ));

    // 创建并启动刷新调度器
    let refresh_scheduler = RefreshScheduler::new(
        account_service.clone(),
        config.clone(),
    );
    let refresh_handle = refresh_scheduler.start().await;

    println!("Refresh scheduler started");

    // ... 运行应用 ...

    // 优雅关闭
    refresh_handle.shutdown().await;
    println!("Refresh scheduler stopped");

    Ok(())
}
```

### 2. 启动会话清理调度器

```rust
use codex_proxy_rs::scheduler::session_cleanup::SessionCleanupScheduler;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let db = sqlx::SqlitePool::connect("sqlite://data.db").await?;

    // 每小时清理一次过期会话
    let session_cleanup = SessionCleanupScheduler::new(db.clone(), 3600);
    let cleanup_handle = session_cleanup.start();

    println!("Session cleanup scheduler started");

    // ... 运行应用 ...

    cleanup_handle.shutdown().await;
    println!("Session cleanup scheduler stopped");

    Ok(())
}
```

### 3. 完整的应用启动示例

```rust
use tokio::signal;
use codex_proxy_rs::{
    runtime::{bootstrap::build_state, build_router, tasks::start_background_tasks},
    config::AppConfig,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    // 1. 加载配置
    let config = AppConfig::load()?;

    // 2. 构建运行时状态和后台任务
    let host = config.server.host.clone();
    let port = config.server.port;
    let (state, db_pool, _) = build_state(config.clone()).await?;
    let background_tasks = start_background_tasks(&state, db_pool.clone(), &config).await;

    // 3. 启动 HTTP 服务器
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;

    // 4. 运行服务器，监听关闭信号
    tokio::select! {
        result = axum::serve(listener, app) => {
            if let Err(e) = result {
                tracing::error!(error = %e, "server error");
            }
        }
        _ = signal::ctrl_c() => {
            tracing::info!("received shutdown signal");
        }
    }

    // 5. 优雅关闭
    background_tasks.shutdown().await;
    db_pool.close().await;

    Ok(())
}
```

## 配置说明

### config.yaml

```yaml
auth:
  # 在令牌过期前多少秒开始刷新
  refresh_margin_seconds: 300  # 5分钟

  # 是否启用自动刷新
  refresh_enabled: true

  # 并发刷新账户数
  refresh_concurrency: 2

admin:
  # 会话清理间隔（秒）
  session_cleanup_interval_secs: 3600  # 1小时

  # 会话过期时间（分钟）
  session_ttl_minutes: 1440  # 24小时
```

## 监控和调试

### 日志输出

调度器会输出结构化日志：

```
[INFO] Refresh scheduler started
[INFO] account_id="acc_123" delay_secs=295 "Refresh scheduled"
[INFO] account_id="acc_123" "Starting token refresh"
[INFO] account_id="acc_123" "Token refreshed successfully"
[INFO] account_id="acc_123" delay_secs=298 "Next refresh scheduled"
```

### 错误处理

```
[WARN] account_id="acc_456" attempt=1 max_attempts=5 delay_secs=5 error="network timeout" "Refresh attempt failed, retrying"
[ERROR] account_id="acc_789" hits=2 "Permanent failure detected"
```

## 故障排查

### 问题：调度器没有刷新账户

检查：
1. `refresh_enabled` 是否为 `true`
2. 账户是否有 `refresh_token`
3. 账户状态是否为 `active` 或 `quota_exhausted`
4. 查看日志中的 JWT 解析错误
5. 如果日志显示 `refresh lease held`，说明同一账户已有其他进程持有刷新租约，当前进程会跳过本次刷新

### 问题：会话没有被清理

检查：
1. `session_cleanup_interval_secs` 配置是否正确
2. 数据库连接是否正常
3. `admin_sessions` 表的 `expires_at` 字段格式

### 问题：内存占用持续增长

可能原因：
- 定时器没有被正确清理
- `in_flight` 标记没有被移除

解决：
- 确保调用 `shutdown()` 优雅关闭
- 检查是否有未捕获的 panic

## 性能考虑

- **定时器开销**: 每个账户一个定时器，对于大量账户建议分批调度
- **数据库查询**: `list_all_for_refresh()` 在启动时调用一次，不影响运行时性能
- **并发控制**: `refresh_concurrency` 已接入后台刷新生产路径，限制本进程同时刷新的账户数，避免突发请求
- **刷新抖动**: 自动刷新和恢复刷新会加入 80%-120% jitter，降低大量账户同一时间命中 OAuth 上游的概率
- **跨进程租约**: `account_refresh_leases` 表按账户持有 5 分钟刷新租约，防止多进程同时消费同一个 refresh_token

## 最佳实践

1. **配置刷新边界**: `refresh_margin_seconds` 应大于最大重试时间（~10分钟）
2. **监控失败率**: 如果永久失败率过高，检查上游 API 状态
3. **定期备份**: 刷新令牌是敏感数据，定期备份数据库
4. **测试崩溃恢复**: 重启服务后验证 `refreshing` 和 `expired` 状态的恢复
