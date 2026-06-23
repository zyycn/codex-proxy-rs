# Architecture

本文档记录当前源码和集成测试目录的架构边界。

## Architecture Intent

系统主轴是 OpenAI 兼容网关，背后聚合 ChatGPT/Codex 上游账号资源，并提供管理面。

核心边界：

- `runtime` 负责启动、状态、服务装配和后台任务生命周期。
- `http` 负责顶层 Axum 路由合并和通用 HTTP 中间件。
- `proxy` 负责 OpenAI 兼容接口、client API key 鉴权接线、请求调度、fallback、recovery、session affinity 和 reasoning replay。
- `admin` 负责管理面 HTTP/BFF、页面 DTO、错误响应、管理面账号服务和管理面统计。
- `admin/auth` 负责管理员登录、管理员本地 session 服务和 session 存储。
- `admin/keys` 负责本地 `/v1` client API key 管理、创建、校验能力、生命周期和存储。
- `admin/monitoring` 是管理面的日志、用量和诊断统计模块。
- `upstream` 负责 ChatGPT/Codex 上游资源、账号池、模型目录、协议、transport、fingerprint 和 token 续期 client。
- `config`、`infra`、`web` 是通用底座。

## Source Directory

```text
src
|-- admin
|   |-- accounts
|   |   |-- mod.rs
|   |   |-- routes.rs
|   |   `-- service
|   |       |-- cookies.rs
|   |       |-- importing.rs
|   |       |-- lifecycle.rs
|   |       |-- mod.rs
|   |       |-- quota.rs
|   |       `-- types.rs
|   |-- keys
|   |   |-- mod.rs
|   |   |-- routes.rs
|   |   `-- service.rs
|   |-- auth
|   |   |-- mod.rs
|   |   |-- routes.rs
|   |   |-- service.rs
|   |   `-- session.rs
|   |-- mod.rs
|   |-- models
|   |   |-- mod.rs
|   |   `-- routes.rs
|   |-- monitoring
|   |   |-- diagnostics.rs
|   |   |-- diagnostics_routes.rs
|   |   |-- event_store.rs
|   |   |-- events.rs
|   |   |-- logs.rs
|   |   |-- mod.rs
|   |   |-- service.rs
|   |   |-- usage.rs
|   |   `-- usage_store.rs
|   |-- response.rs
|   |-- router.rs
|   `-- settings
|       |-- mod.rs
|       `-- routes.rs
|-- config
|   |-- loader.rs
|   |-- mod.rs
|   |-- settings.rs
|   |-- types.rs
|   `-- writeback.rs
|-- proxy
|   |-- auth.rs
|   |-- dispatch
|   |   |-- chat.rs
|   |   |-- cloudflare.rs
|   |   |-- errors.rs
|   |   |-- implicit_resume.rs
|   |   |-- mod.rs
|   |   |-- reasoning_replay.rs
|   |   |-- responses.rs
|   |   |-- session_affinity.rs
|   |   `-- upstream.rs
|   |-- mod.rs
|   |-- openai
|   |   |-- chat.rs
|   |   |-- diagnostics.rs
|   |   |-- errors.rs
|   |   |-- mod.rs
|   |   |-- models.rs
|   |   |-- responses.rs
|   |   |-- routes.rs
|   |   `-- sse.rs
|   `-- router.rs
|-- http
|   |-- middleware
|   |   |-- mod.rs
|   |   |-- request_id.rs
|   |   `-- trace.rs
|   |-- mod.rs
|   `-- router.rs
|-- infra
|   |-- crypto.rs
|   |-- database.rs
|   |-- identity.rs
|   |-- json.rs
|   |-- logging.rs
|   |-- mod.rs
|   |-- paths.rs
|   `-- schema.sql
|-- lib.rs
|-- main.rs
|-- runtime
|   |-- bootstrap.rs
|   |-- mod.rs
|   |-- services.rs
|   |-- shutdown.rs
|   |-- state.rs
|   `-- tasks
|       |-- cookie_cleanup.rs
|       |-- coordinator.rs
|       |-- fingerprint_update.rs
|       |-- mod.rs
|       |-- model_refresh.rs
|       |-- quota_refresh.rs
|       |-- session_affinity_cleanup.rs
|       |-- session_cleanup.rs
|       `-- token_refresh.rs
|-- upstream
|   |-- accounts
|   |   |-- cookies.rs
|   |   |-- import_export.rs
|   |   |-- mod.rs
|   |   |-- model.rs
|   |   |-- pool.rs
|   |   |-- quota
|   |   |   |-- mod.rs
|   |   |   `-- runtime.rs
|   |   |-- service.rs
|   |   |-- store.rs
|   |   `-- token_refresh
|   |       |-- mod.rs
|   |       `-- runtime.rs
|   |-- fingerprint.rs
|   |-- mod.rs
|   |-- models
|   |   `-- mod.rs
|   |-- protocol
|   |   |-- events.rs
|   |   |-- mod.rs
|   |   |-- responses.rs
|   |   |-- schema.rs
|   |   |-- sse.rs
|   |   `-- websocket.rs
|   |-- token_client.rs
|   `-- transport
|       |-- client.rs
|       |-- endpoints.rs
|       |-- headers.rs
|       |-- mod.rs
|       |-- tls.rs
|       |-- usage.rs
|       |-- websocket.rs
|       `-- websocket_pool.rs
`-- web
    |-- assets.rs
    |-- headers.rs
    `-- mod.rs
```

## Test Directory

```text
tests/integration
|-- admin
|   |-- accounts
|   |   |-- import_export.rs
|   |   |-- lifecycle.rs
|   |   |-- list.rs
|   |   |-- mod.rs
|   |   `-- quota.rs
|   |-- keys
|   |   |-- authorization.rs
|   |   |-- import_export.rs
|   |   |-- lifecycle.rs
|   |   |-- mod.rs
|   |   `-- store.rs
|   |-- auth
|   |   |-- mod.rs
|   |   |-- password.rs
|   |   |-- session.rs
|   |   |-- session_routes.rs
|   |   `-- session_store.rs
|   |-- mod.rs
|   |-- models
|   |   |-- mod.rs
|   |   `-- routes.rs
|   |-- monitoring
|   |   |-- events_store.rs
|   |   |-- logs_routes.rs
|   |   `-- mod.rs
|   |-- response
|   |   `-- mod.rs
|   `-- settings
|       |-- mod.rs
|       `-- routes.rs
|-- config
|   `-- mod.rs
|-- fixtures
|   |-- chat
|   |   `-- success.sse
|   `-- responses
|       |-- golden
|       |   `-- reasoning_replay_request.json
|       |-- http_sse
|       |   |-- after_401.sse
|       |   |-- after_402.sse
|       |   |-- after_403.sse
|       |   |-- after_5xx_retry.sse
|       |   |-- after_cloudflare.sse
|       |   |-- after_model_unsupported.sse
|       |   |-- completed_image_usage.sse
|       |   |-- completed_usage.sse
|       |   |-- done_item_completed.sse
|       |   |-- empty_completed.sse
|       |   |-- failed_auth.sse
|       |   |-- failed_model_unsupported.sse
|       |   |-- failed_quota.sse
|       |   |-- stream_after_401.sse
|       |   |-- stream_after_402.sse
|       |   |-- stream_after_403.sse
|       |   |-- stream_after_429.sse
|       |   |-- stream_after_5xx_retry.sse
|       |   |-- stream_after_cloudflare.sse
|       |   |-- stream_after_model_unsupported.sse
|       |   |-- stream_usage.sse
|       |   |-- success.sse
|       |   |-- text_deltas_completed.sse
|       |   `-- tuple_object.sse
|       `-- websocket
|           |-- completed_with_reasoning_replay.json
|           |-- first_account_limited.json
|           |-- history_rate_limited.json
|           |-- invalid_encrypted_content.json
|           |-- previous_response_not_found.json
|           |-- rate_limited.json
|           |-- second_account_limited.json
|           |-- token_revoked.json
|           `-- unanswered_function_call.json
|-- proxy
|   |-- dispatch
|   |   |-- chat_upstream
|   |   |   |-- chat_routes.rs
|   |   |   |-- compact_routes.rs
|   |   |   |-- mod.rs
|   |   |   |-- responses_http.rs
|   |   |   |-- responses_recovery.rs
|   |   |   |-- responses_websocket.rs
|   |   |   `-- usage_logging.rs
|   |   |-- mod.rs
|   |   |-- session_affinity.rs
|   |   `-- session_affinity_integration.rs
|   |-- mod.rs
|   |-- keys
|   |   |-- auth.rs
|   |   `-- mod.rs
|   `-- openai
|       |-- diagnostics_routes.rs
|       |-- mod.rs
|       |-- models_routes.rs
|       `-- responses_routes.rs
|-- http
|   |-- mod.rs
|   |-- trace_middleware
|   |   `-- mod.rs
|   `-- web_assets
|       `-- mod.rs
|-- infra
|   |-- mod.rs
|   |-- crypto
|   |   `-- mod.rs
|   |-- log_rotation
|   |   `-- mod.rs
|   `-- storage_schema
|       `-- mod.rs
|-- main.rs
|-- runtime
|   |-- account_pool_restore
|   |   `-- mod.rs
|   |-- mod.rs
|   |-- tasks
|   |   |-- cleanup.rs
|   |   |-- coordinator.rs
|   |   |-- fingerprint.rs
|   |   |-- mod.rs
|   |   `-- model_refresh.rs
|-- support
|   |-- config.rs
|   |-- mod.rs
|   `-- sqlite.rs
`-- upstream
    |-- accounts
    |   |-- account_pool
    |   |   |-- mod.rs
    |   |   |-- quota.rs
    |   |   |-- selection.rs
    |   |   `-- usage_window.rs
    |   |-- account_repository.rs
    |   |-- cloudflare.rs
    |   |-- cookies.rs
    |   |-- mod.rs
    |   |-- quota_refresh.rs
    |   |-- refresh_leases.rs
    |   `-- token_refresh
    |       |-- failures.rs
    |       |-- mod.rs
    |       |-- scheduling.rs
    |       `-- success.rs
    |-- fingerprint
    |   `-- mod.rs
    |-- fingerprint_integration
    |   `-- mod.rs
    |-- mod.rs
    |-- models
    |   |-- catalog.rs
    |   |-- mod.rs
    |   `-- store.rs
    |-- protocol
    |   |-- codex_websocket.rs
    |   |-- mod.rs
    |   |-- openai_chat.rs
    |   |-- openai_responses.rs
    |   `-- usage_rate_limits.rs
    |-- token_client.rs
    `-- transport
        |-- client.rs
        |-- headers.rs
        |-- http_client.rs
        |-- mod.rs
        |-- websocket.rs
        `-- websocket_pool.rs
```

## Module Responsibilities

| Module | Owns | Must not own |
| --- | --- | --- |
| `runtime` | 进程启动、依赖装配、`AppState`、优雅关闭、后台任务生命周期。 | 协议转换、SQL 查询、HTTP DTO、账号业务规则。 |
| `http` | 顶层 Axum router、路由合并、通用中间件。 | 具体功能 handler 和业务服务。 |
| `proxy` | OpenAI 兼容 API、proxy 鉴权接线、dispatch、fallback、retry、上游错误映射、session affinity、reasoning replay。 | 管理面 DTO、SQLite schema 管理、上游 transport 实现细节。 |
| `proxy/openai` | OpenAI 请求/响应结构、OpenAI route handlers、SSE 输出、OpenAI 兼容错误格式。 | 账号池状态机、Codex transport 内部实现。 |
| `proxy/dispatch` | Chat/Responses 调度编排、账号选择调用、恢复、重试、Cloudflare 处理、affinity 和 replay。 | 非调度 HTTP DTO、原始 SQLite 访问。 |
| `admin` | 管理 API/BFF、管理响应 envelope、管理路由、管理面 DTO、管理面服务门面。 | Proxy 调度算法、上游协议和 transport。 |
| `admin/auth` | 管理员登录、管理员本地 session 校验、默认管理员初始化和 session 存储。 | `/v1` client API key 运行时鉴权。 |
| `admin/accounts` | 管理面账号路由、账号服务门面、导入导出、生命周期、Cookie 和配额管理入口。 | OpenAI 兼容 API 输出格式。 |
| `admin/keys` | 管理面 v1 接口访问 Key 的 HTTP/BFF 入口、Key 创建、hash 校验、启停、使用时间记录和 SQLite 存储。 | `/v1` 请求入口鉴权接线、管理员 session。 |
| `admin/monitoring` | 管理面日志、用量统计、诊断、dashboard 摘要、管理统计查询。 | 通用 tracing 基础设施、账号 token 生命周期、OpenAI 兼容响应格式。 |
| `upstream` | ChatGPT/Codex 上游资源聚合、账号池、模型、协议、transport、fingerprint、token 续期 client。 | 管理页面 DTO、proxy 错误 body 格式。 |
| `upstream/accounts` | 上游账号模型、仓储、池调度、Cookie 状态、导入导出解析、配额状态、token refresh。 | 管理路由 DTO、OpenAI API shape。 |
| `upstream/models` | 上游模型目录、别名、快照、刷新服务和存储端口。 | OpenAI `/v1/models` 输出格式。 |
| `upstream/protocol` | Codex 协议 schema、事件、Responses 结构、WebSocket frame、SSE 解析。 | HTTP client 连接行为和重试编排。 |
| `upstream/transport` | Codex HTTP/SSE/WebSocket client、headers、endpoints、TLS、usage endpoint、WebSocket pool。 | 管理诊断 DTO 和 proxy route handlers。 |
| `config` | 配置类型、加载、运行时设置、配置写回。 | 业务服务和 HTTP handlers。 |
| `infra` | SQLite 连接/schema、crypto、identity hashing、JSON helpers、logging setup、文件路径。 | 任何账号、proxy、admin 领域语义。 |
| `web` | SPA fallback 和静态资源缓存头。 | API routes。 |

## Dependency Rules

Allowed dependency direction:

```text
runtime -> all modules
http -> proxy, admin, web
proxy -> upstream, admin/monitoring, infra
admin -> upstream, config, admin/monitoring, infra
admin/monitoring -> infra, config
upstream -> infra, config
web -> infra
config -> external crates
infra -> external crates
```

Forbidden dependencies:

- `infra` 和 `config` 不依赖业务模块。
- `upstream` 不依赖 `proxy` 或 admin route 模块。
- `proxy/openai` 不直接查询 SQLite。
- `proxy/dispatch` 不依赖 admin DTO 或 admin routes。
- `admin` handlers 不直接构造原始 Codex 上游请求；它们调用 `upstream` 或 admin service。
- `admin/monitoring` 可提供 `proxy` 使用的 service/store 类型，但 `proxy` 不依赖 monitoring routes 或 DTO。
- `runtime/services.rs` 是 composition root；业务模块不导入 `runtime::services`。

## Placement Rules

| Change | Target |
| --- | --- |
| `/v1/chat/completions`、`/v1/responses`、`/v1/models` contract | `proxy/openai` |
| dispatch、fallback、retry、账号恢复、reasoning replay、session affinity | `proxy/dispatch` |
| proxy client API key auth extraction | `proxy/auth.rs` plus `admin/keys/service.rs` |
| 管理面账号页面/API、账号导入导出、管理面账号服务 | `admin/accounts` |
| 上游账号模型、池调度、仓储、配额、token refresh、cookies、导入数据解析 | `upstream/accounts` |
| refresh token 续期 HTTP client | `upstream/token_client.rs` |
| Codex fingerprint 或 installation identity | `upstream/fingerprint.rs` |
| 上游模型目录或模型快照 | `upstream/models` |
| Codex 协议解析或请求/响应结构 | `upstream/protocol` |
| Codex HTTP/SSE/WebSocket 行为 | `upstream/transport` |
| 管理员登录/session | `admin/auth` |
| 管理面 v1 接口访问 Key 页面/API | `admin/keys` |
| 管理面 settings 页面/API | `admin/settings` plus `config` |
| 管理面 logs、usage stats、diagnostics、dashboard summaries | `admin/monitoring` |
| SQLite 连接、schema、crypto、hashing、paths、logging setup | `infra` |
| 顶层路由合并或中间件 | `http` |
| SPA fallback 或静态资源缓存头 | `web` |
| 启动顺序、服务构造、后台任务启动/关闭 | `runtime` |
