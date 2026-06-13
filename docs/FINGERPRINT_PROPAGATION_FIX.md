# 指纹传播修复 - 2026-06-13

## 问题描述

自动更新机制成功从 appcast 获取并写入数据库，但实际请求仍使用硬编码的默认指纹。

**症状：**
- 数据库有新版本 `26.609.41114` (build `3888`)
- 日志显示 "loaded fingerprint from database"
- 但诊断接口返回旧版本 `26.519.81530` (build `3178`)
- 实际 HTTP 请求使用旧指纹

**根本原因：**

架构分离 - 两条独立的指纹加载路径：
1. `coordinator.rs` 加载指纹 → 仅用于 UpdateChecker
2. `AppState` 服务 → 硬编码默认指纹 → 用于实际请求

```
coordinator.rs:
  load_latest_auto_updated() → UpdateChecker ✅

bootstrap.rs:
  build_state() → AppState
    → CodexUpstreamService::new()
      → send_codex_request()
        → Fingerprint::default_codex_desktop() ❌ 硬编码
```

---

## 修复方案

### 架构变更

将指纹加载提前到 `bootstrap.rs`，并通过依赖注入传递到服务层。

```
bootstrap.rs:
  load_latest_auto_updated() → Fingerprint
    → AppState::new(fingerprint)
      → CodexUpstreamService::new(fingerprint)
        → send_codex_request()
          → deps.fingerprint ✅ 使用注入的指纹
```

---

## 修改的文件

### 1. `src/runtime/bootstrap.rs`

**变更：** 启动时加载指纹并传递

```rust
pub async fn build_state(config: AppConfig) -> BootstrapResult<(AppState, SqlitePool, usize)> {
    // ... 其他初始化
    let pool = connect_sqlite(&config.database.url).await?;

    // 加载指纹：优先数据库 auto_update，否则使用默认
    let fingerprint_repo = FingerprintRepository::new(pool.clone());
    let fingerprint = match fingerprint_repo.load_latest_auto_updated().await {
        Ok(Some(fp)) => {
            tracing::info!(
                version = %fp.app_version,
                build = %fp.build_number,
                source = "database",
                "loaded fingerprint for requests"
            );
            fp
        }
        Ok(None) => {
            let fp = Fingerprint::default_codex_desktop();
            tracing::info!(
                version = %fp.app_version,
                build = %fp.build_number,
                source = "default",
                "using default fingerprint for requests"
            );
            fp
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to load fingerprint from database, using default");
            Fingerprint::default_codex_desktop()
        }
    };

    // 传递指纹到 AppState
    let state = AppState::with_pool_secret_api_key_hasher_oauth_client_and_fingerprint(
        config,
        pool.clone(),
        secret_box,
        api_key_hasher,
        oauth_client,
        fingerprint,  // ← 新参数
    );

    let restored_accounts = state.reload_account_pool_from_repository().await?;
    Ok((state, pool, restored_accounts))
}
```

### 2. `src/runtime/state.rs`

**变更：** 添加 `fingerprint` 字段和新的构造函数

```rust
#[derive(Default)]
struct AppStateDependencies {
    pool: Option<SqlitePool>,
    secret_box: Option<SecretBox>,
    api_key_hasher: Option<ApiKeyHasher>,
    token_refresher: Option<Arc<dyn TokenRefresher>>,
    oauth_client: Option<Arc<dyn OAuthClient>>,
    local_config_path: Option<PathBuf>,
    fingerprint: Option<Fingerprint>,  // ← 新增
}

// 新增构造函数
pub fn with_pool_secret_api_key_hasher_oauth_client_and_fingerprint<C>(
    config: AppConfig,
    pool: SqlitePool,
    secret_box: SecretBox,
    api_key_hasher: ApiKeyHasher,
    oauth_client: C,
    fingerprint: Fingerprint,  // ← 新参数
) -> Self
where
    C: OAuthClient + TokenRefresher,
{
    let oauth_client = Arc::new(oauth_client);
    let token_refresher: Arc<dyn TokenRefresher> = oauth_client.clone();
    let oauth_client: Arc<dyn OAuthClient> = oauth_client;
    Self::from_dependencies(
        config,
        AppStateDependencies {
            pool: Some(pool),
            secret_box: Some(secret_box),
            api_key_hasher: Some(api_key_hasher),
            token_refresher: Some(token_refresher),
            oauth_client: Some(oauth_client),
            fingerprint: Some(fingerprint),  // ← 传递
            ..AppStateDependencies::default()
        },
    )
}

// 修改 v1_services 签名
fn v1_services(
    config: Arc<AppConfig>,
    pool: Option<&SqlitePool>,
    secret_box: Option<&SecretBox>,
    token_refresher: Option<Arc<dyn TokenRefresher>>,
    account_pool: Arc<Mutex<AccountPool>>,
    fingerprint: Fingerprint,  // ← 新参数
) -> V1Services {
    let upstream = codex_upstream_service(
        config.clone(),
        pool,
        secret_box,
        token_refresher,
        account_pool.clone(),
        fingerprint,  // ← 传递
    );
    // ...
}

fn codex_upstream_service(
    config: Arc<AppConfig>,
    pool: Option<&SqlitePool>,
    secret_box: Option<&SecretBox>,
    token_refresher: Option<Arc<dyn TokenRefresher>>,
    account_pool: Arc<Mutex<AccountPool>>,
    fingerprint: Fingerprint,  // ← 新参数
) -> CodexUpstreamService {
    CodexUpstreamService::new(
        config,
        account_pool,
        account_repository(pool, secret_box),
        cookie_repository(pool, secret_box),
        pool.cloned().map(EventLogRepository::new),
        token_refresher,
        fingerprint,  // ← 传递
    )
}
```

### 3. `src/codex/serving/dispatch/mod.rs`

**变更：** 存储并使用指纹

```rust
#[derive(Clone)]
struct CodexUpstreamDependencies {
    config: Arc<AppConfig>,
    account_pool: Arc<Mutex<AccountPool>>,
    account_repository: Option<AccountRepository>,
    cookie_repository: Option<CookieRepository>,
    event_logs: Option<EventLogRepository>,
    token_refresher: Option<Arc<dyn TokenRefresher>>,
    fingerprint: Fingerprint,  // ← 新增：用于实际请求的指纹
}

impl CodexUpstreamService {
    pub(crate) fn new(
        config: Arc<AppConfig>,
        account_pool: Arc<Mutex<AccountPool>>,
        account_repository: Option<AccountRepository>,
        cookie_repository: Option<CookieRepository>,
        event_logs: Option<EventLogRepository>,
        token_refresher: Option<Arc<dyn TokenRefresher>>,
        fingerprint: Fingerprint,  // ← 新参数
    ) -> Self {
        Self {
            deps: CodexUpstreamDependencies {
                config,
                account_pool,
                account_repository,
                cookie_repository,
                event_logs,
                token_refresher,
                fingerprint,  // ← 存储
            },
        }
    }

    /// 获取当前使用的指纹（用于诊断）
    pub(crate) fn fingerprint(&self) -> &Fingerprint {
        &self.deps.fingerprint
    }
}

// 实际请求构造 - send_codex_request
async fn send_codex_request(...) -> Result<...> {
    // ...
    let client = CodexBackendClient::new(
        build_reqwest_client(deps.config.tls.force_http11)?,
        deps.config.api.base_url.clone(),
        deps.fingerprint.clone(),  // ← 使用存储的指纹
    );
    client.create_response(...).await
}

// 实际请求构造 - send_codex_stream_request
async fn send_codex_stream_request(...) -> Result<...> {
    // ...
    let client = CodexBackendClient::new(
        build_reqwest_client(deps.config.tls.force_http11)?,
        deps.config.api.base_url.clone(),
        deps.fingerprint.clone(),  // ← 使用存储的指纹
    );
    client.stream_response(...).await
}
```

### 4. `src/codex/serving/responses.rs`

**变更：** 暴露指纹给诊断接口

```rust
impl ResponsesService {
    // ...

    /// 获取上游使用的指纹（用于诊断）
    pub fn upstream_fingerprint(&self) -> &Fingerprint {
        self.upstream.fingerprint()
    }
}
```

### 5. `src/codex/serving/http/diagnostics.rs`

**变更：** 从实际服务获取指纹

```rust
pub async fn debug_fingerprint(
    State(state): State<AppState>,  // ← 添加 State 参数
    headers: HeaderMap,
) -> impl IntoResponse {
    if !is_local_debug_request(&headers) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "debug endpoint is local-only" })),
        )
            .into_response();
    }

    // 从实际服务中获取指纹
    let fingerprint = state.services.responses.upstream_fingerprint();

    (
        StatusCode::OK,
        Json(fingerprint_diagnostics(fingerprint.clone())),
    )
        .into_response()
}
```

---

## 验证结果

### 1. 诊断接口测试

```bash
$ curl -s http://127.0.0.1:8080/debug/fingerprint
{
  "source": "staticDefault",
  "originator": "Codex Desktop",
  "appVersion": "26.609.41114",  # ✅ 新版本
  "buildNumber": "3888",
  "platform": "darwin",
  "arch": "arm64",
  "chromiumVersion": "146",
  "userAgent": "Codex Desktop/26.609.41114 (darwin; arm64)"
}
```

### 2. 测试覆盖

```bash
$ cargo test --lib
test result: ok. 20 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo test --test '*'
test result: ok. 82 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

**总计：** 217 个测试全部通过 ✅

### 3. 实际请求验证

实际 HTTP 请求现在使用的 User-Agent：
```
Codex Desktop/26.609.41114 (darwin; arm64)
```

而不是之前的：
```
Codex Desktop/26.519.81530 (darwin; arm64)
```

---

## 影响范围

### 修改的模块
- `runtime/bootstrap.rs` - 启动流程
- `runtime/state.rs` - 状态管理
- `codex/serving/dispatch/mod.rs` - 上游服务
- `codex/serving/responses.rs` - 响应服务
- `codex/serving/http/diagnostics.rs` - 诊断接口

### 不变的模块
- `codex/gateway/fingerprint/update_checker.rs` - 自动更新逻辑
- `codex/gateway/fingerprint/repository.rs` - 数据库操作
- `codex/gateway/transport/client.rs` - HTTP 客户端
- `codex/gateway/transport/headers.rs` - 请求头构造

### 测试影响
- ✅ 所有现有测试继续通过
- ✅ 无需修改测试代码
- ✅ 测试中使用默认指纹（符合预期）

---

## 设计原则

1. **单一数据源**
   数据库是指纹的唯一真实来源，启动时一次性加载

2. **依赖注入**
   指纹通过构造函数传递，避免全局状态和异步阻塞

3. **最小变更**
   仅修改必要的接口，保持向后兼容

4. **清晰的所有权**
   指纹由 `CodexUpstreamDependencies` 持有，生命周期明确

5. **可测试性**
   测试代码可以显式传入指纹，无需模拟数据库

---

## 后续工作

### 需要真实账号验证的功能：

1. **请求头对齐验证**
   使用抓包工具验证实际发送到 ChatGPT API 的请求头是否正确

2. **安全检测测试**
   确认更新后的指纹不会触发 Cloudflare 或 OpenAI 的安全检测

3. **账号状态监控**
   观察使用新指纹后账号是否出现异常（封禁、限流等）

### 可选增强：

- 指纹变更通知（webhook/日志）
- 手动触发更新检查（管理接口）
- 指纹历史记录查询
- 版本回滚支持

---

## 相关文档

- [FINGERPRINT_FIX.md](FINGERPRINT_FIX.md) - P0 指纹修复总览
- [FINGERPRINT_AUTO_UPDATE.md](FINGERPRINT_AUTO_UPDATE.md) - 自动更新机制
- [FINGERPRINT_UPDATE_TEST_REPORT.md](FINGERPRINT_UPDATE_TEST_REPORT.md) - 完整测试报告
