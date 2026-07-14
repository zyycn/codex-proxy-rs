# Responses 请求生命周期控制器设计

状态：Request → Attempt → Stream 三层洋葱生命周期、明确账号事实的运行时秒级失效，以及单一 HTTP 自动协商路径已经实现；不存在新旧双路径或兼容层。本文“Responses WebSocket v2 与 HTTP/2 低延迟传输审计”一节中的预热、快路径预算、熔断与 HTTP/2 PING 仍是尚未落地的传输目标，不能把这些参数误认为当前生产行为。

本文回答一个具体问题：Responses 代理能否像 Koa 洋葱模型一样，让身份、历史恢复、账号恢复、会话策略、用量和遥测等模块只处理自己的入口与出口，并按固定顺序组合，而不把同一规则散落在请求过程中。

结论是可以，而且值得做。但目标不能是单层 `before / await next / after`。当前代理存在响应提交前后的不可逆边界，并且一次请求可能尝试多个账号，因此需要 Request、Attempt、Stream 三层嵌套生命周期。

## 重构结果

重构前分散在非流式请求、流式建连、已提交长流和客户端错误编码中的生命周期编排，已经收敛到统一的类型化生命周期：

| 范围 | 当前归属 | 责任 |
| --- | --- | --- |
| Request | `dispatch/lifecycle/request.rs` | 统一模型、身份、history/session plan、controller enter 和候选冻结 |
| Attempt | `dispatch/lifecycle/attempt.rs`、`contract.rs` | 统一单账号尝试、typed observation、decision、重试与提交边界 |
| Stream | `dispatch/lifecycle/stream.rs`、`finalizer.rs` | 统一 canonical event、终态、取消、shutdown 和 exactly-once finalize |
| 功能规则 | `dispatch/controllers/<feature>/` | 每个功能唯一拥有自己的规则、状态和 lifecycle hook |
| 协议事实 | `dispatch/transport/` | 将上游响应、首帧和流事件规范化为 typed facts |
| 路由与失败 | `dispatch/routing/`、`failure/` | 通用候选/history 路由，以及失败事实聚合 |

`dispatch/service.rs` 和 `dispatch/stream/lifecycle.rs` 是 complete 与 stream 两种入口适配器：它们选择请求模式、调用同一 `enter_request` 和 `AttemptRunner`，再把已建立的流交给统一 Stream lifecycle。两者不是两套业务生命周期，也不得拥有功能规则。`dispatch/stream/live.rs` 是已提交流的传输执行器，终态统一交给 lifecycle finalizer，不构成兼容路径。

## 设计边界

该控制器模型只属于 `dispatch`，不新增顶层 `application` 层。

- API 层继续负责鉴权提取、HTTP/WebSocket 协议解析和最终编码。
- request ID、访问日志、连接排空继续使用 Tower/Axum middleware。
- `fleet` 继续拥有账号池、quota 和账号状态。
- `upstream` 继续拥有 Codex 协议、HTTP/SSE 和 WebSocket 传输。
- `telemetry` 继续保存已经确定的事实，不参与调度决策。
- controller 不成为运行时任意安装、任意排序的插件系统；顺序是静态架构契约。

## v1/* 适用范围与单功能不越界原则

洋葱模型只应用于代理业务接口 `v1/*`，包括 HTTP/SSE Responses、Responses WebSocket 和 review 变体。管理端 `/api/admin/*`、`/healthz`、静态资源和连接排空继续使用现有 API 层与 Tower/Axum middleware，不挂入这套业务洋葱。

本架构确立一条强制原则：

> 单功能单归属：一个功能的业务语义只能由一个 controller 拥有。其他层只能传递事实、执行通用控制流、适配协议或持久化数据，不得再次识别、推断或决定该功能。

以 `cyber_policy` 为例，适用请求、session key、失败识别、账号排除、CAS 清理和状态转换全部属于 `controllers/cyber_policy/`。它可以暴露 Request、Attempt、Stream、Finalize 四类窄 hook，但不应把同一规则复制到 API、transport、lifecycle 或 store；是否还能 retry 只由 lifecycle 的通用 commit/candidate contract 决定。

边界必须满足以下规则：

- `controller` 唯一拥有本功能的规则、状态机、阈值、typed classification、decision 和 scope；不能选择具体的下一个账号、编码 HTTP/SSE/WebSocket 或直接调用其他 controller。跨 controller 的唯一固定优先级由 `ControllerSet` 声明。
- `lifecycle` 只拥有固定顺序、enter/leave、短路、逆序 unwind、retry 循环、commit 边界和 exactly-once finalize；不得出现 `if cyber_policy` 或功能阈值。
- `transport` / `upstream` 只把原始 HTTP、SSE、WebSocket 输入规范化为通用事实，例如状态码、错误码、terminal、usage；不得解释事实的业务后果。
- `api` 只解析请求，并编码 `ResponseDispatchResponse`、`ResponseDispatchStream` 或包含 `ClientFailure` 的 `ResponseDispatchError`；不得重新识别 cyber、quota、history 等功能。
- `store` 只提供原子读写、CAS、TTL 和数据映射；业务专用 store trait 可以由 controller 定义，基础设施实现不能拥有业务阈值和状态转换。
- controller 之间不得横向调用；共享内容只能是没有业务含义的协议事实和值对象，不能把共享业务规则藏进 `utils`、`common` 或 helper。

允许的唯一挂载点是静态 composition root `ControllerSet`：它负责构造 controller、注入依赖、声明固定优先级，并通过类型化方法分发各阶段 hook。它只能调用 owner 暴露的分类、effect 与 decision，不能替 owner 构造功能专用 decision。业务代码不在各个入口或尝试循环中手工挂载功能。

## 已实现目录与边界

当前源码采用以下真实目录；目录本身就是实现边界，不另设抽象目标树：

```text
backend/src/dispatch/
├── lifecycle/
│   ├── mod.rs                 洋葱组合入口
│   ├── request.rs             Request enter/leave
│   ├── attempt.rs             AttemptRunner
│   ├── pipeline.rs            唯一 retry/commit 编排器
│   ├── contract.rs            typed observation、decision 与提交契约
│   ├── stream.rs              canonical stream 状态与终态
│   ├── finalizer.rs           exactly-once finalizer
│   └── trace.rs               请求与尝试 trace
├── controllers/
│   ├── mod.rs
│   ├── cyber_policy/
│   │   ├── mod.rs
│   │   ├── types.rs           scope 与状态值对象
│   │   └── store.rs           原子状态存取
│   ├── history.rs
│   ├── quota.rs
│   ├── account_failure.rs
│   ├── account_state.rs          运行时失效与 WS 驱逐的共享 effect 执行器
│   ├── cloudflare.rs
│   ├── affinity.rs
│   ├── usage.rs
│   └── telemetry/
│       ├── mod.rs
│       └── events.rs
├── routing/                    通用候选 ledger；不拥有功能策略
├── transport/                  账号调用、prefetch、observation、canonical event
├── failure/                    通用失败聚合与 SSE 终态事实
├── affinity/                   通用会话亲和基础设施
├── stream/
│   ├── lifecycle.rs           stream 入口适配器
│   └── live.rs                已提交流传输执行器
├── errors.rs                   通用 DispatchFailure 与 ClientFailure
└── service.rs                  v1/* complete facade 与共享依赖容器
```

目录规则是：功能代码向 `controllers/<feature>` 收敛；生命周期契约和控制流只能位于 `lifecycle/`；协议事实规范化只能位于 `transport/`；通用候选与失败聚合分别位于 `routing/`、`failure/`；API 不得反向进入 controller 的内部实现。

生产源码和测试必须物理分离：`backend/src/` 不允许出现 `#[cfg(test)]`、`mod tests` 或 test-only helper；所有 contract、controller、transport 和集成测试只能放在 `backend/tests/`，并按生产边界组织。

本次重构没有保留兼容路径：不存在 compatibility re-export、feature flag 切换或新旧双路径。`service.rs`、`stream/lifecycle.rs` 与 `stream/live.rs` 仅按传输阶段承担入口或执行职责，不复制 lifecycle/controller 业务语义。

## 不越界验收

- 修改功能阈值、状态转换或 session 规则时，只修改该 controller 目录及其测试。
- 新增功能只允许新增一个 controller、`ControllerSet` 静态组合项和本模块测试，不修改 request/attempt/stream 主循环的业务判断。
- lifecycle contract test 只验证顺序和控制流；controller test 只验证功能语义；transport test 只验证事实规范化；API test 只验证统一 outcome 编码。
- 用 `rg` 检查功能关键词：除 controller、自有 store/schema、测试和文档外，生产代码不得出现功能专用判断。
- 删除 controller 后，工程只能因静态 `ControllerSet` 或类型引用失败；lifecycle、API、transport 和 store 中不能残留该功能的决策逻辑。
- 以下情况直接判定越界：功能名出现在 lifecycle 控制流条件分支、API 重判错误、store 根据业务阈值决策、多个 controller 互相调用、同一原始事件被多次解析。

## 三层生命周期

```text
RequestLifecycle
  enter: normalize/model -> local identity -> history/session plan -> candidate snapshot

  AttemptLifecycle (每个候选账号一次)
    enter: acquire -> quota verify -> history for account -> interval -> account boundary -> call
    leave: normalize observation -> classify -> runtime invalidation -> retry/fail decision -> trace

    StreamLifecycle (响应提交后)
      enter: canonical decoder -> observers -> downstream encoder
      leave: terminal/cancel -> Cloudflare -> usage -> affinity -> telemetry -> cyber -> lease release

  start: return ResponseDispatchResponse or ResponseDispatchStream to API
  leave: finalize one FinalOutcome immediately or when the stream body ends
```

进入顺序和有副作用的退出顺序都由静态 `ControllerSet` 明确声明。`ControllerRequestScope` 只保存 cyber、history、usage 的真实请求状态，不维护运行时 marker 栈；retry 或 rejection 不执行空 unwind，complete 与已提交 stream 才按固定逆序调用实际存在 leave 副作用的 owner。

三层的责任不同：

- Request 决定请求不变量和本次请求允许尝试的账号集合。
- Attempt 决定单账号调用是否被接受、同账号恢复、换下一个账号或立即失败。
- Stream 只处理已经越过提交边界的事件、下游交付和终态；此时不能再产生换号 decision。

流式请求有两个不同时间点，不能共用一个“最终结果”类型。API 在首帧提交边界拿到 `ResponseDispatchStream`；Request/Attempt 状态已经被消费并转移进后台任务持有的 `StreamFinalizer`，真正的 completed、failed 或 cancelled 要等 stream terminal 后才能形成 `FinalOutcome` 并逆序退出。

## 类型化上下文与结果

controller 不接收一个可随意写入的 Koa 风格字典，也不使用 `HashMap<String, Any>` 或无边界的 `serde_json::Value` 扩展包。公共 context 只保存跨模块稳定事实，每个 controller 通过窄输入或 capability view 读取所需字段。

实际核心类型：

```rust
struct RequestContext {
    request: CodexResponsesRequest,
    display_model: String,
    controller_scope: ControllerRequestScope,
    candidates: AccountAttemptLedger,
}

enum AttemptStep<'runner, 'dependencies> {
    Open(OpenAttempt<'runner, 'dependencies>),
    Committed(CommittedAttempt),
}

enum AttemptDecision {
    Accept,
    RetrySameAccount,
    RetryNextCandidate { /* typed exhaustion */ },
    Return(AttemptReturnKind),
}

enum StreamTerminal {
    Completed { /* canonical response */ },
    Incomplete { /* canonical response */ },
    Failed { /* typed failure */ },
    UpstreamClosed,
    ProtocolError { /* detail */ },
    DownstreamClosed,
    Shutdown,
}

enum AttemptPipelineOutcome {
    Established(EstablishedResponse),
    Rejected(AttemptRejection),
}
```

账号状态、会话恢复和客户端错误映射只分类一次。API 消费 `ResponseDispatchResponse`、`ResponseDispatchStream` 或 `ResponseDispatchError`：非流式 payload 直接编码，stream body 绑定到对应 transport，typed failure 映射到 HTTP JSON、SSE 或 WebSocket。后台 lifecycle 只消费 `StreamTerminal` / `FinalOutcome` 做领域结算，不再重新识别 `cyber_policy`、quota、banned 或 history failure。

## controller 契约

controller 分为两类：

- Decision controller：只在 `OpenAttempt` 上返回 retry、accept 或 fail 等显式 decision；`CommittedAttempt` 不暴露 decision API。
- Observer controller：只消费已经确定的事实。其 Redis、数据库或遥测失败默认是 best-effort，不能改写已经确定或已经发给客户端的 outcome。

错误也分为两类：

- Critical：无法建立正确请求语义，转为 `DispatchFailure` 并触发逆序退出。
- BestEffort：记录警告并 fail-open，不阻塞正常请求。

账号可用性 effect 属于第三种更窄的 `CriticalRoutingState`：它不能改写已经观察到的上游 outcome，但必须在返回换号 decision 前完成内存池失效和 WebSocket 池摘除。PostgreSQL 只是该事实的持久副本，必须后台写入，不能位于换号的等待链上。遥测、Cookie 和普通 usage 持久化仍是 BestEffort，不能与这条关键路径混为一谈。

同层 controller 不直接调用另一个 controller，也不自行选择下一账号。lifecycle 是唯一解释 decision 的组件。这样模块只拥有规则，编排器只拥有顺序和控制流。

## 固定顺序

### Request enter

1. 请求规范化、模型解析和本地身份。
2. history plan。
3. 普通 affinity 与会话 recovery 查询；无依赖项可在同一 stage 并行。
4. 合并 controller enter 结果，明确 preferred 和 excluded 账号集合。
5. 冻结 `AccountAttemptLedger`。
6. 建立请求级 trace/telemetry 外层作用域。

### Attempt enter/leave

1. 获取候选账号 lease。
2. 必要时验证 quota。
3. 为该账号准备 history；不能跨账号时返回 typed failure。
4. 等待账号请求间隔。
5. 从当前账号生成 token/account/Cookie 和稳定 installation ID；会话语义 ID 保持客户端原值。
6. 发起上游调用。
7. 统一规范化 HTTP、SSE 首帧和 WebSocket 错误为 `AttemptObservation`。
8. 按唯一优先级分类 history、session policy、账号隔离和 transport retry。
9. 产生 `AttemptDecision`，由唯一 pipeline 应用为 `AttemptApplyOutcome` 并记录 attempt trace。

classifier 优先级只能存在一处。complete、prefetch 与 committed stream 共同经过同一个 feature failure owner classifier；`cyber_policy` 必须先于通用 403、quota 或 5xx 分类。composition root 只能把不透明的分类结果交还 owner 产生 decision，其他模块不能再次调用 `is_cyber_policy_*` 或自行拼装 Cyber retry。

### 账号边界与会话 ID 边界

官方 Codex 源码将 identifier 分为三种完全不同的状态，lifecycle 不能用一个“账号作用域 identity”概念把它们混在一起：

| 类别 | 字段 | Attempt 规则 |
| --- | --- | --- |
| 当前账号认证/指纹 | token、`chatgpt-account-id`、Cookie、installation ID | 每次按当前账号重建；installation ID 按账号稳定派生 |
| 上游 opaque 状态 | `x-codex-turn-state`、`previous_response_id` | turn state 换号清除；previous ID 只留在原 owner，或在有完整 transcript 时删除并全量重放 |
| 客户端会话语义 | session/thread/conversation、turn、window、parent/forked thread、`prompt_cache_key`、`x-client-request-id` | 原样透传；不得 HMAC、随机化或随账号改写 |

官方客户端将 installation ID 持久化为安装级 UUID，所以它不是登录账号 ID；本工程按账号稳定派生是号池防关联策略。相反，官方 `prompt_cache_key` 默认取 session ID，`x-client-request-id` 使用 thread ID，证明它们属于会话连续性。代理可以把客户端原值投影到 HTTP/WS header，但不能生成第二份账号作用域值。

`AccountScopedRequest` 是 attempt 到 transport 的提交能力：只有当前账号认证、installation ID、Cookie 和 opaque state 已处理的请求才能调用上游。`AccountScopedIdentity` 被刻意收窄到 installation ID，不保存任何 session/thread/turn/window 值；换号时编译期可见的身份处理入口因此只有一个。

### 明确账号事实与秒切号边界

数据库中的 `active` 只是上一次持久化快照，不是当前账号可用性的最终真相。请求已经选中账号后，上游仍可能返回封禁、工作区停用、token 失效、额度耗尽或窗口限流。处理顺序固定为：

```text
typed upstream fact
  -> feature owner 唯一分类
  -> 持有账号池 mutex 时线性化内存失效并移除现有 slots
  -> 将同一状态写入有序 PostgreSQL 后台队列
  -> 从 WebSocket 池摘除该账号的 idle/busy entries
  -> OpenAttempt 返回 RetryNextCandidate
```

任何并发请求即使已经冻结了包含该账号的 candidate snapshot，在真正 `acquire_candidate` 时也必须重读运行时状态，因此内存失效完成后不能再租到旧账号。数据库连接池拥塞、行锁或写失败只能影响持久化收敛，不能让当前请求等待，也不能让其他请求继续看到旧 `Active`。账号运行状态后台写入按产生顺序消费，避免同一进程内的失败事实乱序落库。

账号事实矩阵来自当前工程、官方 Codex error/rate-limit 类型以及 `/home/zyy/桌面/Codes/sub2api` 的 OpenAI failover 实现交叉审计；只吸收明确的 OpenAI/Codex 信号，不复制 sub2api 的多平台、自定义状态码或连续次数产品策略：

| 上游事实 | owner 与运行时 effect | 当前 attempt |
| --- | --- | --- |
| `account_deactivated`、`deactivated_workspace`、明确 banned/suspended/organization disabled | `account_failure` → `Banned` + WS 驱逐 | 立即下一账号 |
| `identity_verification_required` | `account_failure` → `Disabled` + WS 驱逐 | 立即下一账号 |
| 401、`token_invalidated`、`token_revoked`、`refresh_token_invalidated`、`invalid_api_key`、`authentication_error` | `account_failure` → `Expired` + WS 驱逐 | 立即下一账号 |
| `insufficient_quota`、`payment_required`、`workspace_*_credits_depleted` 或普通 402 | `quota` → `QuotaExhausted` + WS 驱逐 | 立即下一账号 |
| 429、`rate_limit_exceeded`、`usage_limit_reached`、`workspace_*_usage_limit_reached` | `quota` → 带 reset/retry 时间的运行时限流 + WS 驱逐 | 立即下一账号 |
| model unavailable/not supported，包括明确的 404 | `account_failure` 只记录 `ModelUnsupported`，不失效整账号 | 立即下一账号 |
| connect/timeout、529、5xx | 通用 transport retry；不写账号状态 | 有候选时下一账号 |
| Cyber、Cloudflare HTML challenge、previous-response failure | 各自 controller/history owner | 不得被通用 403/404 抢占 |

同一失败在 HTTP 状态响应、SSE/WS `response.failed` 和 `error` 事件中都消费相同的 `code`、`type`、status、message 与 retry typed facts。transport 只能提取这些字段，不能决定账号状态。普通 `403` 在 Cyber、Cloudflare 和 model classifier 都未命中后才作为账号禁止事实处理。

`CommittedAttempt` 类型上没有 retry API：如果失败在客户端响应已经提交后才到达，只能在其他 BestEffort finalizer I/O 之前执行同样的内存失效、后台持久化和 WS 驱逐，让后续请求立即避开该账号；绝不能把另一账号的输出拼到当前流。

### Stream enter/leave

1. 上游 SSE 或 WebSocket 事件转换为一次性的 `CanonicalResponseChunk` / `CanonicalResponseEvent`。
2. 保留 raw bytes 供 HTTP SSE 透明转发；typed event 同时提供给 observer 和下游 WebSocket encoder。
3. 在首个可转发事件处记录 commit；commit 后类型上不再允许 retry。
4. 影响下一请求路由的关键状态在 terminal 向下游转发前完成有界更新。
5. terminal、上游断开、协议错误、下游取消和 shutdown 都转换为 `StreamTerminal`。
6. 统一 finalizer 逆序执行 controller，并且只执行一次。
7. 最后释放账号 lease。

`Drop` 不能执行异步 Redis、数据库或遥测操作。response body 的 Drop 只发送取消信号；持有完整生命周期的受监督 task 在 live loop 返回唯一 `StreamSummary` 后，消费式调用 `StreamFinalizer::finalize(...).await`。Drop guard 只作为 panic/abort 的资源释放兜底。

## Rust 落地选择

核心热路径不需要 `Vec<Box<dyn Middleware>>`：

- 非流式与流式模式用泛型或 sealed enum 做静态 dispatch，共享一个 request prelude 和 `AttemptRunner`。
- controller 集合使用具体 `ControllerSet` 或 sealed controller enum，顺序由代码和 contract test 固定。
- 流 decoder 和状态机使用具体类型，不能对每个 token 走 async trait/vtable。
- 只有未来确实需要异构、可配置 controller 集合时，才在注册边界使用 `Arc<dyn Controller>`；每请求私有状态仍应封装在 controller scope 内。
- typestate 只用于 `OpenAttempt` → `CommittedAttempt` 这一不可逆边界，不把每个 controller 小阶段都编码为泛型状态。

与 Koa 相同的是嵌套进入、逆序退出和短路语义；不同的是 Rust 通过所有权、显式 outcome 和提交 typestate 保证边界，而不是依赖一个任意可变 context 和递归 `next()` future。

## 已落地的实现约束

- complete 与 stream 入口共享 `enter_request` 和 `AttemptRunner`，不存在各自维护的账号尝试主循环。
- `ControllerSet` 是静态 composition root；controller 顺序和 typed hook 在编译期可见，不使用动态 middleware 链。
- complete、prefetch 与 committed stream 的公共失败 owner 优先级只在 `ControllerSet::classify_shared_failure` 声明一次；composition root 不实现 Cyber、quota 或账号失败规则。
- ban、deactivated workspace、认证失效与 quota owner 共享同一 `account_state` effect 执行器：内存状态先线性化，PostgreSQL 有序后台持久化，WS entry 随即驱逐；controller 不直接等待数据库。
- Cyber failure 由 `CyberPolicyController` 返回不透明分类，并由该 owner 唯一构造换号或返回 decision；`ControllerSet` 只分发 effect 与 decision。
- `ControllerRequestScope` 只保存 cyber、history、usage 的真实状态；不存在通用 entered-controller marker、容量错误或空 unwind hook。
- complete 的 accepted response 归一为 `FinalOutcome` 后由 `finalize_complete` 消费 scope；retry 继续进入下一 attempt，rejection 在 controller effect 已完成后直接返回，不经过空 finalize。
- complete 的真实退出顺序固定为 Cloudflare → Usage → Affinity → Telemetry；live stream 固定为 Cloudflare → Usage → Affinity → Telemetry → Cyber，无副作用的阶段不制造空分支。
- 上游 HTTP、SSE 首帧与 live stream 统一转换为 observation 或 canonical event，再由 controller 消费 typed facts。
- stream 的 completed、incomplete、failed、断开、协议错误、下游取消和 shutdown 均形成显式终态，并经过 exactly-once finalizer。
- WebSocket upgrade 成功后的 `response.failed` / `error` 保留 code、type、显式 status 和 retry 事实进入 canonical lifecycle；transport 不得据此推导 HTTP 状态或业务后果。
- 入站 Responses WebSocket API 只采集 canonical transcript facts 并持有 service 暴露的 opaque replay snapshot/plan；previous ID 匹配、Replace/Append/Unavailable、清洗与拼接全部由 `HistoryController` 唯一拥有。
- 上游 WebSocket pool 保持独立的连接 lease 与回收职责，dispatch lifecycle 只消费其 typed transport outcome。
- 测试全部位于 `backend/tests/`；`backend/src/` 中不存在 `#[cfg(test)]`、`#[test]`、`mod tests` 或 test-only helper。

## Responses WebSocket v2 与 HTTP/2 低延迟传输审计

状态：审计完成，目标方案已确定，尚未实现。

本节解决的不是“WebSocket 和 HTTP/SSE 谁在理论上更快”，而是两个直接影响体感的问题：如何让健康连接尽量在请求到来前准备好，以及当 WebSocket 建连异常时，如何避免在换账号或 HTTP fallback 之前白等 15 秒。

最终结论是双热通道：精确会话 WebSocket 负责 v2 的连接本地状态和增量续接；按 origin 共享的 HTTP/2 连接负责新链和可持久化续链的低延迟后备。两者不能互相冒充，也不能用一个通用“连接可用”布尔值抹掉 continuation 的所有权差异。

### 审计证据与事实边界

以下结论来自 2026-07-14 的四类证据，必须区分来源：

1. 官方 Codex 源码确认：本地官方仓库当时的 `main` 为 `5bed6447998c754d154dbd796517310b8f04d4ce`。OpenAI provider 默认声明支持 WebSocket；旧 `responses_websockets` / `responses_websockets_v2` feature 已标记 Removed，实际传输路径不再依赖这两个开关；握手固定发送 `OpenAI-Beta: responses_websockets=2026-02-06`。
2. 官方 Codex 源码确认：会话初始化后会在后台构造稳定的 instructions、tools 和空 input，发送 v2 `response.create` 且设置 `generate=false`，等待 `response.completed` 后把连接和 response ID 交给首个真实 turn。首个 turn 只有在请求属性仍匹配时才使用 warmup response ID；模型、service tier、tools 等属性变化时仍可复用 socket，但必须发送新的完整 create。
3. OpenAI 官方文档确认：WebSocket Mode 在活跃 socket 上只保存最新一个 previous-response 的连接本地内存状态；`store=false` / ZDR 下没有持久化 fallback，ID 不在该 socket 缓存时返回 `previous_response_not_found`；单条连接一次只允许一个 in-flight response，不支持 multiplex，连接最长 60 分钟。
4. 当前工程源码确认：已经透明透传 `generate=false`，发送 v2 beta header，按 `(base_url, account_id, conversation_id)` 建池，并在 socket 上记录和校验最新 response ID；池内连接默认 25 秒 ping、5 秒探活、55 分钟最大寿命。
5. 当前工程源码确认：reqwest Client 已按自定义 CA profile 全局缓存，HTTP/2 可以跨账号共享同一 origin 的连接池；强制 HTTP/1.1 的配置和分支已经删除，但尚未配置 HTTP/2 PING，reqwest 0.12.28 默认会在 90 秒后清理 idle pool。
6. 线上与网络探针只能用于校准，不构成协议保证：此前当日生产失败样本中，59 次 WebSocket 超时有 56 次被同账号 HTTP 恢复；OCI 未鉴权探针中，HTTP/2 冷连接完成 TLS 约 32–35ms，WebSocket upgrade 响应约 70–591ms。该探针不包含真实鉴权和模型 TTFT，只说明健康握手通常不需要 15 秒。

官方依据：

- [Codex `client.rs` 的 prewarm 契约](https://github.com/openai/codex/blob/5bed6447998c754d154dbd796517310b8f04d4ce/codex-rs/core/src/client.rs#L15-L24)
- [Codex 会话启动预热](https://github.com/openai/codex/blob/5bed6447998c754d154dbd796517310b8f04d4ce/codex-rs/core/src/session_startup_prewarm.rs#L183-L224)
- [Codex `generate=false` 请求构造与等待完成](https://github.com/openai/codex/blob/5bed6447998c754d154dbd796517310b8f04d4ce/codex-rs/core/src/session_startup_prewarm.rs#L241-L324)
- [OpenAI Responses API WebSocket Mode](https://developers.openai.com/api/docs/guides/websocket-mode)
- [reqwest 0.12.28 HTTP/2 keep-alive 配置](https://docs.rs/reqwest/0.12.28/reqwest/struct.ClientBuilder.html#method.http2_keep_alive_interval)

### WebSocket v2 的收益边界

WebSocket v2 的主要收益不是帧格式比 HTTP/SSE 更快，而是同时消除三类重复工作：

- socket 持续存在，避免每轮重新建立 TCP/TLS/HTTP 请求通道；健康 HTTP/2 keep-alive 也能覆盖这一部分。
- 客户端只发送新增 input 和 `previous_response_id`，不重复上传完整 history、instructions 和 tools。
- 上游直接复用当前 socket 上的 connection-local previous-response 状态；HTTP/2 连接复用不能提供这份模型请求状态。

因此，热 HTTP/2 可以让 fallback 的网络建连接近无感，但不能替代 `store=false` WebSocket 链。OpenAI 官方文档给出的典型收益场景是长工具链，并报告 20 次以上工具调用的 rollout 最高约 40% 端到端加速；这不是所有单轮请求都能获得的固定收益。

### 当前实现缺口

当前实现已经具备 v2 透传和池化基础，但还有四个会影响正确性或尾延迟的缺口：

1. `upstream/openai/transport/client_sse.rs` 只根据是否携带 previous ID 判断 fallback。首个 `generate=false` 没有 previous ID，因此上游 WebSocket 失败后仍可能回退 HTTP。
2. `controllers/affinity.rs` 只根据 `store` 记录 continuation scope。HTTP fallback 产生的 `store=false` response 也会被记为 `ConnectionLocal`，但实际没有任何 WebSocket 持有该 ID。
3. `WebSocketPreferred` 与 `WebSocketRequired` 的冷建连都可能在前台等待完整 15 秒；池只有 Idle / Busy 等连接结果，没有复用中的单飞 Connecting 状态和短快路径预算。
4. HTTP Client 虽然全局缓存并默认协商 HTTP/2，但 90 秒 idle pool timeout 会在长期只走 WebSocket 时主动清掉健康后备连接；TCP keepalive 不能阻止连接池自己驱逐 socket。

第 1、2 点会形成错误链：

```text
客户端后台发送 generate=false
  -> 上游 WebSocket 失败
  -> 代理降级 HTTP，得到 store=false response_id
  -> 客户端下一轮携带 previous_response_id
  -> 新 WebSocket 没有该 connection-local 状态
  -> previous_response_not_found 或被迫全量重放
```

目标实现必须禁止这条链，而不是再增加一次重试掩盖它。

### 最终传输状态机

请求首先被规范化为显式 transport requirement，不继续靠 `use_websocket`、`force_http_sse` 和 `previous_response_id.is_some()` 的组合隐式推断：

| 请求事实 | 传输要求 | 允许行为 |
| --- | --- | --- |
| 客户端显式 `generate=false` 且 `store=false` | Explicit WebSocket Warmup | 必须使用 WebSocket；允许后台完整建连预算；禁止 HTTP fallback |
| `store=false` connection-local previous ID | Exact WebSocket Continuation | 只能租用持有同一最新 ID 的精确 socket；缺失时立即返回 typed unavailable，由 HistoryController 决定全量重放或失败 |
| 已确认 persisted previous ID | Persisted Continuation | 热 WebSocket 优先；没有热 socket 时允许使用 HTTP/2 或新 WebSocket hydration |
| 外部来源且所有权未知的 previous ID | External Unknown | 只在已选定账号原样尝试一次，不遍历账号碰运气；失败后返回观察到的协议错误 |
| 没有 previous ID 的普通新链 | New Chain | 精确热 WebSocket 优先；冷建连只等快路径预算，超时后同账号走热 HTTP/2 |
| 客户端显式要求 HTTP | HTTP Required | 直接使用 HTTP，不启动 WebSocket 冷建连 |

response 完成后的 continuation scope 必须同时由 `store` 和实际 transport 推导：`store=true` 是 Persisted；`store=false + WebSocket completed` 才是 ConnectionLocal；`store=false + HTTP` 必须是 ReplayRequired 或 Unavailable，绝不能伪装成 ConnectionLocal。只有入站 WebSocket 已持有完整 canonical transcript 时才能标记 ReplayRequired，普通 HTTP 客户端没有本地 replay snapshot 时只能标记 Unavailable。

普通新链的目标时序：

```text
获取账号 lease
  -> controller 完成账号与 route 准备
  -> 生成当前账号凭据、installation ID / Cookie / handshake 输入
  -> 启动精确 WebSocket 单飞建连
       与 wait_for_request_interval 并行
  -> 已有热 socket 或 800ms 内完成：发送 response.create
  -> 800ms 未完成：取消尚未发送 payload 的冷连接，同账号立即走热 HTTP/2
  -> HTTP/2 也失败或返回明确可换号失败：Attempt lifecycle 选择下一候选账号
```

800ms 是初始快路径预算，不是永久硬编码真理。它来自当前 OCI 探针的健康区间并为鉴权波动保留余量，必须根据 `ws_connect_ms` 的生产 p95/p99 调整。无论以后调成多少，该预算都只能覆盖发送 payload 之前的握手阶段。

一旦 `response.create` 已可能送达上游，连接断开或首事件超时就是“是否已经开始推理未知”的 ambiguous failure。没有上游幂等保证时，不能为了缩短体感而并发发送同一请求到 HTTP 或其他账号，否则可能重复推理、重复扣费或产生两条不一致输出。现有 20 秒 initial-event guard 与 5 分钟 active-stream idle timeout 必须继续和 pre-send connect budget 分开建模。

### 官方客户端预热与代理预热

官方 Codex 客户端已经提供标准预热信号。代理收到 `generate=false` 后应当：

1. 正常进入 Request / Attempt 洋葱并选定账号，不绕过 quota、history 或账号隔离规则。
2. 在该账号、base URL 和本地 conversation identity 下建立 WebSocket。
3. 原样发送 `response.create(generate=false)`，等待 `response.completed`。
4. 将 socket、最新 warmup response ID 和连接元数据作为一个不可拆分的池条目保存。
5. 首个真实请求只有在 previous ID 和请求属性都满足续接条件时，才在该 socket 上发送增量 input。

代理不得给所有账号盲目建立“空 WebSocket”：握手携带账号凭据、session/thread 会话语义、Cookie 和路由信息，空连接不能安全跨账号或跨 conversation 借用；单 socket 不支持 multiplex，批量空连接还会提前消耗上游连接额度。对不发送标准 warmup 的客户端，只允许在候选账号和会话语义已经确定后做 handshake-only 单飞建连，不得由代理擅自补发 `generate=false`，因为后者可能额外处理完整上下文并引入隐藏成本、状态映射和并发竞态。

### HTTP/2 热后备

HTTP/2 连接按 `(scheme, host, port, proxy/TLS profile)` 共享，不按账号建池；Authorization 和账号头属于单个 HTTP/2 stream，同一 origin 的多个账号可以复用一条 TLS/H2 连接。这正适合作为 WebSocket 冷建连失败后的廉价后备。

工程不再暴露 `tls.force_http11`，也不保留 `http1_only()` 兼容分支；旧 `tls` 配置节点会被 `deny_unknown_fields` 直接拒绝。上游 HTTP 只有一条自动 ALPN 协商路径，主动连接保活优化只针对 HTTP/2，不提供运行时切回“强制 HTTP/1.1”的第二套策略。目标 Client 配置为：

```rust
Client::builder()
    .pool_idle_timeout(None::<Duration>)
    .http2_keep_alive_interval(Duration::from_secs(30))
    .http2_keep_alive_timeout(Duration::from_secs(5))
    .http2_keep_alive_while_idle(true)
```

有可用账号时，现有启动模型目录刷新已经会在服务启动后建立同 origin HTTP 连接，不需要新增一条“为了预热而预热”的业务请求。目标配置的目的只是让该连接在 WebSocket-only 流量期间不被本地 90 秒 idle timeout 清理，并用 HTTP/2 PING 在后台发现已经失活的链路。

### 熔断与账号边界

WebSocket 建连失败通常是 origin、出口网络、代理或 Cloudflare 边缘链路事实，不应直接把账号标为失效。此前生产样本中绝大多数超时能被同账号 HTTP 恢复，因此顺序必须是“同账号换 transport，再由 HTTP 结果决定是否换账号”，而不是“换账号后再重复一次同样的 WebSocket 超时”。

初始熔断规则：

- key 为 origin + 出口 route/TLS profile，不按账号 key。
- 30 秒窗口内连续 3 次 WebSocket 快路径超时，打开 30 秒。
- 熔断期间已有热 WebSocket 仍可使用；普通新链直接走 HTTP/2，不再创建冷 WebSocket。
- 30 秒后只允许一个 half-open 单飞探针；成功关闭熔断，失败重新打开。
- 401、403、quota、账号停用和套餐限制仍由对应 controller 处理，不能污染 transport 熔断。
- `previous_response_not_found` 只驱逐精确 conversation socket 并交给 HistoryController，不打开 origin 熔断。

这些时间和次数属于 transport policy，只能由 `upstream/openai/transport` 单一 owner 定义和观测；lifecycle 只能消费 `ReusedWebSocket`、`ConnectedWebSocket`、`Http2Fallback`、`ContinuationUnavailable`、`PreSendFailure`、`PostSendAmbiguousFailure` 等 typed outcome。

### 洋葱边界下的代码归属

| 责任 | 唯一归属 | 禁止越界 |
| --- | --- | --- |
| v2 payload、握手、快路径预算、单飞、连接池、origin 熔断 | `upstream/openai/transport/` | lifecycle/controller 不解析 WebSocket 错误字符串，不维护连接状态 |
| `store` + 实际 transport 推导 continuation scope | upstream 产出事实，`controllers/history.rs` 解释所有权与重放 | affinity 不能只看 `store` 猜测 scope |
| 账号选择、retry next、提交边界 | `dispatch/lifecycle/` | transport 不选择下一个账号，不写账号状态 |
| history full replay、previous ID owner、跨账号可恢复性 | `controllers/history.rs` | API 和 WebSocket pool 不自行拼接历史 |
| quota、封禁、Cyber 和账号状态 | 各自 classifier controller；`account_state` 只执行通用失效 effect | transport connect failure 不能直接停用账号，effect 执行器不能识别错误语义 |
| HTTP/WS 入站解析和最终事件编码 | `api/client/responses/` | API 不决定 upstream transport fallback |

Attempt lifecycle 只增加一个窄的 transport preparation 调用点：在账号、route、identity 和 Cookie 已确定后启动连接准备，并与账号请求间隔并行；它接收准备结果，不拥有 800ms、30 秒或熔断阈值。这样低延迟优化不会把 WebSocket 规则扩散进 `attempt.rs`，也不会形成第二套 retry 主循环。

### 可观测性与验收

必须分别记录选择账号、连接准备、发送请求、首协议事件和首真实输出，不能只用一个总耗时掩盖慢点：

- `transport_decision`：`ws_reused`、`ws_connected_fast`、`http2_ws_slow`、`http2_breaker_open`、`ws_exact_required`。
- `ws_connect_ms`、`transport_decision_wait_ms`、`upstream_headers_ms`、`first_event_ms`、`first_output_ms`。
- `http_version`、origin breaker state、WebSocket pool decision、connection reused。
- pre-send failure 与 post-send ambiguous failure 必须是不同枚举和不同指标。
- `generate=false` warmup 成功率、warmup 被首轮使用率、请求属性变化导致的 warmup miss。

完成实现必须同时满足：

- `generate=false` 的 `store=false` 请求在 WebSocket 失败后绝不回退 HTTP，也不记录虚假的 ConnectionLocal scope。
- warmup 与首个真实 turn 在属性匹配时使用同一 socket，首轮携带 warmup response ID 和增量 input。
- 精确 connection-local socket 缺失时不冷建连碰运气，不遍历账号；只走 HistoryController 的 typed replay/failure。
- 普通 `WebSocketPreferred` 冷建连不会在前台等待 15 秒；健康快路径预算耗尽后同账号 HTTP/2 立即接管。
- WebSocket transport failure 不更新账号失效状态；只有后续明确的账号事实才能触发账号 controller。
- 明确账号事实发生后，即使 PostgreSQL 行写入被阻塞，当前 attempt 仍立即切到下一账号，并发候选租用也不能再次获得旧账号；解除阻塞后状态最终落库。
- 账号失效必须同时驱逐该账号的 WebSocket entries；已提交流只能影响后续请求，不能构造 retry。
- origin 熔断期间没有冷 WebSocket 建连风暴，已有热连接不受影响。
- payload 发送后的 ambiguous failure 不自动并发 hedge 到第二 transport 或账号。
- HTTP/2 连接在超过 90 秒的 WebSocket-only 空闲窗口后仍可复用，并能在 PING timeout 后清理失活连接。
- 所有测试位于 `backend/tests/`，按 transport、history controller 和 lifecycle contract 边界组织，`backend/src/` 不出现 test-only 代码。

## 性能约束

- 独立且都需要的入口查询并行执行，不能把多个 Redis RTT 串行叠加。
- best-effort controller 必须有有界超时和明确 fail-open 行为。
- 账号可用性内存失效和 WS 池摘除不是数据库 BestEffort I/O；数据库持久化只能进入后台队列，不能占用换号延迟预算。
- 逐事件 observer 只做同步、低成本状态更新；Redis、数据库和遥测 I/O 只在必要的 terminal/finalize 阶段执行。
- canonical event 同时保留 raw payload，避免为透明转发再次序列化。
- 是否引入动态分发以基准和可维护性为依据，不凭感觉优化。

## 验收条件

- 新增一个跨阶段恢复策略时，只新增 controller、静态注册和本模块测试，不同时修改 `service.rs`、`stream/lifecycle.rs`、`stream/live.rs` 和 API handler。
- 非流式与流式共享同一个 request prelude 和 `AttemptRunner`。
- 同一上游失败在 complete、prefetch 和 live terminal 得到一致分类。
- commit 后无法构造 retry decision，不能拼接两个账号的输出。
- enter 顺序、真实 leave 副作用的固定逆序、无运行时 marker/unwind 和 controller 顺序有 contract test。
- completed、incomplete、failed、upstream disconnect、decoder error、oversize、downstream cancel 和 shutdown 都 exactly-once finalize。
- HTTP JSON、HTTP SSE 与 Responses WebSocket 对相同 `ClientFailure` 保持语义一致。
- 并发同会话状态使用不可复用 revision token/CAS 测试，旧成功不能删除新失败，状态删除或过期后重建也不能出现 ABA。
- 生产模块不加入 `#[cfg(test)]`、`mod tests` 或 test-only helper；测试只能放在 `backend/tests/`。
- 不存在 compatibility re-export、旧入口包装器、feature flag 双路径或重复的旧失败分类。
