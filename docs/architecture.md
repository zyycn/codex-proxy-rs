# Codex Proxy RS 架构

本文定义仓库当前生效的系统结构、责任边界和运行时不变量。设计讨论、故障排查、迁移过程和阶段性验收不属于架构文档；代码边界发生变化时，本文必须在同一变更中同步。

## 系统边界

Codex Proxy RS 是一个单进程 Rust 网关，同时提供：

- OpenAI Responses 兼容接口：`/v1/*`，包括 HTTP JSON、HTTP SSE 和官方 Responses WebSocket。
- 管理端接口：`/api/admin/*`。
- Vue 管理端静态资源。
- PostgreSQL 与 Redis 健康检查：`/healthz`。
- Codex 账号池调度、上游传输、历史恢复、用量和运维遥测。

网关为每次代理请求选择一个可用上游账号，注入该账号的认证身份和安装身份，再调用 ChatGPT Codex 后端。请求和响应默认保持上游语义；只有模型别名、账号身份隔离、transport control 和具备完整上下文的历史重放允许改写。

运行时依赖：

| 依赖 | 责任 |
| --- | --- |
| PostgreSQL | 账号、Key、设置、Cookie、指纹和遥测事实的权威存储 |
| Redis | 管理会话、刷新租约、模型快照、响应归属和会话级短期状态 |
| ChatGPT Codex | Responses、模型目录、配额和 OAuth token 刷新 |
| `.runtime/` | 身份派生密钥、更新状态、日志及本地 PostgreSQL/Redis 数据 |

PostgreSQL 和 Redis 都是启动必需依赖。运行中任一健康检查失败时，`/healthz` 返回 `503`。

## 架构不变量

1. `v1/*` 使用 Request → Attempt → Stream 三层静态洋葱生命周期；管理端、健康检查和静态资源不进入业务洋葱。
2. 一个功能只有一个规则 owner。跨入口的账号失败语义属于 `fleet/account_failure.rs`；`v1/*` 的 retry、exhaustion 和 commit decision 属于对应 controller。
3. API 只解析和编码；lifecycle 只编排顺序、retry、commit 和 finalize；transport 只建立连接并产出 typed facts。
4. 上游响应一旦可能收到 payload，transport 或账号都不得自动重放同一请求。
5. 账号认证身份按 attempt 重建；客户端会话拓扑按原值透传；两者不能共用一个“身份”抽象。
6. `v1/*` 热路径中的账号明确失效先在线性化的内存路径生效并驱逐 WebSocket，再异步持久化 PostgreSQL；管理端和后台任务可等待同一状态 effect 持久化完成。
7. 每项生产能力只有一条执行路径，不设置兼容 wrapper、双状态机或运行时切换的新旧实现。
8. `backend/src/` 只放生产代码；所有测试和 test-only helper 位于 `backend/tests/`。

## 仓库结构

```text
.
├── backend/
│   ├── src/                 Rust 生产代码
│   ├── tests/               按生产边界组织的集成与契约测试
│   ├── Cargo.toml
│   └── Cargo.lock
├── frontend/                Vue 管理端
├── deploy/                  Dockerfile、Compose 和运行配置
├── docs/
│   └── architecture.md      当前架构
├── release/                 版本、平台和发布脚本
└── skills/                  仓库内开发约束
```

## 后端分层

`backend/src/lib.rs` 只声明顶层模块。项目不设置通用 `application` 层，跨领域对象仅在 `bootstrap` 装配。

```text
backend/src/
├── main.rs
├── lib.rs
├── infra/
├── upstream/
├── fleet/
├── models/
├── dispatch/
├── telemetry/
├── keys/
├── auth/
├── settings/
├── update/
├── api/
└── bootstrap/
```

依赖方向：

| 模块 | 责任与依赖边界 |
| --- | --- |
| `main` | 解析子命令，只进入 `bootstrap` 或一次性任务 |
| `bootstrap` | 进程 composition root，可装配所有领域模块，不拥有业务规则 |
| `api` | HTTP/WebSocket 契约、鉴权提取和最终响应编码，不写 SQL/Redis |
| `dispatch` | 编排一次 `v1/*` 请求，调用 `fleet`、`models`、`upstream`、`telemetry` |
| `fleet` | 账号、调度、quota、token、Cookie 和管理操作 |
| `upstream` | Codex 协议与 HTTP/WebSocket transport，不选择账号 |
| `telemetry` | 保存已经确定的成功、失败和聚合事实，不参与调度 |
| `infra` | PostgreSQL、Redis、日志、时间、路径和身份基础设施，不依赖 API |

### infra

`infra` 提供：

- `database.rs`：PostgreSQL 连接池、迁移、迁移 checksum 和 ping。
- `redis.rs`：Redis `ConnectionManager`、统一 `cpr:` 前缀和 ping。
- `identity.rs`：密码哈希、API Key、会话令牌和 HMAC 伪名。
- `paths.rs`：数据目录与 `identity_hmac_secret`。
- `logging.rs`：TTY compact 日志与非 TTY/文件 JSON 日志。
- `time.rs`、`json.rs`、`format.rs`：无业务语义的通用值处理。

### upstream

```text
upstream/openai/
├── token_client.rs
├── failure.rs
├── protocol/
│   ├── responses.rs
│   ├── events.rs
│   ├── sse.rs
│   ├── websocket.rs
│   └── schema.rs
├── transport/
│   ├── client.rs
│   ├── client_sse.rs
│   ├── headers.rs
│   ├── tls.rs
│   ├── response_meta.rs
│   ├── diagnostics.rs
│   ├── usage.rs
│   ├── websocket.rs
│   ├── websocket_frames.rs
│   ├── websocket_pool.rs
│   ├── websocket_breaker.rs
│   └── websocket_pump.rs
└── fingerprint/
```

- `protocol` 定义 Responses body、canonical SSE/WS 事实和序列化，不包含账号策略。
- `failure.rs` 将各 transport 错误规范化为稳定的 `UpstreamFailureFacts`，不解释账号状态。
- `transport` 拥有 HTTP/SSE、WebSocket、TLS、header、连接池、熔断和 transport metrics。
- `fingerprint` 拥有 Codex Desktop 指纹的存储、运行时快照和更新。
- `token_client.rs` 只实现 OAuth token 刷新协议。

### fleet

`fleet` 是账号领域：

- `store`：账号与 quota 的 PostgreSQL 映射和事务写入。
- `pool`：进程内账号快照、并发 slot、请求间隔和候选 lease。
- `account_failure.rs`：HTTP、SSE、手动额度查询和后台额度刷新共享的账号失败分类与状态 effect。
- `scheduler`：候选过滤与排序的唯一 owner，支持 `smart`、`quota_reset_priority`、`round_robin`、`sticky`。
- `scheduler/feedback`：进程内 EWMA 错误率和首字延迟。
- `quota`：配额查询、窗口与运行时限制。
- `refresh`：token 刷新策略、Redis 租约和刷新服务。
- `cookies`：账号级 Cloudflare Cookie。
- `manage`：导入、导出、OAuth、探测、刷新和账号生命周期。

### models

- `catalog.rs` 合并官方模型快照与运行时别名。
- `service.rs` 请求 `/codex/models`，按订阅计划维护模型目录。
- `store.rs` 在 Redis 中保存计划模型快照。
- `types.rs` 定义模型、计划和目录值对象。

### telemetry、keys、auth、settings、update

- `telemetry`：成功事实 `usage_records`、失败事实 `ops_error_logs`、请求时间桶、账号用量和计费。
- `keys`：客户端 API Key 生命周期与 PostgreSQL 鉴权。
- `auth`：管理员用户、密码校验和 Redis 登录会话。
- `settings`：`runtime_settings` 读写及 `watch` 广播。
- `update`：Release 查询、checksum 校验、文件替换、状态和回滚。

### api 与 bootstrap

`api` 组合客户端接口、管理端接口、健康检查和 SPA：

- `api/client/responses` 解析 HTTP JSON/SSE 与入站 Responses WebSocket。
- `api/client/errors.rs` 将 typed dispatch error 编码为稳定 OpenAI 错误。
- `api/admin` 提供管理端路由和统一 `no-store` 响应。
- `api/middleware` 负责 request ID、访问日志和连接排空。
- `api/assets.rs` 提供静态资源与 Vue Router fallback，未知 API 路径不回退 SPA。

`bootstrap` 负责配置、依赖装配、启动任务和关闭协调；它不解释账号错误、history 或 transport outcome。

## v1 请求生命周期

`dispatch` 是 `v1/*` 的业务编排边界：

```text
API adapter
  -> Request lifecycle
       -> AttemptRunner(account A)
            -> controller enter
            -> request interval || transport prepare
            -> upstream exchange
            <- typed observation / decision
       -> AttemptRunner(account B)  [仅 OpenAttempt 可 retry]
       -> Stream lifecycle          [CommittedAttempt 无 retry API]
  <- controller finalize / API encoding
```

### Request

`dispatch/lifecycle/request.rs`：

1. 解析模型和请求不变量。
2. 建立 history、cyber policy、identity、usage 等 request scope。
3. 并行读取互不依赖的 Redis 状态，读取有 100ms 上限并 fail-open。
4. 合并 preferred/excluded 账号集合。
5. 按当前 scheduler 冻结本请求的完整候选顺序。

Request scope 在 attempt 之间传递，但 controller 只能访问自己的状态。

### Attempt

`dispatch/lifecycle/attempt.rs` 对每个候选账号执行：

1. 从账号池获取 lease，并在真正获取时重读运行时可用状态。
2. 由 quota、Cookie、history 和 identity controller 构造 `AccountScopedRequest`。
3. 并行执行账号请求间隔等待与 prepared transport。
4. 发送上游请求，将 HTTP、SSE、WebSocket 结果规范化为 `AttemptObservation`。
5. 由 `ControllerSet` 按静态优先级调用 owner 分类和 effect。
6. lifecycle 只解释 `Accept`、`RetrySameAccount`、`RetryNextCandidate`、`Return`。

`OpenAttempt` 暴露 decision/retry；`CommittedAttempt` 只允许建立结果或 stream。提交边界后的类型没有 retry 方法。

### Stream

`dispatch/transport/canonical.rs` 对上游事件只解码一次，同时保留 raw bytes 和 typed facts。prefetch 在首个真实输出或 terminal 前保持请求可恢复；提交后由 `dispatch/stream/live.rs` 转发事件，并由 consuming finalizer 保证所有结束路径只 finalize 一次。

Stream terminal 包括 completed、incomplete、failed、上游断开、下游取消、shutdown 和 capture limit。已提交流中的明确账号事实仍会更新账号状态，但只影响后续请求。

### Controller 所有权

| 功能 | 唯一 owner |
| --- | --- |
| previous response 所有权、scope、full replay | `controllers/history.rs` |
| cyber policy 会话排除和 CAS 清理 | `controllers/cyber_policy/` |
| v1 quota 与 rate limit attempt decision | `controllers/quota.rs` |
| v1 封禁、过期、模型不支持的 exhaustion decision | `controllers/account_failure.rs` |
| transport 错误到稳定上游事实 | `upstream/openai/failure.rs` |
| 跨入口账号失败分类、状态 effect 与 WS 驱逐 | `fleet/account_failure.rs` |
| Cloudflare Cookie 与冷却 | `controllers/cloudflare.rs` |
| 会话亲和写入 | `controllers/affinity.rs` |
| usage 结算 | `controllers/usage.rs` |
| 代理事件与 transport metrics | `controllers/telemetry/` |

Controller 之间不直接调用。跨功能优先级只在 `ControllerSet` 声明；API、transport、store 和 lifecycle 不重复识别功能语义。Controller 只把共享账号分类翻译为 v1 生命周期 decision；手动额度查询和后台额度刷新执行同一状态 effect，但各自保留自己的返回与调度职责。

## 上游双热传输

Responses transport 使用精确 WebSocket 与共享 HTTP/2 两条常热通道。WebSocket 保存 connection-local response 状态；HTTP/2 提供新链和可持久化续链的低延迟后备。

### TransportRequirement

`upstream/openai/protocol/responses.rs` 将请求事实归一化为：

| Requirement | 语义 |
| --- | --- |
| `HttpRequired` | 客户端明确要求 HTTP |
| `ExplicitWebSocketWarmup` | `generate=false + store=false`，必须使用可保留 socket 的 WebSocket |
| `ExactWebSocketContinuation` | 只能使用持有指定 connection-local response ID 的 socket |
| `PersistedContinuation` | previous response 已持久化，可使用 WS 或 HTTP/2 |
| `ExternalUnknown` | previous response owner 未知，只允许选定账号原样尝试 |
| `NewChain` | 无 previous response 的普通新链 |

`generate=false + store=false` 的约束优先于本地 HTTP hint，避免产生无法续接的 warmup response。

### Prepared typestate 与 fallback

`PreparedResponseTransport` 只表示“账号、header 和连接已准备，但 payload 尚未发送”。WebSocket opening 与 `response.create` 发送是单向类型边界：

- 普通新链、persisted continuation 和 external unknown 的冷 WS opening 预算为 800ms。
- 预算内连接成功或复用热 socket 时发送 WS payload。
- 预算耗尽、DNS/TCP/TLS 失败、pool 不可用或 breaker 打开时，同账号使用 HTTP/2。
- opening 已返回明确上游响应时，事实直接进入 controller，不追加 HTTP 请求。
- warmup 与 exact continuation 禁止 HTTP fallback。
- payload 发送后的连接断开、发送超时或首事件超时统一为 `PostSendAmbiguous`，不得换 transport 或账号。

AttemptRunner 将 prepared transport 与账号请求间隔并行，800ms、熔断阈值和连接状态只存在于 `upstream/openai/transport`。

### WebSocket pool

Pool key 为 `(base_url, account_id, local_conversation_id)`，slot 状态为：

- `Idle`：可租用的热 socket。
- `Busy`：单 socket 上已有一个 in-flight response。
- `Connecting`：同 key opening 单飞，其他请求不会再建第二条 socket。

每条连接记录最新 completed response ID。Exact continuation 只有 ID 完全匹配时才能租用；socket 缺失、busy、失活或 ID 不匹配时不新建连接碰运气，由 HistoryController 决定 full replay 或失败。

池化 socket 默认每 25 秒发送带唯一序列的 Ping，匹配 Pong 的 deadline 为 5 秒，最大寿命为 55 分钟。任意业务入站帧同样证明链路存活；下游背压导致 pump 无法读取 socket 时暂停主动探活。后台 pump 统一处理 ping/pong、EOF、close 和 liveness，复用前只读取原子连接状态，不增加网络往返。

### Origin breaker

Breaker key 为 origin 与 TLS profile，不按账号区分：

- 30 秒窗口内 3 次 WS 快路径超时后打开 30 秒。
- 打开期间热 WS 仍可使用；普通冷 opening 直接走 HTTP/2。
- 到期后只允许一个 half-open probe。
- 账号或请求级 4xx opening 响应证明 origin 可达并关闭 half-open。
- 5xx 或 transport probe 失败重新打开 breaker。

Breaker 不更新账号状态；账号失效只由 controller 的明确 typed fact 触发。

### HTTP/2

Reqwest client 按 TLS profile 缓存并自动 ALPN 协商，配置为：

```rust
Client::builder()
    .pool_idle_timeout(None::<Duration>)
    .http2_keep_alive_interval(Duration::from_secs(30))
    .http2_keep_alive_timeout(Duration::from_secs(5))
    .http2_keep_alive_while_idle(true)
```

HTTP/2 连接按 origin/TLS profile 共享，账号 Authorization 只属于单个 stream。系统没有强制 HTTP/1.1 配置或第二套 transport builder。

### Transport metrics

成功与失败事实记录：

- `transportDecision`
- `wsConnectMs`
- `transportDecisionWaitMs`
- `upstreamHeadersMs`
- `firstEventMs`
- `firstTokenMs`
- `httpVersion`
- `websocketPool.kind`

稳定 decision 包括 `ws_reused`、`ws_connected_fast`、`ws_exact_required`、`ws_required`、`http2_ws_slow`、`http2_breaker_open`、`http2_pool_unavailable` 和 `http2_ws_pre_send_failure`。

## 账号调度与失效

Scheduler 先过滤不可用账号，再按运行时 `rotation_strategy` 排序。候选顺序在 Request 阶段冻结；层级只影响顺序，不缩小 failover 集合。候选账本不会重复租用同一账号。

明确上游事实的处理顺序：

1. transport 提取 status、code、type、message、retry-after 等事实。
2. `fleet/account_failure.rs` 将跨入口账号事实统一分类为 quota、rate limit、expired、disabled、banned 或 model unsupported；Cloudflare 与 cyber policy 保持各自 owner。
3. v1 controller 把共享分类翻译为 retry/exhaustion decision。
4. 对账号全局事实，账号池 mutex 内立即失效 runtime state 并移除 slot。
5. 驱逐该账号的 idle/busy/connecting WebSocket entry。
6. lifecycle 从冻结候选中选择下一账号。
6. PostgreSQL 状态写入后台有序队列。

数据库延迟不会延长当前换号路径，也不会使并发请求重新租到已失效账号。

`cyber_policy` 是会话级负亲和，不是账号全局状态。它按客户端 API Key 和会话身份隔离，以 revision token 做 CAS 清理，TTL 为 1 小时；不设置功能私有的重试次数，候选耗尽由通用 lifecycle 返回。

## Previous response 与重放

成功 response 的 Redis 归属记录保存账号、conversation、turn state、variant 和 continuation scope，不保存请求/响应正文。

Continuation scope：

| Scope | 含义 |
| --- | --- |
| `Persisted` | `store=true`，上游可在新 transport hydration |
| `ConnectionLocal` | `store=false` 且 completed socket 仍在池中 |
| `ReplayRequired` | 没有可续接 socket，但当前入站 WS 持有完整 canonical transcript |
| `Unavailable` | 没有 socket，也没有完整 replay snapshot |
| `ExternalUnknown` | owner 无法由代理确认 |

完整 transcript 只存在于当前入站 Responses WebSocket：

- 每次 completed 后追加本轮 input 与 output。
- full replay 前递归删除 item `id` 和 `encrypted_content`，保留工具配对所需的 `call_id`。
- transcript 不写 PostgreSQL/Redis，不跨下游连接共享。
- 有完整 transcript 时，HistoryController 可以删除 previous ID 并在同账号或后续候选全量重放。
- 只有归属而没有 transcript 时，只允许 owner 账号续接。
- owner 与 transcript 都未知时，只在首个选定账号原样尝试。

真实输出提交后不执行透明 history recovery。客户端负责在新的 turn 中重新提交完整上下文。

## 身份隔离

| 字段 | 所属语义 | attempt 行为 |
| --- | --- | --- |
| access token、`chatgpt-account-id`、Cookie | 上游账号认证 | 每次从当前账号重建 |
| `x-codex-installation-id` 及其 metadata 投影 | 安装/设备指纹 | 由实例 HMAC secret 按账号稳定派生 |
| `x-codex-turn-state` | 上游 opaque sticky state | 同账号可复用，换号必须删除 |
| `previous_response_id` | 上游历史句柄 | 只在 owner 账号/socket 使用；full replay 时删除 |
| session/thread/conversation/turn/window/parent ID | 客户端会话拓扑 | 原样透传，不按账号改写 |
| `prompt_cache_key`、`x-client-request-id` | 会话缓存与请求关联 | 客户端提供时原样透传 |
| output item `id`、`encrypted_content` | 上游输出引用 | full replay 时删除 |
| tool `call_id` | 工具输入输出配对 | full replay 时保留 |

`AccountScopedIdentity` 只包含 installation ID；`AccountScopedRequest` 只有在当前账号认证、Cookie、installation ID 和 turn-state 边界处理完成后才能进入 upstream transport。

## 数据存储

### PostgreSQL

PostgreSQL 保存：

| 数据 | 表 |
| --- | --- |
| 管理员 | `admin_users` |
| 客户端 Key | `client_api_keys` |
| 运行时设置 | `runtime_settings` |
| 账号与 quota | `accounts` |
| 账号累计用量 | `account_usage` |
| 成功请求事实 | `usage_records` |
| 失败请求事实 | `ops_error_logs` |
| 请求聚合桶 | `request_time_buckets` |
| Cloudflare Cookie | `account_cookies` |
| Codex 指纹 | `fingerprints`、`fingerprint_update_history` |
| 迁移记录 | `schema_migrations` |

时间列使用 `timestamptz`，应用按 UTC 时间点读写，中国自然日和展示时区由 `infra::time` 定义。迁移版本严格递增并校验名称与 SQL checksum。

### Redis

Redis 使用统一 `cpr:` 前缀：

| Key | 内容 | 生命周期 |
| --- | --- | --- |
| `admin:session:<hash>` | 管理员会话 | 配置 TTL |
| `lease:refresh:<account_id>` | token 刷新租约 | PX TTL |
| `models:plan_snapshots` | 计划模型快照 | 刷新替换 |
| `affinity:v3:resp:<response_id>` | response owner | 4 小时 |
| `affinity:v3:conv:<conversation_id>` | conversation 索引 | 4 小时 |
| `affinity:v3:account:<account_id>` | account 索引 | 4 小时 |
| `affinity:v3:global:*` | 全局裁剪索引和字节计数 | 4 小时 |
| `cyber-policy:v2:session:<hash>` | 会话级账号排除 | 1 小时 |

Affinity metadata 单条上限 64 KiB、全局上限 128 MiB，并在写入时惰性裁剪。

### 本地目录

```text
.runtime/
├── data/
│   ├── identity_hmac_secret
│   ├── update-state.json
│   ├── update.lock
│   └── update-tmp/
├── logs/
├── postgres/
└── redis/
```

Compose 将应用配置只读挂载到 `/app/deploy/config.yaml`。应用数据、日志、PostgreSQL 和 Redis 均使用独立 bind mount；Redis 启用 AOF。

## 启动、任务与关闭

`serve` 按以下顺序启动：

1. 定位并严格解析 `deploy/config.yaml`。
2. 解析相对路径并注入 PostgreSQL/Redis secret。
3. 初始化日志和监听地址。
4. 连接 PostgreSQL，校验并执行迁移。
5. 连接 Redis。
6. 读取 `runtime_settings`。
7. 创建领域 store、service、OpenAI client 和账号池。
8. 读取或创建 `identity_hmac_secret`。
9. 初始化指纹、管理员和模型快照。
10. 启动后台任务并挂载 HTTP router。

后台任务由 `TaskCoordinator` 管理：

- Cookie 清理和事实表 retention。
- 模型目录刷新。
- token 与 quota 刷新。
- Codex 指纹更新。
- settings 订阅。
- WebSocket pool 生命周期。

关闭时停止接收新请求，结束 HTTP 长流和入站 WebSocket，关闭上游 WebSocket pool 与后台任务。连接总排空上限为 20 秒，单任务等待上限为 5 秒。

## 前端与路由

Vue 管理端位于 `frontend/src`，使用 Vue Router、Pinia 和按领域划分的 API client。生产构建写入 `frontend/dist` 并由 Rust 进程托管。

| 路径 | 作用 |
| --- | --- |
| `GET /healthz` | PostgreSQL/Redis 健康检查 |
| `POST /v1/responses` | Responses JSON 或 SSE |
| `GET /v1/responses` | Responses WebSocket upgrade |
| `POST /v1/responses/review` | Review 入口 |
| `GET /v1/models*` | 模型目录和运行信息 |
| `/api/admin/*` | 管理端 API |
| 其他非 API 路径 | Vue SPA 静态资源 |

客户端接口使用 `sk_` API Key。管理端支持登录 Cookie 和管理员 API Key。请求 body 与 WebSocket 文本帧上限均为 16 MiB。
