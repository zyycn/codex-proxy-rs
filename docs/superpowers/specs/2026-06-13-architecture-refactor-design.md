# Architecture Refactor Design: Frontend-Backend Separation

**Date:** 2026-06-13
**Status:** Approved
**Priority:** High
**Estimated Effort:** 6-8 days

---

## Problem Statement

Current architecture issues identified:

1. **Mixed Responsibilities** - Codex protocol logic, account management, and background tasks are intertwined
2. **Unclear Boundaries** - `service/` and `codex/accounts/service/` have overlapping responsibilities
3. **Large Files** - `http/admin/accounts.rs` has 1697 lines, violating single responsibility principle
4. **Frontend-Backend Coupling** - HTTP request handlers and background schedulers share too much state

**Core Issue:** The system lacks a clear "frontend-backend" boundary, causing:
- Request handling paths tangled with background maintenance logic
- `AccountService` has too many responsibilities
- Schedulers directly manipulate service layer, high coupling
- Difficult to test and maintain

---

## Goals

1. **Clear Separation** - Frontend (HTTP) and Backend (tasks) clearly distinguished in directory structure
2. **Single Source of Truth** - Frontend and backend share the same domain logic through `core/`
3. **Better Maintainability** - Files split by responsibility, each 100-500 lines
4. **Explicit Dependencies** - Unidirectional dependency flow, no circular dependencies

---

## Target Architecture

### Directory Structure (4-Layer Separation)

```
src/
├── core/           # Domain logic (pure business, no external dependencies)
│   ├── accounts/
│   ├── models/
│   ├── quota/
│   └── auth/
│
├── api/            # Frontend: all HTTP request handling
│   ├── v1/         # OpenAI-compatible API
│   ├── admin/      # Admin backend API
│   └── middleware/
│
├── tasks/          # Backend: all scheduled tasks and maintenance
│   ├── refresh/    # Token refresh
│   ├── quota/      # Quota maintenance
│   ├── models/     # Model sync
│   └── cleanup/    # Cleanup tasks
│
└── infra/          # Infrastructure layer (technical implementation)
    ├── storage/    # SQLite
    ├── codex/      # Codex API client
    ├── crypto/     # Encryption
    └── http/       # HTTP utilities
```

**Visual Clarity:**
- `api/` = Synchronous request-response (frontend)
- `tasks/` = Asynchronous background maintenance (backend)
- `core/` = Shared business logic
- `infra/` = Low-level technical support

---

## Detailed Module Design

### 1. `core/` - Domain Core

```
core/
├── accounts/
│   ├── domain.rs           # Account entity, state machine
│   ├── repository.rs       # Data access interface (trait)
│   ├── pool.rs             # Account pool logic
│   ├── commands.rs         # Write operations: create, update, refresh
│   ├── queries.rs          # Read operations: list, get, search
│   └── events.rs           # Domain events (optional)
│
├── models/
│   ├── catalog.rs          # Model catalog
│   ├── repository.rs
│   └── commands.rs
│
├── quota/
│   ├── calculator.rs       # Quota calculation logic
│   ├── limiter.rs          # Rate limiter
│   └── tracker.rs          # Usage tracking
│
└── auth/
    ├── session.rs          # Session management
    ├── api_key.rs          # API Key validation
    └── password.rs         # Password hashing
```

**Responsibility:** Pure business logic, no dependencies on HTTP, database, or timers

**CQRS Pattern:**

```rust
// Commands (Write Operations)
pub struct CreateAccountCommand {
    pub email: String,
    pub access_token: SecretString,
    pub refresh_token: Option<SecretString>,
}

pub struct RefreshTokenCommand {
    pub account_id: AccountId,
}

// Command Handler
pub struct AccountCommandHandler {
    repository: Arc<dyn AccountRepository>,
    pool: Arc<Mutex<AccountPool>>,
    oauth_client: Arc<dyn OAuthClient>,
}

// Queries (Read Operations)
pub struct ListAccountsQuery {
    pub status: Option<AccountStatus>,
    pub limit: u32,
    pub cursor: Option<String>,
}

// Query Handler
pub struct AccountQueryHandler {
    repository: Arc<dyn AccountRepository>,
}
```

---

### 2. `api/` - Frontend HTTP

```
api/
├── v1/
│   ├── router.rs           # Route registration
│   ├── chat.rs             # POST /v1/chat/completions
│   ├── responses.rs        # POST /v1/responses
│   ├── models.rs           # GET /v1/models
│   └── auth.rs             # API Key auth middleware
│
├── admin/
│   ├── router.rs
│   ├── auth.rs             # POST /admin/login
│   ├── accounts/           # Account management (split)
│   │   ├── mod.rs          # Route registration
│   │   ├── list.rs         # GET /admin/accounts
│   │   ├── create.rs       # POST /admin/accounts
│   │   ├── update.rs       # PATCH /admin/accounts/:id
│   │   ├── delete.rs       # DELETE /admin/accounts/:id
│   │   ├── oauth/          # OAuth login flows
│   │   │   ├── pkce.rs
│   │   │   ├── device.rs
│   │   │   └── import.rs
│   │   ├── quota.rs        # Quota operations
│   │   ├── health.rs       # Health checks
│   │   ├── cookies.rs      # Cookie management
│   │   └── batch.rs        # Batch operations
│   ├── api_keys.rs
│   ├── models.rs
│   ├── logs.rs
│   └── settings.rs
│
└── middleware/
    ├── auth.rs
    ├── request_id.rs
    └── error_handler.rs
```

**Responsibility:** HTTP request parsing, authentication, calling `core/`, returning responses

**Example:**

```rust
// api/admin/accounts/create.rs
pub async fn create_account(
    State(app): State<AppState>,
    Json(req): Json<CreateAccountRequest>,
) -> Result<Json<ApiResponse<AccountDto>>> {
    // HTTP 请求转换为 Command
    let command = CreateAccountCommand {
        email: req.email,
        access_token: SecretString::new(req.access_token),
        refresh_token: req.refresh_token.map(SecretString::new),
        label: req.label,
    };

    // 调用核心业务逻辑
    let account = app.core.account_commands.handle(command).await?;

    Ok(Json(ApiResponse::success(account.into())))
}
```

---

### 3. `tasks/` - Backend Tasks

```
tasks/
├── refresh/
│   ├── scheduler.rs        # RefreshScheduler
│   ├── strategy.rs         # Refresh strategy (exponential backoff)
│   └── handler.rs          # Execute refresh logic
│
├── quota/
│   ├── refresher.rs        # QuotaRefresher
│   └── monitor.rs          # Quota monitoring
│
├── models/
│   ├── sync.rs             # ModelRefresher
│   └── fetcher.rs          # Fetch models from Codex
│
├── cleanup/
│   ├── sessions.rs         # SessionCleanupScheduler
│   └── logs.rs             # Log cleanup
│
└── coordinator.rs          # Task coordinator (start/stop all tasks)
```

**Responsibility:** Scheduled tasks, background maintenance, calling `core/` to execute operations

**Example:**

```rust
// tasks/refresh/handler.rs
pub async fn refresh_account(
    account_id: AccountId,
    commands: Arc<AccountCommandHandler>,
) -> Result<()> {
    let command = RefreshTokenCommand { account_id };

    // 刷新令牌可能失败（网络、配额、封禁等），失败后会自动标记账户状态
    commands.handle(command).await?;

    Ok(())
}
```

---

### 4. `infra/` - Infrastructure

```
infra/
├── storage/
│   ├── db.rs               # Database connection
│   ├── accounts_repo.rs    # Implements core/accounts/repository trait
│   ├── models_repo.rs
│   └── migrations/
│
├── codex/
│   ├── client.rs           # Codex API client
│   ├── protocol/           # OpenAI ↔ Codex protocol conversion
│   │   ├── chat.rs
│   │   ├── responses.rs
│   │   └── error.rs
│   ├── transport/
│   │   ├── headers.rs
│   │   ├── sse.rs
│   │   └── websocket.rs
│   └── fingerprint/
│       ├── model.rs
│       └── updater.rs
│
├── crypto/
│   ├── encryption.rs       # AES-GCM
│   ├── hashing.rs          # Argon2, HMAC
│   └── secrets.rs
│
└── http/
    ├── client.rs           # HTTP client configuration
    └── utils.rs
```

**Responsibility:** Technical implementation details, called by `core/`, `api/`, `tasks/`

---

## Dependency Relationships

### Unidirectional Dependencies

```
api/     ──┐
           ├──→ core/ ──→ infra/
tasks/   ──┘

main.rs  ──→ api/ + tasks/ + core/ + infra/
```

**Rules:**
- ✅ `api/` and `tasks/` can both depend on `core/`
- ✅ `core/` only depends on `infra/` interfaces (traits), not concrete implementations
- ❌ `core/` cannot depend on `api/` or `tasks/`
- ❌ `infra/` cannot depend on any upper layer modules

---

## State Management Refactor

### Current Problem

```rust
pub struct AppState {
    account_service: Arc<AccountService>,
    model_service: Arc<ModelService>,
    // ... 10+ services
}
```

Every new feature requires modifying `AppState`, and frontend and backend share one large state.

### Refactored Design

```rust
// app/state.rs

pub struct AppState {
    pub core: Arc<CoreState>,
    pub infra: Arc<InfraState>,
}

pub struct CoreState {
    // Command Handlers (write operations)
    pub account_commands: Arc<AccountCommandHandler>,
    pub model_commands: Arc<ModelCommandHandler>,
    pub quota_commands: Arc<QuotaCommandHandler>,

    // Query Handlers (read operations)
    pub account_queries: Arc<AccountQueryHandler>,
    pub model_queries: Arc<ModelQueryHandler>,

    // Shared runtime state
    pub account_pool: Arc<Mutex<AccountPool>>,
    pub session_affinity: Arc<RwLock<SessionAffinityMap>>,
}

pub struct InfraState {
    pub db: SqlitePool,
    pub codex_client: Arc<CodexClient>,
    pub config: Arc<AppConfig>,
    pub encryptor: Arc<SecretEncryptor>,
}

impl AppState {
    pub async fn new(config: AppConfig) -> Result<Self> {
        let infra = InfraState::new(config).await?;
        let core = CoreState::new(&infra).await?;

        Ok(Self {
            core: Arc::new(core),
            infra: Arc::new(infra),
        })
    }
}
```

**Backend Task Usage:**

```rust
// tasks/coordinator.rs

pub struct TaskCoordinator {
    core: Arc<CoreState>,
    infra: Arc<InfraState>,
}

impl TaskCoordinator {
    pub fn new(app: &AppState) -> Self {
        Self {
            core: app.core.clone(),
            infra: app.infra.clone(),
        }
    }

    pub async fn start_all(&self) -> Vec<TaskHandle> {
        vec![
            self.start_refresh_task(),
            self.start_quota_task(),
            self.start_cleanup_task(),
        ]
    }

    fn start_refresh_task(&self) -> TaskHandle {
        let commands = self.core.account_commands.clone();
        let queries = self.core.account_queries.clone();

        tokio::spawn(async move {
            // 刷新任务只需要 commands 和 queries，不需要整个 AppState
            RefreshScheduler::new(commands, queries).run().await
        })
    }
}
```

---

## Startup Flow Refactor

### `main.rs` Simplified

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. 初始化日志
    init_tracing()?;

    // 2. 加载配置
    let config = load_config()?;

    // 3. 构建应用状态
    let app_state = AppState::new(config).await?;

    // 4. 启动后台任务
    let task_coordinator = TaskCoordinator::new(&app_state);
    let task_handles = task_coordinator.start_all().await;

    // 5. 启动 HTTP 服务器
    let app = build_router(app_state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await?;

    // 6. 优雅关闭
    tokio::select! {
        result = axum::serve(listener, app) => {
            result?;
        }
        _ = shutdown_signal() => {
            // 停止所有后台任务
            task_coordinator.stop_all(task_handles).await;
        }
    }

    Ok(())
}
```

---

## Migration Plan (Progressive)

Since this is a large-scale refactor, we'll proceed in phases:

### Phase 1: Foundation (1-2 days)
1. Create new directory structure: `core/`, `api/`, `tasks/`, `infra/`
2. Migrate `AppState` → `CoreState + InfraState`
3. Implement `AccountCommandHandler` and `AccountQueryHandler`
4. ✅ Ensure compilation passes, keep old code temporarily

### Phase 2: HTTP Layer Split (2-3 days)
1. Split `admin/accounts.rs` (1697 lines) → `admin/accounts/*` (multiple small files)
2. Migrate to call `core/accounts/commands` and `queries`
3. ✅ Keep API contract unchanged, integration tests pass

### Phase 3: Backend Task Independence (1-2 days)
1. Create `tasks/` directory structure
2. Migrate `scheduler/` → `tasks/`
3. Implement `TaskCoordinator`
4. ✅ Backend tasks operate data through `CoreState`

### Phase 4: Cleanup and Optimization (1 day)
1. Delete old `service/` directory
2. Delete duplicate `codex/accounts/service/`
3. Update documentation and tests
4. ✅ All tests pass, code quality checks pass

---

## Before and After Comparison

### Adding New Feature: Batch Disable Accounts

**Before Refactor:**
```
1. Add handler in http/admin/accounts.rs (1697 → 1750 lines)
2. Add business logic in service/xxx.rs
3. Add data operation in codex/accounts/service/mutation.rs
4. May need to sync state in scheduler somewhere
→ Modify 4 files, unclear responsibilities
```

**After Refactor:**
```
1. Add BatchDisableCommand in core/accounts/commands.rs
2. Implement handle() in AccountCommandHandler
3. Add HTTP handler in api/admin/accounts/batch.rs
→ Modify 3 files, each with clear responsibility
→ Frontend and backend automatically share the same logic
```

---

## Key Improvements

1. **Frontend-Backend Separation**
   - `api/` = Synchronous HTTP requests
   - `tasks/` = Asynchronous background maintenance
   - `core/` = Shared business logic

2. **Clear Responsibilities**
   - Commands: Write operations, with side effects, require validation
   - Queries: Read operations, no side effects, simple and fast
   - Pool: Runtime scheduling, no persistence

3. **Unidirectional Dependencies**
   ```
   api/  ──┐
           ├──→ core/ ──→ infra/
   tasks/──┘
   ```

4. **Large File Split**
   - 1697 lines → multiple 100-300 line files
   - Grouped by function (OAuth, Quota, Cookies)

5. **State Management**
   - `CoreState` = Business core (Command/Query Handlers)
   - `InfraState` = Technical foundation (DB, Client, Encryptor)

---

## Success Criteria

1. ✅ All existing tests pass
2. ✅ No file exceeds 800 lines
3. ✅ `api/` and `tasks/` have no direct dependencies on each other
4. ✅ Adding a new feature only requires modifying 2-3 files with clear responsibilities
5. ✅ Code coverage remains above 80%

---

## Risks and Mitigation

**Risk 1: Breaking Changes During Migration**
- Mitigation: Phase-based migration, keep old code until new code is verified

**Risk 2: Performance Regression**
- Mitigation: Benchmark before and after, ensure no significant degradation

**Risk 3: Test Complexity Increase**
- Mitigation: Use trait mocking, write unit tests for `core/` independently

---

## Next Steps

1. ✅ Design document approved
2. → Create implementation plan (use writing-plans skill)
3. → Execute Phase 1: Foundation
4. → Execute Phase 2: HTTP Layer Split
5. → Execute Phase 3: Backend Task Independence
6. → Execute Phase 4: Cleanup and Optimization

---

**Status:** Ready for implementation
