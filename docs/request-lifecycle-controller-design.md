# Responses 请求生命周期控制器设计

状态：已实现。`v1/*` 已统一采用 Request → Attempt → Stream 三层洋葱生命周期；不存在新旧双路径或兼容层。

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

- `controller` 唯一拥有功能规则、状态机、优先级、阈值、typed facts、decision 和 scope；不能选择下一个账号、编码 HTTP/SSE/WebSocket 或直接调用其他 controller。
- `lifecycle` 只拥有固定顺序、enter/leave、短路、逆序 unwind、retry 循环、commit 边界和 exactly-once finalize；不得出现 `if cyber_policy` 或功能阈值。
- `transport` / `upstream` 只把原始 HTTP、SSE、WebSocket 输入规范化为通用事实，例如状态码、错误码、terminal、usage；不得解释事实的业务后果。
- `api` 只解析请求，并编码 `ResponseDispatchResponse`、`ResponseDispatchStream` 或包含 `ClientFailure` 的 `ResponseDispatchError`；不得重新识别 cyber、quota、history 等功能。
- `store` 只提供原子读写、CAS、TTL 和数据映射；业务专用 store trait 可以由 controller 定义，基础设施实现不能拥有业务阈值和状态转换。
- controller 之间不得横向调用；共享内容只能是没有业务含义的协议事实和值对象，不能把共享业务规则藏进 `utils`、`common` 或 helper。

允许的唯一挂载点是静态 composition root `ControllerSet`：它负责构造 controller、注入依赖，并通过类型化方法分发各阶段 hook。业务代码不在各个入口或尝试循环中手工挂载功能。

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
    enter: acquire -> quota verify -> history for account -> interval -> scoped identity -> call
    leave: normalize observation -> classify -> account effects -> retry/fail decision -> trace

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
5. 生成账号作用域 identity、Cookie 和上游上下文。
6. 发起上游调用。
7. 统一规范化 HTTP、SSE 首帧和 WebSocket 错误为 `AttemptObservation`。
8. 按唯一优先级分类 history、session policy、账号隔离和 transport retry。
9. 产生 `AttemptDecision`，由唯一 pipeline 应用为 `AttemptApplyOutcome` 并记录 attempt trace。

classifier 优先级只能存在一处。例如 `cyber_policy` 必须先于通用 403、quota 或 5xx 分类；其他模块消费分类后的类型，不能再次调用 `is_cyber_policy_*`。

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
- `ControllerRequestScope` 只保存 cyber、history、usage 的真实状态；不存在通用 entered-controller marker、容量错误或空 unwind hook。
- complete 的 accepted response 归一为 `FinalOutcome` 后由 `finalize_complete` 消费 scope；retry 继续进入下一 attempt，rejection 在 controller effect 已完成后直接返回，不经过空 finalize。
- complete 的真实退出顺序固定为 Cloudflare → Usage → Affinity → Telemetry；live stream 固定为 Cloudflare → Usage → Affinity → Telemetry → Cyber，无副作用的阶段不制造空分支。
- 上游 HTTP、SSE 首帧与 live stream 统一转换为 observation 或 canonical event，再由 controller 消费 typed facts。
- stream 的 completed、incomplete、failed、断开、协议错误、下游取消和 shutdown 均形成显式终态，并经过 exactly-once finalizer。
- WebSocket upgrade 成功后的 `response.failed` / `error` 保留 code、type、显式 status 和 retry 事实进入 canonical lifecycle；transport 不得据此推导 HTTP 状态或业务后果。
- 入站 Responses WebSocket API 只采集 canonical transcript facts 并持有 service 暴露的 opaque replay snapshot/plan；previous ID 匹配、Replace/Append/Unavailable、清洗与拼接全部由 `HistoryController` 唯一拥有。
- 上游 WebSocket pool 保持独立的连接 lease 与回收职责，dispatch lifecycle 只消费其 typed transport outcome。
- 测试全部位于 `backend/tests/`；`backend/src/` 中不存在 `#[cfg(test)]`、`#[test]`、`mod tests` 或 test-only helper。

## 性能约束

- 独立且都需要的入口查询并行执行，不能把多个 Redis RTT 串行叠加。
- best-effort controller 必须有有界超时和明确 fail-open 行为。
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
