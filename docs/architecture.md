# Architecture —— 后端架构权威文档

本文档是 Codex Proxy RS 的**唯一**架构权威文档：分层模型、依赖规则、目录与文件命名规范、模块约束、现状审计结论、旧→新完整映射与分阶段执行方案。

- 存储 schema、Redis 键契约、口径与搬迁见 [database.md](database.md)（2026-07-09 起为 **PostgreSQL + Redis 终态**）；本文不重复。
- 基线：2026-07-08，`backend/src` 共 160 个 `.rs` 文件、约 41k 行。
- 存储终态切换（SQLite → PG + Redis，含 v4 语义）触及**所有** store 文件，无法按域切片——它作为 **PR0** 先行完成（§8）；此后本文的搬家 PR 全部回到"零行为变更"原则。原先"遥测域与 schema v4 同窗口重写"的约定由 PR0 吸收。

---

## 1. 系统总览

单二进制：Rust/Axum 同时承载 OpenAI Responses 兼容代理、管理端 API、后台任务、静态 SPA 托管与在线更新。Vue 3 管理端由同一进程托管。持久数据在 PostgreSQL，运行态与缓存在 Redis（分界与丢失语义见 database.md §1）；**单实例部署，不考虑多实例**（database.md §8"明确不做"），选择外置存储是运维形态决策——原生 TTL、在线备份、容器内无本地数据文件。

```text
客户端 SDK ──/v1/*──┐                       ┌── ChatGPT / Codex backend
                    ├─ api ─ dispatch ─ upstream
浏览器 ──/api/admin─┘    │        │
                         │     accounts / models
                         │        │
                    telemetry / keys / auth / settings / update
                                  │
                     infra（PostgreSQL / Redis / 日志 / 时间）
```

核心原则（第 2 条随存储终态修订，其余不变）：

1. 启动配置只含进程启动必需项；其余全部在 PostgreSQL（`runtime_settings`）热更新。
2. 代理热路径的存储访问只有两类：PG unique 索引点查（client key 鉴权）与 Redis 运行态读写（亲和、租约，亚毫秒）；事实与聚合的 PG 写入发生在请求完成时，重查询只存在于管理端。
3. 上游请求统一经过账号池、session affinity、quota、fingerprint 与 transport。

---

## 2. 分层模型与依赖规则（权威）

### 2.1 层定义

| 层 | 模块 | 职责 | 禁止 |
| --- | --- | --- | --- |
| L0 | `infra` | PostgreSQL 连接/迁移、Redis 连接/键前缀、时间、JSON、日志、身份生成、路径、格式化 | 出现任何业务概念 |
| L1 | `upstream` | 纯上游客户端：协议编解码、HTTP/SSE/WebSocket 传输、指纹、OAuth token client。**每家 provider 一个子目录**（`openai/`，规划中 `cloudflare/`） | 依赖任何领域模块；出现账号池/调度/遥测概念 |
| L1 | `telemetry` | 遥测域：usage_records / ops_error_logs / request_time_buckets / account_usage 的**写入与查询**、billing、rebuild | 被冠以 admin 语义；HTTP handler |
| L1 | `keys` | 客户端 API key 域：store、鉴权校验、管理操作 | — |
| L1 | `auth` | 管理端认证域：admin 用户（PG）、会话（Redis，token_hash 键，database.md §4B.1） | 客户端 key 概念（那是 `keys`） |
| L1 | `settings` | 运行时设置域：校验、持久化、并发串行化的当前快照、admin API key 校验 | 持有账号池、模型或刷新服务引用（§2.3） |
| L1 | `update` | 自更新域：release 查询、下载、校验、解压、替换、状态 | — |
| L2 | `accounts` | 账号域：实体、池、调度器、store、quota、token 刷新、Cookie、导入导出、管理操作 | — |
| L2 | `models` | 模型目录域：catalog、别名、plan 快照 | — |
| L3 | `dispatch` | 请求编排域：账号获取、affinity、上游调用、流转发、失败恢复、重试、遥测记录 | HTTP 类型（axum）出现在签名 |
| L4 | `api` | 全部入站 HTTP：`api/client`（/v1）、`api/admin`（/api/admin）、路由组合、中间件、SPA 资源 | 写 SQL；承载领域逻辑 |
| 装配层 | `bootstrap` | 进程装配：启动配置、服务构造、AppState、后台任务、关闭；`api` 只读取其 AppState 门面 | 被 L0–L3 领域层依赖 |

### 2.2 依赖方向

只允许**高层 → 低层**。同层依赖默认禁止，例外必须登记在下表（新增例外 = 改本文档 + code review）：

| 允许的同层/特殊依赖 | 理由 |
| --- | --- |
| `accounts` → `models` | 候选过滤需要模型允许列表；单向 |

**全库硬禁**（CI 脚本断言，§9）：

- L1–L3 任何模块 import `crate::api` / `crate::bootstrap`。
- `dispatch`、`accounts`、`telemetry` 等领域模块 import `axum`。
- `api` 下出现 `sqlx` 或 `redis`（SQL 与 Redis 命令都只允许在各域的 `store` 文件里，§5.2）。
- `upstream` import `crate::{accounts, dispatch, telemetry, keys, auth, settings, update, models}`。
- 任何模块 import 旧路径 `crate::admin` / `crate::proxy` / `crate::runtime` / `crate::web` / `crate::http` / `crate::config`（改造完成后这些模块不存在）。

### 2.3 控制反转两处（消除向上依赖）

1. **设置传播**：`RuntimeSettingsService` 只负责校验、持久化和当前快照，不持有任何上层消费者。`bootstrap::Services::update_settings` 在持久化成功后同步把同一快照应用到账号池、刷新策略与模型配置；更新串行执行，不保留无人消费的 watch 通道，也不存在低层向上回调。
2. **遥测写入**：现状 proxy 热路径 import `admin::monitoring` 的 Admin* 服务。终态：`dispatch` 依赖 `telemetry::Recorder`（L3 → L1，方向正确），admin 侧只是同一域的查询消费者。

---

## 3. 目标目录树（终态，权威）

```text
backend/src/
├── main.rs  lib.rs
├── infra/                        # database.rs、redis.rs、migrations/ 与通用基础能力
├── upstream/openai/              # 纯 OpenAI/Codex client
│   ├── token_client.rs
│   ├── protocol/                 # schema / responses / events / SSE / WebSocket
│   ├── transport/                # HTTP、TLS、WebSocket 与连接池；大文件可按职责建子文件
│   └── fingerprint/              # types / store / runtime / updater
├── telemetry/                    # 四类账本与查询
│   ├── recorder.rs               # 成功/失败事实的唯一领域写入口
│   ├── billing.rs  diagnostics.rs  rebuild.rs
│   ├── usage/                    # types.rs、query.rs、store.rs、store/query.rs
│   ├── ops/                      # types.rs、query.rs、store.rs
│   ├── buckets/                  # query.rs、store.rs
│   └── account_usage/            # query.rs、store.rs
├── keys/                         # types / PG store / verifier / manage
├── auth/                         # types / PG+Redis store / session service
├── settings/                     # types / PG store / serialized current-snapshot service
├── update/                       # types / state / release / download / archive / service
├── accounts/
│   ├── account.rs  service.rs  import.rs  window.rs
│   ├── pool.rs  pool/{runtime,types}.rs
│   ├── scheduler.rs  scheduler/{candidates,feedback,strategy/...}.rs
│   ├── store.rs  store/{queries,rows,write}.rs
│   ├── quota/{service,runtime}.rs
│   ├── refresh/{lease,service,runtime}.rs  refresh/runtime/types.rs
│   ├── cookies/store.rs
│   └── manage/{service,types,lifecycle,import,export,oauth,probe,quota,quota_view}.rs
├── models/                       # config / info / snapshot / catalog / service / Redis store
├── dispatch/
│   ├── errors.rs  upstream.rs  cloudflare.rs  exhaustion.rs
│   ├── implicit_resume.rs  reasoning_replay.rs
│   ├── affinity/{types,identity,store,service,resolve}.rs
│   └── responses/                # Responses 编排是一个完整子能力，不做无收益扁平化
│       ├── service.rs  service/{complete,stream,compact}.rs
│       ├── errors.rs  event_recording.rs  live_stream.rs
│       └── prefetch.rs  sse_failure.rs  stream_lifecycle.rs  trace.rs
├── api/
│   ├── router.rs  assets.rs  middleware/
│   ├── client/                   # /v1 路由、鉴权与协议响应
│   └── admin/                    # /api/admin handlers 与 DTO
│       ├── accounts_routes.rs  accounts_routes/{actions,stats}.rs
│       ├── dashboard_routes.rs  dashboard_routes/data.rs
│       └── keys/ops/settings/system/usage 等 routes
└── bootstrap/
    ├── application.rs  config.rs  services.rs  state.rs  shutdown.rs
    ├── import_sqlite.rs  import_sqlite/{core,telemetry}.rs
    └── tasks/                    # periodic、retention、token/quota/model refresh 等装配任务
```

这是已落地结构，不是待办蓝图。为控制单文件行数而增加的同名子目录属于同一能力，不构成抽象层。Dashboard 的跨域只读组合保留在 `api/admin/dashboard_routes/data.rs`：它只组装 presentation read model，不写 SQL、不改变领域状态；把它下沉到 `telemetry` 反而会制造 telemetry → accounts 的向上依赖。

`backend/tests/` 顶层按 `src/` 领域镜像；子目录可按公开路由或行为场景组织，因此 `monitoring`、`token_refresh` 等历史行为名称可保留在测试中。`fixtures/`、`support/` 是测试基础设施，不要求与生产文件逐一同名。

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
| `service.rs` | 领域服务（该域对外门面） | 不含 SQL 文本与 Redis 命令 |
| `store.rs` | 持久化/运行态存取（PG SQL 或 Redis 命令） | SQL 与 Redis 命令只允许出现在这里；类型名 `Pg*Store` / `Redis*Store` |
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
| `Pg*Store` | PostgreSQL store 实现 | `PgAccountStore` |
| `Redis*Store` | Redis store 实现 | `RedisSessionAffinityStore` |
| `Runtime*` | 内存缓存/快照服务 | `RuntimeAccountPoolService`、`RuntimeFingerprint` |
| `Admin*` | **仅** `api/admin` 下的 DTO/提取器 | `AdminAccountPayload` |
| 无前缀 | 领域服务与实体 | `Recorder`、`AccountScheduler`、`SettingsService` |

由此更名（现状 → 终态）：`Sqlite*Store` 全系 → `Pg*Store` 或 `Redis*Store`（按 database.md 的归属，引擎切换 PR0 完成）；`AdminUsageRecordService` → `telemetry::usage::UsageQueryService`（读）+ `telemetry::Recorder`（写）；`AdminOpsErrorLogService` → `telemetry::ops` 同拆；`AdminUsageService`（实际管 account_usage）→ `telemetry::account_usage::AccountUsageQueryService`；`AdminSessionService` → `auth::SessionService`；`AdminClientKeyService` → `keys::KeyManageService`；`ClientKeyService` → `keys::KeyVerifier`（完整 key → PG unique 点查，旧 `RuntimeClientKeyStore` 内存鉴权表消亡）。**类型名与文件名对不上的历史包袱全部在搬家时清算，不留别名 re-export。**

禁止无意义别名导入：`use std::sync::Arc as StdArc` 这类一律 `use std::sync::Arc`。

---

## 5. 模块约束

### 5.1 mod.rs 纪律

`mod.rs` 只做 `pub mod` 声明与 `pub use` 门面导出，≤ 50 行。现状违规（`accounts/store/mod.rs` 1126 行、`token_refresh/mod.rs` 495 行、`quota/mod.rs` 419 行、`updater/mod.rs` 811 行、`scheduler/mod.rs` 含门面类型）全部在搬家时把实现移入具名文件。

### 5.2 存储边界（SQL 与 Redis 同规）

业务 SQL 与 Redis 命令只允许出现在 `store.rs` / `store/` / `query.rs`。明确的基础设施/维护例外只有：`infra/database.rs`（迁移与 ping）、`infra/redis.rs`（连接与 ping）、`bootstrap/import_sqlite*`（一次性跨库导入）、`telemetry/rebuild.rs`（账本重建命令）、`accounts/refresh/lease.rs`（Redis 租约适配器）。`api` 层与领域 `service.rs` 出现存储命令即为违规；healthz 只调用 infra 的 ping 门面。

### 5.3 HTTP 边界

axum/tower 类型只允许出现在 `api` 与 `bootstrap`。领域函数签名出现 `HeaderMap`、`StatusCode` 等 HTTP 类型即为违规（用域内枚举/结构表达，api 层转换）。

### 5.4 文件行数预算

- 软上限 **400 行**（超过即在 review 中说明理由）；硬上限 **800 行**（超过必须拆，CI 直接阻断，§9）。
- 迁移前超硬线清单（11 个）及最终落点：

| 文件（现路径） | 行数 | 拆法 |
| --- | --- | --- |
| `proxy/dispatch/responses/service.rs` | 1913 | → `dispatch/responses/service.rs` + `service/{complete,stream,compact}.rs` |
| `upstream/transport/websocket.rs` | 1470 | → `upstream/openai/transport/websocket.rs` + `websocket/{stream,types}.rs` + `websocket_pump.rs` |
| `admin/accounts/routes.rs` | 1455 | → `api/admin/accounts_routes.rs` + `accounts_routes/{actions,stats}.rs`；领域操作进入 `accounts/manage` |
| `upstream/accounts/pool.rs` | 1420 | → `accounts/pool.rs` + `pool/{runtime,types}.rs` |
| `upstream/transport/client.rs` | 1251 | → `upstream/openai/transport/client.rs` + `client/requests.rs` |
| `upstream/accounts/store/mod.rs` | 1126 | → `accounts/store.rs` + `store/{queries,rows,write}.rs` |
| `admin/monitoring/usage_record_store.rs` | 1114 | → `telemetry/usage/store.rs` + `store/query.rs`；查询门面独立为 `usage/query.rs` |
| `admin/monitoring/dashboard.rs` | 1080 | → 账本查询进入 `telemetry`，跨域 presentation 组合进入 `api/admin/dashboard_routes/{data}.rs` |
| `upstream/protocol/websocket.rs` | 1033 | → `upstream/openai/protocol/websocket.rs` + `websocket/validation.rs` |
| `upstream/accounts/token_refresh/runtime.rs` | 953 | → `accounts/refresh/{service,lease,runtime}.rs` + `runtime/types.rs` |
| `admin/system/updater/mod.rs` | 811 | → `update/{service,state}.rs` |

### 5.5 一域一账本

每张 PG 表 / 每个 Redis 键空间恰好属于一个域，只有该域的 store 可以写它：

| PG 表（database.md §4） | 属主域 |
| --- | --- |
| usage_records / ops_error_logs / request_time_buckets / account_usage / account_model_usage | `telemetry` |
| accounts / account_cookies | `accounts` |
| client_api_keys | `keys` |
| admin_users | `auth` |
| runtime_settings | `settings` |
| fingerprints / fingerprint_update_history | `upstream`（fingerprint/store.rs） |
| schema_migrations | `infra` |

| Redis 键空间（database.md §4B） | 属主域 |
| --- | --- |
| `cpr:admin:session:*` | `auth` |
| `cpr:lease:refresh:*` | `accounts`（refresh/lease.rs） |
| `cpr:affinity:*` | `dispatch`（affinity/store.rs） |
| `cpr:models:plan_snapshots` | `models` |

跨域读通过属主域的 query/service 方法，不直接跨域写。**跨域写唯一豁免**：`bootstrap/import_sqlite*`（一次性导入命令，database.md §6）按依赖序直写全部 PG 表；`telemetry/rebuild.rs` 仍只写 telemetry 自有账本，不是跨域豁免。v3 源库退役后导入模块可删。

---

## 6. 现状审计结论（改造动机存档）

### 6.1 分层违规（P1，本次改造的核心动机）

1. **热路径反向依赖管理端**：`proxy/dispatch/usage_events.rs`、`event_recording.rs` import `admin::monitoring` 的 `Admin*Service` 写账本；`runtime/services.rs` 用 Admin 命名的服务装配核心链路。遥测是领域层，管理端只是它的一个读者。→ 建 `telemetry` 域（PR1）。
2. **核心编排寄居 proxy**：`proxy/dispatch/`（约 5600 行，含 1913 行 service）是系统心脏，却挂在客户端 HTTP 入口下；`SqliteSessionAffinityStore`（DB store）住在 proxy 里。→ 建 `dispatch` 域（PR4）。
3. **config 模块越权**：`config/settings.rs`（601 行）= 持久化 + 热更新推送 + admin API key 校验，还持有账号池引用向上推送。config 应只剩启动 schema + loader。→ 建 `settings` 域，由 bootstrap 在持久化成功后统一应用快照（PR2）。
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
| `config/schema.rs` + `config/loader.rs` | `bootstrap/config.rs`（新增 `redis.url`，database.md §7） |
| `config/settings.rs` | `settings/{types,store,service}.rs` † |
| `runtime/{bootstrap,services,state,shutdown}.rs` | `bootstrap/` 同名 |
| `runtime/tasks/*` | `bootstrap/tasks/*`（新增 `retention_trim.rs`；删除 `session_cleanup.rs`、`session_affinity_cleanup.rs`——Redis TTL 接管） |
| —（新增） | `infra/redis.rs`（连接、键前缀、ping） |
| —（新增） | `bootstrap/import_sqlite.rs`（SQLite v3 → PG 导入命令，吸收原 0004 迁移草案） |
| `http/router.rs` | `api/router.rs` |
| `http/middleware/*` | `api/middleware/*` |
| `web/assets.rs` | `api/assets.rs` |
| `proxy/{auth,router}.rs`、`proxy/openai/*` | `api/client/*`（`routes.rs` → `router.rs` 与各资源文件） |
| `proxy/dispatch/usage_events.rs` | `telemetry/recorder.rs` †（Admin* 类型消失） |
| `proxy/dispatch/session_affinity.rs` | `dispatch/affinity/{types,store,service}.rs` † |
| `proxy/dispatch/responses/affinity.rs` | `dispatch/affinity/resolve.rs` |
| `proxy/dispatch/responses/service.rs` | `dispatch/responses/service.rs` + `service/{complete,stream,compact}.rs` † |
| `proxy/dispatch/responses/{live_stream,stream_lifecycle,prefetch,sse_failure,trace}.rs` | `dispatch/responses/` 下同名文件 |
| `proxy/dispatch/responses/event_recording.rs` | `dispatch/responses/event_recording.rs`（只依赖 `telemetry::Recorder`） |
| `proxy/dispatch/responses/errors.rs` + `proxy/dispatch/errors.rs` | `dispatch/responses/errors.rs` + `dispatch/errors.rs`（协议编排错误与通用调度错误分开） |
| `proxy/dispatch/{auth_recovery,cloudflare,exhaustion,implicit_resume,reasoning_replay}.rs` | 通用逻辑保留在 `dispatch/` 根；Responses 专属恢复留在 `dispatch/responses/` |
| `proxy/dispatch/upstream.rs` | `dispatch/upstream.rs` |
| `admin/monitoring/usage_record_{model,store,service}.rs` | `telemetry/usage/{types,store,query}.rs` †（重写已在 PR0 完成，此处纯搬家） |
| `admin/monitoring/ops_error_{model,store,service}.rs` | `telemetry/ops/{types,store,query}.rs` † |
| `admin/monitoring/account_usage_{store,service}.rs` | `telemetry/account_usage/{store,query}.rs` † |
| `admin/monitoring/billing.rs` | `telemetry/billing.rs` |
| `admin/monitoring/dashboard.rs` | 各账本查询进入 `telemetry/*/query.rs`；跨域只读组合进入 `api/admin/dashboard_routes/{data}.rs` † |
| `admin/monitoring/diagnostics.rs` | `telemetry/diagnostics.rs` |
| `admin/monitoring/usage_record_routes.rs` | `api/admin/usage_routes.rs` |
| `admin/keys/service.rs` | `keys/{types,store,service,manage}.rs` †（进程内鉴权缓存消亡，database.md §4.3） |
| `admin/keys/routes.rs` | `api/admin/keys_routes.rs` |
| `admin/auth/service.rs` | `auth/{types,store,service}.rs` †（会话改 Redis，database.md §4B.1） |
| `admin/auth/session.rs` | `api/admin/session.rs` |
| `admin/accounts/routes.rs` | `api/admin/accounts_routes.rs` †（按子资源拆） |
| `admin/accounts/quota_view.rs` | `accounts/manage/quota_view.rs` |
| `admin/accounts/service/*` | `accounts/manage/*`（`importing`→`import`、`exporting`→`export`、`testing`→`probe`、`contracts`→`types`） |
| `admin/{response,update_payload}.rs` | `api/admin/{response,update_payload}.rs` |
| `admin/router.rs` | `api/admin/router.rs` |
| `admin/settings/routes.rs` | `api/admin/settings_routes.rs` |
| `admin/system/{routes,state}.rs`、`admin/system/updater/*` | `api/admin/system_routes.rs`、`update/{state,service,release,download,archive}.rs` † |
| `upstream/accounts/model.rs` | `accounts/account.rs` |
| `upstream/accounts/{pool,window}.rs` | `accounts/{pool,window}.rs`（pool 拆 †） |
| `upstream/accounts/store/*` | `accounts/store.rs` + `accounts/store/{queries,rows}.rs` † |
| `upstream/accounts/quota/*` | `accounts/quota/{service,runtime}.rs` † |
| `upstream/accounts/token_refresh/*` | `accounts/refresh/{lease,service,runtime}.rs` + `runtime/types.rs` † |
| `upstream/accounts/cookies.rs` | `accounts/cookies/store.rs` |
| `upstream/accounts/importing.rs` | `accounts/import.rs` |
| `upstream/accounts/service.rs` | `accounts/service.rs` |
| `upstream/scheduler/*` | `accounts/scheduler.rs` + `accounts/scheduler/*`（mod.rs 门面类型移出） |
| `upstream/models/*` | `models/{config,info,snapshot,catalog,service,store}.rs`（各文件保留真实职责） |
| `upstream/fingerprint.rs` | `upstream/openai/fingerprint/{types,store,runtime,updater}.rs` †（Repository→Store） |
| `upstream/{protocol,transport}/*`、`upstream/token_client.rs` | `upstream/openai/` 下同名路径（websocket/client 按 §5.4 拆） |
| `infra/*` | 原地保留 |
| `backend/build/build.rs` | `backend/build.rs` |

---

## 8. 执行记录（PR0 + 六个顺序搬家阶段）

以下阶段已于 **2026-07-10** 全部完成。固定验收：`cargo fmt --check`、`cargo clippy -- -D warnings`、`cargo test`、`backend/scripts/check_architecture.sh`、`backend/tests` 镜像目录同步搬移。**PR0 是唯一的迁移行为变更阶段**（存储引擎与 schema 语义一次到位）；PR1 起为搬家、更名与拆分。

| PR | 内容 | 关键动作 |
| --- | --- | --- |
| **PR0** | **存储终态切换**（database.md §7 全部 12 条，在现有目录结构内完成） | Cargo：sqlx 加 postgres、新增 redis；`infra/database.rs` 改 PG + 0001 终态基线、新增 `infra/redis.rs`；全部 store 换 PG 方言 / 运行态四键空间改 Redis；成功/失败事实拆表 + provider 维度 + 两条写入事务；client key 可取回、session/admin-key 哈希化 + `client_api_key_id` 归因贯通；Dashboard 口径换源；trim 周期任务化；`import-sqlite` / `rebuild-buckets` 子命令；tests support 换 PG+Redis;deploy compose 加双服务。验收对齐 database.md §6.4 |
| **PR1** | `telemetry` 域成立 | monitoring 全部迁出 admin；`usage_events.rs` → `recorder.rs`；Admin* 遥测类型按 §4.3 更名；dispatch/api 改依赖 telemetry；启用"禁止 `crate::admin::monitoring`"检查 |
| **PR2** | `keys` / `auth` / `settings` / `update` 域成立 | 四个域从 admin/config 拆出；settings 只保留持久化与串行快照，消费者传播由 bootstrap 统一执行（§2.3）；config 只剩启动 schema+loader |
| **PR3** | `accounts` / `models` 域成立 | `upstream/accounts`、`upstream/scheduler`、`admin/accounts/service` 三处合并为 `accounts`；`upstream` 缩为纯客户端并落成 `openai/` 子目录；动名词文件更名；Repository→Store |
| **PR4** | `dispatch` 域成立 | proxy/dispatch 整体迁出；保留内聚的 `responses` 子能力并按行数拆 service 子文件；affinity store 独立成文件；`proxy` 只剩 api/client 的料 |
| **PR5** | `api` / `bootstrap` 壳成立 | http + web + proxy 入口 + admin routes 合并为 `api`；runtime + config 装配部分改名 `bootstrap`；删除 `crate::{admin,proxy,runtime,web,http,config}` 六个旧根模块；启用全部依赖检查 |
| **PR6** | 行数预算清账 | §5.4 清单内 11 个文件拆到硬线以下；启用行数预算 CI 阻断 |

顺序依据：存储切换触及所有 store、无法按域切片，必须整体先行（PR0）——先切引擎再搬家，每个 store 文件只被"重写"一次、"移动"一次，不叠加；PR1–4 自底向上消除旧模块的存在必要；PR5 收壳；PR6 纯内部拆分无跨域影响。原"PR1 与数据库改造互锁"的约定由 PR0 替代，PR2 中的哈希化条目亦已并入 PR0。

---

## 9. CI 强制

`backend/scripts/check_architecture.sh` 是可执行规则的唯一来源，并由 `.github/workflows/ci.yml` 在 clippy 后调用；本文不复制脚本，避免规则双写漂移。当前强制项包括：

- 领域层不得依赖 `api` / `bootstrap`，且同时识别 `crate::api::...` 与 `use crate::{ api::... }` 两种写法；`upstream` 不得依赖任何领域模块。
- axum 只进入 `api` / `bootstrap`；API 不得执行 SQL 或 Redis 命令。
- 存储命令只能位于 §5.2 列出的 adapter/query/维护入口；文件名、类型前缀、旧模块路径、Repository 命名与 Arc 别名均有阻断。
- `mod.rs` 不超过 50 行，其他 Rust 文件不超过 800 行。

全部规则已启用；后续新增代码一旦恢复旧路径、越层依赖、违规命名或突破硬行数上限，CI 直接失败。

---

## 10. 仓库级约定

- `backend/tests/` 顶层镜像 `src/` 领域，不在 `src/` 写测试模块。测试依赖本机（或 CI 服务容器）的 PostgreSQL 与 Redis：`CPR_TEST_DATABASE_URL` 下每测试建独立数据库、`CPR_TEST_REDIS_URL` 下每测试随机键前缀（database.md §7）。
- `architecture.md`（本文）与 `database.md` 是架构/存储权威；`docs/` 可保留故障复盘或修复记录，但不得另行定义架构、schema 或运行契约。行为实现细节以模块文档注释为准。
- `deploy/` 随 PR0 扩容（compose 增 postgres/redis 服务与健康检查依赖）；`release/`、`skills/` 现状合格，不动。
- 前端联动（全部随 PR0）：keys 列表继续返回完整 key，复制与 CCSwitch 导入长期保持可用且交互/文案不变；admin API key 状态不再有脱敏值（哈希不可逆，只余"已启用"）；成功记录响应不再有 `level` 字段；错误明细/筛选改走 `/api/admin/ops/errors`；Dashboard 只修正取数来源，既有卡片文案不变。

---

## 11. 扩展路径：第二上游 provider（已规划：Cloudflare Workers AI）

分层已为多 provider 留好隔离区，接入时**不新增层、不动依赖规则**，动作清单：

| 位置 | 动作 |
| --- | --- |
| `upstream/` | 新增 `upstream/cloudflare/`：client（按账号拼 `accounts/{account_id}/ai/v1` base URL）+ protocol 适配器（**Responses ↔ chat/completions 双向转换**，接入的主要工作量）。无 fingerprint、无 token_client（静态 API key，无刷新）。`upstream/mod.rs` 引入 `UpstreamClient` trait，`dispatch` 从具体 `CodexBackendClient` 改依赖 trait——在只有一家实现时不预建该 trait（投机抽象），第二家动工即引入 |
| `models` | catalog 收录 `@cf/...` 命名空间；别名映射升级为"客户端名 → (provider, 上游名)"；**models 域是"模型 → provider"的路由枢纽**，dispatch 据此选池 |
| `accounts` | `scheduler/candidates` 加 provider 硬过滤（单池 + 过滤，不按 provider 拆池）；`manage/{oauth,import}` 按 provider 分实现（CF 导入格式 = `(account_id, api_token)`）；refresh/租约对 CF 账号整体跳过（`refresh_token` NULL 语义已覆盖） |
| `dispatch` | `affinity` / `recovery` 按 provider 策略分发：CF 无会话语义 → 跳过 affinity；恢复只剩朴素 429/5xx 重试 |
| `telemetry` | 无结构改动——provider 归因维度已随存储终态就位（database.md §4.8–4.10）；billing 单价查找加 provider 参数 |
| 不动 | `api/client`（入站永远是 OpenAI 兼容协议）、`keys`、`auth`、`settings`、`update`、`infra` |

DB 侧一次性迁移（accounts 加 provider、身份列更名、模型缓存 HASH field 加 provider 前缀）见 database.md §8。动工当天需实测验证三点：`cfk_` 老式 authkey 对 OpenAI 兼容端点的鉴权方式（`X-Auth-Email`/`X-Auth-Key` 还是 Bearer）、GLM 类模型是否支持上游 Responses 端点（决定适配器能否对部分模型直通）、SSE 分块与 OpenAI delta 事件的逐字段兼容性。
