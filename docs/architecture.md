# 架构规范

本文档定义 `codex-proxy-rs` 的正式 Rust workspace 架构。每个 Rust 源文件、目录、
依赖方向和命名约定在最终完成时必须与本文档完全一致。

不允许过渡期别名。不允许兼容性重导出。迁移必须直接收敛到此布局。

本文档仅约束 Rust workspace 源码布局。`web/` 前端和 `docs/` 目录有各自的约定，
不属于 Rust 源码白名单。

## 目录

1. [架构风格](#架构风格)
2. [核心规则](#核心规则)
3. [根 Workspace 布局](#根-workspace-布局)
4. [依赖方向](#依赖方向)
5. [Crate 职责](#crate-职责)
6. [精确 Rust 源码形状](#精确-rust-源码形状)
7. [Cargo 集成测试形状](#cargo-集成测试形状)
8. [端口（Port）放置](#端口port放置)
9. [领域模型归属](#领域模型归属)
10. [WebSocket 架构——硬拆分](#websocket-架构硬拆分)
11. [迁移计划](#迁移计划)
12. [架构测试](#架构测试)
13. [完成标准](#完成标准)

## 架构风格

本应用是一个重组为多 crate workspace 的模块化单体，具有严格的六边形边界。
每个 crate 有且仅有一个明确定义的职责。

- **`core`** 拥有领域模型、用例、协议语义、策略和端口（port trait）。
  它是最内层——不依赖本 workspace 中的任何其他 crate。
- **`adapters`** 拥有 `core` 端口的具体实现——SQLx 仓储、Reqwest HTTP 客户端、
  tokio-tungstenite WebSocket 客户端、OAuth 客户端、指纹更新客户端。
- **`runtime`** 拥有应用组装——`AppState`、依赖注入、后台任务启停、配置映射。
- **`server`** 拥有 Axum HTTP 边界——路由、处理器、中间件、响应信封、
  SSE 帧封装、错误到 HTTP 的映射。
- **`platform`** 拥有共享基础设施原语——配置加载、加密、身份哈希、
  SQLite 连接初始化、schema 文件、文件系统路径、日志原语、JSON 和分页帮手。
- **`assets`** 拥有编译后的前端静态资源服务——SPA 回退、静态缓存、安全头。
- **`xtask`** 拥有本地自动化——前端构建、架构检查、发布命令。

## 核心规则

### 依赖规则（不可协商）

1. `core` 不得依赖任何其他 workspace crate。
2. `core` 不得依赖 `axum`、`sqlx`、`reqwest`、`tokio-tungstenite`、
   `tungstenite`、`rustls`、`tokio-rustls` 或 `hyper`。
3. `core` 不得拥有文件系统路径、环境变量读取或具体网络 IO。
4. `platform` 不得依赖 `core`、`server`、`runtime`、`adapters` 或 `assets`。
5. `adapters` 不得依赖 `runtime` 或 `server`。
6. `runtime` 不得依赖 `server`。
7. `server` 不得包含 SQL 查询、具体上游 Codex IO、账号选择策略或后台任务编排。
8. `assets` 不得依赖任何 workspace crate。

### 结构规则

1. [精确 Rust 源码形状](#精确-rust-源码形状) 中列出的每个文件和目录都必须存在。
2. `crates/*/src/` 下不得存在白名单外的任何 Rust 源文件或目录，根目录不得保留
   `src/`。
3. 每个 `Cargo.toml` 的 `[dependencies]` 条目必须至少被该 crate `src/` 中的一个
   `use` 语句引用。
4. 根 package 不提供 library 或 binary target；类似 `crates/server/facade.rs` 的文件被禁止。
5. 端口 trait 存放在各自领域模块的 `ports.rs` 文件中。
6. 仓储实现存放在 `adapters/src/sqlite/` 中。
7. 每条 `TODO` 注释必须引用一个 GitHub issue：`// TODO(#123): ...`

### Rust 最佳实践（不可协商）

以下规则适用于每个 crate，由架构测试和 CI 强制执行。

- **所有权**：参数优先使用 `&T` 而非 `.clone()`。参数使用 `&str`，
  返回自有数据使用 `String`。`Copy` 类型（≤ 24 字节）按值传递。
- **错误处理**：可失败操作返回 `Result<T, E>`。测试外禁止
  `unwrap()` 或 `expect()`。库错误类型使用 `thiserror`。用 `?` 传播。
- **Lint**：`cargo clippy --all-targets --all-features --locked -- -D warnings`
  必须通过。使用 `#[expect(clippy::lint)]` 并附上理由注释。
- **测试**：每个测试一个断言。测试名称描述行为：
  `某操作_在某条件下_应产生某结果()`。生成输出的快照测试使用 `cargo insta`。
  公共 API 示例使用 doc test。
- **文档**：`//` 解释**为什么**（设计理由、安全不变量）。`///` 解释**什么**
  （公共 API 契约）。在 `core`、`adapters` 和 `platform` 上启用
  `#![deny(missing_docs)]`。
- **泛型**：性能关键路径优先使用静态分发（泛型）。仅在异构集合需要时使用
  `dyn Trait`。在 API 边界 Box，而非内部。
- **性能**：绝不在循环中 clone。优先使用迭代器而非手动索引。避免中间的
  `.collect()` 调用。用 `--release` 运行基准测试。

## 根 Workspace 布局

```text
codex-proxy-rs/
├── Cargo.toml          # [workspace] 含 members = ["crates/*"]
├── Cargo.lock
├── README.md
├── AGENTS.md
├── crates/
│   ├── core/
│   │   ├── Cargo.toml
│   │   └── src/
│   ├── adapters/
│   │   ├── Cargo.toml
│   │   └── src/
│   ├── runtime/
│   │   ├── Cargo.toml
│   │   └── src/
│   ├── server/
│   │   ├── Cargo.toml
│   │   └── src/
│   ├── platform/
│   │   ├── Cargo.toml
│   │   └── src/
│   ├── assets/
│   │   ├── Cargo.toml
│   │   └── src/
│   └── xtask/
│       ├── Cargo.toml
│       └── src/
├── tests/
├── web/
└── docs/
```

**根 `Cargo.toml`** 必须声明 `[workspace]`，其中 `members = ["crates/*"]`，
并定义 workspace 级别的依赖、lint 设置和 profile 覆盖。根 package 不提供
library 或 binary target；根 `tests/` 只承载跨 workspace 架构测试。

## 依赖方向

```text
server   ──► runtime + core + assets + platform
runtime  ──► core + adapters + platform
adapters ──► core + platform
core     ──► （无 workspace crate 依赖）
assets   ──► （无 workspace crate 依赖）
xtask    ──► （仅 workspace 工具链）
platform ──► （无 workspace crate 依赖）
```

**被禁止的项目 crate 依赖**（由架构测试强制执行）：

| 源 Crate | 被禁止的依赖 |
|---------|------------|
| `core` | `platform`、`server`、`runtime`、`adapters`、`assets` |
| `platform` | `core`、`server`、`runtime`、`adapters`、`assets` |
| `adapters` | `runtime`、`server` |
| `runtime` | `server` |
| `assets` | 所有 workspace crate |

`runtime` 在构造 `core` 服务前将 `platform::config` 类型映射为 `core` 的配置 DTO。
这使 `core` 独立于配置文件路径、YAML 解析和环境变量。

## Crate 职责

### `crates/core`

拥有领域概念、不变量、用例编排、协议语义和端口。此 crate 零 IO 依赖。

**允许：**
- 账号池和调度策略（`accounts/pool.rs`）
- 管理员业务用例（`admin/`）
- 认证/会话领域行为（`auth/`）
- 模型目录规则（`models/`）
- 用量和事件策略（`usage/`、`events/`）
- OpenAI/Codex 协议数据结构和纯转换（`protocol/`）
- WebSocket 消息编解码（单个帧的序列化、反序列化、验证——**非**连接建立）
- 请求分发、回退、重试、配额检查、亲和性、恢复和用量跟踪策略（`serving/`）
- 面向上游的端口 trait（`gateway/ports.rs`）
- 类型化领域和应用错误（`error.rs` 或各模块的 `errors.rs`）

**禁止：**
- `axum` 提取器、路由、响应、`IntoResponse` 实现
- `sqlx` 连接池、查询、行、事务、迁移
- 具体的 `reqwest::Client` 构造或使用
- 具体的 WebSocket/TLS 连接建立（`tokio_tungstenite`、`tokio-rustls`、`rustls`）
- 文件系统路径构造或环境变量读取
- 平台配置加载器类型（`platform::config::*`）

### `crates/adapters`

拥有 `core` 端口的具体实现。每个实现结构体以其使用的技术命名。

**允许：**
- `SqliteAccountStore` —— 账号存储端口的 SQLx 实现
- `SqliteAdminSessionStore` —— 管理员会话端口的 SQLx 实现
- `SqliteClientKeyStore` —— 客户端密钥端口的 SQLx 实现
- `SqliteEventLogStore` —— 事件日志端口的 SQLx 实现
- `SqliteModelSnapshotStore` —— 模型快照端口的 SQLx 实现
- `SqliteSessionAffinityStore` —— 会话亲和性的 SQLx 实现
- `SqliteCookieStore` —— Cookie 端口的 SQLx 实现
- `SqliteUsageStore` —— 用量跟踪端口的 SQLx 实现
- `ReqwestCodexClient` —— 上游 Codex HTTP/SSE 的 Reqwest 实现
- `ReqwestFingerprintClient` —— 指纹更新的 Reqwest 实现
- `OpenAiOAuthClient` —— OpenAI OAuth 流程的 Reqwest 实现
- `CodexWebSocketConnection` —— WS 连接建立和消息收发的 tokio-tungstenite 实现
- `CodexWebSocketPool` —— WS 连接池的 tokio-tungstenite 实现
- `ReqwestTlsConfig` —— 自定义 CA 配置的 rustls 实现
- 适配器特定错误从框架错误到 `core` 错误类型的翻译

**禁止：**
- 业务策略决策
- Axum 路由处理器
- 运行时任务编排
- 领域不变量

### `crates/runtime`

拥有应用组装。这是 "胶水" 层。

**允许：**
- `AppState` 结构体构造
- 服务构造（用 `adapters` 实现实例化 `core` 服务）
- 仓储和适配器构造
- 向 `server` 处理器注入依赖
- 后台任务启动和协调关闭
- 启动恢复流程（从数据库加载账号到池中）
- `platform::config` → `core` 配置 DTO 映射

**禁止：**
- HTTP 请求/响应映射
- 原始 SQL 查询字符串
- 具体上游协议的副作用
- 账号选择、回退、配额、模型或恢复策略决策

### `crates/server`

拥有 HTTP 边界。所有 Axum 类型都在这里。

**允许：**
- Axum 路由和路由处理器
- 提取器和中间件（`request_id`、tracing、CORS、auth）
- 管理员响应信封（`AdminResponse`、`AdminEnvelope`）
- OpenAI 兼容的 HTTP 响应体和错误形状
- HTTP 状态码和头映射
- 客户端响应的 SSE 帧封装
- 路由本地的 OpenAI API 错误/事件帧封装

**禁止：**
- SQL 查询
- 具体上游 Codex IO
- 账号选择、回退、配额、模型或恢复策略
- 后台任务编排

### `crates/platform`

拥有无领域知识的跨领域基础设施原语。

**允许：**
- 配置加载（`config/loader.rs`）和配置 schema 类型（`config/types.rs`）
- 加密原语：AES-GCM 密钥箱（`crypto/secret_box.rs`）、HMAC-SHA256 哈希（`crypto/hash.rs`）
- 身份哈希：管理员密码（Argon2）、客户端 API 密钥（HMAC）
- SQLite 连接初始化（`storage/sqlite.rs`）和 schema 文件（`storage/schema.sql`）
- 文件系统路径帮手（`storage/paths.rs`）
- 日志原语（`logging/`）—— tracing subscriber 初始化、轮转
- JSON 帮手（`json/`）—— cursor 分页、信封帮手

**禁止：**
- Codex/OpenAI 业务用例
- 管理员业务用例
- Axum 处理器
- `core` 端口的实现
- 领域模型类型

### `crates/assets`

拥有前端静态资源服务。

**允许：**
- 根 `/` → `index.html` 响应
- `/assets/*` 静态文件服务
- SPA 回退（API 路由后的任何未匹配路径 → `index.html`）
- 静态缓存和安全头

### `crates/xtask`

拥有本地自动化。它是二进制 crate，不是 library。

**允许：**
- 前端构建编排（`build_web.rs`）
- 架构检查（`check_architecture.rs`）
- 发布/打包命令（`release.rs`）

## 精确 Rust 源码形状

下面列出的每个文件和目录在最终完成时必须存在。`crates/*/src/` 下
不得存在其他 Rust 源文件或目录，根目录不得保留 `src/`。

### Core

```text
crates/core/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── error.rs
    ├── admin/
    │   ├── mod.rs
    │   ├── ports.rs              # AdminSessionStore、ClientKeyStore trait
    │   ├── auth.rs               # 管理员密码验证逻辑
    │   ├── client_keys.rs        # API 密钥服务（仅业务逻辑）
    │   └── settings.rs           # 设置领域逻辑
    ├── accounts/
    │   ├── mod.rs
    │   ├── model.rs              # Account、AccountStatus、NewAccount
    │   ├── ports.rs              # AccountStore、CookieStore、UsageStore trait
    │   ├── service.rs            # 账号管理用例
    │   ├── pool.rs               # AccountPool 调度
    │   ├── lifecycle.rs          # 状态转换、过期规则
    │   ├── cloudflare.rs         # Cloudflare path-block 纯状态追踪
    │   ├── cookies.rs            # Cookie 捕获/重放策略
    │   ├── jwt.rs                # JWT 过期检查（纯逻辑，无网络）
    │   └── usage.rs              # 用量累积策略
    ├── auth/
    │   ├── mod.rs
    │   ├── ports.rs              # OAuthClient trait
    │   ├── oauth.rs              # OAuth 流程编排（无 HTTP）
    │   └── session.rs            # 管理员会话领域逻辑
    ├── models/
    │   ├── mod.rs
    │   ├── model.rs              # Model、ModelPlan、ModelCatalog
    │   ├── ports.rs              # ModelSnapshotStore trait
    │   ├── catalog.rs            # 模型目录规则
    │   └── service.rs            # 模型服务用例
    ├── events/
    │   ├── mod.rs
    │   ├── model.rs              # EventLog、EventLevel
    │   ├── ports.rs              # EventLogStore trait
    │   └── service.rs            # 日志策略（启用、容量、保留）
    ├── usage/
    │   ├── mod.rs
    │   ├── model.rs              # UsageSnapshot、UsageWindow
    │   ├── ports.rs              # UsageStore trait
    │   └── service.rs            # 用量聚合策略
    ├── protocol/
    │   ├── mod.rs
    │   ├── openai/
    │   │   ├── mod.rs
    │   │   ├── chat.rs           # ChatCompletionRequest/Response
    │   │   ├── responses.rs      # Response API 类型
    │   │   ├── models.rs         # Model list 类型
    │   │   └── errors.rs         # OpenAI 错误类型
    │   └── codex/
    │       ├── mod.rs
    │       ├── chat.rs           # Codex 特定 chat 扩展
    │       ├── responses.rs      # CodexResponsesRequest
    │       ├── events.rs         # Codex SSE 事件类型
    │       ├── sse.rs            # SSE 事件解析（纯函数，无 IO）
    │       ├── websocket.rs      # WS 帧类型、payload 编解码
    │       └── schema.rs         # JSON schema 验证规则
    ├── gateway/
    │   ├── mod.rs
    │   ├── ports.rs              # CodexUpstreamClient、FingerprintClient trait
    │   ├── fingerprint.rs        # 指纹模型和版本规则
    │   ├── conversation.rs       # 会话标识派生
    │   └── installation.rs       # 安装 ID 生成规则
    └── serving/
        ├── mod.rs
        ├── chat.rs               # Chat 补全编排
        ├── responses.rs          # Response 创建编排
        ├── errors.rs             # 服务错误类型
        ├── routing.rs            # 账号选择路由策略
        ├── fallback.rs           # 回退策略规则
        ├── affinity.rs           # 会话亲和性策略
        ├── quota.rs              # 配额检查策略
        ├── implicit_resume.rs    # Responses 隐式续接策略
        ├── reasoning_replay.rs   # Responses reasoning replay 缓存策略
        ├── stream.rs             # 流生命周期策略
        ├── recovery.rs           # 错误恢复规则
        └── usage.rs              # 用量跟踪策略
```

**`core` 的关键设计决策：**

- `protocol/codex/websocket.rs` 包含 WS 帧类型定义和纯编解码
  （序列化/反序列化单个消息）。它**不**打开连接、不执行 TLS 握手、不调用
  `tokio_tungstenite`。
- `protocol/codex/sse.rs` 包含 SSE 事件解析，作为 `&[u8]` → `Vec<SseEvent>` 的纯函数。
  无网络 IO。
- `serving/` 模块包含策略决策（"用哪个账号"、"是否重试"），但将所有 IO
  委托给 `gateway/ports.rs` 中定义的 trait。
- `core/src/` 中没有任何文件以 `_repository.rs` 结尾。
  存储实现位于 `adapters` 中。

### Adapters

```text
crates/adapters/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── sqlite/
    │   ├── mod.rs
    │   ├── accounts.rs          # SqliteAccountStore
    │   ├── account_tokens.rs    # token 在 DB 中的加解密/轮换
    │   ├── account_usage.rs     # SqliteUsageStore
    │   ├── refresh_leases.rs    # 分布式刷新锁
    │   ├── cookies.rs           # SqliteCookieStore
    │   ├── events.rs            # SqliteEventLogStore
    │   ├── models.rs            # SqliteModelSnapshotStore
    │   ├── session_affinity.rs  # SqliteSessionAffinityStore
    │   ├── admin_sessions.rs    # SqliteAdminSessionStore
    │   └── client_keys.rs       # SqliteClientKeyStore
    ├── codex/
    │   ├── mod.rs
    │   ├── client.rs            # ReqwestCodexClient（HTTP + SSE）
    │   ├── models.rs            # 通过 Reqwest 获取模型列表
    │   ├── fingerprint.rs       # ReqwestFingerprintClient
    │   └── websocket/
    │       ├── mod.rs
    │       ├── connect.rs       # CodexWebSocketConnection — tokio-tungstenite
    │       ├── pool.rs          # CodexWebSocketPool
    │       ├── deflate.rs       # permessage-deflate 处理
    │       └── opening.rs       # WS 打开握手（TLS + Upgrade）
    └── oauth/
        ├── mod.rs
        └── openai.rs            # OpenAiOAuthClient（Reqwest）
```

**`adapters` 的关键设计决策：**

- `sqlite/` 模块按**存储什么**命名，而非怎么存储。`SqliteAccountStore` 实现
  `core::accounts::ports::AccountStore`。
- `codex/websocket/connect.rs` 包装 `tokio_tungstenite::connect_async`，
  返回 `core::protocol::codex::websocket::WsConnection`（trait 或 opaque handle）。
  `core` 永远不会看到 `tokio_tungstenite` 类型。
- `codex/websocket/opening.rs` 处理原始 TCP/TLS 连接、自定义头顺序和 WebSocket 升级。
  它产生 `OpeningAuditSnapshot` 用于一致性测试。
- `oauth/openai.rs` 使用 Reqwest 实现 `core::auth::ports::OAuthClient`。

### Runtime

```text
crates/runtime/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── bootstrap.rs             # 启动：加载配置、初始化 DB、恢复池
    ├── state.rs                 # AppState 构造
    ├── services.rs              # 服务工厂函数
    ├── repositories.rs          # 仓储工厂函数
    ├── upstream.rs              # 适配器工厂函数
    ├── config.rs                # platform::config → core 配置 DTO 映射
    └── tasks/
        ├── mod.rs
        ├── coordinator.rs       # 后台任务生成/关闭
        ├── token_refresh.rs     # 定期 token 刷新任务接线
        ├── quota_refresh.rs     # 定期配额刷新任务接线
        ├── model_refresh.rs     # 定期模型刷新任务接线
        ├── cookie_cleanup.rs    # 定期 cookie 清理任务接线
        ├── session_cleanup.rs   # 定期会话清理任务接线
        ├── session_affinity_cleanup.rs # 定期会话亲和性清理任务接线
        └── fingerprint_update.rs # 定期指纹更新任务接线
```

**`runtime` 的关键设计决策：**

- 任务文件仅包含接线：它们从 `core` 接收 `Arc<dyn PortTrait>`，从配置中获取间隔，
  并启动 `tokio::spawn` 循环。它们不包含领域策略。
- `runtime/src/config.rs` 将 `platform::config::AppConfig` 翻译为
  `core::admin::AdminConfig`、`core::accounts::PoolConfig` 等。
- `runtime/src/bootstrap.rs` 编排启动序列，但将所有具体 IO
  委托给 `adapters` 工厂。

### Server

```text
crates/server/
├── Cargo.toml
└── src/
    ├── main.rs                  # #[tokio::main] 入口
    ├── lib.rs
    ├── router.rs                # 顶层 Axum Router
    ├── error/
    │   ├── mod.rs
    │   ├── admin.rs             # 管理员错误 → HTTP 响应
    │   └── openai.rs            # 领域错误 → OpenAI 错误形状
    ├── middleware/
    │   ├── mod.rs
    │   ├── request_id.rs        # X-Request-Id 头 + 扩展
    │   ├── trace.rs             # HTTP tracing 层
    │   ├── auth.rs              # API 密钥 / 会话提取
    │   └── cors.rs              # CORS 配置
    ├── admin_api/
    │   ├── mod.rs
    │   ├── router.rs            # /api/admin/* 路由
    │   ├── response.rs          # AdminEnvelope、AdminResponse
    │   ├── session.rs           # 登录/登出处理器
    │   ├── settings.rs          # GET/PATCH /api/admin/settings
    │   ├── diagnostics.rs       # GET /api/admin/diagnostics
    │   ├── models.rs            # POST /api/admin/refresh-models
    │   ├── usage.rs             # GET /api/admin/usage-stats
    │   ├── accounts/
    │   │   ├── mod.rs
    │   │   ├── list.rs
    │   │   ├── create.rs
    │   │   ├── import.rs
    │   │   ├── import_cli.rs   # Codex CLI auth.json 导入
    │   │   ├── export.rs
    │   │   ├── lifecycle.rs     # status/label/delete/batch
    │   │   ├── quota.rs         # 配额警告
    │   │   ├── cookies.rs       # cookie 管理
    │   │   ├── oauth.rs         # OAuth 流程处理器
    │   │   └── health.rs        # 健康检查
    │   ├── client_keys/
    │   │   ├── mod.rs
    │   │   ├── list.rs
    │   │   ├── create.rs
    │   │   ├── import.rs
    │   │   ├── export.rs
    │   │   └── lifecycle.rs     # label/status/delete/batch
    │   └── logs/
    │       ├── mod.rs
    │       ├── query.rs
    │       ├── detail.rs
    │       └── state.rs
    └── openai_api/
        ├── mod.rs
        ├── router.rs            # /v1/* 路由
        ├── auth.rs              # Bearer token 提取
        ├── chat.rs              # POST /v1/chat/completions
        ├── responses.rs         # POST /v1/responses
        ├── models.rs            # GET /v1/models
        ├── diagnostics.rs       # GET /debug/*
        ├── error.rs             # OpenAI 错误 → HTTP 响应
        └── sse.rs               # 客户端的 SSE 事件帧封装
```

**`server` 的关键设计决策：**

- 路由处理器是薄的：提取认证、解析请求体、调用 `core::serving::*` 用例、
  将响应映射为 HTTP。处理器中没有业务逻辑。
- `server/src/openai_api/sse.rs` 将 `core::protocol::codex::events::SseEvent`
  帧封装为 HTTP 响应的 `data: {...}\n\n`。它拥有线格式，不拥有事件语义。
- `server/src/error/openai.rs` 将 `core::serving::errors::ServingError`
  映射为 OpenAI 兼容的 `{"error": {...}}` JSON 响应。

### Platform

```text
crates/platform/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── config/
    │   ├── mod.rs
    │   ├── loader.rs             # YAML 配置加载（含 local.yaml 覆盖）
    │   └── types.rs              # AppConfig、ServerConfig 等
    ├── crypto/
    │   ├── mod.rs
    │   ├── secret_box.rs         # AES-256-GCM 加解密
    │   └── hash.rs               # HMAC-SHA256 + 常数时间比较
    ├── identity/
    │   ├── mod.rs
    │   ├── admin_password.rs     # Argon2 密码哈希/验证
    │   └── client_key.rs         # API 密钥生成 + HMAC 哈希
    ├── storage/
    │   ├── mod.rs
    │   ├── sqlite.rs             # SqlitePool 构造 + WAL pragma
    │   ├── schema.sql            # 所有表的 DDL
    │   └── paths.rs              # data/ 目录路径帮手
    ├── logging/
    │   ├── mod.rs
    │   └── rotation.rs           # tracing-subscriber 初始化 + 文件轮转
    └── json/
        ├── mod.rs
        └── pagination.rs         # Cursor 编解码 + Page<T>
```

**`platform` 的关键设计决策：**

- `config/types.rs` 定义配置 schema 类型，供 `config/loader.rs` 使用。
  这些是纯数据，不是领域类型。
- `crypto/secret_box.rs` 是 `aes_gcm` 上的薄包装，输出 base64 编码密文。
  它不知道账号或 token。
- `identity/client_key.rs` 包含基于 HMAC 的 API 密钥哈希器，但**不**包含端口 trait——
  那在 `core::admin::ports` 中。
- `storage/schema.sql` 是 DDL 的唯一真实来源。它在连接时被
  `storage/sqlite.rs` 通过 `include_str!` 引入。

### Assets

```text
crates/assets/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── router.rs                 # / 和 /assets/* 路由
    └── headers.rs                # Cache-Control、CSP、安全头
```

### Xtask

```text
crates/xtask/
├── Cargo.toml
└── src/
    ├── main.rs                   # CLI 分发器
    ├── build_web.rs              # 构建 web/ 前端
    ├── check_architecture.rs     # 运行架构测试
    └── release.rs                # 发布打包
```

## 端口（Port）放置

端口是 `core` 拥有的 trait。每个端口 trait 在其领域模块中存放在名为 `ports.rs` 的文件中。

| 端口 Trait | 位置 | 实现（在 `adapters` 中） |
|---|---|---|
| `AccountStore` | `core/src/accounts/ports.rs` | `SqliteAccountStore` |
| `CookieStore` | `core/src/accounts/ports.rs` | `SqliteCookieStore` |
| `UsageStore` | `core/src/accounts/ports.rs` | `SqliteUsageStore` |
| `AdminSessionStore` | `core/src/admin/ports.rs` | `SqliteAdminSessionStore` |
| `ClientKeyStore` | `core/src/admin/ports.rs` | `SqliteClientKeyStore` |
| `OAuthClient` | `core/src/auth/ports.rs` | `OpenAiOAuthClient` |
| `ModelSnapshotStore` | `core/src/models/ports.rs` | `SqliteModelSnapshotStore` |
| `EventLogStore` | `core/src/events/ports.rs` | `SqliteEventLogStore` |
| `SessionAffinityStore` | `core/src/serving/affinity.rs` | `SqliteSessionAffinityStore` |
| `CodexUpstreamClient` | `core/src/gateway/ports.rs` | `ReqwestCodexClient` |
| `CodexWebSocketTransport` | `core/src/gateway/ports.rs` | `CodexWebSocketConnection` |
| `FingerprintClient` | `core/src/gateway/ports.rs` | `ReqwestFingerprintClient` |

每个 `ports.rs` 文件仅包含 trait 定义和相关类型别名
（例如 `AccountStoreResult<T> = Result<T, AccountStoreError>`）。

## 领域模型归属

领域模型是纯数据结构体，不包含 IO。所有模型类型归 `core` 所有。

| 模型 | 位置 | 备注 |
|------|------|------|
| `Account` | `core/src/accounts/model.rs` | 运行时账号表示 |
| `AccountStatus` | `core/src/accounts/model.rs` | 枚举：Active、Expired、Disabled、Banned |
| `StoredAccount` | `adapters/src/sqlite/accounts.rs` | DB 行类型——适配器内部 |
| `EventLog` | `core/src/events/model.rs` | 结构化事件 |
| `EventLevel` | `core/src/events/model.rs` | Info、Warn、Error |
| `Model`、`ModelPlan` | `core/src/models/model.rs` | 模型目录条目 |
| `UsageWindow` | `core/src/usage/model.rs` | 聚合用量窗口 |
| `Fingerprint` | `core/src/gateway/fingerprint.rs` | 设备指纹 |
| `ChatCompletionRequest` | `core/src/protocol/openai/chat.rs` | OpenAI 请求/响应类型 |
| `CodexResponsesRequest` | `core/src/protocol/codex/responses.rs` | Codex 请求类型 |
| `SseEvent` | `core/src/protocol/codex/sse.rs` | 解析后的 SSE 事件 |
| `CodexWebSocketResponse` | `core/src/protocol/codex/websocket.rs` | WS 响应聚合 |
| `OpeningAuditSnapshot` | `core/src/protocol/codex/websocket.rs` | WS 握手审计快照 |
| `PayloadAuditSnapshot` | `core/src/protocol/codex/websocket.rs` | WS payload 审计快照 |
| `WsParityDiff` | `core/src/protocol/codex/websocket.rs` | 一致性差异报告 |

**规则**：`StoredAccount` 和类似的 DB 行类型是 `adapters` 内部类型。它们
**不**作为 `core` 端口 trait 的输入或输出暴露。端口 trait 使用方法签名
中的领域模型类型（`Account`、`EventLog` 等）。适配器在内部将 DB 行
转换为领域类型。

## WebSocket 架构——硬拆分

WebSocket 代码是架构上最具挑战性的拆分，因为当前
`src/codex/gateway/transport/websocket/` 目录混淆了三个不同的关注点：

### 1. 纯协议编解码 → `core`

处理 WS 消息格式、验证和单个帧序列化/反序列化的文件。这些无 IO 依赖。

- **`core/src/protocol/codex/websocket.rs`**：帧类型定义、
  `CodexWebSocketResponse`、`CodexWebSocketStreamResponse`、`WsMessage<T>`。
  纯数据类型。
- `CodexResponsesRequest` ↔ WS 文本帧 JSON 的序列化/反序列化逻辑。纯转换。

### 2. 连接管理 → `adapters`

处理实际 TCP/TLS 连接、WebSocket 握手和消息收发的文件。
这些依赖 `tokio-tungstenite`、`tokio-rustls`、`rustls`。

- **`adapters/src/codex/websocket/connect.rs`**：包装
  `tokio_tungstenite::connect_async`，返回实现 `core` 中定义的
  `WsTransport` trait 的 opaque handle。
- **`adapters/src/codex/websocket/pool.rs`**：连接池管理、保活、驱逐。
- **`adapters/src/codex/websocket/opening.rs`**：原始 TLS 连接、
  自定义头顺序、WebSocket 升级握手、accept key 验证。
  产生 `OpeningAuditSnapshot`（一个 `core` 类型）用于一致性对比。
- **`adapters/src/codex/websocket/deflate.rs`**：permessage-deflate
  扩展协商和帧压缩/解压。

### 3. 审计快照 → `core`

审计快照类型是纯数据，用于比较代理的行为与官方 Codex Desktop 客户端。
它们属于 `core`，因为代表协议级别的一致性关注点。

- **`core/src/protocol/codex/websocket.rs`**（或单独的 `audit.rs`）：
  `OpeningAuditSnapshot`、`PayloadAuditSnapshot`、`WsParityDiff`。

### Trait 桥接

WebSocket 的 `core` 和 `adapters` 之间的桥接是 `core/src/gateway/ports.rs` 中的一个 trait：

```rust
// core/src/gateway/ports.rs

#[async_trait]
pub trait CodexWebSocketTransport: Send + Sync {
    async fn connect(
        &self,
        request: &CodexResponsesRequest,
        context: &RequestContext,
    ) -> Result<CodexWebSocketStreamResponse, CodexWebSocketError>;

    async fn connect_with_pool(
        &self,
        request: &CodexResponsesRequest,
        context: &RequestContext,
    ) -> Result<CodexWebSocketStreamResponse, CodexWebSocketError>;
}
```

`adapters` 用 `tokio-tungstenite` 实现此 trait。`core` 调用该 trait 而无需知道具体实现。

## 迁移计划

迁移分八个阶段进行。每个阶段必须产出**可编译、测试通过的 workspace**，
然后才能开始下一阶段。不允许多个阶段交错进行，不允许过渡期别名。

### 阶段 1：Workspace 脚手架

1. 在根 `Cargo.toml` 中添加 `[workspace]`，`members = ["crates/*"]`。
2. 创建 `crates/` 目录及所有七个子目录，每个含 stub `Cargo.toml`。
3. 在根 `Cargo.toml` 中定义 workspace 级别依赖、lint 设置和 profile 覆盖。
4. 验证 `cargo check --workspace` 通过（所有 crate 为空）。

**验收**：`cargo check --workspace` 编译通过。workspace 成员数量架构测试通过。

### 阶段 2：Platform 提取

1. 从当前 `src/platform/`、`src/config/` 和 `src/utils/` 创建 `crates/platform/`。
2. 移动文件：`platform/` 直接映射。`config/` → `platform/src/config/`。
   `utils/` → `platform/src/json/` 和 `platform/src/`（合并小型帮手）。
3. 更新 `platform` 的 `Cargo.toml` 依赖：仅提取 `serde`、`serde_yaml`、
   `config`、`aes-gcm`、`argon2`、`hmac`、`sha2`、`base64`、`rand`、
   `sqlx`、`tracing`、`tracing-subscriber`、`tracing-appender`、`chrono`。
4. 删除旧的 `src/platform/`、`src/config/`、`src/utils/`。
5. 更新所有 `use crate::platform::` → `use platform::`。
6. 更新所有 `use crate::config::` → `use platform::config::`。
7. 更新所有 `use crate::utils::` → `use platform::json::`。

**验收**：`cargo test -p platform` 通过。所有现有测试通过。

### 阶段 3：Core 提取——协议 + 模型

1. 创建 `crates/core/`，含空的 `src/`。
2. 将 `src/codex/gateway/protocol/` 和 `src/codex/serving/http/`
   中的协议类型和请求/响应类型移动到 `core/src/protocol/`。
3. 将领域模型从 `src/codex/accounts/model.rs`、`src/codex/models/`、
   `src/codex/events/model.rs` 移动到 `core/src/accounts/model.rs`、
   `core/src/models/` 等。
4. 在 `core/src/*/ports.rs` 中定义端口 trait——从当前内联 trait 定义
   和仓储签名中提取。
5. Core 的 `Cargo.toml` 依赖：仅 `serde`、`serde_json`、`chrono`、
   `thiserror`、`async-trait`、`uuid`。**零 IO crate**。

**验收**：`cargo check -p core` 通过。Core 零 IO crate 依赖。

### 阶段 4：Adapters 提取

1. 创建 `crates/adapters/`，含 `src/sqlite/`、`src/codex/`、`src/oauth/`。
2. 将所有 `*_repository.rs` 文件和 `repository/` 目录移动到
   `adapters/src/sqlite/`。重命名去掉 `_repository` 后缀
   （例如 `accounts.rs` 而非 `account_repository.rs`）。
3. 移动 `src/codex/gateway/transport/http_client.rs` →
   `adapters/src/codex/client.rs`。
4. 移动 `src/codex/gateway/transport/endpoints.rs` →
   `adapters/src/codex/endpoints.rs`。
5. 移动 `src/codex/gateway/transport/custom_ca.rs` →
   `adapters/src/codex/tls.rs`。
6. 移动 `src/codex/gateway/oauth/client.rs` →
   `adapters/src/oauth/openai.rs`。
7. 移动 `src/codex/gateway/fingerprint/update_checker.rs` →
   `adapters/src/codex/fingerprint.rs`。
8. 拆分 `src/codex/gateway/transport/websocket/`：
   - `codec.rs` 协议逻辑 → `core/src/protocol/codex/websocket.rs`
   - `mod.rs`、`pool.rs`、`opening.rs`、`deflate.rs` →
     `adapters/src/codex/websocket/`
9. 在适配器结构体上实现 `core` 端口 trait。

**验收**：`cargo test -p adapters` 通过。架构测试确认 `core` 依赖中无
`axum`、`sqlx`、`reqwest` 或 `tokio-tungstenite`。

### 阶段 5：Core 提取——业务逻辑

1. 将剩余 `src/codex/accounts/` 业务逻辑（pool、lifecycle、cookies、
   jwt、usage policy）移动到 `core/src/accounts/`。
2. 将 `src/codex/serving/dispatch/`（affinity、fallback、routing、
   recovery、stream、usage、implicit_resume、reasoning_replay）移动到
   `core/src/serving/`。
3. 将 `src/admin/session/service.rs`、`src/admin/client_keys/service.rs`、
   `src/admin/settings.rs` 移动到 `core/src/admin/`。
4. 将 `src/codex/events/service.rs` 移动到 `core/src/events/service.rs`。
5. 所有移动的文件必须使用端口 trait，而非具体实现。
   替换 `AccountRepository` 为 `dyn AccountStore` 等。

**验收**：`cargo test -p core` 通过。所有 core 业务逻辑测试通过。

### 阶段 6：Server 提取

1. 移动 `src/admin/api/` → `crates/server/src/admin_api/`。
2. 移动 `src/codex/serving/http/` → `crates/server/src/openai_api/`。
3. 移动 `src/runtime/router.rs` → `crates/server/src/router.rs`。
4. 移动 `src/platform/http/` 中间件 → `crates/server/src/middleware/`。
5. 移动 `src/web/` → `crates/assets/`。
6. 移动 `src/main.rs` → `crates/server/src/main.rs`。
7. 所有 Axum 类型（`Router`、`IntoResponse`、提取器）必须仅存在于
   `server` 和 `assets` 中。

**验收**：`cargo test -p server` 通过。架构测试确认 `core`、`adapters`、
`runtime`、`platform` 中无 Axum 类型。

### 阶段 7：Runtime 清理

1. `runtime/src/` 仅保留 `bootstrap.rs`、`state.rs`、`services.rs`、
   `repositories.rs`、`upstream.rs`、`config.rs` 和 `tasks/`。
2. 移除 `runtime/` 中残留的任何 HTTP、SQL 或领域策略。
3. `runtime` 通过将 `core` 服务与 `adapters` 实现接线来构造 `AppState`。

**验收**：`cargo test -p runtime` 通过。架构测试确认 `runtime` 中无
SQL 查询字符串或 Axum 处理器。

### 阶段 8：架构强制执行

1. 编写 `tests/architecture/directory_shape.rs` —— 验证
   [精确 Rust 源码形状](#精确-rust-源码形状) 中的每个文件和目录存在，
   且无额外文件/目录。
2. 编写 `tests/architecture/dependency_direction.rs` —— 通过
   `cargo metadata` + 解析 `Cargo.toml` 依赖验证 crate 级别依赖规则。
3. 编写 `tests/architecture/forbidden_imports.rs` —— grep 查找被禁止的
   crate 使用（`core` 中的 `use axum::`、`runtime` 中的 `use sqlx::` 等）。
4. 运行完整 CI：`cargo fmt --check`、`cargo check --workspace --all-targets`、
   `cargo test --workspace`、`cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`。

**验收**：所有架构测试通过。所有 CI 检查通过。

## 架构测试

架构测试是验证 workspace 符合本文档的集成测试。
它们作为 `cargo test --workspace` 的一部分运行。

### `tests/architecture/directory_shape.rs`

验证：
- [精确 Rust 源码形状](#精确-rust-源码形状) 中的每个必需文件存在。
- 每个必需目录存在。
- `crates/*/src/` 下无额外 Rust 源文件或目录，根目录无 `src/`。
- `Cargo.toml` workspace 成员匹配 `["crates/core", "crates/adapters",
  "crates/runtime", "crates/server", "crates/platform", "crates/assets",
  "crates/xtask"]`。
- 根 package 不提供 library 或 binary target。

### `tests/architecture/dependency_direction.rs`

验证：
- `core/Cargo.toml` 无 workspace crate 依赖。
- `platform/Cargo.toml` 无 workspace crate 依赖。
- `adapters/Cargo.toml` 不依赖 `runtime` 或 `server`。
- `runtime/Cargo.toml` 不依赖 `server`。
- `assets/Cargo.toml` 无 workspace crate 依赖。
- `xtask/Cargo.toml` 不依赖 `assets`。

### `tests/architecture/forbidden_imports.rs`

验证：
- `core`、`adapters`、`runtime`、`platform` 中无 `use axum::`。
- `core`、`server`、`runtime` 中无 `use sqlx::`。
- `core`、`server`、`runtime` 中无 `use reqwest::`。
- `core`、`server`、`runtime` 中无 `use tokio_tungstenite::` 或
  `use tungstenite::`。
- `core` 中无 `use rustls::` 或 `use tokio_rustls::`。
- `core/src/` 下无名为 `*_repository.rs` 的文件。
- `core/src/` 下无名为 `repository/` 的目录。
- `core/src/` 下无名为 `transport/` 的目录。
- `crates/server/src/` 下无名为 `facade.rs` 的文件。

## 完成标准

当**所有**以下条件为真时，迁移完成：

1. Rust workspace 源码树与 [精确 Rust 源码形状](#精确-rust-源码形状) 完全匹配。
2. 被禁止的名称（架构测试中列出）不存在。
3. 所有架构测试通过（`cargo test --test architecture`）。
4. `cargo fmt --check --workspace` 通过。
5. `cargo check --workspace --all-targets` 通过。
6. `cargo test --workspace` 通过（所有 473+ 现有测试 + 新的架构测试）。
7. `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` 通过。
8. `cargo doc --workspace --no-deps` 无错误生成。
9. `core`、`adapters` 和 `platform` 上启用了 `#![deny(missing_docs)]`。
10. 非测试代码中无不安全的 `unwrap()`、`expect()`、`panic!` 或 `todo!`。
