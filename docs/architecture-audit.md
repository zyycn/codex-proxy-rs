# 架构审计：职责边界、WebSocket 收敛与前端治理

> 初审日期：2026-07-17<br>
> 初审基线：`dc2399d9` 及当时工作区快照<br>
> 复核基线：`3398fa95`（2026-07-17）<br>
> 范围：`backend/src`、`backend/tests`、`frontend/src`、`docs/architecture.md`<br>
> 性质：本文件是问题清单和实施建议，不替代 `docs/architecture.md` 的架构约束。

> 实施进度（2026-07-17）：当前未提交工作区已完成阶段 0 至阶段 4，最终全量验证通过。

用户给出的 `/home/zyy/桌面/Codes/codex-proxy-rs` 会解析到当前仓库
`/home/zyy/Codes/codex-proxy-rs`，因此两条路径指向同一份代码。

初审期间并行开发的账号探测、dispatch、模型刷新和前端账号连接测试改动，已在
`3398fa95` 合并。复核确认这些改动没有解决本文列出的三个 WebSocket P0 问题，也没有改变后端主要依赖方向。
文中的路径和行号用于定位 2026-07-17 快照；后续代码变化后应按符号重新搜索，不应把行号当作长期契约。

本次复核同时更正一项前端治理原则：**类型以推导为主，不主动维护可由实现推导的镜像类型。**
外部数据先在边界收窄或归一化，局部变量、函数返回值、`computed`、composable 返回值和 view model
默认交给 TypeScript 推导；只有组件 Props/Emits、不可由实现稳定推导的领域联合状态、可复用泛型边界等真实契约才显式声明。

## 结论

当前项目不是“没有架构”，而是**局部洋葱清楚、全局依赖没有闭合**：

- `v1/*` 的 Request → Attempt → Stream 生命周期已较好地使用类型状态、一次性 finalize 和 pre-send/post-send 边界，值得保留。
- 管理 API、账号池、遥测、设置、上游适配器之间仍存在跨层取数、反向依赖和展示模型泄漏；全局洋葱只完成了一半。
- 不建议增加一个全局、万能的 `application/` 层。更合适的是在每个垂直功能内增加小型 query/use-case service，并把端口定义在消费方。
- 五个 WebSocket 文件共 3,453 行，应该新建 `transport/websocket/` 子目录；但必须先修复三个确定性的状态竞态，再做纯目录移动。
- 前端已经具备 `view → composable → API module → request` 的外形，Vue 写法也较统一；主要缺口是外部数据边界仍让 `any` 扩散、副作用归属、共享状态所有权、并发查询和重复展示逻辑。
- 不建议为了迎合某种风格把 Tailwind 4 全量迁移为 UnoCSS，也不建议为了“分层”把单体立即拆成多 crate 或大量空 trait。

## 优先级总览

| 优先级 | 问题                                                      | 影响                                                | 首个动作                                 |
| ------ | --------------------------------------------------------- | --------------------------------------------------- | ---------------------------------------- |
| P0     | half-open breaker 取消后可永久停在 `HalfOpen`             | 对应 origin 此后一直回退 HTTP，直至 client/进程重建 | 补 cancellation 回归测试并修状态释放     |
| P0     | connection-local continuation 任取 logical candidate      | 正确 socket 存在时仍可能续接失败或选错分支          | 让 pool 按 `response_id` 精确选择        |
| P0     | 新连接先公开为 Idle，再交付原发起者                       | 后来请求可抢走发起者刚建好的 socket                 | 原子完成 `Connecting → Busy(owner)`      |
| P1     | API 层承担跨域查询与业务聚合                              | 改字段、配额或排序会同时牵动 API、fleet、telemetry  | 建立 feature-scoped query service        |
| P1     | `fleet` 反向依赖 `telemetry/settings/upstream` 的具体模型 | 规则 owner 和依赖方向与架构文档冲突                 | 将消费端 port/value 移入 `fleet`         |
| P1     | 前端 HTTP/SSE transport 依赖或散落到 UI                   | 401、更新流、连接测试无法独立测试和复用             | 下沉 transport adapter，注入认证失效处理 |
| P1     | 前端外部数据边界让 `any` 扩散                             | 非法响应可静默进入视图和状态                        | 边界收窄/归一化，业务代码保持类型推导    |
| P1     | 列表请求和 system-update 所有权不明确                     | 旧响应覆盖、未处理异常、共享 SSE 被其他实例清理     | `usePagedQuery`；Modal 上提 layout 单例  |
| P2     | WebSocket 模块平铺且 public 面过大                        | 隐式耦合、集成测试迫使内部 API 全公开               | 目录化后收窄 visibility/test-support     |
| P2     | 重复 quota、图表、header、CSS token 和格式化函数          | 同规则多 owner，视觉与语义容易漂移                  | 先抽纯函数/语义组件，再删可证实冗余      |

前三项 P0 已在当前工作区修复并通过专项及全量测试；表格保留原问题和影响，作为设计动机与回归依据。

## 1. 洋葱模型利用程度

### 1.1 已充分利用的局部

`dispatch/lifecycle` 是当前最清楚的边界：

- `dispatch/lifecycle/attempt.rs:67-107` 用 `OpenAttempt` 和 `CommittedAttempt` 区分仍可 retry 与已经 commit 的状态。
- `dispatch/lifecycle/pipeline.rs:18-52` 把一次 attempt 的执行路径收敛到单 pipeline。
- `dispatch/lifecycle/finalizer.rs:25-33,74-104` 消费 `self` 完成一次性 finalize，避免重复提交。
- WebSocket 的 `PreparedWebSocket` 也保留了重要边界：只有 pre-send 阶段允许安全 fallback；payload 可能送达后不自动重放，见 `transport/websocket.rs:430-468,840-925`。
- `websocket_breaker.rs` 和 `websocket_pump.rs` 分别是策略原语与 IO actor，基本不反向依赖 client/domain。

这些设计与 `docs/architecture.md:30-36` 的 Request → Attempt → Stream、单 owner、禁止 post-send 重放相符，不应在收敛中退化为布尔标志和散落的 retry 分支。

### 1.2 没有闭合的全局边界

当前真实依赖包含以下越界：

```text
api ───────────────> AccountStore / fleet / telemetry / settings
fleet ─────────────> telemetry::AccountUsageStore
fleet ─────────────> upstream transport/protocol concrete types
models/fleet ──────> upstream-defined ports
upstream client ───> websocket pool
websocket pool ────> client::CodexResponseMetadata
```

其中最明显的事实是：

- `api/router.rs:36-59` 的 `ApiServices` 暴露约二十个具体 service/store。
- `api/admin/dashboard_routes.rs:215-455` 自己读取账号、账号池、用量、设置并推导状态与配额。
- `api/admin/accounts_routes/query.rs:64-391` 在 API 内负责 load-all、窗口选择、billing、排序和分页。
- `fleet/window.rs:1-3` 直接重导出 `telemetry::account_usage::should_reset_usage_window`。
- `fleet/pool/mod.rs:21-31,258-287` 依赖遥测 store/snapshot，并让遥测事实直接参与池状态。
- `websocket_pool.rs:543-550` 反向依赖 `client::CodexResponseMetadata`，形成 client/pool 概念环。

目标方向应是：

```text
HTTP/SSE contract
       │
       ▼
feature query/use-case service
       │
       ├── domain values / policy
       └── consumer-owned ports
                    ▲
                    │ bootstrap 绑定
       PostgreSQL / Redis / OpenAI adapters
```

这里的 `feature query/use-case service` 是按 dashboard、account-list、quota、refresh 等用例就地组织的小层，不是一个可以引用全仓的通用 `application` 大包。

## 2. 后端职责审计

### 2.1 API 层承担了业务查询和展示规则

问题：API 不仅“解析和编码”，还拥有跨域 join、窗口选择、quota 分组、状态判定、排序和展示格式。这与 `docs/architecture.md:32,84` 不一致。

证据：

- dashboard 聚合：`api/admin/dashboard_routes.rs:215-455`。
- account list 聚合：`api/admin/accounts_routes/query.rs:64-391`。
- `dashboard_routes.rs:14-23` 直接复用 sibling `usage_routes` 的 presenter DTO/helper，形成 API route 之间的横向依赖。
- `api/admin/usage_routes.rs:501-557` 与 `dashboard_routes.rs:535-595` 重复 UsageRecord 展示映射。

建议：

- 增加 `DashboardQueryService`、`AccountListQueryService` 等读用例。
- query service 返回不含中文标签、camelCase 和 CSS 语义的 read model。
- API presenter 负责 DTO casing、文本和显示格式；route 只做参数解析、鉴权和编码。
- 不让 query service 直接成为另一个全局 service bag。

### 2.2 `fleet` 的规则 owner 被遥测和上游模型穿透

问题：架构文档说 telemetry “保存已经确定的事实，不参与调度”，但账号池会读取 telemetry snapshot 决策；同时 fleet 直接消费上游协议类型。

证据：

- `fleet/window.rs:1-3` 反向重导出 telemetry 的窗口规则。
- `fleet/pool/mod.rs:258-287` 用 `AccountUsageStore` snapshot 更新 pool。
- `fleet/pool/mod.rs:549-666` 直接消费 upstream `TokenUsage`、rate-limit headers/parser。
- `fleet/manage/probe.rs:329-423` 在 fleet 内手工解析原始 SSE stream。

建议：

- `fleet/usage` 拥有 `UsageDelta`、`QuotaObservation`、窗口 reset 规则和消费端 port。
- telemetry/PG adapter 实现该 port；它仍是事实存储，而不是策略 owner。
- upstream adapter 把 raw SSE/header 转为 typed probe result、quota observation 后再交给 fleet。
- pool 只认识账号调度所需的领域值，不认识 OpenAI header 或 SSE wire format。

### 2.3 端口定义在 provider 侧，具体 adapter 泄漏到服务

典型情况：

- models service 依赖 upstream 定义的 `CodexModelCatalogClient`。
- `upstream/token_client.rs` 把 `TokenRefresher` port 与 reqwest adapter 放在一起，fleet refresh 再反向消费。
- fleet refresh/manage service 持有 PG、Redis、Codex 等具体依赖。

端口应由消费用例定义，并只为真实 IO seam 建 trait：

- `models` 定义 `ModelCatalogSource`。
- `fleet/refresh` 定义 `TokenRefreshGateway`、租约和持久化 port。
- `bootstrap` 把 OpenAI/PG/Redis adapter 绑定进去。
- 纯计算函数和值对象不需要为了“洋葱”套 trait。

### 2.4 全局设置快照传播过宽

`SettingsSnapshot` 同时进入 fleet pool、refresh policy、models service，并由各自订阅。它让一个新增设置字段容易扩散到多个模块。

建议由 bootstrap 订阅一次，再映射为窄配置：

- `AccountPoolOptions`
- `RefreshPolicy`
- `ModelConfig`

领域服务只接收自己拥有的配置，不知道全局设置 schema。

### 2.5 领域、read model 与 presenter 混在一起

- `fleet/manage/quota_view.rs` 包含 serde camelCase、中文标签和 display 字段。
- `telemetry/dashboard.rs` 包含序列化与显示字符串。
- API 同时还在重新聚合和格式化这些对象。

建议把 raw read model 留在领域/query service，把语言、casing、格式化和前端兼容字段留在 API presenter。否则 UI 改文案也会触碰 fleet/telemetry。

### 2.6 quota 规则有多个 owner

以下位置重复 5 小时/周/月窗口、容差和 JSON 选择逻辑：

- `fleet/manage/quota_view.rs:14-16,148-170,349-405`
- `api/admin/dashboard_routes.rs:52-54,463-525`
- 前端 `AccountQuotaSummaryCell.vue:28-79`
- 前端 `AccountQuotaPanel.vue:21-67`

同时 upstream 已有 typed `ParsedRateLimits`，后续却转成 JSON，再被下游重新解析。

建议建立唯一的 `fleet/quota` owner：

```text
QuotaSnapshot
└── Vec<QuotaWindow>
    ├── kind: ShortTerm | Weekly | Monthly | Other
    ├── used_percent
    ├── reset_at
    └── order
```

前端只负责 clamp、颜色和显示，不再根据秒数猜业务分组。

### 2.7 typed telemetry 在 recorder 又退回 stringly JSON

`telemetry/recorder.rs:252-348` 会重新提取 camelCase/snake_case key、删除字段并推断内容；`:432-447` 又解释 `CodexResponsesRequest`。`telemetry/usage/types.rs` 同时保留显式字段和 `Value` bag。

建议 dispatch telemetry controller 一次性构造完整 typed fact，recorder 只持久化。未知扩展字段可放显式 `extensions`，但不能让 recorder 重新拥有协议解释规则。

### 2.8 错误语义被多层重复翻译

dispatch error 同时包装 transport/SSE failure、决定 HTTP status 和 camelCase metadata，API client error 又映射一次。

建议：

- dispatch 只拥有 retry/commit/exhaustion 等语义 enum。
- upstream 保留可追溯的 typed transport source。
- API 统一映射 HTTP/OpenAI error envelope。
- telemetry 单独把语义 error 映射为事件事实。

### 2.9 bootstrap 和 service bag 过大

`bootstrap::Services` 与 `api::ApiServices` 都是高扇出的依赖袋，且 bootstrap 还要逐字段转换。新增一个管理功能会迫使根对象和 router 一起变化。

建议按入口组合：

- `ProxyRuntime`
- `AdminQueries`
- `AdminCommands`
- `BackgroundTasks`

这些只是 composition grouping，不应变成可从任意模块访问的 service locator。

### 2.10 测试策略迫使生产 public 面过大

`backend/src/lib.rs` 将所有顶层模块公开，而 `backend/tests` 的黑盒测试又直接使用很多内部构造器：

- `Services::new`
- `build_codex_headers`
- `CodexWebSocketConnection::new/responses`
- `CodexWebSocketPool::new`
- breaker 的 config/decision helper

架构测试还大量使用 `source.contains(...)` 和字面量计数，只能守局部代码形状，不能证明 `api !-> store`、`fleet !-> telemetry` 等全局依赖方向。

建议：

- 优先用 `pub(crate)`/`pub(super)` 和模块结构让编译器守边界。
- 纯策略的 private unit test 可就地放置；若必须坚持 `backend/src` 无测试，则创建显式 `test-support` feature/crate，只公开少量 fixture。
- 用 AST/dependency graph guard 检查全局 forbidden edges；源码字符串断言只保留给确实无法由类型系统表达的形状约束。

## 3. WebSocket 专项审计

### 3.1 是否应新建文件夹

**应该。** 但不是把五个文件简单合并或只换路径。

| 文件                   | 行数 | 当前真实职责                                                           | 处理方式                                |
| ---------------------- | ---: | ---------------------------------------------------------------------- | --------------------------------------- |
| `websocket_breaker.rs` |  262 | circuit config、状态迁移、RAII permit                                  | 修 cancel 后只移动                      |
| `websocket_pump.rs`    |  532 | keepalive、actor handle、socket event loop、backpressure               | 只移动，暂不再拆                        |
| `websocket_pool.rs`    |  866 | key、slot、acquire、lease、connect、maintenance、metadata、diagnostics | 拆到 `pool/mod.rs` 与子模块             |
| `websocket_frames.rs`  |  831 | error/DTO、aggregate reducer、stream forwarder、pool return            | 改为 `exchange/mod.rs` 与子模块         |
| `websocket.rs`         |  962 | audit IO、model、handshake、pool/breaker 编排、exchange facade         | 改为窄 `websocket/mod.rs`，其余职责下沉 |

当前 `websocket.rs:175-176` 用 `#[path = "websocket_frames.rs"] mod frames` 构造目录里不可见的父子关系，`websocket_frames.rs:3-5` 又用 `use super::*` 隐藏全部依赖；frames 还反向调用父模块的 prepare/execute。这是平铺结构已无法表达模块树的直接证据。

### 3.2 三个应先修的确定性问题

#### P0-A：half-open 取消后永久卡住

**状态：当前工作区已修复。** `cancel` 仅在仍持有同一 probe ownership 时把状态恢复为可立即重新探测的 Open，不把账号驱逐或服务关闭记作 origin failure；新增真实账号驱逐回归测试验证下一次 probe 可获取。

状态路径：

1. breaker 从 Open 到期进入 `HalfOpen`：`websocket_breaker.rs:87-97`。
2. 账号驱逐或 shutdown 取消建连任务：`websocket.rs:668-674`。
3. `permit.cancel()` 只设置 `armed = false`，不改变 breaker state：`websocket_breaker.rs:197-200`。
4. 后续申请全部得到 `HalfOpenBusy`，且原 permit 已销毁，再无人能 success/fail。

结果是该 origin 会一直走 HTTP fallback，直到重建 client/breaker。修复时 cancellation 不应计作 origin failure，但必须释放 probe ownership，使其可立即或按策略再次探测。

#### P0-B：continuation 没有按 response ID 精确查找

**状态：当前工作区已修复。** pool acquire 现在接收目标 `response_id`，遍历同一 logical connection 的 slot 精确匹配；Busy reservation 同样保留其 latest response ID。找不到、已过期或已死亡时直接 fail-closed，不创建新连接。新增两个 profile socket 并发续接测试，分别验证请求到达对应 socket。

pool key 包含 logical key 和 connection profile：`websocket_pool.rs:28-33`。当当前 profile key 不存在时，`:195-203` 用 `HashMap::find` 任取一个 logical candidate；真正的 `latest_response_id` 校验直到连接被取走后才发生：`websocket.rs:491-500`，失败也不会继续找其他候选。

同一 account/conversation 可以保留多个 profile socket，已有测试覆盖这一事实：`backend/tests/upstream/openai/transport/headers.rs:607-720`。因此正确 socket 明明存在时，分支切换仍可能得到 `LatestResponseMismatch` 或 busy。

建议让 acquire 显式接收 continuation response ID，并维护 `(logical_key, response_id) → slot` 索引；至少也要遍历全部候选后按 continuation state 选择。`allow_profile_mismatch: bool` 信息不足。

#### P0-C：新建连接交付可被抢占

**状态：当前工作区已修复。** 建连成功后原子执行 `Connecting → Busy(owner reservation)`，并通过 oneshot 直接交付 connection + lease；若原发起者已超时、receiver 关闭，后台任务才把连接回收到 Idle。现有 singleflight 测试已强化为同时断言原发起者获得 `ConnectedWebSocket`。

`websocket_pool.rs:336-358` 先把 `Connecting` 改成任何请求都可租用的 `Idle`；随后 `websocket.rs:685-690` 才通知原发起者。原发起者收到通知后还要在 `:602-607` 再次 `take_idle`。

这个窗口内后来请求可以先抢走 socket，使原发起者得到 `PoolUnavailable`。required WebSocket/warmup 请求尤其会出现“自己建好、自己失败”。

应原子执行 `Connecting → Busy(owner reservation)`，直接把 connection 与 lease 交给原发起者；若 receiver 已消失，再发布为 Idle。

### 3.3 其他高风险状态管理

- **已修复：** pool 现在同时执行单账号上限、全局 64 slot 上限和全局 16 opening semaphore；总量满时只淘汰 LRU Idle，不抢占 Busy/Connecting。8 账号并发压力测试分别覆盖 opening cap 与 total cap，另有顺序测试覆盖 LRU 回收。
- **已修复：** pool 状态改用短临界区同步 mutex，lease `Drop` 直接按 reservation ownership 释放 Busy/Connecting，不再依赖 Tokio runtime 或 detached cleanup。
- **已修复：** pool-owned stream forwarder 由同一 `TaskTracker` 托管，并监听 pool shutdown token；shutdown 会取消、关闭 socket 并等待 forwarder 退出。
- **已修复：** runtime 外构造的 pool 在首次 acquire 时补启动 supervisor；零 maintenance interval 明确按禁用处理，不创建 panic task。

pool 状态临界区不执行 await 或网络 IO，因此当前实现选择同步 ownership compare-and-remove，而不是增加 release actor 和命令队列；socket close 仍在锁外异步执行。

### 3.4 模块内可收敛项

- **已修复：** `PreparedWebSocket` 使用 `PoolBinding::{Unpooled, Pooled { lease, reused, decision }}`，非法字段组合不可构造。
- **已修复：** aggregate 和 streaming 共用 `ExchangeAction` reducer，metadata、rate limit、turn state、completed ID 与 terminal 只解释一次。
- **已修复：** `collect_websocket_response` 返回命名的 `CollectedWebSocket`。
- **已修复：** handshake 复用 `endpoints::CODEX_RESPONSES_PATH`，不再维护重复常量。
- **已修复：** 删除 `wait_for_shared_connect` 未使用的 decision 时间参数。
- **已修复：** 错误改名为 `ReusedConnectionDiedBeforeFirstEvent`，与实际判定一致。
- **已修复：** `PostSendAmbiguous` 保留稳定 message，同时通过 error source 保存原 typed failure。

**已修复：** `CodexWebSocketUpstreamError` 已删除唯一构造器永远不填充的 `error_type/code/message/param/headers`；错误分类只从真实 body/diagnostics 推导，正常 client 路径仍统一映射为通用 Upstream error。

### 3.5 目标目录

本仓库采用传统 `mod.rs` 风格：目录就是模块边界，模块声明和精选 re-export 放在对应目录的 `mod.rs`，不使用 `websocket.rs + websocket/`、`pool.rs + pool/` 这种混合入口。

```text
backend/src/upstream/openai/transport/
└── websocket/
    ├── mod.rs                   # 窄入口：模块声明、精选 re-export
    ├── audit.rs                 # artifact IO、header redaction
    ├── model.rs                 # request、continuation、prepared/result
    ├── error.rs                 # 分阶段 typed errors
    ├── handshake.rs             # endpoint、TLS、opening request、send
    ├── coordinator.rs           # pre-send/post-send、pool、breaker 编排
    ├── breaker.rs               # 原 breaker，只移动
    ├── pump.rs                  # 原 pump，只移动
    ├── pool/
    │   ├── mod.rs               # pool config/acquire 与子模块声明
    │   ├── state.rs             # key、slot、continuation index
    │   ├── lease.rs             # ownership/waiter/handoff
    │   └── supervisor.rs        # maintenance、shutdown、global cap
    └── exchange/
        ├── mod.rs               # exchange 类型与子模块声明
        ├── reducer.rs           # aggregate/stream 共用状态机
        ├── collect.rs
        └── stream.rs
```

固定依赖方向：

```text
client → coordinator
coordinator → handshake / breaker / pool / exchange
pool → model / pump
exchange → model / pump / protocol / response_meta
handshake → model / pump / tls
pump → tokio / tungstenite
```

把 `CodexResponseMetadata` 从 client 移到中立的 `response_meta.rs` 或 websocket model，即可消除 client/pool 反向依赖。

### 3.6 迁移顺序

1. 先补 half-open cancel、multi-profile exact continuation、initiator handoff 抢占、zero/outside-runtime config 四组回归测试。
2. 在当前目录先修 breaker cancel、精确 continuation lookup 和原子 handoff；不要把行为修复混入大规模 move。
3. 建立 `websocket/mod.rs`，机械移动 breaker/pump，原子更新 import，不保留 `#[path]` 或旧模块兼容 wrapper。
4. 移动并拆为 `pool/mod.rs` 与子模块；保持现有 global cap/LRU 行为，再把 maintenance/shutdown 收敛到 supervisor。
5. 抽共用 `ExchangeReducer`，再移动到 `exchange/mod.rs` 与 collect/stream，删除 `use super::*`。
6. 拆 audit/handshake/coordinator/model/error，把 `websocket/mod.rs` 保持为窄入口。
7. 最后收窄 public 面、清理 error 死字段，并按新模块树拆测试。

## 4. 前端架构与 `$antfu` 审计

### 4.1 已做对的部分

- 80/80 个 Vue SFC 都使用 `<script setup lang="ts">`。
- 11 个 style block 全部是 scoped，未发现 prop mutation。
- route view 都是 lazy import，ECharts/ZRender 也有显式 chunk split。
- `v-html` 仅用于 release notes，并经过 `utils/markdown.ts:10-20` 的 DOMPurify。
- 全局样式已拆为 token/base/index，当前问题不是“CSS 完全失控”。

因此不需要改写为另一套 Vue 范式；应继续 Composition API + TypeScript，重点修外部数据和副作用边界。

### 4.2 HTTP/SSE 基础设施反向依赖 UI

- `api/request.ts:50-62` 在 401 时动态导入 auth store 和 router，底层 HTTP client 因而依赖状态层和导航层。
- `composables/useSystemUpdate.ts:86-140` 直接创建和维护 `EventSource`，`:234-292` 又负责轮询和页面 reload。
- `views/accounts/composables/useAccountConnectionTest.ts:95-101,310-315` 也在 feature composable 拼 SSE URL、管理 transport。

目标：

- `request.ts` 只发送、解包并归一化 `ApiError`。
- app bootstrap 注入 `onUnauthorized` 或订阅 typed auth event。
- HTTP/SSE 都由 `api/modules`、`api/streams` transport adapter 负责。
- composable 只拥有用例状态和 reducer，不创建 EventSource、不拼 endpoint。
- `window.location.reload()` 等动作放到明确 browser/application adapter。

### 4.3 外部数据边界让 `any` 扩散

审计快照中：

- `request<T = any>`：`api/request.ts:95`。
- API modules 有 35 个函数，26 个入参是 `any`。
- `frontend/src` 有 217 次 `any`，分布于 56 个文件。
- `useDashboard.ts:15` 连 composable 返回值也是 `any`。
- settings 的 `RotationStrategy`、`RotationOption`、`AliasRow`、`AdminApiKeyStatus` 在 route 和子组件重复声明。

治理原则不是为每个 endpoint 手写一套 DTO，也不是给所有函数补返回类型：

- 局部变量、对象字面量、函数返回值、`computed` 和 composable 返回值默认依赖推导，不写重复注解。
- API 原始响应属于不可信边界。按 endpoint 逐步把它从 `any` 收敛为 `unknown`，在 adapter 内做最小运行时检查或归一化，再让下游从归一化函数的返回值推导类型。
- 不用 `as ResponseDto` 冒充运行时校验；如果没有检查器，类型断言不能证明上游响应符合结构。
- 不为目录整齐主动创建 `api/contracts/*.ts`、feature `types.ts` 或镜像后端 JSON 的接口。只有真实复用并能作为稳定契约时才提取文件。
- 显式类型保留给 Vue Props/Emits 等跨组件边界、领域判别联合、无法由初始化值正确推导的状态，以及确实需要约束调用方的可复用泛型 API。
- 对 `request<T = any>` 不做一次性全仓替换，否则只会制造大量断言。应随 endpoint adapter 迁移改为安全默认值，最终让 `any` 不再穿透到 feature/view。

因此，本节的目标是“边界可验证、内部可推导”，不是“显式类型越多越好”。

### 4.4 Sidebar/Modal/system-update 的所有权冲突

- layout 同时保留桌面 Sidebar，并在移动抽屉打开时挂第二个 Sidebar：`layout/index.vue:40,63`。
- 每个 Sidebar 都实例化 About 和 SystemUpdate Modal：`AppSidebar.vue:519-520`。
- `useSystemUpdate.ts:24-49` 的 refs 是模块级隐式 singleton，三个消费者共享。
- `SystemUpdateModal.vue:300-302` 卸载时会断开这条共享 EventSource。

因此移动 Sidebar 出现/消失时，可能清理桌面 Modal 正在使用的共享连接。建议 layout 只挂一份 Modal，Sidebar 仅 emit open intent。若更新状态确实是应用级 singleton，用显式 Pinia store，由 app shell 管理 transport 生命周期，并只向组件暴露 readonly state + actions。

### 4.5 列表查询存在旧响应覆盖和异常窗口

accounts、API keys、usage 三个 filter composable 都采用后绑定 loader；accounts/API keys 没有 request sequence 或 AbortController，并发筛选或翻页时旧响应可覆盖新状态。accounts 的 loader 还没有 catch，而调用方通过 `void loadAccounts()` 丢弃 rejection。

usage 已有正确对照：`useUsageRecordsTable.ts:38-46,75,98-105` 用 request id 忽略旧响应。

抽取 `usePagedQuery`：

- loader 通过 options 显式注入，不再 `bindXxxLoader()`。
- 统一 page/pageSize/search/sort、debounce、loading/error。
- 支持 AbortController；不支持时至少 latest-request-wins。
- catch 参数使用 `unknown` 和共享 `errorMessage()`。

### 4.6 职责过重文件

| 文件                          |   规模 | 混合职责                                                         | 建议                                        |
| ----------------------------- | -----: | ---------------------------------------------------------------- | ------------------------------------------- |
| `useDashboard.ts`             | 525 行 | IO/刷新、默认 DTO、icon view model、格式化、趋势映射、颜色阈值   | 拆纯映射/格式/常量模块，composable 只留状态 |
| `useAccountMutations.ts`      | 518 行 | 查询、导入、OAuth、删除/批删/导出、token/quota refresh、状态切换 | 拆 query/import/oauth/row mutations         |
| `useAccountConnectionTest.ts` | 467 行 | API、SSE、解析、日志 view model、状态机、UI metadata             | stream adapter + reducer + orchestration    |
| `settings/index.vue`          | 369 行 | form adapter、alias validation、load/save、admin key、clipboard  | `useSettingsForm`、`useAdminApiKey`         |
| `RequestTrendCard.vue`        | 529 行 | ECharts option/axis/tooltip/series/interaction                   | `useRequestTrendChart.ts`                   |
| `AppSidebar.vue`              | 532 行 | 导航、认证、主题、Modal、GSAP                                    | Modal 上提，抽 `useSidebarMotion`           |
| `SystemUpdateModal.vue`       | 597 行 | 更新编排、摘要、日志、notes、确认、markdown 样式                 | 建 `layout/system-update/` feature          |

文件行数本身不是问题；表中项目之所以应拆，是因为 IO、业务映射和展示状态有不同变化原因。

### 4.7 可收敛的逻辑与样式

#### 明确重复逻辑

- `useAccountFilters.ts`、`useApiKeyFilters.ts`、`useUsageFilters.ts`：分页、防抖、loader 绑定高度同构，收敛为 `usePagedListFilters`/`usePagedQuery`。
- `useAccountsTable.ts` 与 `useApiKeysTable.ts`：页内选择逻辑同构，抽 `usePageSelection<T>`。
- `AccountQuotaSummaryCell.vue` 与 `AccountQuotaPanel.vue`：percent clamp、tone、bar/text class、visible 判断重复，抽纯 `quota.ts`。
- `AccountQuotaPanel.vue:98-201`：monthly/shortTerm/other 复制同一模板，改成 `AccountQuotaWindow.vue` + `v-for`。
- 三张 usage chart card 重复 animation、grid、legend、card padding 和 chart shell；继续扩展现有 `views/usage/utils/chart.ts`，不要造一个承载业务的万能图表组件。
- `readCssVariable()` 在三处重复，抽到 browser-only `utils/css.ts`。
- dashboard、usage、API keys、accounts、settings 的 page header 结构与 `text-[34px]` 完全重复，抽 `BasePageHeader.vue`。
- `AppSidebar.vue:303,309` 在同一 watcher 对 `.sidebar-label` 重复启动 tween；保留一次，并只在 DOM 条件变化时 `nextTick()`。

#### 样式治理判断

当前 arbitrary utilities 高频项为：

| 片段             | 次数 |
| ---------------- | ---: |
| `text-[12px]`    |   95 |
| `font-[650]`     |   88 |
| `font-[760]`     |   62 |
| `text-[13px]`    |   51 |
| `leading-[1.15]` |   47 |
| `text-[34px]`    |    6 |

重复 utility 不等于必须抽 class。只对稳定的语义组合建立 `BasePageHeader`、`SectionTitle`、`MetricValue`、`StatusBadge` 等组件/shortcut；不要建立全局 `@apply` 汤，也不要把一次性布局抽成无语义 class。

`UsageRecordDetailModal.vue:146` 读取不存在的 `--cp-focus-ring`，因此总走 `#8B5CF6` fallback。应使用集中 palette 或新增有明确语义的 reasoning/chart token。

### 4.8 死代码与无效公开面

Knip 没发现整个未引用文件，不能写成“存在大量孤儿文件”。手工复核确认以下函数全仓只有定义：

- `views/usage/constants.ts:151`：`tokenTotal`
- `views/usage/constants.ts:176`：`formatUsageMetric`
- `views/usage/constants.ts:180`：`formatCompactUsageMetric`
- `views/usage/constants.ts:194`：`formatLatencyAverage`
- `views/usage/utils/format.ts:19`：`formatNumber`
- `views/usage/utils/format.ts:27`：`formatSignedPercent`

以下实现有文件内消费者，不是死代码，只应取消无必要的 `export`：

- `BaseTable/columns.ts`：`normalizeWidth`、`isEmptyCellValue`、`BaseTableSortDirection`
- `api-keys/utils/ccswitchImport.ts`：`CODEX_CC_SWITCH_MODEL`
- `usage/constants.ts`：`formatTokenCount`
- `BaseToast/toast.ts`：`ToastMessage`
- `useSystemUpdate.ts`：`SystemUpdateLog`

`api/index.ts:23` 对 `request` 的 barrel re-export 也没有消费者。

### 4.9 CSS token 删除候选

`tokens.css` 有 48 个外部零引用 token。其中 `--cp-control-focus-ring` 会在该文件内被两个 live shadow token 间接使用，不能删除；其余 47 个在全仓和 token 文件内部都零引用，也未通过 `setProperty`、字符串拼接或动态模板消费。

```text
--cp-bg-primary
--cp-button-radius-small
--cp-control-bg
--cp-control-bg-hover
--cp-control-border
--cp-control-border-hover
--cp-control-boundary
--cp-control-boundary-focus
--cp-control-height-default
--cp-control-height-large
--cp-control-height-small
--cp-control-highlight
--cp-control-padding-x
--cp-control-radius-base
--cp-control-radius-small
--cp-danger-hover
--cp-danger-on
--cp-danger-pressed
--cp-danger-ring
--cp-default-bg
--cp-default-text
--cp-default-text-active
--cp-default-text-hover
--cp-disabled-border
--cp-gap-card
--cp-info-bg-active
--cp-info-bg-hover
--cp-input-height-compact
--cp-input-height-inline
--cp-input-height-large
--cp-normal-bg-active
--cp-normal-bg-hover
--cp-normal-on
--cp-popper-radius
--cp-radius-circle
--cp-scrollbar-track
--cp-sidebar-collapsed-width
--cp-sidebar-width
--cp-space-page-x
--cp-space-page-y
--cp-success-hover
--cp-success-on
--cp-success-pressed
--cp-tag-radius
--cp-warning-hover
--cp-warning-on
--cp-warning-pressed
```

建议按语义组删除并做 light/dark 截图回归，不要一次删除后只依赖编译，也不要误删 `--cp-control-focus-ring`。

### 4.10 工具链建议

当前 `frontend/package.json` 只有 build/Prettier，没有 ESLint、Vitest、Storybook 或 Playwright。

按 `$antfu` 方向建议：

- 引入 `@antfu/eslint-config`，统一 TypeScript/Vue/a11y/import/style 检查。
- 若使用 ESLint formatter，则移除独立 Prettier 格式链，避免两套 formatter 并行争夺格式。
- 用 Vitest 覆盖 mappers、quota helpers、分页 latest-request-wins、SSE reducer。
- 只有在组件隔离开发确有收益时再引入 Storybook；它不是当前首要缺口。
- 不为风格一致性全量迁移 Tailwind 4 → UnoCSS。

`echarts` 已独立 chunk。`BaseMotionIcon` 因轻量 hover reaction 引入 GSAP 且被多个首屏/主要页面使用，但这不是仅凭包大小就能判定的缺陷；应结合 coverage、首屏网络和低端设备 profile 决定是否用 CSS/WAAPI 替代，并保留 reduced-motion。

## 5. 其他死代码/异味候选

| 项目                                                                        | 证据                                                          | 判断                                                      |
| --------------------------------------------------------------------------- | ------------------------------------------------------------- | --------------------------------------------------------- |
| account DTO 的 `empty_response_count`、`reasoning_tokens`、image token 字段 | `api/admin/accounts_routes/mod.rs:228-258` 恒为 0，前端无引用 | 高置信删除或从真实 read model 填充，先确认外部 API 消费者 |
| `event_kind(route)`、`api_kind(route)`                                      | `telemetry/recorder.rs:377-385` 丢弃 route 并返回常量         | 删除参数或直接用常量                                      |
| WebSocket test-only constructors                                            | 生产无调用，integration tests 直接依赖                        | 不是功能死代码；应移入 test-support/收窄 public           |
| 全仓 729 个顶层 `pub` item                                                  | 单进程、`publish = false`，测试主要在 crate 外                | public 面偏大，逐模块收窄，不能机械全改                   |
| architecture source-string guards                                           | 局部 `contains`/字面计数                                      | 易受改名影响，且守不住全局依赖方向                        |

“静态只有定义”仍不等于可无条件删除公开 API。后端 DTO、admin API 和测试构造器在删除前要确认仓外消费者；本文对前端内部函数和 CSS token 的结论更强，因为已检查仓内静态引用和动态 token 构造。

当前工作区已删除 account admin DTO 中始终为 0 且前端无消费者的空响应、reasoning 和 image token 字段；`response_event_kind(route)`、`response_api_kind(route)` 的无效参数也已改为稳定常量。WebSocket integration test 仍通过正式 public facade 验证协议和并发行为，本轮没有机械收窄全仓 public item；全局依赖方向改由 AST 守卫覆盖。

## 6. 推荐实施路线

### 阶段 0：先恢复正确性与稳定基线

1. [x] 补并修三个 WebSocket P0 竞态。
2. [x] 增加 global connect/connection cap 的压力测试；默认采用 64 个全局 slot、16 个并发 opening。
3. [x] 保持当前全量 Clippy 和测试基线通过；修复 P0 时增加对应并发回归测试。

验收：

- half-open cancel 后可再次 acquire。
- 多 profile 下按 response ID 精确续接。
- 发起者 handoff 不可被并发请求抢占。
- required WS 不因自身新建连接被抢而失败。

### 阶段 1：机械目录收敛

1. [x] 移动 breaker/pump。
2. [x] 目录化 pool/exchange，并拆出 state/lease/supervisor 与 collect/reducer/stream。
3. [x] 删除 WebSocket 子树的 `#[path]`、`use super::*` 和旧模块兼容入口。
4. [x] 行为测试全绿后拆出 coordinator/handshake/audit/model/error。

验收：只有一个正式入口，不保留 transition shim；`websocket/mod.rs` 只做模块声明、精选 re-export 和必要的高层编排，`pool/mod.rs`、`exchange/mod.rs` 分别守住自己的子模块边界。

### 阶段 2：后端边界闭合

1. [x] 建 Dashboard/AccountList query service，route 退回 contract/presenter。
2. [x] quota 建唯一 typed owner，消除 JSON 往返解析和前端秒数猜测。
3. [x] 把 usage/window/probe 值与 port 移到 fleet 消费侧。
4. [x] bootstrap 将 SettingsSnapshot 映射为窄配置。
5. [x] 统一错误责任和 telemetry typed fact。

验收：

- `api` 不直接持有 AccountStore/PG/Redis。
- `fleet` 不 import `telemetry`，不解析 upstream raw SSE/header。
- consumer 不 import provider 为自己定义的 port。
- 展示文本/serde casing 不进入 fleet/telemetry domain model。

### 阶段 3：前端边界与所有权

1. [x] 按 endpoint 收窄 API 外部数据，在 adapter 归一化后依赖返回值推导；不批量手写 DTO。
2. [x] 401 inversion 和两类 SSE adapter。
3. [x] `usePagedQuery` 修 accounts/API keys 竞态与错误处理。
4. [x] Modal 上提 layout 单例，system update 显式 store 生命周期。
5. [x] 拆 dashboard/accounts/settings 大职责文件。

验收：核心 API 边界没有无约束 `any` 穿透；feature/view 主要依赖推导且没有重复镜像类型；分页具备 latest-request-wins；关闭移动 Sidebar 不会断开桌面更新流。

### 阶段 4：低风险冗余清理与守护

1. [x] 删除 6 个前端死函数、无效 export、47 个 token 候选。
2. [x] 抽 quota helper、page header、page selection、chart preset。
3. [x] 引入 ESLint/Vitest 和 dependency guard。
4. [x] 做 light/dark、mobile/desktop、reduced-motion 截图与交互回归。

## 7. 明确不建议做的事

- 不新增全局万能 `application` 层或 service locator。
- 不为每个纯函数建立 trait；trait 只用于真实 IO/替换边界。
- 不用兼容 re-export、`#[path]` 或双实现保留旧 WebSocket 路径。
- 不在一次提交中同时修竞态、搬目录和改协议状态机。
- 不因文件长就拆；以不同变化原因和副作用 owner 为拆分依据。
- 不把 Tailwind 4 全量迁移 UnoCSS 作为本轮架构治理目标。
- 不把高频 arbitrary utility 全部抽成 class；只收敛稳定语义组合。
- 不把 test-only public API 直接判为功能死代码；先解决测试边界。
- 不为了“类型完整”给可推导的局部值、函数返回值和 API 结果重复写 interface/type/`Promise<T>`；显式类型只服务真实边界和不变量。

## 8. 验证记录与限制

初审期间已执行：

- `pnpm run build`：通过。
- `pnpm run format:check`：通过。
- `pnpm dlx knip` + `rg` 复核：无 unused file；确认 6 个未引用函数和若干仅需取消 export 的符号。
- CSS token 完整边界搜索：48 个外部零引用，其中 47 个为全仓零引用候选，1 个在 token 内部仍活跃。

在复核基线 `3398fa95` 上，并行 WIP 已合并，重新验证结果为：

- `cargo +1.97 clippy --manifest-path backend/Cargo.toml --all-targets --locked -- -D warnings`：通过。
- `cargo +1.97 test --manifest-path backend/Cargo.toml --all-targets --locked`：759 项通过。
- `pnpm run build`：通过。
- `pnpm run format:check`：通过。

这些结果恢复了静态检查和现有测试基线，但现有测试没有覆盖下列并发反例，因此不能据此关闭三个 P0。

完成三个 P0 修复后的当前工作区验证结果为：

- `cargo +1.97 test --manifest-path backend/Cargo.toml --test main upstream::openai::transport::latency -- --nocapture`：17 项通过。
- WebSocket pool 专项：21 项通过，其中 8 账号并发覆盖 global opening/slot cap，顺序场景覆盖 LRU Idle 淘汰。
- profile rotation、backend source test boundary、WebSocket preparation boundary：通过。
- `cargo +1.97 clippy --manifest-path backend/Cargo.toml --all-targets --locked -- -D warnings`：通过。
- `cargo +1.97 test --manifest-path backend/Cargo.toml --all-targets --locked`：766 项通过。

因此三个 WebSocket P0 和阶段 0 的本地 global cap 压测在当前工作区关闭；尚未完成的是生产负载验证以及后续架构阶段。

完成阶段 1 WebSocket 模块收敛后的验证结果为：

- WebSocket/连接池专项：54 项通过，新增 active stream shutdown join、runtime 外构造后延迟启动 supervisor、零 maintenance interval 三项回归。
- `cargo +1.97 clippy --manifest-path backend/Cargo.toml --all-targets --locked -- -D warnings`：通过。
- `cargo +1.97 test --manifest-path backend/Cargo.toml --all-targets --locked`：769 项通过。

因此阶段 1 的目录迁移、模块拆分和 pool 生命周期风险在本地关闭；生产容量与延迟仍按下列动态验证项观察。

完成阶段 2 至阶段 4 后的最终工作区验证结果为：

- `cargo +1.97 fmt --manifest-path backend/Cargo.toml --all -- --check`：通过。
- `cargo +1.97 clippy --manifest-path backend/Cargo.toml --all-targets --locked -- -D warnings`：通过。
- `cargo +1.97 test --manifest-path backend/Cargo.toml --all-targets --locked`：769 项通过。
- `pnpm run lint`：通过；`frontend/src` 显式 `any` 为 0。
- `pnpm run test`：4 个文件、7 项 Vitest 通过，覆盖分页 latest-request-wins、quota helper、dashboard presenter 和 system-update reducer。
- `pnpm run build`：Vue typecheck 与 Vite production build 通过。
- Playwright + Google Chrome：light/dark desktop、light mobile + Sidebar、reduced-motion 四组无 console/page error；截图位于 `.runtime/audit/`。

首次最终全量测试暴露了 `/codex/usage` 只接受 `rate_limit`、误拒绝独立 `additional_rate_limits` 的既有条件错误；transport 现在只接受 core、additional、spend control、credits 四类已知 typed 根结构，修复后两项回归与 769 项全量测试均通过。

本审计及本轮本地测试不替代以下动态验证：

- global WebSocket cap 部署后的真实容量、fallback 比例与延迟观察。
- 修复部署后的 WebSocket fallback、exact continuation 和连接复用指标观察。
- 上游 appcast 自动更新画像后的真实请求指纹与新旧 WebSocket 分代观察。
