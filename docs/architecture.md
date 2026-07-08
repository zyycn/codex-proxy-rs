# Architecture —— 后端架构权威文档

本文档是 Codex Proxy RS 的**唯一**架构权威文档：分层模型、依赖规则、目录与文件命名规范、模块约束、现状审计结论、旧→新完整映射与分阶段执行方案。

- 数据库 schema、口径与迁移见 [database.md](database.md)；本文不重复。
- 基线：2026-07-08，`backend/src` 共 160 个 `.rs` 文件、约 41k 行。
- 本文的目标结构与 database.md 的 v4 改造**共用同一个改造窗口**：遥测域（usage/ops/buckets）的文件本来就要因 schema v4 重写，搬家与重写一次完成，不做两遍。

---

## 1. 系统总览

单进程：Rust/Axum 同时承载 OpenAI Responses 兼容代理、管理端 API、SQLite 持久化、后台任务、静态 SPA 托管与在线更新。Vue 3 管理端由同一进程托管。

```text
客户端 SDK ──/v1/*──┐                       ┌── ChatGPT / Codex backend
                    ├─ api ─ dispatch ─ upstream
浏览器 ──/api/admin─┘    │        │
                         │     accounts / models
                         │        │
                    telemetry / keys / auth / settings / update
                                  │
                               infra（SQLite / 日志 / 时间）
```

核心原则（不变）：

1. 启动配置只含进程启动必需项；其余全部在 SQLite 热更新。
2. 代理热路径只依赖内存态服务；持久化写入在请求完成、后台任务或设置更新时发生。
3. 上游请求统一经过账号池、session affinity、quota、fingerprint 与 transport。

---

## 2. 分层模型与依赖规则（权威）

### 2.1 层定义

| 层 | 模块 | 职责 | 禁止 |
| --- | --- | --- | --- |
| L0 | `infra` | SQLite 连接/迁移、时间、JSON、日志、身份生成、路径、格式化 | 出现任何业务概念 |
| L1 | `upstream` | 纯上游客户端：协议编解码、HTTP/SSE/WebSocket 传输、指纹、OAuth token client。**每家 provider 一个子目录**（`openai/`，规划中 `cloudflare/`） | 依赖任何领域模块；出现账号池/调度/遥测概念 |
| L1 | `telemetry` | 遥测域：usage_records / ops_error_logs / request_time_buckets / account_usage 的**写入与查询**、billing、dashboard 聚合、rebuild | 被冠以 admin 语义；HTTP handler |
| L1 | `keys` | 客户端 API key 域：store、内存缓存、鉴权校验、管理操作 | — |
| L1 | `auth` | 管理端认证域：admin 用户、会话（token_hash） | 客户端 key 概念（那是 `keys`） |
| L1 | `settings` | 运行时设置域：持久化、快照、变更广播（watch）、admin API key 校验 | 直接调用上层服务推送变更（§2.3） |
| L1 | `update` | 自更新域：release 查询、下载、校验、解压、替换、状态 | — |
| L2 | `accounts` | 账号域：实体、池、调度器、store、quota、token 刷新、Cookie、导入导出、管理操作 | — |
| L2 | `models` | 模型目录域：catalog、别名、plan 快照 | — |
| L3 | `dispatch` | 请求编排域：账号获取、affinity、上游调用、流转发、失败恢复、重试、遥测记录 | HTTP 类型（axum）出现在签名 |
| L4 | `api` | 全部入站 HTTP：`api/client`（/v1）、`api/admin`（/api/admin）、路由组合、中间件、SPA 资源 | 写 SQL；承载领域逻辑 |
| L5 | `bootstrap` | 进程装配：启动配置、服务构造、AppState、后台任务、关闭 | 被任何层依赖 |

### 2.2 依赖方向

只允许**高层 → 低层**。同层依赖默认禁止，例外必须登记在下表（新增例外 = 改本文档 + code review）：

| 允许的同层/特殊依赖 | 理由 |
| --- | --- |
| `accounts` → `models` | 候选过滤需要模型允许列表；单向 |
| `accounts` → `settings` | 池参数/刷新策略订阅设置快照（watch 类型）；单向 |
| `auth` → `settings` | admin API key 哈希存放在 runtime_settings 表，经 settings 的 store 方法读取；单向 |

**全库硬禁**（CI 脚本断言，§9）：

- L1–L3 任何模块 import `crate::api` / `crate::bootstrap`。
- `dispatch`、`accounts`、`telemetry` 等领域模块 import `axum`。
- `api` 下出现 `sqlx`（SQL 只允许在各域的 `store` 文件里）。
- `upstream` import `crate::{accounts, dispatch, telemetry, keys, auth, settings, update, models}`。
- 任何模块 import 旧路径 `crate::admin` / `crate::proxy` / `crate::runtime` / `crate::web` / `crate::http` / `crate::config`（改造完成后这些模块不存在）。

### 2.3 控制反转两处（消除向上依赖）

1. **设置传播**：现状 `RuntimeSettingsService` 持有账号池与刷新策略的引用、主动推送更新（低层持有高层引用）。终态：`settings` 只暴露 `tokio::sync::watch::Receiver<SettingsSnapshot>`；`accounts`（池、刷新策略）、`models`（别名）各自订阅。装配在 `bootstrap` 完成。
2. **遥测写入**：现状 proxy 热路径 import `admin::monitoring` 的 Admin* 服务。终态：`dispatch` 依赖 `telemetry::Recorder`（L3 → L1，方向正确），admin 侧只是同一域的查询消费者。

---

## 3. 目标目录树（终态，权威）

```text
backend/src/
├── main.rs
├── lib.rs
├── infra/                        # L0
│   ├── mod.rs
│   ├── database.rs               # 连接、pragma、迁移框架（含 Rust post 钩子，database.md §6.1）
│   ├── migrations/               # *.sql
│   ├── identity.rs  time.rs  json.rs  format.rs  logging.rs  paths.rs
├── upstream/                     # L1 纯上游客户端（每家 provider 一个子目录）
│   ├── mod.rs                    # 第二家 provider 出现时，UpstreamClient trait 落此
│   └── openai/
│       ├── mod.rs
│       ├── token_client.rs
│       ├── protocol/
│       │   ├── mod.rs  schema.rs  responses.rs  events.rs  sse.rs  websocket.rs
│       ├── transport/
│       │   ├── mod.rs  client.rs  endpoints.rs  headers.rs  tls.rs
│       │   ├── response_meta.rs  usage.rs  diagnostics.rs
│       │   ├── websocket.rs  websocket_pool.rs  websocket_pump.rs
│       └── fingerprint/
│           ├── mod.rs  types.rs  store.rs  runtime.rs  updater.rs
├── telemetry/                    # L1 遥测域（原 admin/monitoring + proxy/dispatch/usage_events）
│   ├── mod.rs
│   ├── recorder.rs               # 唯一写入口：成功/失败两条事务（database.md §5.2）
│   ├── billing.rs
│   ├── rebuild.rs                # rebuild-derived（database.md §7.6）
│   ├── dashboard.rs              # Dashboard 聚合查询（无 HTTP）
│   ├── usage/                    # usage_records
│   │   ├── mod.rs  types.rs  store.rs  query.rs
│   ├── ops/                      # ops_error_logs
│   │   ├── mod.rs  types.rs  store.rs  query.rs
│   ├── buckets/                  # request_time_buckets
│   │   ├── mod.rs  store.rs  query.rs
│   └── account_usage/            # account_usage / account_model_usage
│       ├── mod.rs  store.rs  query.rs
├── keys/                         # L1 客户端 key 域（原 admin/keys/service.rs 拆分）
│   ├── mod.rs  types.rs  store.rs  cache.rs  service.rs  manage.rs
├── auth/                         # L1 管理端认证域（原 admin/auth/service.rs）
│   ├── mod.rs  types.rs  store.rs  service.rs
├── settings/                     # L1 运行时设置域（原 config/settings.rs）
│   ├── mod.rs  types.rs  store.rs  service.rs   # service 持 watch 广播
├── update/                       # L1 自更新域（原 admin/system/*）
│   ├── mod.rs  types.rs  state.rs  release.rs  download.rs  archive.rs  service.rs
├── accounts/                     # L2 账号域（原 upstream/accounts + upstream/scheduler + admin/accounts/service）
│   ├── mod.rs
│   ├── account.rs                # 实体（原 model.rs）
│   ├── window.rs
│   ├── pool.rs                   # 池：存储态/槽位/过滤（§5.4 行数预算内需拆出 pool/ 子文件）
│   ├── scheduler.rs              # 调度门面（原 scheduler/mod.rs 的类型）
│   ├── scheduler/
│   │   ├── candidates.rs  feedback.rs
│   │   └── strategy/
│   │       ├── mod.rs  smart.rs  quota_reset.rs  round_robin.rs  sticky.rs
│   ├── store.rs                  # AccountStore trait + Sqlite 门面
│   ├── store/
│   │   ├── queries.rs  rows.rs
│   ├── quota/
│   │   ├── mod.rs  service.rs  runtime.rs
│   ├── refresh/                  # 原 token_refresh
│   │   ├── mod.rs  policy.rs  lease.rs  service.rs
│   ├── cookies/
│   │   ├── mod.rs  store.rs
│   ├── import.rs                 # 导入 payload 解析（原 upstream/accounts/importing.rs）
│   └── manage/                   # 管理操作（原 admin/accounts/service/*）
│       ├── mod.rs  types.rs  service.rs  lifecycle.rs
│       ├── import.rs  export.rs  oauth.rs  probe.rs  quota.rs  quota_view.rs
├── models/                       # L2 模型目录域
│   ├── mod.rs  types.rs  catalog.rs  service.rs  store.rs
├── dispatch/                     # L3 请求编排域（原 proxy/dispatch）
│   ├── mod.rs
│   ├── service.rs                # 编排入口（原 responses/service.rs，按 §5.4 拆分）
│   ├── upstream_call.rs          # 原 dispatch/upstream.rs
│   ├── errors.rs                 # 原 dispatch/errors.rs + responses/errors.rs 合并
│   ├── recording.rs              # 原 responses/event_recording.rs，调 telemetry::recorder
│   ├── affinity/                 # 原 session_affinity.rs（683 行三合一）+ responses/affinity.rs
│   │   ├── mod.rs  types.rs  store.rs  service.rs  resolve.rs
│   ├── recovery/
│   │   ├── mod.rs  auth.rs  cloudflare.rs  exhaustion.rs
│   │   ├── implicit_resume.rs  reasoning_replay.rs
│   └── stream/
│       ├── mod.rs  live.rs  lifecycle.rs  prefetch.rs  sse_failure.rs  trace.rs
├── api/                          # L4 全部入站 HTTP（原 http + web + proxy 入口 + admin routes）
│   ├── mod.rs
│   ├── router.rs                 # 总路由 + healthz（经 infra 的 db ping，不穿透到具体 store 的 pool）
│   ├── assets.rs                 # SPA 静态资源（原 web/assets.rs）
│   ├── middleware/
│   │   ├── mod.rs  request_id.rs  trace.rs
│   ├── client/                   # /v1（原 proxy/auth + proxy/openai）
│   │   ├── mod.rs  router.rs  auth.rs
│   │   ├── responses.rs  models.rs  errors.rs  sse.rs
│   └── admin/                    # /api/admin（原 admin 的全部 routes 与 DTO）
│       ├── mod.rs  router.rs  response.rs  session.rs      # session = 鉴权提取器/中间件
│       ├── auth_routes.rs  settings_routes.rs  system_routes.rs
│       ├── accounts_routes.rs  keys_routes.rs
│       ├── usage_routes.rs  dashboard_routes.rs
└── bootstrap/                    # L5 进程装配（原 runtime + config/schema,loader）
    ├── mod.rs
    ├── config.rs                 # 启动配置 schema + loader（原 config/schema.rs + loader.rs）
    ├── services.rs  state.rs  shutdown.rs
    └── tasks/
        ├── mod.rs  coordinator.rs  periodic.rs  cleanup.rs
        ├── cookie_cleanup.rs  session_cleanup.rs  session_affinity_cleanup.rs
        ├── model_refresh.rs  token_refresh.rs  quota_refresh.rs
        ├── fingerprint_update.rs  retention_trim.rs   # retention_trim 新增（database.md §7.7）
```

`backend/tests/` 目录结构**镜像 `src/`**：模块搬家时测试目录同步搬家，`fixtures/`、`support/` 不动。

---

## 4. 命名规范

### 4.1 目录名

1. snake_case，全小写，禁止连字符与点。
2. **实体集合域用复数**（`accounts`、`models`、`keys`、`buckets`）；**能力/抽象域用单数**（`dispatch`、`telemetry`、`auth`、`settings`、`update`、`upstream`、`api`、`infra`、`bootstrap`）。
3. 子目录表达子能力（`scheduler/strategy/`），不表达文件类型——禁止 `services/`、`stores/`、`utils/`、`helpers/`、`common/`、`misc/` 这类"按技术角色装桶"的目录。

### 4.2 文件名

1. snake_case。**主类型 = 文件名的 PascalCase**（`account.rs` ↔ `Account`；`scheduler.rs` ↔ `AccountScheduler` 属可接受的域前缀加长）。
2. **角色后缀词表**（一个文件一个角色）：

| 文件名 | 角色 | 约束 |
| --- | --- | --- |
| `router.rs` | 路由组合 | 只 merge/nest，不写 handler |
| `*_routes.rs` / `routes.rs` | HTTP handler + 请求/响应 DTO | 不写 SQL、不含领域规则 |
| `service.rs` | 领域服务（该域对外门面） | 不含 SQL 文本 |
| `store.rs` | SQLite 持久化 | SQL 只允许出现在这里；类型名 `Sqlite*Store` |
| `cache.rs` | 内存缓存/运行时快照 | 类型名 `Runtime*` |
| `query.rs` | 复杂只读查询（列表/聚合/分页） | 与写路径分文件 |
| `types.rs` | 该域跨文件共享的实体/值类型 | 无 IO |
| `rows.rs` | DB 行结构（store 私有） | `pub(crate)` 以内 |
| `errors.rs` | 该域错误类型 | — |
| `mod.rs` | 声明 + re-export | **≤ 50 行，零逻辑** |

3. **禁止动名词文件名**：`importing.rs` → `import.rs`、`exporting.rs` → `export.rs`、`testing.rs` → `probe.rs`（"testing" 会被误读为测试代码，永久禁用）。
4. 禁止 `util.rs` / `helper.rs` / `common.rs` / `misc.rs`——放不进角色词表的代码说明职责没想清楚。
5. 缩写白名单：`sse`、`tls`、`ws`（仅类型名内）、`db`、`id`、`json`、`oauth`。其余单词写全。
6. 一个概念一个词，全库同义同名：持久化一律 **Store**（废除 Repository——现 `FingerprintRepository` 改 `FingerprintStore`）；行类型一律 rows；DTO 一律在 routes/types，废除 `*_model.rs` 与 `contracts.rs` 两种叫法。

### 4.3 类型名

| 前缀/形态 | 保留给 | 示例 |
| --- | --- | --- |
| `Sqlite*Store` | store 实现 | `SqliteAccountStore` |
| `Runtime*` | 内存缓存/快照服务 | `RuntimeClientKeyCache` |
| `Admin*` | **仅** `api/admin` 下的 DTO/提取器 | `AdminAccountPayload` |
| 无前缀 | 领域服务与实体 | `Recorder`、`AccountScheduler`、`SettingsService` |

由此更名（现状 → 终态）：`AdminUsageRecordService` → `telemetry::usage::UsageQueryService`（读）+ `telemetry::Recorder`（写）；`AdminOpsErrorLogService` → `telemetry::ops` 同拆；`AdminUsageService`（实际管 account_usage）→ `telemetry::account_usage::AccountUsageQueryService`；`AdminSessionService` → `auth::SessionService`；`AdminClientKeyService` → `keys::KeyManageService`；`ClientKeyService` → `keys::KeyVerifier`。**类型名与文件名对不上的历史包袱全部在搬家时清算，不留别名 re-export。**

禁止无意义别名导入：`use std::sync::Arc as StdArc` 这类一律 `use std::sync::Arc`。

---

## 5. 模块约束

### 5.1 mod.rs 纪律

`mod.rs` 只做 `pub mod` 声明与 `pub use` 门面导出，≤ 50 行。现状违规（`accounts/store/mod.rs` 1126 行、`token_refresh/mod.rs` 495 行、`quota/mod.rs` 419 行、`updater/mod.rs` 811 行、`scheduler/mod.rs` 含门面类型）全部在搬家时把实现移入具名文件。

### 5.2 SQL 边界

SQL 文本只允许出现在 `store.rs` / `store/` / `query.rs`。`api` 层与 `service.rs` 出现 `sqlx::query` 即为违规（现状 `http/router.rs` 的 healthz 直接拿某个 store 的连接池执行 SQL——终态 `infra::database` 提供 `ping(pool)`，router 调它）。

### 5.3 HTTP 边界

axum/tower 类型只允许出现在 `api` 与 `bootstrap`。领域函数签名出现 `HeaderMap`、`StatusCode` 等 HTTP 类型即为违规（用域内枚举/结构表达，api 层转换）。

### 5.4 文件行数预算

- 软上限 **400 行**（超过即在 review 中说明理由）；硬上限 **800 行**（超过必须拆，CI 警告，§9）。
- 现状超硬线清单（11 个，全部有既定拆法）：

| 文件（现路径） | 行数 | 拆法 |
| --- | --- | --- |
| `proxy/dispatch/responses/service.rs` | 1913 | → `dispatch/service.rs`（编排骨架）+ `upstream_call.rs` + `stream/*` + `recovery/*` 已分走的逻辑归位 |
| `upstream/transport/websocket.rs` | 1470 | → `websocket.rs`（连接/握手）+ `websocket_frames.rs`（帧编解码） |
| `admin/accounts/routes.rs` | 1455 | → `api/admin/accounts_routes.rs` 按子资源拆（accounts / oauth / quota / import-export） |
| `upstream/accounts/pool.rs` | 1420 | → `accounts/pool.rs`（门面）+ `pool/state.rs`（槽位/在途）+ `pool/filters.rs` |
| `upstream/transport/client.rs` | 1251 | → `client.rs` + `client_sse.rs`（SSE 请求路径） |
| `upstream/accounts/store/mod.rs` | 1126 | → `store.rs` 门面 + `store/queries.rs` 按读写再分 |
| `admin/monitoring/usage_record_store.rs` | 1114 | v4 重写时天然拆为 `telemetry/usage/{store,query}.rs` |
| `admin/monitoring/dashboard.rs` | 1080 | → `telemetry/dashboard.rs`（聚合）+ `api/admin/dashboard_routes.rs`（HTTP） |
| `upstream/protocol/websocket.rs` | 1033 | → `websocket.rs`（事件转换）+ `websocket_errors.rs`（错误帧解析） |
| `upstream/accounts/token_refresh/runtime.rs` | 953 | → `accounts/refresh/{service,lease,policy}.rs` |
| `admin/system/updater/mod.rs` | 811 | → `update/{service,state}.rs` |

### 5.5 一域一账本

每张数据库表恰好属于一个域，只有该域的 store 可以写它：

| 表（database.md v4） | 属主域 |
| --- | --- |
| usage_records / ops_error_logs / request_time_buckets / account_usage / account_model_usage | `telemetry` |
| accounts / account_refresh_leases / account_cookies | `accounts` |
| model_plan_snapshots | `models` |
| client_api_keys | `keys` |
| admin_users / admin_sessions | `auth` |
| runtime_settings | `settings` |
| session_affinities | `dispatch`（affinity/store.rs） |
| fingerprints / fingerprint_update_history | `upstream`（fingerprint/store.rs） |
| schema_migrations | `infra` |

跨域读通过属主域的 query/service 方法，不直接跨域写 SQL。

---

## 6. 现状审计结论（改造动机存档）

### 6.1 分层违规（P1，本次改造的核心动机）

1. **热路径反向依赖管理端**：`proxy/dispatch/usage_events.rs`、`event_recording.rs` import `admin::monitoring` 的 `Admin*Service` 写账本；`runtime/services.rs` 用 Admin 命名的服务装配核心链路。遥测是领域层，管理端只是它的一个读者。→ 建 `telemetry` 域（PR1）。
2. **核心编排寄居 proxy**：`proxy/dispatch/`（约 5600 行，含 1913 行 service）是系统心脏，却挂在客户端 HTTP 入口下；`SqliteSessionAffinityStore`（DB store）住在 proxy 里。→ 建 `dispatch` 域（PR4）。
3. **config 模块越权**：`config/settings.rs`（601 行）= 持久化 + 热更新推送 + admin API key 校验，还持有账号池引用向上推送。config 应只剩启动 schema + loader。→ 建 `settings` 域 + watch 反转（PR2）。
4. **admin/keys 四合一**：`admin/keys/service.rs`（698 行）同时装着 proxy 鉴权用的 `ClientKeyService`、运行时缓存、SQLite store、管理端 service——客户端鉴权路径穿过 admin 模块。→ 建 `keys` 域（PR2）。
5. **web/http 假分家**：`web/` 只有 69 行 assets，是路由的 fallback 组成部分。→ 并入 `api`（PR5）。
6. healthz 穿透三层拿 store 连接池执行裸 SQL（§5.2）。

### 6.2 命名不规范（P2）

1. 持久化四种形态并存：目录 `store/`、后缀 `_store.rs`、藏在功能文件（`cookies.rs`、`session_affinity.rs`）、`Repository`（fingerprint）。
2. 行/DTO 三种叫法：`rows.rs` / `*_model.rs` / `contracts.rs`。
3. 动名词文件：`importing.rs` ×2、`exporting.rs`、`testing.rs`（后者必然被误读为测试）。
4. 类型名与文件名脱节：`account_usage_service.rs` 里是 `AdminUsageService`；`SqliteUsageStore` 存的是 account_usage；"usage" 一词横跨三个概念。
5. `model.rs`（账号实体）与 `models/`（LLM 目录域）一词双义——实体文件改 `account.rs` 后消解。
6. `admin/system/routes.rs` 是 5 行 re-export 假门面；`admin/response.rs`、`admin/update_payload.rs`、`upstream/token_client.rs`、`upstream/fingerprint.rs` 游离在模块根。
7. `Arc as StdArc` 式无意义别名。

### 6.3 模块肥大（P2）

超 800 行文件 11 个（§5.4 已列）；巨型 `mod.rs` 5 个（§5.1）。

### 6.4 仓库级（P3）

1. `backend/build/build.rs`：Cargo 惯例是包根 `build.rs`；非标准位置徒增一行配置与一次困惑。→ 移回 `backend/build.rs`。
2. 根目录 `server-pulls.runtime/`：点分隔命名不合仓库其余惯例（gitignored 运行产物）。→ 更名 `.server-pulls/` 或移出仓库根。
3. `frontend/src` 结构合格（api/components/views/stores 分层清晰），仅一条规范：`utils/` 内文件必须按 §4.2 角色词表命名，禁止继续堆积泛用文件。

**结论**：模块内聚质量整体不差（scheduler 的文档注释、tests 镜像结构都是亮点），债务集中在**域归属错位**与**命名不一致**，属于"搬家 + 更名"能解决的范畴，不需要重写逻辑。

---

## 7. 旧 → 新映射总表（可执行）

按目录粒度；`†` = 搬家同时拆分/更名，见 §5.4 与 §4.2。

| 现路径 | 终态路径 |
| --- | --- |
| `config/schema.rs` + `config/loader.rs` | `bootstrap/config.rs` |
| `config/settings.rs` | `settings/{types,store,service}.rs` † |
| `runtime/{bootstrap,services,state,shutdown}.rs` | `bootstrap/` 同名 |
| `runtime/tasks/*` | `bootstrap/tasks/*`（新增 `retention_trim.rs`） |
| `http/router.rs` | `api/router.rs` |
| `http/middleware/*` | `api/middleware/*` |
| `web/assets.rs` | `api/assets.rs` |
| `proxy/{auth,router}.rs`、`proxy/openai/*` | `api/client/*`（`routes.rs` → `router.rs` 与各资源文件） |
| `proxy/dispatch/usage_events.rs` | `telemetry/recorder.rs` †（Admin* 类型消失） |
| `proxy/dispatch/session_affinity.rs` | `dispatch/affinity/{types,store,service}.rs` † |
| `proxy/dispatch/responses/affinity.rs` | `dispatch/affinity/resolve.rs` |
| `proxy/dispatch/responses/service.rs` | `dispatch/service.rs` † |
| `proxy/dispatch/responses/{live_stream,stream_lifecycle,prefetch,sse_failure,trace}.rs` | `dispatch/stream/{live,lifecycle,prefetch,sse_failure,trace}.rs` |
| `proxy/dispatch/responses/event_recording.rs` | `dispatch/recording.rs` |
| `proxy/dispatch/responses/errors.rs` + `proxy/dispatch/errors.rs` | `dispatch/errors.rs` † |
| `proxy/dispatch/{auth_recovery,cloudflare,exhaustion,implicit_resume,reasoning_replay}.rs` | `dispatch/recovery/{auth,cloudflare,exhaustion,implicit_resume,reasoning_replay}.rs` |
| `proxy/dispatch/upstream.rs` | `dispatch/upstream_call.rs` |
| `admin/monitoring/usage_record_{model,store,service}.rs` | `telemetry/usage/{types,store,query}.rs` †（v4 重写同窗口） |
| `admin/monitoring/ops_error_{model,store,service}.rs` | `telemetry/ops/{types,store,query}.rs` † |
| `admin/monitoring/account_usage_{store,service}.rs` | `telemetry/account_usage/{store,query}.rs` † |
| `admin/monitoring/billing.rs` | `telemetry/billing.rs` |
| `admin/monitoring/dashboard.rs` | `telemetry/dashboard.rs` + `api/admin/dashboard_routes.rs` † |
| `admin/monitoring/diagnostics.rs` | `telemetry/diagnostics.rs` |
| `admin/monitoring/usage_record_routes.rs` | `api/admin/usage_routes.rs` |
| `admin/keys/service.rs` | `keys/{types,store,cache,service,manage}.rs` † |
| `admin/keys/routes.rs` | `api/admin/keys_routes.rs` |
| `admin/auth/service.rs` | `auth/{types,store,service}.rs` † |
| `admin/auth/session.rs` | `api/admin/session.rs` |
| `admin/accounts/routes.rs` | `api/admin/accounts_routes.rs` †（按子资源拆） |
| `admin/accounts/quota_view.rs` | `accounts/manage/quota_view.rs` |
| `admin/accounts/service/*` | `accounts/manage/*`（`importing`→`import`、`exporting`→`export`、`testing`→`probe`、`contracts`→`types`） |
| `admin/{response,update_payload}.rs` | `api/admin/response.rs`、`update/types.rs` |
| `admin/router.rs` | `api/admin/router.rs` |
| `admin/settings/routes.rs` | `api/admin/settings_routes.rs` |
| `admin/system/{routes,state}.rs`、`admin/system/updater/*` | `api/admin/system_routes.rs`、`update/{state,service,release,download,archive}.rs` † |
| `upstream/accounts/model.rs` | `accounts/account.rs` |
| `upstream/accounts/{pool,window}.rs` | `accounts/{pool,window}.rs`（pool 拆 †） |
| `upstream/accounts/store/*` | `accounts/store.rs` + `accounts/store/{queries,rows}.rs` † |
| `upstream/accounts/quota/*` | `accounts/quota/{service,runtime}.rs` † |
| `upstream/accounts/token_refresh/*` | `accounts/refresh/{policy,lease,service}.rs` † |
| `upstream/accounts/cookies.rs` | `accounts/cookies/store.rs` |
| `upstream/accounts/importing.rs` | `accounts/import.rs` |
| `upstream/accounts/service.rs` | 并入 `accounts/mod.rs` 门面导出（32 行） |
| `upstream/scheduler/*` | `accounts/scheduler.rs` + `accounts/scheduler/*`（mod.rs 门面类型移出） |
| `upstream/models/*` | `models/{types,catalog,service,store}.rs`（`backend_entry`+`info`+`snapshot`+`config` 并入 `types.rs`/`service.rs`） |
| `upstream/fingerprint.rs` | `upstream/openai/fingerprint/{types,store,runtime,updater}.rs` †（Repository→Store） |
| `upstream/{protocol,transport}/*`、`upstream/token_client.rs` | `upstream/openai/` 下同名路径（websocket/client 按 §5.4 拆） |
| `infra/*` | 原地保留 |
| `backend/build/build.rs` | `backend/build.rs` |

---

## 8. 执行计划（六个顺序 PR，每个独立编译绿、测试绿）

每个 PR 的固定验收：`cargo fmt --check`、`cargo clippy -- -D warnings`、`cargo test`、`backend/scripts/check_architecture.sh`（§9，随 PR 逐步启用对应规则）、`backend/tests` 镜像目录同步搬移。**搬家 PR 原则上零行为变更**（更名与移动，不改逻辑）；唯一例外是 PR1 与 database.md v4 的联动重写。

| PR | 内容 | 关键动作 |
| --- | --- | --- |
| **PR1** | `telemetry` 域成立（与 database.md 0004 + §7 代码改造同窗口） | monitoring 全部迁出 admin；`usage_events.rs` → `recorder.rs`；Admin* 遥测类型按 §4.3 更名；dispatch/api 改依赖 telemetry；启用"禁止 `crate::admin::monitoring`"检查 |
| **PR2** | `keys` / `auth` / `settings` / `update` 域成立 | 四个域从 admin/config 拆出；settings 改 watch 广播（§2.3）；admin API key / session token 哈希化（database.md）落在此处；config 只剩 schema+loader |
| **PR3** | `accounts` / `models` 域成立 | `upstream/accounts`、`upstream/scheduler`、`admin/accounts/service` 三处合并为 `accounts`；`upstream` 缩为纯客户端并落成 `openai/` 子目录；动名词文件更名；Repository→Store |
| **PR4** | `dispatch` 域成立 | proxy/dispatch 整体迁出并按目标树重组；affinity store 独立成文件；`proxy` 只剩 api/client 的料 |
| **PR5** | `api` / `bootstrap` 壳成立 | http + web + proxy 入口 + admin routes 合并为 `api`；runtime + config 装配部分改名 `bootstrap`；删除 `crate::{admin,proxy,runtime,web,http,config}` 六个旧根模块；启用全部依赖检查 |
| **PR6** | 行数预算清账 | §5.4 清单内 11 个文件拆到硬线以下；启用行数预算 CI 警告 |

顺序依据：PR1 与数据库改造互锁必须最先；PR2–4 自底向上消除旧模块的存在必要；PR5 收壳；PR6 纯内部拆分无跨域影响。

---

## 9. CI 强制

新增 `backend/scripts/check_architecture.sh`，进 `.github/workflows/ci.yml`（在 clippy 之后）：

```bash
#!/usr/bin/env bash
# 架构依赖与命名断言。违规即失败。
set -euo pipefail
cd "$(dirname "$0")/.."
fail=0
deny() { # deny <描述> <grep-pattern> <路径...>
  if grep -rn --include='*.rs' -E "$2" "${@:3}" ; then
    echo "ARCH VIOLATION: $1" >&2; fail=1
  fi
}

# 分层：领域层不得依赖 api / bootstrap
deny "domain -> api/bootstrap" 'crate::(api|bootstrap)\b' \
  src/infra src/upstream src/telemetry src/keys src/auth src/settings src/update \
  src/accounts src/models src/dispatch
# upstream 是纯客户端
deny "upstream -> domain" 'crate::(accounts|dispatch|telemetry|keys|auth|settings|update|models)\b' src/upstream
# HTTP 类型只在 api / bootstrap
deny "axum outside api" '\baxum(::|\b)' \
  src/infra src/upstream src/telemetry src/keys src/auth src/settings src/update \
  src/accounts src/models src/dispatch
# SQL 只在 store / query
deny "sqlx outside store/query" 'sqlx::query' \
  $(find src/api -name '*.rs')
# 旧模块根不得复活
deny "legacy module path" 'crate::(admin|proxy|runtime|web|http|config)::' src
# 禁用文件名
banned=$(find src -name 'importing.rs' -o -name 'exporting.rs' -o -name 'testing.rs' \
  -o -name 'util.rs' -o -name 'helper.rs' -o -name 'common.rs' -o -name 'misc.rs')
[ -n "$banned" ] && { echo "ARCH VIOLATION: banned file names: $banned" >&2; fail=1; }
# mod.rs 纪律
while read -r f; do
  lines=$(wc -l < "$f")
  [ "$lines" -gt 50 ] && { echo "ARCH VIOLATION: $f has $lines lines (mod.rs <= 50)" >&2; fail=1; }
done < <(find src -name mod.rs)
# 行数硬上限（警告期：仅报告；PR6 后改 fail）
find src -name '*.rs' ! -name mod.rs | while read -r f; do
  lines=$(wc -l < "$f")
  [ "$lines" -gt 800 ] && echo "WARN: $f has $lines lines (> 800)"
done
exit $fail
```

规则随 PR 分阶段启用（PR1 起启用 legacy-path 的 monitoring 子集，PR5 后全量）；`deny` 的路径参数在过渡期只指向已完成搬家的目录。

---

## 10. 仓库级约定

- `backend/tests/` 镜像 `src/`，不在 `src/` 写测试模块。
- `docs/` 只保留权威文档：`architecture.md`（本文）、`database.md`。行为细节（调度策略、WS 池、更新流程等）以模块文档注释为准（`scheduler/mod.rs` 是范本），不再在 docs 里维护第二份会腐化的副本。
- `deploy/`、`release/`、`skills/` 现状合格，不动。
- 前端改动仅一处联动：keys 列表不再返回完整 key（database.md §7.8）。

---

## 11. 扩展路径：第二上游 provider（已规划：Cloudflare Workers AI）

分层已为多 provider 留好隔离区，接入时**不新增层、不动依赖规则**，动作清单：

| 位置 | 动作 |
| --- | --- |
| `upstream/` | 新增 `upstream/cloudflare/`：client（按账号拼 `accounts/{account_id}/ai/v1` base URL）+ protocol 适配器（**Responses ↔ chat/completions 双向转换**，接入的主要工作量）。无 fingerprint、无 token_client（静态 API key，无刷新）。`upstream/mod.rs` 引入 `UpstreamClient` trait，`dispatch` 从具体 `CodexBackendClient` 改依赖 trait——在只有一家实现时不预建该 trait（投机抽象），第二家动工即引入 |
| `models` | catalog 收录 `@cf/...` 命名空间；别名映射升级为"客户端名 → (provider, 上游名)"；**models 域是"模型 → provider"的路由枢纽**，dispatch 据此选池 |
| `accounts` | `scheduler/candidates` 加 provider 硬过滤（单池 + 过滤，不按 provider 拆池）；`manage/{oauth,import}` 按 provider 分实现（CF 导入格式 = `(account_id, api_token)`）；refresh/租约对 CF 账号整体跳过（`refresh_token` NULL 语义已覆盖） |
| `dispatch` | `affinity` / `recovery` 按 provider 策略分发：CF 无会话语义 → 跳过 affinity；恢复只剩朴素 429/5xx 重试 |
| `telemetry` | 无结构改动——provider 归因维度已随 schema v4 就位（database.md §4.10–4.12）；billing 单价查找加 provider 参数 |
| 不动 | `api/client`（入站永远是 OpenAI 兼容协议）、`keys`、`auth`、`settings`、`update`、`infra` |

DB 侧一次性迁移（accounts 加 provider、身份列更名、快照表 PK 加维度）见 database.md §8。动工当天需实测验证三点：`cfk_` 老式 authkey 对 OpenAI 兼容端点的鉴权方式（`X-Auth-Email`/`X-Auth-Key` 还是 Bearer）、GLM 类模型是否支持上游 Responses 端点（决定适配器能否对部分模型直通）、SSE 分块与 OpenAI delta 事件的逐字段兼容性。
