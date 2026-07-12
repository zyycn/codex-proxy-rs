# 架构说明

本文描述当前仓库的实际结构。目录调整、存储边界或请求链路发生变化时，代码和本文应在同一提交中更新。

## 系统边界

Codex Proxy RS 是一个单进程网关。Rust 进程同时提供：

- OpenAI Responses 兼容接口：`/v1/*`，包括 HTTP/SSE 和官方 Responses WebSocket
- 管理端接口：`/api/admin/*`
- Vue 管理端静态资源
- PostgreSQL、Redis 健康检查：`/healthz`
- 账号池调度、上游连接、会话恢复和遥测记录

网关接收客户端请求后，从账号池选择一个可用账号，补充该账号的凭据和上游要求的请求头，再把请求发送到 ChatGPT Codex 后端。账号错误可以触发换号，但候选顺序始终由当前配置的调度策略决定。项目不维护独立的业务协议；除模型别名、账号作用域身份、传输控制和历史恢复所需的改写外，请求与响应尽量保持上游语义。

运行时依赖：

- PostgreSQL：权威业务数据和遥测事实
- Redis：带 TTL 的运行态数据、分布式租约和模型快照
- ChatGPT Codex 后端：Responses、模型列表、配额和 token 刷新
- 本地数据目录：身份派生密钥、在线更新状态和文件日志

PostgreSQL 和 Redis 都是必需依赖。任一连接失败时进程不能完成启动；运行中 `/healthz` 检查任一依赖失败都会返回 `503`。

## 仓库目录

```text
.
├── backend/                 Rust 后端和集成测试
│   ├── src/                 生产代码
│   ├── tests/               后端测试，目录结构与 src 对应
│   ├── Cargo.toml
│   └── Cargo.lock
├── frontend/                Vue 管理端
│   ├── src/
│   ├── public/
│   └── dist/                前端构建产物
├── deploy/                  Dockerfile、Compose 和部署模板
├── docs/
│   └── architecture.md      当前架构说明
├── release/                 版本、目标平台和发布脚本
└── skills/                  仓库内开发约束
```

后端生产代码只放在 `backend/src/`。后端测试统一放在 `backend/tests/`，不在生产模块中加入 `#[cfg(test)]` 测试代码。

## 后端模块

`backend/src/lib.rs` 只声明顶层模块。项目没有 `application` 层；业务编排由各领域服务承担，跨领域对象只在 `bootstrap` 中装配。

```text
backend/src/
├── main.rs
├── lib.rs
├── infra/
├── upstream/
├── telemetry/
├── keys/
├── auth/
├── settings/
├── update/
├── fleet/
├── models/
├── dispatch/
├── api/
└── bootstrap/
```

依赖关系按以下规则维护：

1. `main.rs` 只解析子命令并进入 `bootstrap` 或一次性任务。
2. `bootstrap` 是进程装配根，可以依赖所有领域模块，但不承载业务规则。
3. `api` 负责 HTTP 契约、鉴权提取和响应映射，只调用领域服务，不直接写 SQL 或 Redis 命令。
4. `dispatch` 负责编排一次代理请求，可以调用 `fleet`、`models`、`upstream` 和 `telemetry`。
5. `fleet`、`keys`、`auth`、`settings`、`models`、`telemetry` 各自拥有本领域的服务与存储实现。
6. `upstream` 只处理上游协议和传输，不选择账号，也不处理管理端业务。
7. `infra` 提供数据库、Redis、日志、身份和通用格式工具，不依赖 HTTP 层。

### main.rs 与 lib.rs

- `main.rs`：命令行入口。
  - `serve` 或无参数：启动 HTTP 服务。
  - `rebuild-buckets`：从保留期内的成功、失败事实重建请求时间桶。
- `lib.rs`：导出后端顶层模块，供二进制和集成测试使用。

### infra

```text
infra/
├── database.rs
├── redis.rs
├── migrations/
│   └── 0001_initial.sql
├── identity.rs
├── paths.rs
├── logging.rs
├── time.rs
├── json.rs
└── format.rs
```

- `database.rs`：创建 PostgreSQL 连接池、执行迁移、校验迁移顺序和 SQL checksum、提供 ping。
- `migrations/0001_initial.sql`：PostgreSQL 终态基线。迁移版本由迁移目录在代码中的登记顺序派生。
- `redis.rs`：创建 Redis `ConnectionManager`，统一添加 `cpr:` 键前缀，提供 ping。
- `identity.rs`：管理员密码哈希、API Key 和会话令牌生成、账号作用域 HMAC 伪名。
- `paths.rs`：确定本地数据目录，读取或创建 `identity_hmac_secret`。
- `logging.rs`：结构化日志、stdout 输出和文件轮转。
- `time.rs`、`json.rs`、`format.rs`：跨模块使用的解析与格式化工具。

### upstream

```text
upstream/openai/
├── token_client.rs
├── protocol/
│   ├── schema.rs
│   ├── responses.rs
│   ├── events.rs
│   ├── sse.rs
│   ├── websocket.rs
│   └── websocket_errors.rs
├── transport/
│   ├── client.rs
│   ├── client_sse.rs
│   ├── endpoints.rs
│   ├── headers.rs
│   ├── tls.rs
│   ├── response_meta.rs
│   ├── usage.rs
│   ├── diagnostics.rs
│   ├── websocket.rs
│   ├── websocket_frames.rs
│   ├── websocket_pool.rs
│   └── websocket_pump.rs
└── fingerprint/
    ├── types.rs
    ├── store.rs
    ├── runtime.rs
    └── updater.rs
```

- `protocol`：Responses 请求、SSE 事件、WebSocket 帧和响应聚合，不包含账号池逻辑。
- `transport`：HTTP/SSE 与 WebSocket 连接、请求头、TLS、响应诊断和连接池。
- `fingerprint`：Codex Desktop 指纹的 PostgreSQL 存储、运行时快照和更新。
- `token_client.rs`：OAuth token 刷新客户端。

当前只有 OpenAI/ChatGPT Codex provider。出现第二个 provider 时，共享的上游接口应放在 `upstream/mod.rs`，provider 特有实现仍留在各自子目录。

### fleet

`fleet` 是账号域，负责账号实体、持久化、账号池、调度、配额、token 刷新、Cookie 和管理操作。

```text
fleet/
├── account.rs
├── window.rs
├── import.rs
├── store/
│   ├── mod.rs
│   ├── queries.rs
│   ├── rows.rs
│   └── write.rs
├── pool/
│   ├── mod.rs
│   ├── filters.rs
│   └── state.rs
├── scheduler/
│   ├── mod.rs
│   ├── candidates.rs
│   ├── feedback.rs
│   └── strategy/
│       ├── smart.rs
│       ├── quota_reset.rs
│       ├── round_robin.rs
│       └── sticky.rs
├── quota/
│   ├── service.rs
│   └── runtime.rs
├── refresh/
│   ├── policy.rs
│   ├── lease.rs
│   └── service.rs
├── cookies/
│   └── store.rs
└── manage/
    ├── service.rs
    ├── types.rs
    ├── lifecycle.rs
    ├── import.rs
    ├── export.rs
    ├── oauth.rs
    ├── probe.rs
    ├── quota.rs
    └── quota_view.rs
```

- `store`：`AccountStore` 接口和 PostgreSQL 实现。账号、刷新时间和 quota 状态按事务写入。
- `pool`：内存账号快照、并发槽位、请求间隔、状态同步和候选租用。
- `scheduler`：账号选择的唯一归属地。候选过滤和排序分离，支持 `smart`、`quota_reset_priority`、`round_robin`、`sticky`。
- `scheduler/feedback.rs`：保存进程内 EWMA 错误率和首字延迟，供 `smart` 策略打分。
- `quota`：配额查询、窗口状态和运行时更新。
- `refresh`：token 刷新策略、Redis 分布式租约和刷新服务。
- `cookies`：按账号保存 Cloudflare Cookie。
- `manage`：管理端账号导入、导出、OAuth、探测、刷新和生命周期操作。

### models

- `catalog.rs`：模型合并、别名解析和目录查询。
- `service.rs`：运行时模型服务、刷新和设置订阅。
- `store.rs`：按订阅计划保存 Redis 模型快照。
- `types.rs`：模型和计划快照类型。

### dispatch

`dispatch` 是代理请求的编排边界。账号选择、历史恢复、上游调用、流生命周期和遥测归因在这里汇合。

```text
dispatch/
├── service.rs
├── upstream_call.rs
├── attempts.rs
├── errors.rs
├── recording.rs
├── affinity/
│   ├── identity.rs
│   ├── resolve.rs
│   ├── service.rs
│   ├── store.rs
│   └── types.rs
├── recovery/
│   ├── account_failure.rs
│   ├── auth.rs
│   ├── cloudflare.rs
│   ├── exhaustion.rs
│   └── history.rs
└── stream/
    ├── lifecycle.rs
    ├── live.rs
    ├── prefetch.rs
    ├── sse_failure.rs
    └── trace.rs
```

- `service.rs`：非流式 Responses 主循环。
- `stream/lifecycle.rs`：流式 Responses 主循环。
- `upstream_call.rs`：把账号凭据、账号作用域身份和 Cookie 交给上游客户端，处理单账号有限重试。
- `attempts.rs`：在请求开始时冻结完整候选顺序，记录已尝试、忙碌和状态排除的账号。
- `errors.rs`：错误分类与客户端错误映射。
- `recording.rs`：把调度轨迹和上游诊断写入 `telemetry::Recorder`。
- `affinity`：会话亲和、账号作用域身份、Redis 索引和响应历史快照。
- `recovery`：账号风险隔离、Cloudflare 冷却、候选耗尽和 previous response 恢复。
- `stream`：首段预取、SSE 失败识别、下游流转发和结束时结算。

### telemetry

```text
telemetry/
├── recorder.rs
├── billing.rs
├── dashboard.rs
├── rebuild.rs
├── usage/
├── ops/
├── buckets/
└── account_usage/
```

- `recorder.rs`：真实 `/v1` 代理调用的统一记录入口。成功事实写入 `usage_records`，失败事实写入 `ops_error_logs`，同时更新请求时间桶。
- `billing.rs`：token 和模型计费口径。
- `dashboard.rs`：Dashboard 聚合查询，不处理 HTTP。
- `usage`、`ops`：事实表的类型、存储和查询。
- `buckets`：`request_time_buckets` 实时聚合、查询和重建。
- `account_usage`：账号累计用量和当前额度窗口统计。
- `rebuild.rs`：`rebuild-buckets` 子命令实现。

### keys、auth、settings、update

- `keys`：客户端 API Key 的创建、分页查询、更新、删除和完整 Key 校验。鉴权通过 PostgreSQL 唯一点查完成。
- `auth`：管理员用户、密码校验和登录会话。管理员用户在 PostgreSQL，登录会话在 Redis。
- `settings`：PostgreSQL `runtime_settings` 的读写和 `watch` 广播。模型别名、刷新参数、单账号并发、请求间隔和调度策略可在运行时更新。
- `update`：Release 查询、下载、checksum 校验、归档替换、更新状态和回滚。

### api

```text
api/
├── router.rs
├── assets.rs
├── middleware/
│   ├── request_id.rs
│   └── trace.rs
├── client/
│   ├── router.rs
│   ├── auth.rs
│   ├── responses/
│   │   ├── mod.rs
│   │   ├── sse.rs
│   │   └── websocket.rs
│   ├── models.rs
│   └── errors.rs
└── admin/
    ├── router.rs
    ├── response.rs
    ├── session.rs
    ├── auth_routes.rs
    ├── settings_routes.rs
    ├── system_routes.rs
    ├── keys_routes.rs
    ├── usage_routes.rs
    ├── ops_routes.rs
    ├── dashboard_routes.rs
    └── accounts_routes/
```

- `router.rs`：组合客户端 API、管理端 API、SPA 静态资源和 `/healthz`。
- `middleware/request_id.rs`：接收或生成请求 ID，并写入响应头。
- `middleware/trace.rs`：HTTP 访问日志。
- `client`：`/v1` 路由、Bearer API Key 鉴权和 Responses 入站协议。`responses/mod.rs` 负责共享请求构造，`sse.rs` 与 `websocket.rs` 只处理各自的下游传输。
- `admin`：`/api/admin` 路由、会话或管理员 API Key 鉴权、统一响应结构。所有管理端响应带 `Cache-Control: no-store`。
- `assets.rs`：静态文件服务和 Vue Router history fallback；未知 `/api`、`/v1` 路径不会回退到 SPA。

客户端请求体上限为 16 MiB。生产环境由同一个 Rust 进程托管 `web/dist`，不运行独立 Node 服务。

### bootstrap

```text
bootstrap/
├── config.rs
├── services.rs
├── state.rs
├── shutdown.rs
└── tasks/
    ├── coordinator.rs
    ├── periodic.rs
    ├── cleanup.rs
    ├── cookie_cleanup.rs
    ├── retention_trim.rs
    ├── model_refresh.rs
    ├── token_refresh.rs
    ├── quota_refresh.rs
    └── fingerprint_update.rs
```

- `config.rs`：启动配置 schema、YAML 加载和密码环境变量覆盖。
- `services.rs`：创建存储、领域服务、上游客户端、路由和后台任务。
- `state.rs`：进程级运行配置和 PostgreSQL/Redis 健康检查。
- `shutdown.rs`：信号、管理端重启和进程替换协调。
- `tasks/coordinator.rs`：统一启动和关闭后台任务。

## 启动流程

`serve` 的启动顺序固定：

1. 从 `CPR_CONFIG_FILE` 指定路径加载 YAML；未设置时读取当前目录的 `config.yaml`。
2. 用 `CPR_ADMIN_DEFAULT_PASSWORD`、`CPR_POSTGRES_PASSWORD`、`CPR_REDIS_PASSWORD` 覆盖对应密码。
3. 初始化日志并绑定监听地址。
4. 连接 PostgreSQL，校验并执行迁移。
5. 连接 Redis，统一使用 `cpr:` 键前缀。
6. 读取或初始化 `runtime_settings`，把数据库设置应用到运行配置。
7. 创建各领域存储、服务和 OpenAI 上游客户端。
8. 从本地数据目录读取或创建 `identity_hmac_secret`。
9. 初始化默认指纹、管理员、模型运行时缓存和内存账号池。
10. 启动后台任务，挂载 HTTP 路由。

关闭时先停止接收新请求，HTTP 连接最多排空 20 秒；后台任务并行关闭，单任务最多等待 5 秒。

## 代理请求链路

一次 `POST /v1/responses` 请求或 WebSocket `response.create` 消息经过以下步骤：

1. 请求 ID 中间件建立全链路标识，访问日志记录基础 HTTP 信息。
2. `client::auth` 从 `Authorization: Bearer sk_...` 取出完整 Key，并在 PostgreSQL 校验是否存在且启用。WebSocket 在 HTTP upgrade 前完成鉴权。
3. API 层解析 HTTP JSON 或官方 `response.create` 文本帧，提取客户端 IP、User-Agent 和 transport-only 参数。
4. `models` 解析模型别名；`dispatch` 建立会话变体标识和 previous response 恢复计划。
5. `AccountAttemptLedger` 按当前调度策略冻结完整候选顺序。会话亲和账号可以排在前面，但不会绕过可用性检查。
6. 账号池租用并发槽位，必要时先刷新 quota；请求间隔在发送上游前执行。
7. `upstream_call` 注入账号 token、`chatgpt-account-id`、账号作用域身份、指纹请求头和 Cookie。
8. 上游通过 HTTP/SSE 或 WebSocket 返回结果。单账号可重试的 5xx 最多额外重试两次。
9. 在结果尚未提交给客户端时，账号级失败可继续尝试候选账本中的下一个账号。
10. 完成后更新账号用量、会话亲和、调度反馈和遥测事实，并释放账号槽位。

`POST /v1/responses/compact` 使用相同的账号池、身份隔离和错误分类，但由 `upstream_call.rs` 的 compact 分支执行。

## 账号调度与换号

候选生成分为两步：

- `scheduler::candidates` 过滤非 active、配额不可用、模型不匹配、处于 Cloudflare 冷却或明确排除的账号。
- 当前 `rotation_strategy` 对剩余账号排序。请求级快照保留所有符合条件的层级，层级只影响顺序，不缩小 failover 边界。

单个请求的候选顺序在开始时冻结。瞬时并发槽位占满的账号进入 busy 队列，等待槽位变化后再判断；状态已经变化的账号被排除。候选账本不会重复取出同一个账号。单账号内部的 5xx 重试，以及 previous response 失效后在原账号执行的完整重放，是账本之外的明确重试。

会触发账号隔离或换号的典型情况：

- quota 或 rate limit 已耗尽
- token 失效、账号过期或封禁
- 当前账号不支持请求模型
- Cloudflare challenge 或路径阻断
- 可重试的账号传输故障
- 未产生可见输出的空响应

这些失败只更新当前账号的状态、quota 或冷却时间，不启用独立熔断器，也不固定切到某一种策略。下一账号仍来自本次请求按当前设置生成的调度顺序。

请求本身不可修复的 4xx、协议错误或已经向客户端提交内容后的流错误不会通过换号掩盖。候选耗尽后，返回最后一个有业务意义的账号错误；没有候选时返回无可用账号错误。

## previous response 与流式恢复

成功响应写入 Redis 亲和记录，保存响应 ID、账号 ID、会话 ID、turn state、变体哈希和受限的 replay 节点。

previous response 分两类处理：

- 受管历史：Redis 能找到响应归属。先使用原账号和原 previous response；若上游返回 `previous_response_not_found`、连接续接忙碌或原连接不可复用，先在原账号执行完整历史重放。需要换号且 replay 完整时，其他账号也使用完整历史重放。
- 外部未知历史：Redis 没有该响应。只能在第一个账号原样尝试一次，因为服务端没有足够历史安全地转移到其他账号；失败时保留上游错误。

replay 按轮保存增量输入和输出，不保存每一轮累计的完整输入。写入前递归删除对象 ID 和 `encrypted_content`。容量边界：

- 单轮 snapshot：2 MiB
- 单会话累计 replay：16 MiB
- 最大深度：128 轮
- Redis TTL：4 小时

超过任一边界时不再生成新的 replay 节点，正常代理响应不因此失败。

流式请求只有在响应尚未提交给客户端时才能透明恢复或换号。首段预取会识别终止事件和账号级失败；一旦真实输出已经发给客户端，代理会用 `response.failed` 结束当前 SSE 或 WebSocket response，不能在另一账号上重放后拼接到已有输出。

Responses 协议没有按事件序号续传未完成 response 的请求字段。官方 Codex 在 turn 层持有完整会话历史和工具执行状态，收到可重试的断流后会建立新请求重新采样。代理保留这一责任边界：新请求仍进入账号池调度和会话恢复，但不会伪造同一个 response 的续传。

## 账号作用域身份

`identity_hmac_secret` 是实例级 256-bit 持久密钥。服务端用它对以下值做 HMAC 派生：

- prompt cache key
- session、thread、turn、window 和 parent thread ID
- client request ID
- installation ID

派生输入包含字段域和账号 ID。同一客户端标识在不同账号上得到不同值；同一账号、同一输入在密钥不变时保持稳定。installation ID 只由账号 ID 和持久密钥派生，因此每个账号有一个稳定值，不在每次请求时随机生成。

镜像更新或容器重建时必须保留数据目录中的 `identity_hmac_secret`。丢失该文件会为所有账号生成新的身份集合，已有会话亲和也失去连续性。

## 数据存储

### PostgreSQL

PostgreSQL 是权威持久化存储。

| 数据 | 表 |
| --- | --- |
| 管理员 | `admin_users` |
| 客户端 API Key | `client_api_keys` |
| 运行时设置 | `runtime_settings` |
| 账号与 quota 状态 | `accounts` |
| 账号累计和窗口用量 | `account_usage` |
| 成功请求事实 | `usage_records` |
| 失败请求事实 | `ops_error_logs` |
| 请求聚合桶 | `request_time_buckets` |
| Cloudflare Cookie | `account_cookies` |
| Codex Desktop 指纹 | `fingerprints`、`fingerprint_update_history` |
| 迁移记录 | `schema_migrations` |

迁移框架要求版本严格递增，并校验已执行迁移的名称和 SQL checksum。修改已经发布的迁移会阻止启动；schema 变化必须新增迁移。

保留期清理任务每小时执行一次。默认值保存在 `runtime_settings`：

- `usage_records`：30 天
- `ops_error_logs`：30 天
- `request_time_buckets`：90 天
- `fingerprint_update_history`：最多 100 条

`account_usage` 是每账号一行的累计状态，账号删除时级联删除；账号、Key、设置、Cookie 和当前指纹按业务生命周期删除，不按日志保留期裁剪。

### Redis

Redis 使用统一的 `cpr:` 前缀。

| 键 | 内容 | 生命周期 |
| --- | --- | --- |
| `cpr:admin:session:<hash>` | 管理员登录会话 | `admin.session_ttl_minutes` |
| `cpr:lease:refresh:<account_id>` | token 刷新租约 | 租约 PX TTL |
| `cpr:models:plan_snapshots` | 各订阅计划模型快照 HASH | 下一次完整刷新替换 |
| `cpr:affinity:v2:resp:<response_id>` | 响应归属和 replay 节点 | 4 小时 |
| `cpr:affinity:v2:conv:<conversation_id>` | 会话响应 ZSET 索引 | 4 小时 |
| `cpr:affinity:v2:account:<account_id>` | 账号响应 ZSET 索引 | 4 小时 |

账号删除时会批量删除该账号的亲和响应并清理会话索引。Redis 写入失败不应改变已经得到的上游响应语义，但会失去对应的会话恢复、模型缓存或分布式协调能力。

### 本地文件

Docker 默认把宿主机 `.runtime/data` 挂载到 `/app/data`，并设置 `XDG_DATA_HOME=/app/data`。

```text
.runtime/
├── config.yaml
├── data/
│   ├── identity_hmac_secret
│   ├── update-state.json
│   ├── update.lock
│   └── update-tmp/
└── logs/
```

PostgreSQL 数据不在 `.runtime/data`。Compose 使用 `postgres-data` 命名卷；Redis 使用 `redis-data` 命名卷并启用 AOF。更换应用镜像不会删除这些卷，执行 `docker compose down -v` 才会删除命名卷。

## 后台任务

`TaskCoordinator` 管理以下任务：

- 过期 Cookie 清理
- PostgreSQL 事实表和指纹历史裁剪
- 模型目录周期刷新和 ETag 触发刷新
- token 提前刷新
- quota 周期刷新
- Codex Desktop 指纹更新
- WebSocket 连接池生命周期
- settings 的模型、账号池和刷新策略订阅

任务共享领域服务和存储，不通过 HTTP 回调自身。Redis TTL 已接管管理会话、刷新租约和会话亲和的过期，不存在单独的 session cleanup 或 affinity cleanup 轮询任务。

## 前端

```text
frontend/src/
├── api/                     Axios 实例、错误处理和按领域划分的 API
├── components/base/         基础控件
├── components/charts/       图表封装
├── composables/             跨页面组合逻辑
├── directives/              Vue 指令
├── layout/                  管理端框架和侧栏
├── router/                  路由表与登录守卫
├── stores/                  Pinia 状态
├── styles/                  全局样式和设计 token
├── utils/                   通用前端工具
└── views/
    ├── login/
    ├── dashboard/
    ├── accounts/
    ├── api-keys/
    ├── usage/
    └── settings/
```

前端只调用 `/api/admin/*`。开发服务器把 `/dev/*` 代理到本地后端并去掉 `/dev` 前缀；生产构建写入 `frontend/dist`，镜像构建时复制到后端的 `web/dist`。

## HTTP 路由

| 路径 | 作用 |
| --- | --- |
| `GET /healthz` | PostgreSQL 和 Redis 健康检查，成功返回 `204` |
| `POST /v1/responses` | Responses 非流式或 SSE 流式入口 |
| `GET /v1/responses` | 官方 Responses WebSocket upgrade 入口 |
| `POST /v1/responses/review` | Review 请求入口 |
| `POST /v1/responses/compact` | Compact 请求入口 |
| `GET /v1/models*` | 模型列表、详情、catalog 和运行信息 |
| `/api/admin/*` | 登录、账号、Key、设置、用量、错误、Dashboard 和系统更新 |
| 其他非 API 路径 | Vue SPA 静态资源或 `index.html` |

客户端接口使用 `sk_` API Key。管理端支持登录 Cookie，也支持设置中生成的管理员 API Key。

## 验证边界

CI 的质量门禁包括：

- Rust `fmt`
- Rust `clippy --all-targets --all-features --locked`
- 后端集成测试
- 前端 Prettier 检查与生产构建
- GitHub Actions workflow lint
- Docker 镜像构建和 Compose smoke test
- 依赖与镜像安全扫描

后端集成测试默认连接本机 PostgreSQL 和 Redis，也可通过 `CPR_TEST_DATABASE_URL`、`CPR_TEST_REDIS_URL` 指定。每个数据库测试创建独立数据库；Redis 测试使用独立键前缀并清理旧测试键。
