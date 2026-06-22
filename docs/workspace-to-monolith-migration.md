# AI 中转站 Workspace 回迁单体架构文档

基准提交：`74914b95b28588d26a0ccfb16bf872f3a312d4ef`

本文档是重新审计后的最终迁移目标。目标不是“拆得越散越好”，也不是把
`core/adapters/runtime/server/platform/assets` 原样搬进 `src/`。目标是一个合理的
modular monolith：单个 Rust package，少量高内聚模块，模块内部按复杂度拆文件。

## 审计依据

本文档基于 `74914b95b28588d26a0ccfb16bf872f3a312d4ef` 的真实代码边界整理，重点审计了：

- `crates/server/src/openai_api/router.rs` 和 `crates/server/src/admin_api/router.rs` 的路由合约。
- `crates/runtime/src/services.rs`、`crates/runtime/src/state.rs` 的运行时服务装配。
- `crates/runtime/src/tasks/*` 的后台任务。
- `crates/platform/src/storage/schema.sql` 的持久化表。
- `crates/core/src/protocol/*`、`crates/core/src/serving/*` 的协议转换和调度规则。
- `crates/adapters/src/codex/*`、`crates/adapters/src/oauth/*`、`crates/adapters/src/sqlite/*` 的上游和存储适配。
- `web/src/api/modules/*`、`web/src/views/*` 的前端调用面。

审计结论是：这个系统的主轴不是“后台管理系统”，也不是“纯 OpenAI proxy”，而是一个
AI 中转站/协议网关。核心链路是 `client -> OpenAI compatible API -> gateway dispatch ->
account pool -> Codex/ChatGPT upstream -> OpenAI compatible response`。Admin 和 dashboard 是
围绕这条链路提供账号、密钥、统计、诊断、配置和日志管理的 BFF/API。

## 系统定位

这是一个 AI 中转站/协议网关系统。

它对外提供 OpenAI 兼容接口：

- `/v1/chat/completions`
- `/v1/responses`
- `/v1/responses/review`
- `/v1/responses/compact`
- `/v1/models`

它内部完成：

- client API key 鉴权。
- OpenAI Chat/Responses 请求解析。
- OpenAI 协议到 Codex/ChatGPT 上游协议转换。
- ChatGPT/Codex 账号池选择、轮换、会话亲和性和 fallback。
- Codex HTTP SSE / WebSocket 上游请求。
- Codex 响应、SSE、WebSocket 帧到 OpenAI 兼容响应转换。
- token、quota、Cloudflare、cookie、reasoning replay、implicit resume 等运行策略。

它同时提供管理端能力：

- 管理员登录、session。
- client API key 管理。
- ChatGPT/Codex 账号导入、刷新、状态、Cookie、配额、健康检查。
- 用量统计、事件日志、诊断信息、模型刷新、配置读写。
- 前端 SPA 静态资源 fallback。

## 当前系统能力审计

### 对外 OpenAI 兼容接口

| 路由 | 能力 |
| --- | --- |
| `POST /v1/chat/completions` | Chat Completions 到 Codex Responses 的转换、非流式/流式输出、OpenAI 错误映射。 |
| `POST /v1/responses` | OpenAI Responses 到 Codex Responses 的转换、HTTP SSE/WebSocket 调度、fallback、事件记录。 |
| `POST /v1/responses/review` | 强制 review subagent 的 Responses 变体。 |
| `POST /v1/responses/compact` | Compact 请求变体。 |
| `GET /v1/models` | OpenAI 兼容模型列表。 |
| `GET /v1/models/catalog` | 模型目录详情。 |
| `GET /v1/models/{model_id}` | 单模型详情。 |
| `GET /v1/models/{model_id}/info` | 模型解析信息。 |
| `GET /debug/models` | 本地 debug 模型信息。 |
| `GET /debug/diagnostics` | 本地 debug 运行诊断。 |
| `GET /debug/fingerprint` | 本地 debug 当前 fingerprint。 |
| `GET /debug/upstream` | 本地 debug 上游连通性 probe。 |

### 管理端和前端接口

| 路由 | 能力 |
| --- | --- |
| `POST /api/admin/login`、`POST /api/admin/logout` | 管理员登录和退出。 |
| `GET/PATCH /api/admin/settings` | 管理端配置读取和本地写回。 |
| `GET /api/admin/diagnostics` | 给前端 dashboard 的运行诊断。 |
| `GET /api/admin/auth/status` | OAuth/账号池状态。 |
| `POST /api/admin/auth/logout` | 清理账号 OAuth 状态。 |
| `POST /api/admin/auth/login-start` | PKCE 登录开始。 |
| `POST /api/admin/auth/device-login` | Device code 登录开始。 |
| `GET /api/admin/auth/device-poll/{device_code}` | Device code 轮询。 |
| `POST /api/admin/auth/code-relay`、`GET /auth/callback` | OAuth callback/code relay。 |
| `POST /api/admin/refresh-models` | 手动刷新模型目录。 |
| `GET /api/admin/usage-stats`、`GET /api/admin/usage-stats/summary` | 前端 usage/dashboard 统计。 |
| `/api/admin/accounts*` | 账号列表、创建、导入、导出、批量删除、状态、标签、刷新、Cookie、quota、健康检查。 |
| `/api/admin/logs*` | 事件日志列表、详情、清空、日志开关和容量。 |
| `/api/admin/api-keys*` | client API key 列表、创建、导入、导出、启停、标签、删除。 |

### 持久化数据

SQLite schema 包含这些业务数据：

- `admin_users`、`admin_sessions`
- `client_api_keys`
- `accounts`
- `account_refresh_leases`
- `account_usage`
- `account_cookies`
- `fingerprints`
- `fingerprint_update_history`
- `event_logs`
- `model_plan_snapshots`
- `session_affinities`

### 后台任务

| 任务 | 能力 |
| --- | --- |
| `cookie_cleanup` | 清理过期账号 Cookie。 |
| `session_cleanup` | 清理过期管理员 session。 |
| `session_affinity_cleanup` | 清理过期 Responses 会话亲和性。 |
| `model_refresh` | 启动后和周期性刷新 Codex 模型目录。 |
| `token_refresh` | 调度账号 token 刷新、租约、防并发刷新、失败恢复。 |
| `quota_refresh` | 周期性刷新 quota 锁定/待验证账号。 |
| `fingerprint_update` | 检查 Codex Desktop appcast，更新 fingerprint history/current。 |

## 业务能力地图

这张表决定最终模块归属。后续实现时以这里为准，不再按旧 crate 名字搬运。

| 业务能力 | 当前入口/数据 | 最终拥有模块 | 说明 |
| --- | --- | --- | --- |
| OpenAI Chat/Responses/Models 兼容 API | `/v1/chat/completions`、`/v1/responses*`、`/v1/models*` | `gateway` | 对外协议合约和请求调度入口。 |
| OpenAI 到 Codex 协议转换 | `core/src/protocol/openai/*`、`core/src/protocol/codex/*` | `gateway/openai`、`codex/protocol` | OpenAI 形状归 `gateway`，Codex 上游形状归 `codex`。 |
| 请求调度、fallback、recovery、implicit resume、reasoning replay | `core/src/serving/*`、`runtime/src/services/dispatch_*` | `gateway/dispatch` | 中转站核心请求链路，不能放进 admin。 |
| client API key 鉴权和管理 | `/api/admin/api-keys*`、`client_api_keys` | `access`，HTTP 合约在 `admin/api_keys.rs` | 既服务 OpenAI API 鉴权，也服务 admin 页面管理。 |
| 管理员登录和 session | `/api/admin/login`、`/api/admin/logout`、`admin_users`、`admin_sessions` | `access`，HTTP 合约在 `admin/session.rs` | 管理端访问控制，不和 client API key 混到 gateway。 |
| ChatGPT/Codex 账号池 | `/api/admin/accounts*`、`accounts` | `accounts` | 账号是运行时调度资源，不是 admin 子模块。 |
| 账号导入导出、标签、状态、健康检查 | `runtime/src/services/admin_accounts.rs`、`server/src/admin_api/accounts/*` | `accounts`，HTTP 合约在 `admin/accounts.rs` | 业务规则在 `accounts`，前端 DTO 在 `admin`。 |
| OAuth PKCE、device code、callback | `/api/admin/auth/*`、`/auth/callback`、`auth/oauth.rs`、`oauth/openai.rs` | `accounts/oauth.rs`、`codex/oauth_client.rs` | 账号登录流程归 `accounts`，裸 HTTP OAuth client 归 `codex`。 |
| token refresh 策略、租约和失败状态转换 | `account_refresh_leases`、`auth/oauth.rs` 中 refresh 相关类型 | `accounts/token_refresh/`，任务启动在 `app/tasks/token_refresh.rs` | token refresh 是账号生命周期规则，不属于 OAuth 登录文件。 |
| quota 查询、quota 锁定、quota 恢复 | `/api/admin/accounts/*/quota`、`account_usage`、`accounts.cached_quota` | `accounts/quota/`、`telemetry/usage.rs` | quota 状态属于账号池，聚合统计属于 telemetry。 |
| Cookie 和 Cloudflare 状态 | `account_cookies`、`accounts.cloudflare_*` | `accounts/cookies.rs` | Cookie 是账号凭证/风控状态的一部分。 |
| Codex 上游 HTTP/SSE/WebSocket | `adapters/src/codex/*` | `codex/transport` | 所有“怎么请求 Codex 上游”的细节集中在这里。 |
| Codex fingerprint、installation id、appcast 更新 | `fingerprints`、`fingerprint_update_history`、`fingerprint_update` | `codex/fingerprint.rs`，任务在 `app/tasks/fingerprint_update.rs` | fingerprint 是上游身份模拟能力，不是通用配置。 |
| 模型目录和模型快照 | `/v1/models*`、`/api/admin/refresh-models`、`model_plan_snapshots` | `codex/models.rs`，OpenAI 展示在 `gateway/openai/models.rs` | Codex 模型来源归 `codex`，OpenAI 响应格式归 `gateway`。 |
| 事件日志 | `/api/admin/logs*`、`event_logs` | `telemetry/events.rs`、`telemetry/event_store.rs` | 供 dashboard、排错、审计使用。 |
| 用量统计 | `/api/admin/usage-stats*`、`account_usage` | `telemetry/usage.rs`、`telemetry/usage_store.rs` | 统计归 telemetry，不作为顶层 `usage` 模块；账号池只暴露窄的 usage delta。 |
| 运行诊断 | `/api/admin/diagnostics`、`/debug/diagnostics`、`/debug/upstream` | `telemetry/diagnostics.rs`，debug route 在 `gateway/openai` | 诊断聚合多个模块状态，但不拥有业务规则。 |
| 管理端设置 | `/api/admin/settings`、`config.yaml` | `config`，HTTP 合约在 `admin/settings.rs` | 配置类型/写回归 config，前端接口归 admin。 |
| 前端 SPA fallback 和静态资源缓存 | `web/dist`、`assets` crate | `web` | 只处理静态资源，不承载 admin API。 |
| 启动、状态、服务装配、后台任务生命周期 | `server/src/main.rs`、`runtime/src/bootstrap.rs`、`runtime/src/state.rs`、`runtime/src/services.rs`、`runtime/src/tasks/*` | `app` | 组合根只接线，不写协议和账号业务。 |

## 数据归属地图

| 表 | 最终归属 | 说明 |
| --- | --- | --- |
| `admin_users` | `access` | 管理员登录凭据。 |
| `admin_sessions` | `access` | 管理员 session 和清理任务。 |
| `client_api_keys` | `access` | OpenAI compatible API 的 client key。 |
| `accounts` | `accounts` | ChatGPT/Codex 账号、token、状态、quota/cache、Cloudflare 标记。 |
| `account_refresh_leases` | `accounts/token_refresh/mod.rs` | 多实例或并发 token refresh 租约。 |
| `account_cookies` | `accounts/cookies.rs` | 账号 Cookie 存储和清理。 |
| `account_usage` | `telemetry/usage_store.rs` | 按账号聚合的请求和 token 用量；`accounts` 只能通过 usage delta 写入，不拥有报表查询。 |
| `fingerprints` | `codex/fingerprint.rs` | 当前和历史 Codex Desktop fingerprint。 |
| `fingerprint_update_history` | `codex/fingerprint.rs` | appcast 检查和 fingerprint 更新历史。 |
| `event_logs` | `telemetry/event_store.rs` | 请求事件、错误、上游响应和审计日志。 |
| `model_plan_snapshots` | `codex/models.rs` | Codex 模型目录快照。 |
| `session_affinities` | `gateway/dispatch/session_affinity.rs` | Responses 会话到账号的亲和性。 |

## 前端页面地图

| 前端页面/API 模块 | 调用的后端能力 | 最终后端归属 |
| --- | --- | --- |
| `LoginView.vue`、`api/modules/auth.ts` | 管理员登录、退出、OAuth 状态 | `admin/session.rs`、`admin/auth.rs` -> `access`、`accounts` |
| `DashboardView.vue`、`useDashboard.ts` | diagnostics、usage summary、usage stats、accounts、logs | `admin/diagnostics.rs`、`admin/usage.rs`、`admin/accounts.rs`、`admin/logs.rs` -> `telemetry`、`accounts` |
| `AccountsView.vue`、`api/modules/accounts.ts` | 账号列表、创建、刷新、状态、标签、quota | `admin/accounts.rs` -> `accounts` |
| `ApiKeysView.vue`、`api/modules/api-keys.ts` | client API key 管理 | `admin/api_keys.rs` -> `access` |
| `LogsView.vue`、`api/modules/logs.ts` | 事件日志查询、详情、清空 | `admin/logs.rs` -> `telemetry` |
| `SettingsView.vue`、`api/modules/settings.ts` | 配置读取和写回 | `admin/settings.rs` -> `config` |

## 架构原则

1. 只有一个 Cargo package：`codex-proxy-rs`。
2. 模块按“改动一起发生的范围”划分，不按名词数量划分。
3. 不机械套 `domain/application/infrastructure/interface` 四层。
4. 文件职责清楚即可；只有模块变大时才拆子目录。
5. HTTP handler 可以集中在 `admin` 或 `gateway`，因为它们是明确的 API 合约层。
6. SQLite store 靠近拥有该数据的模块，不做全局 `repositories` 包。
7. 上游 Codex 链路集中在 `codex`，OpenAI 对外兼容集中在 `gateway`。
8. 统计和事件作为 `telemetry`，因为它同时服务调度、admin dashboard 和审计。
9. `app` 只装配依赖和后台任务，不写业务规则。
10. `infra` 只放无业务含义的底层工具。
11. trait 只在测试替身、外部系统抽象或确实需要动态分发时保留；旧的 `ports` 目录不整体迁入。
12. 每个业务模块维护自己的 `Error` 类型并用 `thiserror` 显式建模；不要把 `anyhow` 扩散进库代码。

## 当前架构审计结论

2026-06-23 使用 `codegraph` 重新审计当前单体实现后，确认迁移方向正确。审计时发现：

- `src/app/services.rs` 目前不是单纯组合根，而是承载了多组业务服务和错误类型。后续必须把这些服务按业务归属移回 `access/accounts/codex/telemetry/config`。
- `admin`、`gateway`、`telemetry` 仍有代码直接引用 `crate::app::services::*`。这会让业务模块反向依赖组合根，后续修改任意业务服务都会扩大改动半径。
- `AdminLogService`、`AdminLogError`、`AdminLogFilter`、`AdminLogState` 等日志服务类型存在重复归属迹象，最终必须只归 `telemetry`，`app` 只持有构造好的实例。
- `gateway/dispatch/responses.rs` 仍然较大，但 Cloudflare challenge/path-block 识别和恢复是 Chat/Responses 共享规则，已从 Responses 主流程中抽到 `gateway/dispatch/cloudflare.rs`；模型后缀/default reasoning/service tier 的请求选项应用、Codex SSE 完成/失败收集和 replay metadata 提取已下沉到 `codex/protocol/responses.rs`；`codex/transport` 已按 HTTP client、headers、TLS、usage、WebSocket、WebSocket pool 拆开，后续只在职责继续变大时再拆。

本轮优化从“明确错误归属和依赖方向”的地方开始，不做兼容层，不保留迁移别名。

### 2026-06-23 收敛结果

本轮已完成第一阶段收敛：

- `AdminLogService` 及日志查询/状态类型只保留在 `telemetry/event_store.rs`。
- `RuntimeAccountPoolService` 只保留在 `accounts/pool.rs`。
- client API key 管理服务和 DTO 移到 `access/client_keys.rs`。
- 管理员 session 服务移到 `access/admin_session.rs`。
- 账号 CRUD、导入导出、quota、Cookie、账号状态、OAuth 状态聚合移到 `accounts/admin_service/`。
- `accounts/admin_service/` 按内部职责拆分：`lifecycle.rs` 管账号生命周期和状态，`cookies.rs` 管 Cookie，`quota.rs` 管 quota 和健康检查，`importing.rs` 管导入导出，`types.rs` 管 DTO、错误和转换辅助。
- OAuth PKCE/device/callback 管理服务移到 `accounts/oauth.rs`，未被路由调用的占位 JSON 方法已删除。
- token refresh 策略、上游 refresh 失败分类、并发调度器和状态转换从 `accounts/oauth.rs` 移到 `accounts/token_refresh/`。
- 模型刷新管理服务移到 `codex/models.rs`。
- 用量统计管理服务移到 `telemetry/usage.rs`。
- 运行时设置读写服务和设置 patch 领域逻辑移到 `config/settings.rs`。

当前 `rg "crate::app::services|app::services" src --glob '*.rs'` 的结果只允许出现在 `app` 模块自身。
业务模块不得再反向引用组合根。

### 目录对齐审计

2026-06-23 继续对比“最终源码目录”和当前 `src/` 后，确认有两类问题：

| 位置 | 结论 | 处理 |
| --- | --- | --- |
| `codex/transport/connect.rs`、`opening.rs`、`pool.rs` | 当前文件名仍沿用迁移前 WebSocket 适配器拆法，和最终 `websocket.rs`、`websocket_pool.rs` 不一致。 | 代码收敛到 `client.rs`、`endpoints.rs`、`headers.rs`、`tls.rs`、`usage.rs`、`websocket.rs`、`websocket_pool.rs`。 |
| `access/stores.rs` | 文档目标不合理；当前 `SqliteAdminSessionStore` 与 `SqliteClientKeyStore` 分别靠近 `admin_session.rs`、`client_keys.rs`，职责更清楚。 | 删除 `access/stores.rs` 目标，保留当前按能力聚合的文件。 |
| `accounts/store.rs` 中的 `SqliteCookieStore` | Cookie 持久化属于 `account_cookies` 和 Cloudflare/Cookie 状态，不应留在账号主 store。 | 代码移动到 `accounts/cookies.rs`，不在 `accounts/store.rs` 保留旧出口。 |
| `accounts/store.rs` 中的 usage 列表/汇总查询 | `account_usage` 的 dashboard 报表查询属于 telemetry；账号 store 只保留账号池运行态所需的 usage delta、窗口同步和 reset。 | `list_usage`、`usage_summary`、报表 DTO 和 SQL 移到 `telemetry/usage_store.rs`，`AdminUsageService` 改依赖 `SqliteUsageStore`。 |
| `accounts/store.rs` 中 `AccountStore` 的默认 no-op 方法 | quota JSON、quota snapshot、rate-limit window 这些是账号池运行态必需能力，trait 默认返回空值会掩盖实现缺失。 | 删除默认 no-op，生产 store 和测试 fake 都必须显式实现；只保留 `get_pool_account`、`record_request` 这种基于必需方法的真实派生默认实现。 |
| `codex/transport/websocket.rs` 中内联的协议解析/错误分类 | WebSocket 错误帧分类、metadata turn state、incomplete reason、SSE usage 和 retry-after 解析属于 Codex 协议语义，不应在 transport 再复制一份。 | 补齐 `codex/protocol/websocket.rs` 和 `codex/protocol/events.rs` 的行为后，transport 直接调用协议层函数，删除内联重复实现。 |
| `gateway/openai/mod.rs` 中的 diagnostics DTO/聚合逻辑 | debug HTTP handler 属于 `gateway/openai/diagnostics.rs`，诊断数据聚合属于 `telemetry/diagnostics.rs`；`mod.rs` 只应声明路由。 | `/debug/diagnostics`、`/debug/fingerprint`、`/debug/upstream` handler 移到 `gateway/openai/diagnostics.rs`，DTO 和聚合只保留在 `telemetry/diagnostics.rs`。 |
| `accounts/admin_service/types.rs` 依赖 `gateway::dispatch::responses` 判断封禁 | 账号导入/补信息不能依赖 gateway 调度层；封禁/停用是 Codex 上游错误语义。 | 封禁 signal 和 `CodexClientError` 分类下沉到 `codex/transport/client.rs`，`accounts` 和 `gateway` 共同引用。 |
| `gateway/dispatch/responses.rs` 中的 `CodexTransport` 决策 | 传输选择只依赖 `CodexResponsesRequest` 字段，不属于 gateway 调度私有规则；Codex protocol、transport 都需要读取。 | `CodexTransport`、`transport_for_request`、`http_sse_fallback_allowed` 移到 `codex/protocol/responses.rs`。 |
| `telemetry/diagnostics.rs` 直接接收 `AppState` | telemetry 不能反向依赖组合根；diagnostics 聚合应接收明确输入。 | 改为 `DiagnosticsInput`，由 admin/debug handler 从 `AppState` 取配置、账号、容量和 fingerprint 后传入。 |
| `gateway/dispatch/mod.rs` 中重复定义模型目录 client 端口 | 模型目录上游 client 属于 `codex/transport`，gateway dispatch 不应持有重复端口定义。 | 删除 gateway 内重复的 `CodexModelCatalogClient*` 类型，只保留 `codex/transport` 真实出口。 |
| `accounts/oauth.rs` 中的 token refresh 策略/调度器/失败状态 | OAuth 文件只应管理 PKCE、device code、callback 和 OAuth client 端口；refresh 状态机属于账号 token 生命周期。 | `RefreshPolicy`、`RefreshTrigger`、`RefreshFailure`、`RefreshError`、`RefreshScheduler`、刷新状态转换移动到 `accounts/token_refresh/`。 |
| `codex/transport`、`accounts/oauth`、`accounts/store` 的跨模块 `pub use crate::...` | 这些再出口会模糊真实归属，容易让新代码继续从错误模块取类型。 | 调用点改为从 `codex/protocol/*`、`accounts/token_refresh/`、`accounts/model.rs` 直接导入，删除借道出口。 |
| `accounts/quota` 中重复的 Cloudflare tracker | Cloudflare path-block/challenge cooldown 已由 `accounts/cookies.rs` 持有，quota 模块中的实现是重复残留。 | 删除重复 tracker，测试改为从 `accounts/cookies.rs` 导入。 |
| `gateway/dispatch/responses.rs` 中的 Cloudflare recovery | Cloudflare challenge/path-block 识别、cookie 清理、cooldown 和 path-block 禁用同时服务 Chat 和 Responses，不属于 Responses 单一路由。 | 移到 `gateway/dispatch/cloudflare.rs`，Chat/Responses 都直接导入真实归属模块，不保留旧路径再出口。 |
| `gateway/dispatch/responses.rs` 中的通用上游错误分类 | 401/402/429、模型不支持、history recovery、retry-after、transport 名称和上游错误体提取同时服务 Chat/Responses，不属于 Responses 主流程；单独 `fallback.rs` 只有状态码布尔判断，属于过渡拆分。 | 共享错误分类和状态码判断收敛到 `gateway/dispatch/upstream_errors.rs`，Chat/Responses 直接依赖共享分类模块。 |
| `gateway/dispatch/responses.rs` 中的 request option 和账号级 upstream 调用 | 模型后缀/default reasoning/service tier 应用属于 Codex Responses 请求语义；transport 映射、quota verify、按账号调用 Codex Responses/Compact 同时服务 Chat/Responses，不属于 Responses 路由主流程；单独 `dispatch/transport.rs` 只有一层映射，属于过渡拆分。 | 请求选项应用移到 `codex/protocol/responses.rs`；Codex 请求到 backend transport 的映射归入 `codex/transport/client.rs`；账号级 upstream 调用移到 `gateway/dispatch/upstream_requests.rs`，同时修正 Chat quota verify 错误文案使用常量值而不是常量名字符串。 |
| `gateway/openai/responses.rs` 中的 Codex SSE 收集和完成 metadata 提取 | 从上游 SSE 判断 `response.completed`/`response.failed`、提取 function call ids 和 replay items 是 Codex Responses 协议语义，dispatch 也需要这些信息做故障转移和亲和性记录。 | `CollectedResponse`、`ResponsesSseFailure`、`response_from_codex_sse`、`completed_response_metadata` 和 tuple schema 回转换下沉到 `codex/protocol/responses.rs`；OpenAI handler 只保留 HTTP DTO、handler、请求转换和 `response.failed` 输出帧编码。 |
| `app/tasks/token_refresh.rs` | 原来包含账号 refresh 扫描、定时器、租约、重试、状态转换等业务编排。 | 业务执行器下沉到 `accounts/token_refresh/runtime.rs`；`app/tasks/token_refresh.rs` 只保留 ticker、shutdown 和日志包装。 |
| `app/tasks/quota_refresh.rs` | 原来包含配额锁定账号扫描、usage 拉取、quota snapshot 持久化、请求错峰和最近刷新窗口。 | 业务执行器下沉到 `accounts/quota/runtime.rs`；`app/tasks/quota_refresh.rs` 只保留 ticker、shutdown 和日志包装。 |
| `accounts/admin_service/` | 当前拆分比文档原先的 `accounts/service.rs` 更清楚；管理端账号 DTO/错误/导入/quota/cookie/lifecycle 不应塞回单个文件。 | 文档补充 `accounts/admin_service/` 子目录。 |
| `gateway/dispatch/implicit_resume.rs` | 当前文件是 Responses 调度内部的独立规则，职责明确。 | 文档补充该文件。 |
| `gateway/openai/diagnostics.rs` | debug handler 属于 OpenAI/debug API 合约层，和 `telemetry/diagnostics.rs` 的聚合数据职责不同。 | 文档补充该文件。 |
| `tests/integration/**` | 文档原测试目录过粗，当前按模块、路由、子能力拆分更利于定位真实链路和边界场景。 | 文档测试目录按当前结构更新。 |

后续继续按这个方法处理：如果当前代码的拆分更符合模块职责，就改文档；如果当前代码只是旧迁移残留，就改代码。不要为了“对上目录”把合理拆分压回大文件，也不要保留过渡期别名。

### `app/services.rs` 收敛规则

`app/services.rs` 只能保留：

- `BackgroundTaskStores`
- `Services`
- 服务实例构造和接线 helper
- 启动时需要的少量组合根私有函数

`app/services.rs` 不允许新增：

- admin 业务服务实现
- gateway dispatch 业务规则
- telemetry 查询/写入服务
- accounts 状态机、导入导出、quota、OAuth、token refresh 规则
- 面向 handler 的业务错误类型和 DTO

除 `app` 自身和测试构造器外，业务模块不允许 import `crate::app::services::*`。如果需要共享服务类型，先把类型移动到真实归属模块。

## 最终仓库目录

迁移完成后的仓库保留下面这些顶层入口。Rust 后端是单 package；`web/` 仍是前端 SPA 工程，
但不再有 Rust workspace。

```text
Cargo.toml
Cargo.lock
rust-toolchain.toml
config.yaml
README.md
Dockerfile
docker-compose.yml

src/
tests/
web/
docs/
```

不要保留：

- `crates/`
- `crates/xtask`
- 只为绕开 workspace 依赖关系存在的 `core/adapters/runtime/server/platform/assets` 边界
- 全局 `repositories` 包
- 机械的 `ports` 包

## 最终源码目录

最终迁移完成后，Rust 源码只创建下面这些文件和目录。没有列出的旧目录不要迁入。

```text
src/
  lib.rs
  main.rs

  app/
    mod.rs
    bootstrap.rs
    state.rs
    services.rs
    shutdown.rs
    tasks/
      mod.rs
      coordinator.rs
      cookie_cleanup.rs
      fingerprint_update.rs
      model_refresh.rs
      quota_refresh.rs
      session_affinity_cleanup.rs
      session_cleanup.rs
      token_refresh.rs

  config/
    mod.rs
    loader.rs
    settings.rs
    types.rs
    writeback.rs

  infra/
    mod.rs
    crypto.rs
    database.rs
    identity.rs
    json.rs
    logging.rs
    paths.rs
    schema.sql

  http/
    mod.rs
    router.rs
    middleware/
      mod.rs
      request_id.rs
      trace.rs

  web/
    mod.rs
    assets.rs
    headers.rs

  access/
    mod.rs
    admin_session.rs
    client_keys.rs

  accounts/
    mod.rs
    model.rs
    pool.rs
    service.rs
    store.rs
    cookies.rs
    import_export.rs
    oauth.rs
    quota/
      mod.rs
      runtime.rs
    token_refresh/
      mod.rs
      runtime.rs
    admin_service/
      mod.rs
      lifecycle.rs
      importing.rs
      cookies.rs
      quota.rs
      types.rs

  codex/
    mod.rs
    fingerprint.rs
    models.rs
    oauth_client.rs
    protocol/
      mod.rs
      events.rs
      responses.rs
      schema.rs
      sse.rs
      websocket.rs
    transport/
      mod.rs
      client.rs
      endpoints.rs
      headers.rs
      tls.rs
      usage.rs
      websocket.rs
      websocket_pool.rs

  gateway/
    mod.rs
    auth.rs
    dispatch/
      mod.rs
      chat.rs
      cloudflare.rs
      responses.rs
      implicit_resume.rs
      reasoning_replay.rs
      session_affinity.rs
      upstream_errors.rs
      upstream_requests.rs
    openai/
      mod.rs
      chat.rs
      diagnostics.rs
      errors.rs
      models.rs
      responses.rs
      sse.rs

  telemetry/
    mod.rs
    diagnostics.rs
    events.rs
    event_store.rs
    usage.rs
    usage_store.rs

  admin/
    mod.rs
    router.rs
    response.rs
    session.rs
    api_keys.rs
    accounts.rs
    auth.rs
    diagnostics.rs
    logs.rs
    models.rs
    settings.rs
    usage.rs
```

## 源码文件职责清单

这一节是实现时的落点清单。每个文件只承担这里写明的职责；如果代码找不到对应位置，先回到
“业务能力地图”判断归属，而不是新增顶层模块。

### `app`

| 文件 | 迁入内容 |
| --- | --- |
| `app/mod.rs` | 只声明 `bootstrap/state/services/shutdown/tasks`。 |
| `app/bootstrap.rs` | 配置加载、路径解析、SQLite 初始化、fingerprint 初始化、router/server 启动入口。 |
| `app/state.rs` | `AppState`，持有配置和服务集合；测试构造器也放这里。 |
| `app/services.rs` | 构造 `access/accounts/codex/gateway/telemetry/admin` 需要的服务实例。 |
| `app/shutdown.rs` | signal、graceful shutdown、后台任务停止信号。 |
| `app/tasks/coordinator.rs` | 根据配置启动/停止所有后台任务。 |
| `app/tasks/cookie_cleanup.rs` | 调用 `accounts/cookies.rs` 清理过期 Cookie。 |
| `app/tasks/fingerprint_update.rs` | 调用 `codex/fingerprint.rs` 检查 appcast 并写入更新历史。 |
| `app/tasks/model_refresh.rs` | 调用 `codex/models.rs` 周期刷新模型目录。 |
| `app/tasks/quota_refresh.rs` | 启动 quota refresh ticker、处理 shutdown、调用 `accounts/quota/runtime.rs`。 |
| `app/tasks/session_affinity_cleanup.rs` | 调用 `gateway/dispatch/session_affinity.rs` 清理过期亲和性。 |
| `app/tasks/session_cleanup.rs` | 调用 `access/admin_session.rs` 清理过期管理员 session。 |
| `app/tasks/token_refresh.rs` | 启动 token refresh ticker、处理 shutdown、调用 `accounts/token_refresh/runtime.rs`。 |

### `config`、`infra`、`http`、`web`

| 文件 | 迁入内容 |
| --- | --- |
| `config/mod.rs` | 导出配置类型、加载和写回 API。 |
| `config/loader.rs` | `config.yaml` 配置加载。 |
| `config/settings.rs` | 管理端可变设置、设置 patch 校验、运行时设置读写服务。 |
| `config/types.rs` | `AppConfig` 及所有配置子结构。 |
| `config/writeback.rs` | `config.yaml` 序列化写回与写回错误。 |
| `infra/mod.rs` | 导出无业务语义的底层工具。 |
| `infra/crypto.rs` | `SecretBox`、hash、加解密错误。 |
| `infra/database.rs` | SQLite pool、schema 初始化、migration helper。 |
| `infra/identity.rs` | 管理员密码 hash/verify、client key hash/prefix 生成。 |
| `infra/json.rs` | 分页 cursor、通用 JSON helper。 |
| `infra/logging.rs` | tracing 初始化、日志轮转。 |
| `infra/paths.rs` | 数据目录、配置路径、运行时文件路径。 |
| `infra/schema.sql` | SQLite schema。 |
| `http/mod.rs` | 导出顶层 router 和 middleware。 |
| `http/router.rs` | merge `gateway`、`admin`、`web` 三组路由。 |
| `http/middleware/request_id.rs` | request id 生成/透传。 |
| `http/middleware/trace.rs` | HTTP tracing layer。 |
| `web/mod.rs` | 导出 SPA asset router。 |
| `web/assets.rs` | SPA fallback、静态文件服务。 |
| `web/headers.rs` | 静态资源缓存头策略。 |

### `access`

| 文件 | 迁入内容 |
| --- | --- |
| `access/mod.rs` | 导出 admin session、client key 服务和 store。 |
| `access/admin_session.rs` | 管理员登录校验、session 创建/验证/删除、`admin_users`/`admin_sessions` SQLite 操作、session 清理 API。 |
| `access/client_keys.rs` | client API key 创建、验证、启停、标签、导入导出、`client_api_keys` SQLite 操作。 |

### `accounts`

| 文件 | 迁入内容 |
| --- | --- |
| `accounts/mod.rs` | 导出账号模型、服务、账号池、OAuth、quota、cookie API。 |
| `accounts/model.rs` | `Account`、`AccountStatus`、账号 metadata、usage delta、状态转换 helper。 |
| `accounts/pool.rs` | 运行时账号池、选择策略、并发槽位、窗口用量、Cloudflare/rate-limit 状态。 |
| `accounts/service.rs` | 账号共享用例辅助，不承载 admin handler DTO。 |
| `accounts/store.rs` | `accounts` 表的 SQLite 读写、token 加解密、基础账号查询。 |
| `accounts/cookies.rs` | `account_cookies` 表、Cookie 导入/导出/清理、Cloudflare Cookie 状态。 |
| `accounts/import_export.rs` | sub2api/CLI/JSON 导入导出格式和校验。 |
| `accounts/oauth.rs` | PKCE、device code、callback、账号登录流程、logout 清理账号态。 |
| `accounts/quota/mod.rs` | quota snapshot 解析、quota 锁定/恢复 helper。 |
| `accounts/quota/runtime.rs` | 运行时配额锁定账号扫描、usage 拉取、quota snapshot 持久化、请求错峰和最近刷新窗口。 |
| `accounts/token_refresh/mod.rs` | JWT expiry、账号 claims 解码、refresh token 策略、上游 refresh 失败分类、lease 存储。 |
| `accounts/token_refresh/runtime.rs` | 运行时账号 refresh 扫描、账号级定时器、租约协调、重试、失败状态转换、下一次刷新时间。 |
| `accounts/admin_service/mod.rs` | 管理端账号业务服务组合、账号池同步、WebSocket 池驱逐。 |
| `accounts/admin_service/lifecycle.rs` | 账号列表、创建、更新、删除、状态切换、刷新、批量状态。 |
| `accounts/admin_service/importing.rs` | 管理端账号导入导出和导入后补充信息。 |
| `accounts/admin_service/cookies.rs` | 管理端账号 Cookie 读写入口。 |
| `accounts/admin_service/quota.rs` | 管理端 quota 查询、健康检查和 warning 聚合。 |
| `accounts/admin_service/types.rs` | 管理端账号 DTO、错误类型和转换辅助。 |

### `codex`

| 文件 | 迁入内容 |
| --- | --- |
| `codex/mod.rs` | 导出上游协议、transport、fingerprint、models、OAuth client。 |
| `codex/fingerprint.rs` | fingerprint 模型、SQLite repository、appcast update checker、installation id 相关逻辑。 |
| `codex/models.rs` | Codex 模型目录、模型别名、model snapshot store、模型刷新服务。 |
| `codex/oauth_client.rs` | OpenAI/Codex OAuth HTTP client 和 token refresh HTTP 调用。 |
| `codex/protocol/events.rs` | Codex usage、rate-limit header、上游事件解析。 |
| `codex/protocol/responses.rs` | Codex Responses request/response 结构、模型后缀/default reasoning/service tier 请求选项应用、Codex SSE 完成/失败收集、tuple schema 回转换、完成 metadata 提取、基于请求字段的上游传输决策。 |
| `codex/protocol/schema.rs` | Codex 协议共享 schema。 |
| `codex/protocol/sse.rs` | Codex SSE parse/encode、`[DONE]` 判断。 |
| `codex/protocol/websocket.rs` | Codex WebSocket 帧结构和解析。 |
| `codex/transport/client.rs` | CodexBackendClient、HTTP 请求、SSE 请求、错误类型。 |
| `codex/transport/endpoints.rs` | 上游 URL 和 endpoint 拼装。 |
| `codex/transport/headers.rs` | Codex Desktop 风格 headers、fingerprint headers、auth headers。 |
| `codex/transport/tls.rs` | rustls、自定义 CA、native roots。 |
| `codex/transport/usage.rs` | 上游 usage/quota endpoint HTTP 调用。 |
| `codex/transport/websocket.rs` | WebSocket connect/opening/request/response 流。 |
| `codex/transport/websocket_pool.rs` | WebSocket 连接池和复用策略。 |

### `gateway`

| 文件 | 迁入内容 |
| --- | --- |
| `gateway/mod.rs` | 导出 OpenAI 兼容层、鉴权接线、dispatch。 |
| `gateway/auth.rs` | 从 HTTP header 提取 client API key 并调用 `access` 校验。 |
| `gateway/openai/chat.rs` | Chat Completions DTO、handler、到 Codex Responses 的转换入口。 |
| `gateway/openai/diagnostics.rs` | `/debug/diagnostics`、`/debug/fingerprint`、`/debug/upstream` debug handler。 |
| `gateway/openai/errors.rs` | OpenAI 兼容错误响应、上游错误映射。 |
| `gateway/openai/models.rs` | `/v1/models*` DTO、handler、OpenAI 模型展示格式。 |
| `gateway/openai/responses.rs` | Responses DTO、handler、review/compact 变体入口、OpenAI `response.failed` SSE 输出帧编码。 |
| `gateway/openai/sse.rs` | OpenAI SSE 输出编码、stream 错误帧。 |
| `gateway/dispatch/chat.rs` | Chat Completions 调度服务。 |
| `gateway/dispatch/cloudflare.rs` | Cloudflare challenge/path-block 识别、Cookie 清理、账号 cooldown 和 path-block 禁用恢复。 |
| `gateway/dispatch/responses.rs` | Responses HTTP SSE/WebSocket 调度主流程、implicit resume。 |
| `gateway/dispatch/implicit_resume.rs` | implicit resume 条件判断、history strip 和 previous response 缺失恢复辅助。 |
| `gateway/dispatch/reasoning_replay.rs` | reasoning replay cache 和 replay 条件。 |
| `gateway/dispatch/session_affinity.rs` | conversation/session affinity 计算、存储、清理。 |
| `gateway/dispatch/upstream_errors.rs` | Chat/Responses 共享的上游错误分类、fallback 状态码判断、retry-after 冷却、transport 名称和错误体提取。 |
| `gateway/dispatch/upstream_requests.rs` | Chat/Responses 共享的 quota verify、账号级 Codex Responses/Compact 调用和单账号 5xx retry。 |

### `telemetry`

| 文件 | 迁入内容 |
| --- | --- |
| `telemetry/mod.rs` | 导出 events、usage、diagnostics。 |
| `telemetry/diagnostics.rs` | dashboard/debug diagnostics 聚合，读取 config、accounts、codex、telemetry 状态。 |
| `telemetry/events.rs` | 事件日志模型、写入服务、查询服务、日志开关和容量规则。 |
| `telemetry/event_store.rs` | `event_logs` SQLite 查询和写入。 |
| `telemetry/usage.rs` | token usage 标准化、账号用量聚合、dashboard summary/stats 查询服务。 |
| `telemetry/usage_store.rs` | `account_usage` dashboard 列表和聚合查询；运行态 usage delta、窗口同步和 reset 仍由账号池经 `accounts/store.rs` 写入。 |

### `admin`

| 文件 | 迁入内容 |
| --- | --- |
| `admin/mod.rs` | 导出 admin router 和各 handler。 |
| `admin/router.rs` | 所有 `/api/admin/**` 和 `/auth/callback` 路由。 |
| `admin/response.rs` | 统一 response envelope、request id、admin 错误响应。 |
| `admin/session.rs` | `/api/admin/login`、`/api/admin/logout` handler。 |
| `admin/api_keys.rs` | `/api/admin/api-keys*` handler、DTO。 |
| `admin/accounts.rs` | `/api/admin/accounts*` handler、DTO。 |
| `admin/auth.rs` | `/api/admin/auth/*`、`/auth/callback` handler、DTO。 |
| `admin/diagnostics.rs` | `/api/admin/diagnostics` handler。 |
| `admin/logs.rs` | `/api/admin/logs*` handler、DTO。 |
| `admin/models.rs` | `/api/admin/refresh-models` handler。 |
| `admin/settings.rs` | `/api/admin/settings` handler、DTO、patch 校验。 |
| `admin/usage.rs` | `/api/admin/usage-stats*` handler、DTO。 |

## 顶层模块职责

| 模块 | 职责 | 不放什么 |
| --- | --- | --- |
| `app` | 组合根、启动、状态、服务装配、后台任务生命周期。 | 协议转换、SQL 查询、HTTP DTO。 |
| `config` | 配置类型、配置加载、`config.yaml` 写回。 | 业务服务。 |
| `infra` | SQLite 连接/schema、加密、hash、日志初始化、分页/JSON、runtime 路径。 | 账号、协议、admin 语义。 |
| `http` | 顶层 Axum router 和通用 middleware。 | 具体 handler 业务。 |
| `web` | SPA fallback、静态资源缓存头。 | admin API。 |
| `access` | 管理员 session、client API key 的创建/验证/导入导出和 store。 | ChatGPT/Codex 账号。 |
| `accounts` | ChatGPT/Codex 账号、账号池、Cookie、导入导出、OAuth 管理流程、quota 状态、token refresh 规则。 | OpenAI 请求/响应 DTO。 |
| `codex` | Codex/ChatGPT 上游协议、transport、fingerprint、headers/TLS/WebSocket、模型目录、OAuth HTTP client。 | OpenAI 兼容错误格式。 |
| `gateway` | OpenAI 兼容 API、协议转换、client key 鉴权接线、请求调度、fallback/retry/reasoning replay/session affinity。 | admin 前端 DTO、SQLite schema 初始化。 |
| `telemetry` | 事件日志、用量统计、诊断数据聚合、dashboard 数据来源。 | 账号状态机、协议转换。 |
| `admin` | 管理端 HTTP/BFF 合约，调用其他模块服务支撑前端页面。 | 核心调度算法、上游 transport。 |

## 为什么这样拆

### `gateway` 是中转站核心入口

OpenAI 兼容接口、协议转换和调度是同一条用户请求链路。把它们放进 `gateway` 可以让一次
Responses 行为修改集中在：

```text
gateway/openai/responses.rs
gateway/dispatch/responses.rs
codex/protocol/responses.rs
codex/transport/*
```

而不是散到 `server`、`runtime`、`core`、`adapters` 多个包。

### `codex` 是上游适配核心

fingerprint、headers、TLS、自定义 CA、WebSocket opening、WebSocket pool、usage endpoint、
Codex SSE/WebSocket 协议都属于“怎么像 Codex Desktop 一样请求上游”。这些应聚合在 `codex`。

### `accounts` 不是 admin 的子功能

账号池是中转站运行核心，不只是前端管理页面。它同时被 gateway dispatch、token refresh、
quota refresh、admin 账号页面使用，所以独立成顶层模块。

### `telemetry` 合并 usage、events、diagnostics

用量统计、事件日志和诊断都服务前端 dashboard，也服务调度排错。它们可以共享查询和聚合逻辑，
不必拆成多个小顶层模块。

### `admin` 是前端 BFF/API 合约层

admin 模块保留所有 `/api/admin/**` 的 handler、DTO、响应 envelope、错误码映射。这样前端 API
变化只需要看一个地方。它内部调用 `access/accounts/codex/telemetry/config`。

## 旧代码到新模块映射

### `app`

| 当前位置 | 目标位置 |
| --- | --- |
| `crates/server/src/main.rs` | `src/main.rs`、`src/app/bootstrap.rs`、`src/app/shutdown.rs` |
| `crates/runtime/src/bootstrap.rs` | `src/app/bootstrap.rs` |
| `crates/runtime/src/state.rs` | `src/app/state.rs` |
| `crates/runtime/src/services.rs` | `src/app/services.rs` |
| `crates/runtime/src/repositories.rs` | 删除，按数据归属移动到各模块 store 初始化。 |
| `crates/runtime/src/tasks/*` | `src/app/tasks/*` |

### `config` 和 `infra`

| 当前位置 | 目标位置 |
| --- | --- |
| `crates/platform/src/config/loader.rs` | `src/config/loader.rs` |
| `crates/platform/src/config/types.rs` | `src/config/types.rs` |
| `crates/platform/src/config/mod.rs` 中写回错误 | `src/config/writeback.rs` |
| `crates/runtime/src/services/settings.rs`、`crates/core/src/admin/settings.rs` 中设置领域逻辑 | `src/config/settings.rs` |
| `crates/platform/src/crypto/hash.rs` | `src/infra/crypto.rs` |
| `crates/platform/src/crypto/secret_box.rs` | `src/infra/crypto.rs` |
| `crates/platform/src/identity/*` | `src/infra/identity.rs` |
| `crates/platform/src/json/*` | `src/infra/json.rs` |
| `crates/platform/src/logging/*` | `src/infra/logging.rs` |
| `crates/platform/src/storage/sqlite.rs` | `src/infra/database.rs` |
| `crates/platform/src/storage/paths.rs` | `src/infra/paths.rs` |
| `crates/platform/src/storage/schema.sql` | `src/infra/schema.sql` |

### `http` 和 `web`

| 当前位置 | 目标位置 |
| --- | --- |
| `crates/server/src/router.rs` | `src/http/router.rs` |
| `crates/server/src/middleware/*` | `src/http/middleware/*` |
| `crates/assets/src/headers.rs` | `src/web/headers.rs` |
| `crates/assets/src/router.rs` | `src/web/assets.rs` |

### `access`

| 当前位置 | 目标位置 |
| --- | --- |
| `crates/core/src/admin/auth.rs` | `src/access/admin_session.rs` |
| `crates/core/src/auth/session.rs` | `src/access/admin_session.rs` |
| `crates/adapters/src/sqlite/admin_sessions.rs` | `src/access/admin_session.rs` |
| `crates/core/src/admin/client_keys.rs` | `src/access/client_keys.rs` |
| `crates/core/src/admin/ports.rs` | 删除，只保留必要 trait 或 concrete store。 |
| `crates/adapters/src/sqlite/client_keys.rs` | `src/access/client_keys.rs` |
| `crates/runtime/src/services/admin_sessions.rs` | `src/access/admin_session.rs` |
| `crates/runtime/src/services/admin_client_keys.rs` | `src/access/client_keys.rs` |

### `accounts`

| 当前位置 | 目标位置 |
| --- | --- |
| `crates/core/src/accounts/model.rs` | `src/accounts/model.rs` |
| `crates/core/src/accounts/pool.rs` | `src/accounts/pool.rs` |
| `crates/core/src/accounts/lifecycle.rs` | `src/accounts/model.rs` 或 `src/accounts/service.rs` |
| `crates/core/src/accounts/jwt.rs` | `src/accounts/token_refresh/mod.rs` |
| `crates/core/src/accounts/cookies.rs` | `src/accounts/cookies.rs` |
| `crates/core/src/accounts/cloudflare.rs` | `src/accounts/cookies.rs` |
| `crates/core/src/accounts/usage.rs` | `src/accounts/model.rs` 或 `src/accounts/service.rs` 中的 usage delta；统计聚合逻辑放 `src/telemetry/usage.rs` |
| `crates/adapters/src/sqlite/accounts.rs` | `src/accounts/store.rs` |
| `crates/adapters/src/sqlite/account_tokens.rs` | `src/accounts/store.rs` |
| `crates/adapters/src/sqlite/cookies.rs` | `src/accounts/cookies.rs` |
| `crates/adapters/src/sqlite/refresh_leases.rs` | `src/accounts/token_refresh/mod.rs` |
| `crates/runtime/src/services/account_pool.rs` | `src/accounts/pool.rs` 和 `src/accounts/service.rs` |
| `crates/runtime/src/services/admin_accounts.rs` | `src/accounts/admin_service/*`，共享账号辅助保留在 `src/accounts/service.rs`、`src/accounts/import_export.rs`、`src/accounts/oauth.rs`、`src/accounts/quota/` |

### `codex`

| 当前位置 | 目标位置 |
| --- | --- |
| `crates/core/src/gateway/fingerprint.rs` | `src/codex/fingerprint.rs` |
| `crates/adapters/src/codex/fingerprint.rs` | `src/codex/fingerprint.rs` |
| `crates/core/src/gateway/installation.rs` | `src/codex/fingerprint.rs` 或 `src/infra/paths.rs`，按实现选择。 |
| `crates/core/src/gateway/conversation.rs` | `src/gateway/dispatch/session_affinity.rs` |
| `crates/core/src/gateway/ports.rs` | 删除，模型目录 client 直接用 concrete Codex transport。 |
| `crates/core/src/protocol/codex/*` | `src/codex/protocol/*` |
| `crates/adapters/src/codex/client.rs` | `src/codex/transport/client.rs`、`headers.rs`、`endpoints.rs`、`tls.rs`、`usage.rs` |
| `crates/adapters/src/codex/websocket/connect.rs` | `src/codex/transport/websocket.rs` |
| `crates/adapters/src/codex/websocket/opening.rs` | `src/codex/transport/websocket.rs` |
| `crates/adapters/src/codex/websocket/pool.rs` | `src/codex/transport/websocket_pool.rs` |
| `crates/core/src/auth/oauth.rs` 中 OAuth 类型 | `src/accounts/oauth.rs` 或 `src/codex/oauth_client.rs`，按“账号流程”和“HTTP client”拆。 |
| `crates/adapters/src/oauth/openai.rs` | `src/codex/oauth_client.rs` |
| `crates/core/src/models/*` | `src/codex/models.rs` |
| `crates/adapters/src/sqlite/models.rs` | `src/codex/models.rs` |
| `crates/runtime/src/services/admin_models.rs` | `src/codex/models.rs` |
| `crates/runtime/src/upstream.rs` | `src/codex/transport/client.rs`，模块出口由 `src/codex/transport/mod.rs` 统一声明。 |

### `gateway`

| 当前位置 | 目标位置 |
| --- | --- |
| `crates/core/src/protocol/openai/chat.rs` | `src/gateway/openai/chat.rs` |
| `crates/core/src/protocol/openai/responses.rs` | `src/gateway/openai/responses.rs` |
| `crates/core/src/protocol/openai/models.rs` | `src/gateway/openai/models.rs` |
| `crates/server/src/openai_api/auth.rs` | `src/gateway/auth.rs` |
| `crates/server/src/openai_api/error.rs` | `src/gateway/openai/errors.rs` |
| `crates/server/src/openai_api/sse.rs` | `src/gateway/openai/sse.rs` |
| `crates/server/src/openai_api/chat.rs` | `src/gateway/openai/chat.rs` |
| `crates/server/src/openai_api/responses.rs` | `src/gateway/openai/responses.rs` |
| `crates/server/src/openai_api/models.rs` | `src/gateway/openai/models.rs` |
| `crates/server/src/openai_api/diagnostics.rs` | 诊断聚合在 `src/telemetry/diagnostics.rs`，debug handler 在 `src/gateway/openai/diagnostics.rs`。 |
| `crates/server/src/openai_api/router.rs` | `src/gateway/openai/mod.rs` |
| `crates/core/src/serving/fallback.rs` | 状态码级 fallback 判断并入 `src/gateway/dispatch/upstream_errors.rs`。 |
| `crates/core/src/serving/recovery.rs` | Cloudflare 识别和账号恢复在 `src/gateway/dispatch/cloudflare.rs`，上游错误分类和通用 fallback 状态判断在 `src/gateway/dispatch/upstream_errors.rs`。 |
| `crates/core/src/serving/reasoning_replay.rs` | `src/gateway/dispatch/reasoning_replay.rs` |
| `crates/core/src/serving/implicit_resume.rs` | `src/gateway/dispatch/implicit_resume.rs` |
| `crates/core/src/serving/responses.rs` | 主流程在 `src/gateway/dispatch/responses.rs`，Codex 请求选项和 SSE 收集在 `src/codex/protocol/responses.rs`，backend transport 映射在 `src/codex/transport/client.rs`，账号级 upstream 调用在 `src/gateway/dispatch/upstream_requests.rs`。 |
| `crates/core/src/serving/affinity.rs` | `src/gateway/dispatch/session_affinity.rs` |
| `crates/adapters/src/sqlite/session_affinity.rs` | `src/gateway/dispatch/session_affinity.rs` |
| `crates/runtime/src/services/dispatch_chat.rs` | `src/gateway/dispatch/chat.rs` |
| `crates/runtime/src/services/dispatch_responses.rs` | `src/gateway/dispatch/responses.rs` |

### `telemetry`

| 当前位置 | 目标位置 |
| --- | --- |
| `crates/core/src/events/*` | `src/telemetry/events.rs` |
| `crates/adapters/src/sqlite/events.rs` | `src/telemetry/event_store.rs` |
| `crates/runtime/src/services/admin_logs.rs` | `src/telemetry/events.rs` |
| `crates/core/src/usage/*` | `src/telemetry/usage.rs` |
| `crates/adapters/src/sqlite/account_usage.rs` | `src/telemetry/usage_store.rs` |
| `crates/runtime/src/services/admin_usage.rs` | `src/telemetry/usage.rs` |
| `crates/server/src/openai_api/diagnostics.rs` | `src/telemetry/diagnostics.rs` |

### `admin`

| 当前位置 | 目标位置 |
| --- | --- |
| `crates/server/src/admin_api/response.rs` | `src/admin/response.rs` |
| `crates/server/src/admin_api/router.rs` | `src/admin/router.rs` |
| `crates/server/src/admin_api/session.rs` | `src/admin/session.rs` |
| `crates/server/src/admin_api/client_keys/*` | `src/admin/api_keys.rs` |
| `crates/server/src/admin_api/accounts/*` | `src/admin/accounts.rs`、`auth.rs` |
| `crates/server/src/admin_api/logs/*` | `src/admin/logs.rs` |
| `crates/server/src/admin_api/models.rs` | `src/admin/models.rs` |
| `crates/server/src/admin_api/settings.rs` | `src/admin/settings.rs` |
| `crates/server/src/admin_api/usage.rs` | `src/admin/usage.rs` |
| `crates/server/src/admin_api/diagnostics.rs` | `src/admin/diagnostics.rs` |
| `crates/core/src/admin/settings.rs` | `src/admin/settings.rs` 和 `src/config/writeback.rs` |
| `crates/runtime/src/services/settings.rs` | `src/admin/settings.rs` |

## 模块依赖规则

允许依赖：

```text
app -> all modules
http -> admin, gateway, web
admin -> access, accounts, codex, telemetry, config
gateway -> access, accounts, codex, telemetry
accounts -> codex/oauth_client, telemetry, infra, config
codex -> infra, config
telemetry -> infra, config
access -> infra, config
web -> infra
config -> external crates
infra -> external crates
```

禁止依赖：

- `codex` 不能依赖 `gateway` 或 `admin`。
- `accounts` 不能依赖 `admin`。
- `telemetry` 不能依赖 `admin`。
- `infra` 不能依赖任何业务模块。
- `config` 不能依赖任何业务模块。
- `gateway/openai` 不能直接写 SQLite，只调用 service/store 抽象或具体 service。
- `admin` handler 不直接拼上游请求，只调用 `accounts/codex/telemetry/access` 的服务。

## Cargo 目标

根 `Cargo.toml` 删除 workspace 配置，改为单包。

```toml
[package]
name = "codex-proxy-rs"
version = "0.1.0"
edition = "2021"
license = "MIT"
publish = false
rust-version = "1.95"
description = "AI protocol gateway backed by ChatGPT/Codex accounts."

[dependencies]
aes-gcm = "0.10.3"
argon2 = "0.5.3"
async-trait = "0.1.89"
axum = "0.8.9"
base64 = "0.22.1"
bytes = "1.11.1"
chrono = { version = "0.4.45", features = ["serde", "clock"] }
config = "0.15.23"
dirs = "5.0.1"
futures = "0.3.32"
hex = "0.4.3"
hmac = "0.13.0"
indexmap = { version = "2.0", features = ["serde"] }
rand = "0.10.1"
reqwest = { version = "=0.12.28", default-features = false, features = ["json", "stream", "cookies", "rustls-tls-native-roots", "gzip", "brotli", "zstd", "deflate", "http2"] }
rustls = { version = "=0.23.36", default-features = false, features = ["ring", "std", "tls12"] }
rustls-native-certs = "0.8.4"
rustls-pki-types = "1.14.1"
secrecy = "0.10.3"
serde = { version = "1.0.228", features = ["derive"] }
serde_json = "1.0.150"
serde_yml = "0.0.13"
sha2 = "0.11.0"
sqlx = { version = "0.9.0", features = ["runtime-tokio", "tls-rustls", "sqlite", "chrono", "uuid", "migrate"] }
thiserror = "2.0.18"
tokio = { version = "1.52.3", features = ["macros", "rt-multi-thread", "signal", "fs", "time", "sync", "net", "test-util"] }
tokio-rustls = { version = "0.26.4", default-features = false, features = ["ring", "tls12"] }
tokio-tungstenite = { version = "0.28.0", features = ["proxy", "rustls-tls-native-roots"] }
tower-http = { version = "0.6.11", features = ["trace", "cors", "request-id", "timeout"] }
tracing = "0.1.44"
tracing-appender = "0.2.5"
tracing-subscriber = { version = "0.3.23", features = ["env-filter", "json"] }
tungstenite = { version = "0.27.0", features = ["deflate", "proxy"] }
uuid = { version = "1.23.3", features = ["v4", "v7", "serde"] }
zeroize = "1.8.2"

[dev-dependencies]
tempfile = "3.27.0"
tower = { version = "0.5.3", features = ["util"] }
wiremock = "0.6.5"

[patch.crates-io]
tokio-tungstenite = { git = "https://github.com/openai-oss-forks/tokio-tungstenite", rev = "132f5b39c862e3a970f731d709608b3e6276d5f6" }
tungstenite = { git = "https://github.com/openai-oss-forks/tungstenite-rs", rev = "9200079d3b54a1ff51072e24d81fd354f085156f" }

[lints.rust]
future_incompatible = { level = "warn", priority = -1 }
nonstandard_style = { level = "deny", priority = -1 }
unsafe_code = "forbid"

[lints.clippy]
all = { level = "deny", priority = 10 }
redundant_clone = { level = "deny", priority = 9 }
large_enum_variant = { level = "warn", priority = 8 }
needless_collect = { level = "deny", priority = 7 }

[profile.dev]
debug = true

[profile.release]
debug = true
```

不迁入当前 workspace 中未使用的 `anyhow`。`tower` 只放 `dev-dependencies`。

## `lib.rs` 和 `main.rs`

`src/lib.rs`：

```rust
pub mod access;
pub mod accounts;
pub mod admin;
pub mod app;
pub mod codex;
pub mod config;
pub mod gateway;
pub mod http;
pub mod infra;
pub mod telemetry;
pub mod web;
```

`src/main.rs`：

```rust
use std::error::Error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    codex_proxy_rs::app::bootstrap::run().await
}
```

`main.rs` 只进入组合根。

## 测试目录

测试按最终模块组织，不按旧 crate 组织。`tests/` 顶层不放 `access.rs`、`admin.rs` 这类散落
入口文件，只保留一个 integration test crate 目录。

```text
tests/
  integration/
    main.rs
    config.rs

    support/
      mod.rs
      config.rs
      sqlite.rs

    fixtures/
      chat/
        success.sse
      responses/
        golden/
          reasoning_replay_request.json
        http_sse/
          after_401.sse
          after_402.sse
          after_403.sse
          after_5xx_retry.sse
          after_cloudflare.sse
          after_model_unsupported.sse
          completed_image_usage.sse
          completed_usage.sse
          done_item_completed.sse
          empty_completed.sse
          failed_auth.sse
          failed_model_unsupported.sse
          failed_quota.sse
          stream_after_401.sse
          stream_after_402.sse
          stream_after_403.sse
          stream_after_429.sse
          stream_after_5xx_retry.sse
          stream_after_cloudflare.sse
          stream_after_model_unsupported.sse
          stream_usage.sse
          success.sse
          text_deltas_completed.sse
          tuple_object.sse
        websocket/
          completed_with_reasoning_replay.json
          first_account_limited.json
          history_rate_limited.json
          invalid_encrypted_content.json
          previous_response_not_found.json
          rate_limited.json
          second_account_limited.json
          token_revoked.json
          unanswered_function_call.json

    access/
      mod.rs
      admin_password.rs
      admin_session_store.rs
      admin_session.rs
      auth.rs
      client_key_auth.rs
      client_key_store.rs
      client_keys.rs
      client_keys/
        authorization.rs
        import_export.rs
        lifecycle.rs

    accounts/
      mod.rs
      account_pool.rs
      account_pool/
        quota.rs
        selection.rs
        usage_window.rs
      account_repository.rs
      cloudflare.rs
      cookies.rs
      oauth.rs
      quota_refresh.rs
      refresh_leases.rs
      quota.rs
      token_refresh.rs
      token_refresh_failures.rs
      token_refresh_scheduling.rs
      token_refresh_success.rs

    codex/
      mod.rs
      fingerprint.rs
      fingerprint_integration.rs
      models.rs
      models_core.rs
      protocol.rs
      protocol/
        protocol_codex_websocket.rs
        protocol_openai_chat.rs
        protocol_openai_responses.rs
        protocol_usage_rate_limits.rs
      headers.rs
      http_client.rs
      upstream.rs
      websocket.rs
      websocket_pool.rs

    gateway/
      mod.rs
      chat_upstream.rs
      chat_upstream/
        chat_routes.rs
        compact_routes.rs
        responses_http.rs
        responses_recovery.rs
        responses_websocket.rs
        usage_logging.rs
      diagnostics_routes.rs
      openai_models.rs
      responses_routes.rs
      session_affinity.rs
      session_affinity_integration.rs

    telemetry/
      mod.rs
      events.rs

    admin/
      mod.rs
      accounts_routes.rs
      accounts_routes/
        import_export.rs
        lifecycle.rs
        list.rs
        oauth.rs
        quota.rs
      api_contract.rs
      api_keys_routes.rs
      api_keys_routes/
        authorization.rs
        import_export.rs
        lifecycle.rs
      logs_routes.rs
      models_routes.rs
      session_routes.rs
      settings.rs
      settings_routes.rs

    app/
      mod.rs
      account_pool_restore.rs
      tasks.rs
      tasks/
        cleanup.rs
        coordinator.rs
        fingerprint.rs
        model_refresh.rs

    http/
      mod.rs
      trace_middleware.rs
      web_assets.rs

    infra/
      mod.rs
      crypto.rs
      log_rotation.rs
      storage_schema.rs
```

测试迁移规则：

1. `tests/integration/main.rs` 是 Cargo integration test 的唯一入口，只声明模块，不写测试逻辑。
2. 重复的 `test_config`、SQLite seed、HTTP JSON helper、mock upstream helper 放入 `tests/integration/support`。
3. 每个业务目录用自己的 `mod.rs` 声明子测试文件，例如 `tests/integration/gateway/mod.rs`。
4. `include_str!("../fixtures/...")` 或 `include_str!("fixtures/...")` 按实际文件位置调整，fixtures 统一放 `tests/integration/fixtures`。
5. 所有 `codex_proxy_core::*`、`codex_proxy_runtime::*`、`codex_proxy_server::*` 等 import 改为 `codex_proxy_rs::*`。

## 迁移顺序

1. 改根 `Cargo.toml` 为单包。
2. 建 `config`、`infra`，迁配置、SQLite、schema、crypto、identity、json、logging、paths。
3. 建 `access`，迁管理员 session 和 client API key。
4. 建 `accounts`，迁账号模型、账号池、store、cookie、import/export、quota、token refresh。
5. 建 `codex/protocol`，迁 Codex 协议纯逻辑。
6. 建 `codex/transport`、`codex/fingerprint`，迁上游链路、headers、TLS、WebSocket、fingerprint。
7. 建 `codex/models` 和 `codex/oauth_client`。
8. 建 `telemetry`，迁 event logs、usage stats、diagnostics。
9. 建 `gateway/openai` 和 `gateway/dispatch`，迁 OpenAI 兼容层、协议转换、请求调度、fallback/retry。
10. 建 `admin`，迁所有 `/api/admin/**` handler、DTO、response envelope。
11. 建 `web`、`http`，迁 SPA fallback、顶层 router、middleware。
12. 建 `app`，迁启动、状态、服务装配、后台任务。
13. 迁测试到新测试目录。
14. 删除 `crates/`。
15. 更新 README、Dockerfile、历史命令文档。

每一步至少运行：

```bash
cargo fmt --check
cargo check --all-targets
```

最终运行完整验收命令。

## README 和 Dockerfile

README 开发命令：

```bash
cargo run
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets
```

Dockerfile：

```dockerfile
FROM rust:1.95-bookworm AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
COPY web/dist ./web/dist
RUN cargo build --release --locked

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/codex-proxy-rs /usr/local/bin/codex-proxy-rs
COPY config.yaml ./config.yaml

EXPOSE 8080

CMD ["codex-proxy-rs"]
```

`crates/xtask` 不保留，本次迁移目标也不创建 `scripts/`。以后如果确实需要发布自动化，再以独立
变更增加脚本，不要为了脚本重新引入 workspace helper package。

## 验收标准

最终迁移必须满足：

1. 根 `Cargo.toml` 没有 `[workspace]`。
2. 仓库没有 `crates/`。
3. `src/` 顶层只包含本文档列出的模块。
4. `rg "codex_proxy_(core|platform|adapters|runtime|server|assets)" src tests` 无结果。
5. `cargo fmt --check` 通过。
6. `cargo clippy --all-targets --all-features --locked -- -D warnings` 通过。
7. `cargo test --all-targets` 通过。
8. `cargo run` 能初始化 SQLite、管理员账号、runtime fingerprint、后台任务。
9. 路由级测试覆盖 `/v1/models`、`/v1/responses`、`/v1/chat/completions`。
10. 路由级测试覆盖 `/api/admin/settings`、`/api/admin/accounts`、`/api/admin/usage-stats/summary`、`/api/admin/logs`。
11. SPA fallback 和静态资源缓存头有测试。

## 以后如何判断代码放哪里

| 需求 | 目标位置 |
| --- | --- |
| 改管理员登录、session、client API key | `access`，对应 admin API 在 `admin` |
| 改 ChatGPT/Codex 账号状态、账号池、导入导出、Cookie、quota、token refresh | `accounts` |
| 改 Codex fingerprint、headers、TLS、自定义 CA、WebSocket opening/pool、usage endpoint | `codex` |
| 改 OpenAI Chat/Responses/Models 兼容格式 | `gateway/openai` |
| 改 dispatch、fallback、retry、reasoning replay、session affinity | `gateway/dispatch` |
| 改事件日志、用量统计、dashboard 数据、诊断聚合 | `telemetry` |
| 改前端管理接口 DTO、错误码、路由合约 | `admin` |
| 改配置字段或 `config.yaml` 写回 | `config`，必要时改对应业务模块 |
| 改 SQLite 连接/schema、密钥、日志初始化、路径 | `infra` |
| 改顶层 middleware、路由 merge、CORS、request id | `http` |
| 改 SPA fallback、静态资源缓存 | `web` |
| 改启动顺序、服务装配、后台任务生命周期 | `app` |

如果一个普通需求需要同时修改五六个顶层模块，优先检查模块归属是否错了。合理单体的目标是
改动半径稳定，不是文件越少越好，也不是目录越多越好。
