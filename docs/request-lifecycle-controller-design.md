# Responses 请求生命周期控制器设计

状态：工程审计结论与目标设计，尚未实现。

本文回答一个具体问题：Responses 代理能否像 Koa 洋葱模型一样，让身份、历史恢复、账号恢复、会话策略、用量和遥测等模块只处理自己的入口与出口，并按固定顺序组合，而不把同一规则散落在请求过程中。

结论是可以，而且值得做。但目标不能是单层 `before / await next / after`。当前代理存在响应提交前后的不可逆边界，并且一次请求可能尝试多个账号，因此需要 Request、Attempt、Stream 三层嵌套生命周期。

## 当前审计结论

当前实现是 phase orchestration，不是 controller onion：

| 范围 | 当前归属 | 主要问题 |
| --- | --- | --- |
| 非流式请求 | `dispatch/service.rs::complete` | 自己完成请求准备、候选冻结、账号循环、失败优先级和最终结算 |
| 流式建连 | `dispatch/stream/lifecycle.rs::stream` | 与非流式重复请求准备、候选、quota、history 和上游失败处理 |
| 已提交长流 | `dispatch/stream/live.rs` | handler 返回后继续持有账号 lease、会话状态、诊断、用量和遥测，并在多个提前返回分支分别释放资源 |
| 客户端错误 | `api/client/errors.rs` | HTTP JSON、SSE 和下游 WebSocket 分别判断同一种领域失败 |
| 流事件 | `stream/live.rs`、`recovery/cyber_policy.rs`、下游 WebSocket adapter | 同一 SSE 被多次解码或扫描，以重新推断 terminal、首输出和策略事实 |

领域规则并非全部重复。`HistoryRecoveryPlan`、`AccountAttemptLedger`、quota 验证、账号失败隔离和 `CyberPolicyRecovery` 已经分别集中。真正分散的是“何时调用、如何排序、如何把结果传到下一阶段”的生命周期布线。新增一个跨阶段策略时，仍可能同时修改非流式主循环、流式建连、live finalizer 和 API encoder。

当前取消路径也说明普通 handler 后置钩子不够：HTTP body 或下游 WebSocket 被丢弃后，live task 会收到取消并释放账号槽位，但不会经过完整的流终态结算。此时取消事实、部分用量、Cookie、rate-limit、turn state 和遥测可能缺失。

## 设计边界

该控制器模型只属于 `dispatch`，不新增顶层 `application` 层。

- API 层继续负责鉴权提取、HTTP/WebSocket 协议解析和最终编码。
- request ID、访问日志、连接排空继续使用 Tower/Axum middleware。
- `fleet` 继续拥有账号池、quota 和账号状态。
- `upstream` 继续拥有 Codex 协议、HTTP/SSE 和 WebSocket 传输。
- `telemetry` 继续保存已经确定的事实，不参与调度决策。
- controller 不成为运行时任意安装、任意排序的插件系统；顺序是静态架构契约。

## 三层生命周期

```text
RequestLifecycle
  enter: normalize/model -> local identity -> history/session plan -> candidate snapshot

  AttemptLifecycle (每个候选账号一次)
    enter: acquire -> quota verify -> history for account -> interval -> scoped identity -> call
    leave: normalize observation -> classify -> account effects -> retry/fail decision -> trace

    StreamLifecycle (响应提交后)
      enter: canonical decoder -> observers -> downstream encoder
      leave: terminal/cancel -> policy effects -> affinity/usage/telemetry -> lease release

  start: produce DispatchStart for API
  leave: finalize one FinalOutcome immediately or when StreamHandle ends
```

进入按声明顺序执行，退出按逆序执行。controller 在进入阶段短路时，只退出已经成功进入的 controller。

三层的责任不同：

- Request 决定请求不变量和本次请求允许尝试的账号集合。
- Attempt 决定单账号调用是否被接受、同账号恢复、换下一个账号或立即失败。
- Stream 只处理已经越过提交边界的事件、下游交付和终态；此时不能再产生换号 decision。

流式请求有两个不同时间点，不能共用一个“最终结果”类型。API 在首帧提交边界只拿到 `DispatchStart::Streaming(StreamHandle)`，Request/Attempt 已进入 controller 的退出状态随 handle 转移给后台 lifecycle；真正的 completed、failed 或 cancelled 要等 stream terminal 后才能形成 `FinalOutcome` 并逆序退出。

## 类型化上下文与结果

controller 不接收一个可随意写入的 Koa 风格字典，也不使用 `HashMap<String, Any>` 或无边界的 `serde_json::Value` 扩展包。公共 context 只保存跨模块稳定事实，每个 controller 通过窄输入或 capability view 读取所需字段。

建议的核心类型：

```rust
struct RequestEnvelope {
    request_id: String,
    route: String,
    requested_model: String,
    started_at: Instant,
    client_api_key_id: String,
}

struct RequestContext {
    envelope: RequestEnvelope,
    request: CodexResponsesRequest,
    display_model: String,
    history: HistoryRecoveryPlan,
    route_plan: RoutePlan,
    candidates: AccountAttemptLedger,
    trace: ResponseDispatchTrace,
}

struct AttemptContext {
    lease: AccountLease,
    attempt_request: CodexResponsesRequest,
    transport: CodexBackendTransport,
    attempt: ResponseDispatchAttempt,
}
```

过程控制使用显式枚举，不使用 `bool` 表达多种语义：

```rust
enum AttemptOutcome {
    Accepted(EstablishedResponse),
    RetrySameAccount { reason: RetryReason },
    RetryNextCandidate { failure: AttemptFailure },
    ReturnFailure(DispatchFailure),
}

enum StreamTerminal {
    Completed,
    Incomplete,
    Failed(StreamFailure),
    Disconnected(StreamFailure),
    InvalidProtocol(StreamFailure),
    DownstreamCancelled,
    Shutdown,
}

enum DispatchStart {
    Immediate(ResponsePayload),
    Streaming(StreamHandle),
    Failed(ClientFailure),
}

enum FinalOutcome {
    Completed(ResponseFacts),
    Incomplete(ResponseFacts),
    Failed(ClientFailure),
    Cancelled(CancelFacts),
}
```

账号状态、会话恢复和客户端错误映射只分类一次。API 立即消费 `DispatchStart`：非流式 payload 直接编码，stream handle 绑定到对应 transport body，失败由统一 `ClientFailure` 映射到 HTTP JSON、SSE 或 WebSocket。后台 lifecycle 只消费 `FinalOutcome` 做领域结算，不再重新识别 `cyber_policy`、quota、banned 或 history failure。

## controller 契约

controller 分为两类：

- Decision controller：可以返回 retry、fail 或 commit 等显式 decision。
- Observer controller：只消费已经确定的事实。其 Redis、数据库或遥测失败默认是 best-effort，不能改写已经确定或已经发给客户端的 outcome。

错误也分为两类：

- Critical：无法建立正确请求语义，转为 `DispatchFailure` 并触发逆序退出。
- BestEffort：记录警告并 fail-open，不阻塞正常请求。

同层 controller 不直接调用另一个 controller，也不自行选择下一账号。pipeline 是唯一解释 decision 的组件。这样模块只拥有规则，编排器只拥有顺序和控制流。

## 固定顺序

### Request enter

1. 请求规范化、模型解析和本地身份。
2. history plan。
3. 普通 affinity 与会话 recovery 查询；无依赖项可在同一 stage 并行。
4. 合并为 `RoutePlan`，明确 preferred、required、excluded 和 fail-fast。
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
9. 产生 `AttemptOutcome` 并记录 attempt trace。

classifier 优先级只能存在一处。例如 `cyber_policy` 必须先于通用 403、quota 或 5xx 分类；其他模块消费分类后的类型，不能再次调用 `is_cyber_policy_*`。

### Stream enter/leave

1. 上游 SSE 或 WebSocket 事件转换为一次性的 `CanonicalStreamEvent`。
2. 保留 raw bytes 供 HTTP SSE 透明转发；typed event 同时提供给 observer 和下游 WebSocket encoder。
3. 在首个可转发事件处记录 commit；commit 后类型上不再允许 retry。
4. 影响下一请求路由的关键状态在 terminal 向下游转发前完成有界更新。
5. terminal、上游断开、协议错误、下游取消和 shutdown 都转换为 `StreamTerminal`。
6. 统一 finalizer 逆序执行 controller，并且只执行一次。
7. 最后释放账号 lease。

`Drop` 不能执行异步 Redis、数据库或遥测操作。response body 的 Drop 只发送取消信号；持有完整生命周期的受监督 task 必须显式调用 `finalize_once(StreamTerminal::DownstreamCancelled).await`。Drop guard 只作为 panic/abort 的资源释放兜底。

## Rust 落地选择

核心热路径不需要 `Vec<Box<dyn Middleware>>`：

- 非流式与流式模式用泛型或 sealed enum 做静态 dispatch，共享一个 request prelude 和 `AttemptRunner`。
- controller 集合使用具体 `ControllerSet` 或 sealed controller enum，顺序由代码和 contract test 固定。
- 流 decoder 和状态机使用具体类型，不能对每个 token 走 async trait/vtable。
- 只有未来确实需要异构、可配置 controller 集合时，才在注册边界使用 `Arc<dyn Controller>`；每请求私有状态仍应封装在 controller scope 内。
- typestate 只用于 Prepared、Routed、Committed 等少数不可逆边界，不把每个小阶段都编码为泛型状态。

与 Koa 相同的是嵌套进入、逆序退出和短路语义；不同的是 Rust 通过所有权、显式 outcome 和提交 typestate 保证边界，而不是依赖一个任意可变 context 和递归 `next()` future。

## 迁移顺序

迁移必须按纵向切片完成，每一步迁移后删除原调用点，不长期保留新旧双路径。

1. 增加 characterization tests，固定当前 HTTP JSON、HTTP SSE、Responses WebSocket、history 和错误优先级行为。
2. 引入 `RequestEnvelope`、typed failure/outcome、统一错误映射与各 transport encoder，先消除三种客户端错误输出的领域特判。
3. 抽取共享 request prelude，删除 `complete` 与 `stream` 开头的重复准备逻辑。
4. 抽取 `AttemptRunner`，统一 quota、history、账号作用域调用、失败优先级和候选 decision。
5. 把 live task 改成单出口状态机；所有路径返回 `StreamTerminal` 后调用一个 exactly-once finalizer。
6. 引入单次 SSE decode 的 canonical event pipeline，逐个迁移 cyber、usage、affinity、Cloudflare、rate-limit/turn state 和 telemetry observer。
7. 删除旧的 phase wiring 和专用 API 特判，更新本文件状态为已实现。

不建议一开始重写上游 WebSocket pool。它已有独立的 lease/Drop 边界和连接回收测试，应先让 dispatch 生命周期消费它的 typed transport outcome。

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
- enter 顺序、leave 逆序、short-circuit unwind 和 controller 顺序有 contract test。
- completed、incomplete、failed、upstream disconnect、decoder error、oversize、downstream cancel 和 shutdown 都 exactly-once finalize。
- HTTP JSON、HTTP SSE 与 Responses WebSocket 对相同 `ClientFailure` 保持语义一致。
- 并发同会话状态使用不可复用 revision token/CAS 测试，旧成功不能删除新失败，状态删除或过期后重建也不能出现 ABA。
- 生产模块不加入 `#[cfg(test)]`；测试放在 `backend/tests/dispatch/lifecycle/`。
