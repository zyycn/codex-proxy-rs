# Responses WebSocket previous_response_id 低延迟恢复实施方案

日期：2026-07-11

目标基线：edbedd31ffb502b432e3c61def2ba41b51e858cd

文档状态：P0/P1 已实施并通过本地完整测试；真实链路验证待执行

适用问题：

    stream disconnected before completion:
    Upstream stream closed before response.completed:
    previous_response_not_found

## 1. 结论

不要把本问题实现成 GPT-5.6 模型特判，也不要直接照搬提交
1b5bfb37e171e7478f3d4c666100d03a501da4ff 的全局预取改法。

推荐实现由三个部分组成：

1. 在 WebSocket 连接对象自身记录该连接当前可以续接的最新 response ID。
2. 发送 store=false + previous_response_id 前，先验证当前取出的池连接是否仍持有这个 ID。
3. 只有代理自己做过隐式续接、手里保留完整输入快照时，才允许清除旧 ID 并透明重放；客户端显式传入但代理没有完整历史时，返回明确错误。

流式预取不能对所有请求统一等待真实输出。应根据请求是否具备安全重放能力选择提交边界：

- 普通请求、显式 previous_response_id：保持原有低延迟提交行为。
- 代理生成的隐式续接：在首个不可逆输出或终态前暂存结构帧，以便失败时安全重放。

这个设计比“连接代次写入 Redis”更直接。OpenAI 的缓存语义是“一条具体 WebSocket 上最近的 response”，因此最准确的真相源就是池中的连接对象，而不是账号亲和表、模型名或持久化数据库。

## 2. 基线说明

本文以 `edbedd31ffb502b432e3c61def2ba41b51e858cd` 作为事故复现与缺陷定位基线；P0/P1 实现落在后续依赖治理基线 `977d94d5812eebe57b319b4f00a99af485d28040` 之上。

不要直接 cherry-pick 1b5bfb37：

- 它的父提交是 ddd6fe160f64c40df15f6260ed2513ead54e04e6，不是目标基线。
- 它同时包含依赖、配置、监控、测试整理和文档变化。
- 它把所有 WebSocket 流都推进到“真实输出或终态”才返回，存在不必要的首包延迟和首输出超时语义变化。

## 3. 已确认的生产事实

### 3.1 故障范围

线上 previous_response_not_found 按模型统计：

| 模型 | 错误数 | 结论 |
| --- | ---: | --- |
| gpt-5.6-sol | 116 | 当前唯一观测到错误的模型 |
| 其他模型 | 0 | 不能据此证明相同连接条件下不会失败 |

116 次失败全部满足：

    model = gpt-5.6-sol
    transport = websocket
    websocket_pool_kind = new
    previous_response_id present = true
    store = false

成功请求的关键对照：

| 模型 | previous_response_id + reuse 成功 | previous_response_id + new 成功 |
| --- | ---: | ---: |
| gpt-5.5 | 162 | 0 |
| gpt-5.6-sol | 117 | 0 |

因此目前只能确认“故障在 GPT-5.6 上被观测到”，不能确认“GPT-5.5 在相同的 new + previous_response_id + store=false 条件下会成功”。线上没有这个对照样本。

### 3.2 本次 response ID 的时间线

失效 ID：

    resp_0de25f0dd99612a3016a51087f8b5c8194b20293e08fb6aa5f

关键请求：

| 北京时间 | 请求 ID | 事实 |
| --- | --- | --- |
| 22:58:06 | req_3090a40b-200e-4997-a910-01d522974a59 | gpt-5.6-sol，复用 WebSocket，正常完成并产生上述 response ID |
| 22:58:30 | 同上 | completed=true，store=false，首输出约 5599ms |
| 22:58:31 | req_aee4bb05-d67e-454e-9913-10a5db5afd64 | 同账号、同 conversation hash、同 pool key，再次复用；HTTP response 已返回，但没有流终结记录 |
| 23:00:22 起 | 后续重试 | 连接已经变成 new，继续携带旧 response ID |
| 23:05:16 | req_0b171310-8b4e-4b0b-90fa-fd27d334eda9 | new WebSocket，返回嵌套的 previous_response_not_found |
| 23:05:22 | req_215248f4-d0ed-44c2-a565-4a6fdd393bee | 同样失败 |

原响应和失败请求使用同一账号、同一 conversation hash、同一 pool key。账号轮换不是根因。

### 3.3 首包与首输出数据

线上两个 Rust 容器均未覆盖 first_token_timeout_ms，因此目标基线会使用默认 20000ms。

最近三天 5722 条 gpt-5.6-sol 成功流记录：

| 指标 | first_token_ms |
| --- | ---: |
| p50 | 2716ms |
| p95 | 6929ms |
| p99 | 11275ms |
| 大于 15 秒 | 21 条 |
| 大于 20 秒 | 9 条 |
| 最大值 | 37334ms |

first_token_ms 与 WebSocket 内部熔断的起算点不完全相同，因此不能断言这 9 条都会触发超时；但足以证明不能把“等待真实输出”直接套进现有 20 秒超时而不做语义拆分。

故障请求中，上游错误通常在服务端返回 HTTP response 后约 39 至 75ms 到达。对可恢复请求多等待这几十毫秒可以捕获错误，但把同一等待策略扩大到所有正常请求是不必要的。

## 4. 官方协议依据

OpenAI WebSocket Mode 文档明确说明：

- 活跃 WebSocket 只在连接本地内存缓存最近一条 previous response。
- store=false 没有持久化回退。
- ID 不在连接缓存时返回 previous_response_not_found。
- 连接关闭后，如果不能从持久化响应续接，应清空 previous_response_id 并发送完整输入。

参考：

- https://developers.openai.com/api/docs/guides/websocket-mode/
- https://developers.openai.com/api/docs/guides/conversation-state/

官方 Codex 0.144.1 的实现也把增量状态绑定到当前 WebSocket session：

- 构造增量请求：
  https://github.com/openai/codex/blob/rust-v0.144.1/codex-rs/core/src/client.rs#L1223-L1253
- 创建新连接前清空 last_request 和 last_response_rx：
  https://github.com/openai/codex/blob/rust-v0.144.1/codex-rs/core/src/client.rs#L1323-L1354
- 连接错误后新连接发送完整输入的测试：
  https://github.com/openai/codex/blob/rust-v0.144.1/codex-rs/core/tests/suite/client_websockets.rs#L1986-L2073

官方行为是协议级状态管理，没有 GPT-5.6 模型分支。

## 5. 目标基线的缺陷位置

目标提交 edbedd31 中的相关入口：

| 文件 | 当前职责 | 缺陷 |
| --- | --- | --- |
| backend/src/upstream/openai/transport/websocket_pool.rs | 按账号和 conversation 保存池连接 | PooledWebSocketConnection 不记录该连接缓存的最新 response ID |
| backend/src/upstream/openai/transport/websocket.rs | acquire、复用、新建和 stale-reuse 重试 | 新连接仍原样发送 store=false 的旧 previous_response_id |
| backend/src/upstream/openai/transport/websocket/stream.rs | 预取和 live stream 转发 | 首内容、连接健康、真实输出和下游提交共用近似语义 |
| backend/src/upstream/openai/protocol/websocket.rs | WebSocket 帧解析 | 错误分类丢失结构化 code；没有提取 completed response ID 的公共函数 |
| backend/src/dispatch/responses/service.rs | 隐式续接与 history recovery | recover_request_history 会在没有完整快照时直接删除客户端显式 history |
| backend/src/dispatch/responses/service/stream.rs | 流式重试循环 | 结构帧可能先越过预取边界，错误进入 live stream 后无法回到调度循环 |
| backend/src/dispatch/responses/service/complete.rs | 非流式重试循环 | 显式 previous ID 也可能被无损性不足的 strip-history 重试 |
| backend/src/dispatch/errors.rs | 错误分类 | previous_response_not_found 依赖正文字符串扫描 |
| backend/src/dispatch/responses/errors.rs | 对外调度错误 | 没有独立 HistoryUnavailable 类型 |
| backend/src/api/client/errors.rs | OpenAI 兼容错误输出 | 无稳定 previous_response_unavailable 错误码 |

目标基线的 recover_request_history：

    1. 忘记 affinity 中的旧 response ID。
    2. 如果有 ImplicitResumeSnapshot，则恢复完整输入。
    3. 无论是否有快照，都清除 previous_response_id 和 turn_state。
    4. 重试同一请求。

第 3 步对客户端显式增量请求是不安全的。客户端可能只发送了“本轮新增 input”，删除 ID 后直接上送会丢失全部历史。

## 6. 根因模型

    完成响应 A，store=false
      -> 上游 WebSocket W1 内存缓存 latest_response_id = A
      -> 代理 session affinity 只记住账号和 conversation
      -> 下一轮下游取消，代理销毁 W1
      -> W1 的连接本地缓存随之消失
      -> 客户端仍携带 A
      -> 连接池没有 W1，只能创建 W2
      -> 当前代码把 A 原样发给 W2
      -> 上游返回 previous_response_not_found
      -> 非输出结构帧先提交 HTTP/SSE
      -> 错误只能包装成 stream_disconnected

需要修复两个独立问题：

1. 不允许把连接本地 ID 发到错误连接。
2. 只有确实拥有完整上下文时才允许透明重放。

## 7. 必须保持的设计不变量

1. store=false 的 previous_response_id 只能发送到明确缓存该 ID 的同一池连接。
2. store=true 可以在新连接上尝试持久化 hydration，不做本地提前拒绝。
3. session affinity 只负责选账号和 conversation，不负责证明 response ID 在某条连接上可用。
4. 连接对象是 connection-local previous response 状态的唯一真相源。
5. 透明重放必须有完整输入快照作为能力凭证。
6. 客户端显式 previous ID 且没有完整历史时，禁止删除 ID 后重试。
7. 真实输出、工具调用参数或有效 output item 已经发给客户端后，禁止透明重试。
8. 同一个入站请求最多做一次 history replay。
9. 不通过 model == gpt-5.6 判断行为。
10. 不把 store=false 静默改成 store=true。
11. 不把 response ID 原文写入普通日志、错误消息或 WebSocket audit。
12. 正常请求不能因为修复而统一推迟到真实 token 才返回 HTTP response。

## 8. 方案比较

### 8.1 GPT-5.6 模型特判

做法：仅在模型为 gpt-5.6-sol 时捕获错误或删除 ID。

拒绝原因：

- 官方语义属于 WebSocket 和 store，不属于模型。
- gpt-5.5 没有 new + previous_response_id 的线上对照样本。
- 后续模型会继续复制同一协议行为。
- 模型别名、路由后缀和新模型会形成维护表。

### 8.2 所有流统一预取到真实输出或终态

做法：把 prefetch_first_content_stream_frames 改为对所有请求等待真实输出或终态。

优点：实现直接，能够在 HTTP response 返回前捕获结构帧之后的错误。

拒绝作为最终方案的原因：

- 正常请求的 response.created 或结构事件可能被延迟。
- 会把现有 20 秒首内容熔断变成更严格的真实输出熔断。
- 线上已有成功请求的首输出超过 20 秒。
- 对客户端显式、不可重放的请求，即使提前捕获错误也无法透明恢复。

### 8.3 连接本地状态 + 能力感知预取

这是本文推荐方案：

- 池连接记录 latest_response_id。
- 发送前主动验证 continuation。
- 只有 Replayable 请求使用 UntilOutputOrTerminal 预取。
- 其他请求保持 FirstForwardableContent。
- 上游仍返回 previous_response_not_found 时作为后备保护。

## 9. 推荐的数据结构

### 9.1 WebSocketContinuationRequirement

在 backend/src/upstream/openai/transport/websocket/types.rs 增加：

    enum WebSocketContinuationRequirement {
        NewChain,
        Persisted { response_id: String },
        ConnectionLocal { response_id: String },
    }

构造规则：

    previous_response_id 为空
        -> NewChain

    previous_response_id 存在且 store=true
        -> Persisted

    previous_response_id 存在且 store=false
        -> ConnectionLocal

把该字段加入 CodexWebSocketRequest。它是本地控制信息，不进入上游 JSON。

不要在 transport 中重新解析 payload_text 获取 store 和 previous ID。构造 prepared request 时已经持有 CodexResponsesRequest，应在这里一次性生成强类型 requirement。

### 9.2 WebSocketContinuationState

在 backend/src/upstream/openai/transport/websocket_pool.rs 增加：

    struct WebSocketContinuationState {
        latest_response_id: Option<String>,
    }

把它作为独立字段加入 PooledWebSocketConnection：

    struct PooledWebSocketConnection {
        websocket: PumpedWebSocket,
        metadata: CodexWebSocketConnectionMetadata,
        continuation: WebSocketContinuationState,
        created_at: Instant,
    }

不要把 latest_response_id 塞入 CodexWebSocketConnectionMetadata。握手 metadata、限流头、turn state 与协议续接状态生命周期不同，独立类型更清楚。

状态只在收到并成功校验 response.completed 后更新。以下情况不能记录：

- response.created
- response.in_progress
- response.failed
- error
- response.incomplete
- completed 帧解析失败
- 下游发送失败导致连接被 discard
- 上游在 terminal 前关闭

每条连接只保存一个最新 ID，正好对应官方“一条连接缓存最近一条 previous response”的语义。不需要 response ID 集合。

### 9.3 PreviousResponseUnavailableReason

增加内部原因枚举：

    enum PreviousResponseUnavailableReason {
        PoolUnavailable,
        FreshConnectionRequired,
        ConnectionBusy,
        LatestResponseMismatch,
        ReusedConnectionLost,
        UpstreamRejected,
    }

Display 不得包含 response ID 原文。

对外错误统一为：

    type: invalid_request_error
    code: previous_response_unavailable
    status: 400
    message: Previous response context is no longer available. Start a new chain and resend the full input context.

ConnectionBusy 不表示历史已经丢失，应单独返回可重试错误：

    status: 503
    code: continuation_connection_busy

不得为了避开 Busy 在新连接上发送 store=false 的旧 ID。

### 9.4 HistoryRecoveryPlan

在 dispatch 层用显式类型代替“Option 是否刚好为 Some”的隐含约定：

    enum HistorySource {
        None,
        Explicit,
        Implicit,
    }

    struct HistoryRecoveryPlan {
        source: HistorySource,
        replay: Option<ImplicitResumeSnapshot>,
    }

规则：

- 客户端原请求带 previous_response_id：source=Explicit，replay=None。
- 代理根据完整输入自行应用 implicit resume：source=Implicit，replay=Some(snapshot)。
- 没有 history：source=None，replay=None。

take_replay 消耗 Option，天然保证最多恢复一次。

### 9.5 StreamCommitPolicy

增加：

    enum StreamCommitPolicy {
        FirstForwardableContent,
        UntilOutputOrTerminal,
    }

选择规则：

    recovery_plan.replay.is_some()
        -> UntilOutputOrTerminal

    其他
        -> FirstForwardableContent

恢复后完整输入请求已经不带 previous ID，下一次尝试必须回到 FirstForwardableContent。

## 10. 连接池发送前验证

在 execute_response_create_request_with_pool 和
execute_response_create_request_stream_with_pool 中，必须在 send 之前执行验证。

行为矩阵：

| requirement | acquire 结果 | 行为 |
| --- | --- | --- |
| NewChain | Reused | 允许发送 |
| NewChain | FreshReserved / Bypass | 保持现有行为 |
| Persisted | 任意 | 允许发送，由上游 hydration 决定 |
| ConnectionLocal | Reused 且 latest ID 相同 | 允许发送 |
| ConnectionLocal | Reused 但 latest ID 不同 | 将连接原样 put 回池，返回 LatestResponseMismatch |
| ConnectionLocal | FreshReserved | discard 预留 slot，不建连，返回 FreshConnectionRequired |
| ConnectionLocal | Bypass(Busy) | 不建新连接，返回 ConnectionBusy |
| ConnectionLocal | Bypass(Disabled/Cap) | 不建新连接，返回 PoolUnavailable |
| ConnectionLocal | pool=None | 不建新连接，返回 PoolUnavailable |

关键点：

- mismatch 时不能销毁一条仍然健康的连接；它可能仍可服务携带 latest ID 的正确请求。
- FreshReserved 已在池里放入 Busy 占位，提前返回前必须 discard，避免 slot 永久卡住。
- 验证必须同时覆盖流式和非流式入口。

## 11. stale-reuse 重试规则

目标基线当前行为：

    复用连接在首帧前死亡或首 token 超时
      -> 使用同一个 payload 新建 WebSocket 重试

这个逻辑对 NewChain 合理，对 store=false + previous_response_id 错误。

修改为：

| 请求类型 | 复用连接失效后的行为 |
| --- | --- |
| NewChain | 保持现有 fresh retry |
| Persisted | 可以 fresh retry |
| ConnectionLocal | 禁止 transport 原样 fresh retry，返回 ReusedConnectionLost |

ReusedConnectionLost 交给 dispatch：

- HistoryRecoveryPlan 有 replay：恢复完整请求并重试。
- 无 replay：返回 previous_response_unavailable。

这样 transport 不需要理解完整上下文，dispatch 也不会错误猜测连接状态。

## 12. 成功响应如何更新 latest_response_id

### 12.1 协议解析

在 backend/src/upstream/openai/protocol/websocket.rs 增加：

    websocket_response_completed_id(raw: &str) -> Result<Option<String>, String>

行为：

- 非 response.completed 返回 Ok(None)。
- completed 先沿用现有 ResponseCompleted 强类型校验。
- 校验成功后返回 response.id。
- id 缺失或为空返回解析错误。

不要通过搜索原始字符串 resp_ 提取。

### 12.2 非流式 WebSocket

collect_websocket_response 接收并返回 WebSocketContinuationState。

读取到 response.completed：

1. 校验 completed shape。
2. 提取 ID。
3. continuation.latest_response_id = Some(id)。
4. 返回 exchange、websocket、metadata、continuation。
5. pool.put 时把 continuation 放回 PooledWebSocketConnection。

### 12.3 流式 WebSocket

WebSocketStreamPoolReturn 增加 continuation 字段。

forward_websocket_response_stream 读取到 response.completed 时：

1. 在发送 terminal SSE 前完成 ID 校验和状态更新。
2. 如果下游发送成功，finish_stream_websocket 把新状态放回池。
3. 如果下游发送失败，discard 连接，不保留状态。

新建池连接的 continuation 为 default；复用连接沿用取出的 continuation。

## 13. 错误必须结构化分类

目标基线的 is_history_recovery_signal 会扫描：

- previous_response_not_found
- previous response + not found
- no tool output found
- invalid encrypted content

本问题不能继续依赖 message 文本。

修改 protocol/websocket.rs 的 ClassifiedWebSocketError，使其保留 error.code。至少精确识别：

    previous_response_not_found

WebSocket error 事件中 code 的读取路径继续支持现有官方形状：

    /response/error/code
    /error/code

ResponsesSseFailure 已有 upstream_code。增加精确函数：

    fn is_previous_response_not_found_failure(failure: &ResponsesSseFailure) -> bool {
        failure.upstream_code.as_deref() == Some("previous_response_not_found")
    }

消息字符串只能用于诊断展示，不能决定是否清除历史和重放。

invalid_encrypted_content 和 tool output 缺失属于不同失败种类，可以继续走各自恢复策略，但不要再与 previous_response_not_found 合并为一个模糊 bool。

建议增加：

    enum HistoryFailureKind {
        PreviousResponseNotFound,
        InvalidEncryptedContent,
        MissingToolOutput,
    }

## 14. dispatch 安全恢复

### 14.1 调整快照捕获时机

目标基线 apply_implicit_resume 在完成 affinity 查询前就 clone 完整 input。

将 ImplicitResumeSnapshot::capture(request) 移到所有资格检查通过之后、第一次修改 request 之前：

    1. 找 continuation_start。
    2. 查 conversation、variant、instructions、function call、account。
    3. 构造 replay input。
    4. 此时 capture 原始完整请求。
    5. 再 set_previous_response_id、set_input、修改 transport。

这样未命中隐式续接时不会无意义 clone 大输入。

### 14.2 替换 recover_request_history

删除“无快照也 strip history”的行为。

新函数语义：

    async fn try_replay_full_request(
        request,
        recovery_plan,
        account_id,
        evict_reasoning_replay,
    ) -> bool

流程：

1. recovery_plan.take_replay()。
2. 没有 snapshot，返回 false，不修改请求。
3. 必要时清除 reasoning replay。
4. 忘记失效 response ID 的 session affinity。
5. snapshot.restore(request)。
6. 清除 previous_response_id 和 turn_state。
7. 返回 true。

只有返回 true 才能 continue 调度循环。

### 14.3 显式 previous ID

客户端显式 previous ID 失败时：

- 不删除 ID 后重试。
- 不切换账号后用残缺输入重试。
- 释放账号 lease。
- 返回 ResponseDispatchError::HistoryUnavailable。

非流式响应：

    HTTP 400
    type = invalid_request_error
    code = previous_response_unavailable

流式请求如果尚未开始下游 body：

    保持 Responses 流式兼容形状
    event = response.failed
    code = previous_response_unavailable

不得把完整上游 JSON 嵌套进 stream_disconnected message 返回给客户端。

### 14.4 同账号重放

有快照的恢复必须固定到刚才使用的账号：

    next_required_account_id = Some(release_account_id)

原因：

- reasoning replay 和 account scope 相关。
- 避免恢复动作变成账号轮换。
- 与现有 session affinity 语义一致。

## 15. 流式提交和超时必须拆成三个概念

不要再用 saw_content_frame 同时表示三个状态。

至少拆为：

    saw_upstream_activity
    saw_irreversible_output
    downstream_commit_ready

含义：

| 状态 | 用途 |
| --- | --- |
| saw_upstream_activity | 判断连接是否已经有响应，切换初始等待与 active idle timeout |
| saw_irreversible_output | 判断透明重试是否还安全，并记录真实 TTFT |
| downstream_commit_ready | 由 StreamCommitPolicy 决定何时返回下游 Response |

事件分类：

| 事件 | activity | irreversible output | terminal |
| --- | --- | --- | --- |
| response.created | 是 | 否 | 否 |
| response.in_progress | 是 | 否 | 否 |
| response.content_part.added 且文本为空 | 是 | 否 | 否 |
| metadata / rate_limits | 是 | 否 | 否 |
| 非空 reasoning delta | 是 | 是 | 否 |
| 非空 output_text delta | 是 | 是 | 否 |
| function_call_arguments delta | 是 | 是 | 否 |
| 有效 output_item.added | 是 | 是 | 否 |
| response.completed | 是 | 否 | 是 |
| response.failed / error | 是 | 否 | 是 |

### 15.1 超时调整

目标基线的 first_token_timeout_ms 名字和行为已经混合“连接健康”与“真实输出”。

推荐在本次实现中拆分：

    ws_pool:
      initial_event_timeout_ms: 20000
      first_output_timeout_ms: 0

语义：

- initial_event_timeout_ms：发出 response.create 后，等待第一个有效上游活动事件的时间。
- first_output_timeout_ms：等待不可逆输出的可选业务上限；0 表示禁用。
- 收到任何有效活动后使用现有 active stream idle timeout。

生产配置目前没有显式 first_token_timeout_ms，因此可以直接把示例和默认配置迁移到新字段，不需要保留旧字段兼容分支。

如果暂时不想改配置结构，最低要求也是：

- guarded prefetch 收到 response.created 或其他结构事件后，不得继续使用 20 秒“无活动”熔断。
- 之后使用 active idle timeout 等待 output 或 terminal。

不要简单把默认值从 20 秒拍脑袋改成 60 秒，这只是在掩盖两个不同概念。

## 16. 按文件实施步骤

### 第一步：协议与纯类型

修改：

- backend/src/upstream/openai/protocol/websocket.rs
- backend/src/upstream/openai/transport/websocket/types.rs

增加：

- completed response ID 提取。
- 结构化 WebSocket error code。
- WebSocketContinuationRequirement。
- PreviousResponseUnavailableReason。
- StreamCommitPolicy。

先写纯单元测试，再进入连接池代码。

### 第二步：连接池 continuation 状态

修改：

- backend/src/upstream/openai/transport/websocket_pool.rs
- backend/tests/upstream/openai/transport/websocket_pool.rs

增加 WebSocketContinuationState，并覆盖 acquire、put、discard、过期关闭的状态生命周期。

### 第三步：transport 发送前校验

修改：

- backend/src/upstream/openai/transport/websocket.rs
- backend/src/upstream/openai/transport/client/requests.rs
- backend/src/upstream/openai/transport/client.rs

确保流式、非流式、pool=None、FreshReserved、Bypass 和 stale-reuse 全部分支遵循行为矩阵。

### 第四步：完成事件更新连接状态

修改：

- backend/src/upstream/openai/transport/websocket/stream.rs
- backend/src/upstream/openai/transport/websocket.rs

让聚合和 live stream 两条路径都只在合法 response.completed 后更新 latest ID。

### 第五步：dispatch 恢复能力

修改：

- backend/src/dispatch/implicit_resume.rs
- backend/src/dispatch/responses/service.rs
- backend/src/dispatch/responses/service/complete.rs
- backend/src/dispatch/responses/service/stream.rs
- backend/src/dispatch/responses/sse_failure.rs

引入 HistoryRecoveryPlan，延后快照 clone，删除显式 history 的不安全 strip-and-retry。

### 第六步：客户端错误和可观测性

修改：

- backend/src/dispatch/errors.rs
- backend/src/dispatch/responses/errors.rs
- backend/src/api/client/errors.rs
- backend/src/dispatch/responses/event_recording.rs
- backend/src/dispatch/responses/stream_lifecycle.rs

增加 stable error code 和诊断字段。

### 第七步：超时语义

修改：

- backend/src/bootstrap/config.rs
- backend/src/bootstrap/services.rs
- backend/src/upstream/openai/transport/websocket_pool.rs
- deploy/config.example.yaml

拆分 initial activity 和 first output timeout。不要保留两套字段别名。

## 17. 必须新增的测试

### 17.1 协议单元测试

1. response.completed 能提取 response.id。
2. 非 completed 返回 None。
3. completed 缺少 ID 返回解析错误。
4. error.code=previous_response_not_found 精确分类。
5. 仅 message 包含相同文字但 code 不匹配时，不触发自动恢复。

### 17.2 连接池测试

1. 首次成功响应后，池连接保存 latest response ID。
2. 相同 ID 在同一连接上允许续接。
3. 不同 ID 被拒绝，并把健康连接放回池。
4. FreshReserved + connection-local ID 不建立 TCP/WebSocket。
5. Busy + connection-local ID 不 bypass 到新连接。
6. store=true previous ID 允许新连接 hydration。
7. discard 后 continuation state 一起消失。
8. max-age 过期后旧 ID 不可继续发送。

### 17.3 transport 集成测试

1. new chain 完成后，第二轮在同一 socket 发送 previous ID。
2. 第二轮前关闭池连接，断言不会在新 socket 发送旧 ID。
3. reused socket 在 send 前死亡，connection-local 请求不得 fresh retry 原 payload。
4. 无 previous ID 的 stale-reuse 仍可 fresh retry。
5. store=true 的 previous ID 可以在 fresh socket 发送。
6. 上游没有收到被提前拒绝请求的 payload。

### 17.4 dispatch 恢复测试

1. implicit resume + 池连接丢失：直接恢复完整 input，只向上游发送一次完整请求。
2. explicit previous ID + 池连接丢失：返回 previous_response_unavailable，上游零请求。
3. implicit resume 在匹配连接上收到：

       response.created
       response.content_part.added
       previous_response_not_found

   断言结构事件未下发，完整请求重放一次。

4. explicit previous ID 收到同样错误：不重放。
5. 真实 output_text.delta 后再失败：不重放。
6. function call 参数产生后再失败：不重放。
7. recovery plan 消耗后第二次错误不再重试。
8. 恢复固定在原账号。
9. 测试模型至少包含 gpt-5.6-sol，但实现断言不能检查模型名。

### 17.5 延迟和超时测试

使用 Tokio paused time 覆盖：

1. FirstForwardableContent 策略在结构内容到达时立即返回，不等真实 delta。
2. UntilOutputOrTerminal 策略缓存结构内容。
3. 收到 response.created 后等待 30 秒再输出，不触发 20 秒“无活动”超时。
4. 真实输出到达后立即提交，额外本地开销不包含固定 sleep。
5. terminal error 到达后立即返回恢复循环。

## 18. 可观测性

普通日志和 ops error metadata 增加：

    history_source = none | explicit | implicit
    history_replay_available = true | false
    ws_continuation = new_chain | persisted | matched | missing | mismatch | busy
    recovery_action = none | replay_full | reject
    recovery_attempt = 0 | 1
    prefetch_policy = first_content | output_or_terminal
    upstream_request_sent = true | false
    initial_event_ms
    first_output_ms
    response_ready_ms

response ID 和 connection 标识只记录稳定短 hash，不记录原值。

这些内部诊断不得写成额外 usage_records。usage_records 只代表真实的 /v1 请求事实；恢复原因写入现有请求的 metadata 或 ops error log。

WebSocket audit 继续脱敏：

- instructions
- input
- previous_response_id
- prompt_cache_key
- client_metadata
- tools

## 19. 延迟验收标准

需要分别比较：

    request_start -> downstream HTTP response ready
    request_start -> first SSE event
    request_start -> first irreversible output
    upstream first output -> downstream first output

验收要求：

1. NewChain 和显式匹配续接的 response-ready 分布不得因修复系统性后移。
2. upstream first output 到 downstream first output 不得出现固定等待窗口。
3. normal success 的 p50/p95 TTFT 与基线差异应处于本地调度抖动范围。
4. connection-local mismatch 应在建立新 WebSocket 前失败。
5. replayable mismatch 应比“先请求上游再等 400”更快。
6. first output 超过 20 秒但已有上游活动的请求不能被误判为死连接。

不要使用 50ms、100ms 之类固定 grace window 等待错误。生产错误碰巧在 39 至 75ms 内到达，不代表协议保证这个时序。

## 20. 验证命令

回退后先验证基线：

    cargo test --manifest-path backend/Cargo.toml previous_response
    cargo test --manifest-path backend/Cargo.toml websocket_pool

开发中按阶段运行：

    cargo test --manifest-path backend/Cargo.toml websocket_response_completed
    cargo test --manifest-path backend/Cargo.toml previous_response
    cargo test --manifest-path backend/Cargo.toml responses_websocket
    cargo test --manifest-path backend/Cargo.toml websocket_pool

最终验证：

    cargo fmt --manifest-path backend/Cargo.toml -- --check
    cargo clippy --manifest-path backend/Cargo.toml --all-targets --all-features -- -D warnings
    cargo test --manifest-path backend/Cargo.toml
    bash backend/scripts/check_architecture.sh

如果测试中确认真实缺陷，先修复该缺陷并重新跑当前阶段，再进入下一组测试。

## 21. next 实例真实链路验证

不要直接切主实例。先在 next 实例开启 WebSocket audit：

    CODEX_PROXY_WS_AUDIT_DIR=.runtime/ws-audit

场景矩阵：

| 场景 | 预期 |
| --- | --- |
| gpt-5.6-sol，同 socket 连续两轮 | 第二轮 matched，正常增量 |
| 第一轮完成后主动 discard socket，第二轮为显式 ID | 本地 previous_response_unavailable，上游零请求 |
| 第一轮完成后 discard，第二轮为代理 implicit resume | 直接发送完整 input，不发送旧 ID |
| store=true，第一轮 socket 丢失 | 允许新连接尝试 hydration |
| 同 conversation 并发导致 Busy | 不创建新连接发送 connection-local ID |
| 结构事件后 upstream not found，且可重放 | 客户端不看到旧结构事件，完整重放成功 |
| 首输出超过 20 秒但持续有生命周期活动 | 不因 initial activity timeout 失败 |
| 真实输出后 upstream 错误 | 不透明重放 |

真实链路文档必须记录每个场景的 request ID、pool decision、是否真的发送上游、first output 和 response-ready 时间。

## 22. 部署观察

灰度时重点观察：

- previous_response_not_found 按 ws_continuation 原因分组。
- previous_response_unavailable 数量。
- recovery_action=replay_full 成功率。
- upstream_request_sent=false 的提前拦截数量。
- gpt-5.6-sol 的 response-ready、first-output p50/p95/p99。
- ConnectionBusy 数量。
- WebSocket pool discard reason。
- 是否仍出现同一个 response ID 连续重试风暴。

不应观察到：

- gpt-5.6 专属分支命中日志。
- 显式 previous ID 被 strip 后当作新链发送。
- store=false 旧 ID 出现在 ws_pool=new 的上游 payload。
- 正常请求统一延迟到文本 delta 才返回。
- 因内部恢复产生额外 usage_records。

## 23. 回滚

本方案不引入数据库 schema，也不依赖 Redis 持久化，因此回滚只涉及二进制和配置。

如果采用新的超时字段，回滚旧版本前必须同步恢复旧配置字段，否则旧版本会因 deny_unknown_fields 拒绝启动。

回滚触发条件：

- 正常请求 response-ready p95 明显回退。
- gpt-5.6-sol 首输出超时增加。
- pool Busy 导致大量可用请求失败。
- store=true hydration 被错误提前拒绝。
- full replay 出现重复工具调用或重复输出。

## 24. 实施完成定义

只有全部满足才算修复完成：

- store=false 旧 ID 不会发送到新连接。
- 连接只保存最近一个成功 completed response ID。
- 显式增量请求没有完整历史时不会伪恢复。
- 隐式续接丢失连接时能够用完整输入恢复。
- 上游 exact code 结构化分类，不靠 message 判断本错误。
- 已输出后不重试。
- 正常请求不承担全局预取延迟。
- initial activity 与 first output 超时语义已经分开。
- 流式和非流式测试均覆盖。
- GPT-5.6 真实链路验证通过。
- ops 日志可解释每次 reject 或 replay。
- 完整 fmt、clippy、test、architecture check 通过。

## 25. 参考工程的取舍

sub2api：

    /home/zyy/桌面/Codes/sub2api/backend/internal/service/openai_ws_forwarder_ingress.go

其优点是保留 replay input、只做一次恢复，并避免在 function_call_output 无完整链路时盲目删除 ID。可以参考“有完整重放输入才恢复”的原则。

CLIProxyAPI：

    /home/zyy/桌面/Codes/CLIProxyAPI/sdk/api/handlers/openai/openai_responses_websocket.go

其优点是保存 transcript，并在可恢复错误后释放 pinned auth、强制下一次完整 transcript replay。可以参考“连接失效后增量状态也必须失效”的原则。

不应直接照抄：

- sub2api 的 message/场景启发式不能替代强类型连接状态。
- CLIProxyAPI 的完整 transcript 常驻策略会增加内存和隐私边界。
- 本工程已有 ImplicitResumeSnapshot，应该复用现有完整输入能力，而不是再建第二套 transcript 缓存。

## 26. 最终建议

实现时优先完成以下最小闭环：

    PooledWebSocketConnection.latest_response_id
      + store=false 发送前精确校验
      + stale-reuse 禁止原 payload 新连接重试
      + HistoryRecoveryPlan
      + 显式 history 不再 strip-and-retry
      + 结构化 previous_response_unavailable

能力感知预取和超时状态拆分必须与流式回归测试一起完成，不能只移植全局 UntilOutputOrTerminal。

这套实现既能解决当前 GPT-5.6 事故，也保持模型无关、低延迟、无额外持久化和清晰的状态所有权。
