# Codex 上游交互与 Responses WebSocket 恢复审计

日期：2026-07-11

工程基线：`977d94d5812eebe57b319b4f00a99af485d28040`

上游基线：OpenAI Codex `5c19155cbd93bfa099016e7487259f61669823ff`

桌面版本：Codex Desktop `26.707.41301`

文档状态：P0/P1 已实施并通过本地完整测试；真实链路验证待执行。

## 1. 审计范围

本次审计同时对照以下三类证据：

1. 当前工程的 `/v1/responses`、HTTP/SSE、Responses WebSocket、连接池、隐式续接、账号调度和遥测链路。
2. `/home/zyy/桌面/Codes/codex-desktop-linux` 中锁定的最新桌面版本与其依赖信息。
3. OpenAI Codex 官方源码在 `5c19155c` 的请求构造、连接状态机、响应解析、错误分类和默认阈值。

重点检查：

- `previous_response_id` 在连接复用、断线、换连接和恢复时的所有权。
- HTTP/SSE 与 WebSocket 的请求体、请求头和客户端元数据。
- 根据响应头、流事件和错误字段做出的运行时判断。
- 首输出、空闲、连接寿命、重试和账号轮换阈值。
- 模型目录、限流、有效模型、安全元数据和诊断信息的更新链路。

本工程是透明代理，不应机械复制 Codex CLI 的请求构造器。官方源码用于确认协议语义和服务端约定；客户端请求的业务字段和上游响应的业务字段默认保持原样。

### 1.1 透明代理边界

这里的“透明”是 JSON、header 和 SSE 事件的协议语义透明。代理会解析并重新序列化 JSON，因此不承诺空白字符逐字节相同，但字段、值、类型、未知扩展和事件顺序不能被无理由修改。

允许的变化是封闭清单：

1. 将客户端 API key 替换为选中上游账号的 Authorization，并设置对应 `chatgpt-account-id`。
2. 剥离 hop-by-hop header、客户端本地 transport hint，以及不能泄漏给另一侧的 cookie、token 和账号身份。
3. 注入上游连接必需的 Host、WebSocket upgrade、beta、指纹和受控风控 header。
4. 在客户端显式使用代理公开的模型别名时，将别名解析成真实 model ID；同时保留 requested model 供审计。
5. HTTP/SSE 与 WebSocket 之间只转换传输 envelope，事件 JSON 载荷保持不变。
6. 在账号认证、额度、模型能力或单节点 Cloudflare 风控失败时更新账号状态，并按 failure scope 决定继续候选或触发级联保护。
7. 对会跨账号建立关联的 identity/risk 字段做账号作用域伪名化；原始值只留在本地请求上下文，不直接暴露给上游账号。

以下行为默认禁止：

- 给 body 注入客户端没有提供的业务默认值。
- 给缺失的 `prompt_cache_key` 生成值，或生成/覆盖 `previous_response_id`、input、reasoning、tools、text、service tier。
- 覆盖普通客户端 metadata；只有明确列入安全策略的 reserved identity key 可以被账号作用域值替换。
- 在正常路径为了优化而把完整请求改写成增量请求；账号 failover 只有持有完整快照时才允许派生语义等价的完整请求。
- 把可由客户端处理的上游 4xx code、message、param 或 JSON body 包装成另一种错误。
- 给成功响应增加便利字段，或改写上游已经返回的 output/event 内容。

账号轮换是明确的代理职责，不受普通透明透传限制。history 所属账号不可用且账号池仍有可用账号时，代理不能直接中断客户端；它必须使用自己完整保存的上下文，在当前设置所选调度策略排出的新账号上执行语义等价的 full replay。约束是不能把原账号/原 socket 绑定的同一 `previous_response_id` 直接发给新账号，也不能用残缺上下文假装恢复。

### 1.2 账号池与账号保护不变量

“发挥账号池效果”和“保护账号”必须同时成立：

1. 每个请求进入 dispatch 时冻结一份符合模型、计划、账号状态和冷却条件的候选账号 ID 快照；亲和账号和订阅层级只决定顺序，不得裁掉低优先级但可用的账号。
2. node-local 账号失败必须更新账号状态、从本请求排除该账号并继续下一个候选。只有候选快照中的账号都已尝试或被实时状态排除，才能生成账号池耗尽错误；瞬时 slot/connection Busy 只改变租用时机，应先尝试其他账号再回访，不能冒充账号耗尽。本实现不设代理自定义的候选次数、换号时间预算或风险熔断。
3. 固定数字只能限制单账号内部的网络重试，不能限制账号候选遍历。候选有 20 个时，quota verify、model unsupported 或 5xx failover 都不能在第 2 或第 5 个账号提前结束。
4. 每次换账号都重新构造账号 envelope：Authorization、上游 account ID、cookie、Cloudflare 状态、连接、配额、冷却和受控指纹只来自新账号及代理运行时，不能继承客户端或前一账号的值。
5. `previous_response_id`、turn state、连接本地状态和账号绑定的加密 reasoning 不能跨账号；history-bound 请求必须从不含凭据的完整业务快照派生 full replay。
6. 客户端业务 body、工具调用和工具输出默认不变。风控剥离只处理账号/连接绑定信息、不可信身份 header 和明确的 reserved identity metadata，不能借机改写 prompt、reasoning 配置、tools 或响应内容。
7. 账号禁用、封禁、额度耗尽、429 冷却、Cloudflare challenge/path block、cookie 更新和模型不支持都要反馈到账号池，避免后续请求继续伤害同一账号。
8. 已经向客户端提交不可逆输出后，盲目换账号重放可能重复文本、工具调用或副作用，禁止这样做；账号池耗尽保证适用于请求仍可安全重放的阶段。

只有精确的账号信号才能修改持久/运行时账号状态。generic 5xx、网络断开、空响应和客户端请求错误只在当前请求 ledger 中排除该账号，不得据此把账号永久禁用、写长期冷却或污染模型能力；这类状态变化需要明确 code/header 或独立探测证据。

账号保护也意味着不能把确定性的客户端错误广播到整个账号池。请求结构非法、上下文超限、缺少 tool output 等与账号无关的 4xx 应保留首个上游原始错误并立即返回；账号认证、额度、风控、能力和可恢复传输故障才进入账号遍历。错误分类必须基于结构化 status/code，而不是 message 文本猜测。

### 1.3 故障域与风险剖离

账号池中的每个账号是独立故障节点，但账号共享代理出口、运行时指纹和请求来源，因此不能假设所有失败都只属于单节点。

| 故障域 | 识别依据 | 动作 |
| --- | --- | --- |
| 节点级 | 精确 token/quota/account/model capability，或只在一个账号出现的 cookie/连接故障 | 隔离该账号，清理其连接/状态，使用干净 envelope 继续下一候选 |
| 会话污染 | previous ID、turn state、加密 reasoning 与目标账号/socket 不匹配 | 不发送污染值；从完整快照派生 full replay |
| 请求级 | schema、param、context、tool output、policy 等确定性 4xx | 立即停止 fanout，原样返回，不触碰其他账号 |
| 单节点风控 | challenge/path-block/new-ban 等结构化信号 | 只隔离该账号的 cookie、连接和状态，重建干净 envelope 后继续遍历当前设置所选调度策略排序后的剩余候选 |
| 共享上游 | 多账号返回相同明确的 global overload/maintenance 信号 | 仅作为当前请求的结构化失败证据，不打开 endpoint circuit，不修改未证实异常的账号状态 |

风险剖离不得变成请求级熔断。任何账号出现风控信号时，只清理该账号及该连接绑定的 cookie、turn state、previous ID、encrypted reasoning 和 identity；换号仍调用当前设置所选调度策略，不改成固定 Smart、插入顺序、tier 顺序或手工风险队列。只有确定性客户端请求错误、外部未知 history 缺少完整快照或输出已提交时才停止跨账号重放。

每次 failover 必须从不可变请求重新构造，不能在同一个可变 JSON 上连续剥字段。否则第一个节点的响应状态、cookie、turn state 或恢复 patch 会悄悄进入后续所有节点，形成真正的池级污染。

## 2. 审计基线结论（实施前）

当前实现与最新官方行为及透明代理约束存在以下差异。

### P0

1. `previous_response_id` 仍未绑定到产生它的具体 WebSocket。连接池对象没有保存 `latest_response_id`，新连接、旁路连接或 stale-reuse 后的新连接仍可能发送代理已知源自旧连接 `store=false` 响应的 ID。
2. history-bound 请求遇到 429 等账号错误后，现有实现和测试会换账号但仍原样发送同一 `previous_response_id`。账号应该轮换，但轮换请求必须由完整上下文重建为 full replay，不能复用旧 ID。
3. 生产上的上游 `400 previous_response_not_found` 被改写成 HTTP 200 流中的代理 `stream_disconnected`，原始 code 只被塞进 message 字符串，客户端无法按官方错误结构处理。
4. 所有 WebSocket 流当前都会预取到“首个真实输出或终态”，并受默认 20 秒绝对首输出超时约束。它已改变正常流的提交边界，且会把仍有生命周期活动但思考超过 20 秒的请求误判为故障。
5. dispatch 没有“完整候选集已耗尽”的证明：quota verify 固定最多 5 个账号，model unsupported 只允许一次换号，单账号 5xx 重试结束后直接返回，普通可恢复 transport error 也直接返回。账号池可能仍有可用账号。
6. 当前跨账号 history 清理只在临时 `ImplicitResumeSnapshot` 存在时生效。客户端显式 previous ID 或跨请求恢复没有完整快照，旧账号的 ID/turn state 仍可能进入新账号，账号轮换与会话隔离不能同时保证。
7. Cloudflare/path-block 必须只作为账号级 tracker：隔离当前账号并重建 envelope，换号继续使用当前设置所选调度策略，不引入跨账号 risk guard 或 endpoint circuit。

### P1

1. `from_body()` 注入缺失的 input、stream、store；session logic 会给缺失的 `prompt_cache_key` 生成值；implicit resume 还会生成 previous ID 并替换 input。这些均超出透明代理职责。显式 identity 的账号作用域转换属于保护职责，但必须与业务默认注入分开。
2. `response.incomplete` 会被 `completed_response_metadata()` 当作可续接完成响应，进而写入 affinity 和 reasoning replay；官方只允许 `response.completed` 推进续接状态。
3. `session_id`、`thread_id` 和 `x-client-request-id` 被合并成一个账号作用域值，破坏了字段各自语义；同一个全局 installation ID 还会发送给池内全部账号，形成跨账号关联标识。
4. WebSocket 包裹错误只保留状态码，丢失精确 `error.code`、完整 headers 和认证诊断；恢复逻辑仍需要扫描字符串。
5. 非流式响应会补写 `output` 和 `output_text`，流异常会补写代理 failure；只有 transport 确实缺少等价表示时才允许合成，并必须保留原始上游字段。
6. 未消费或未向客户端传递 `x-models-etag`、`openai-model`、`x-reasoning-included`、模型验证、审核/安全缓冲和 `end_turn` 等数据。
7. 限流解析只覆盖 core 与 code review 固定族，缺少任意 metered limit、credits、plan type、promo message 和 reached type。

### P2

1. Responses Lite、memory consolidation、W3C trace context 和 attestation 只应做受控透传；代理不应复制官方客户端的 body 构造逻辑。
2. 同账号 5xx 重试没有退避和抖动，账号内重试结束后也不会继续完整候选集；empty response 只计固定次数且不明确排除当前账号。
3. 模型目录按三个端点顺序全量读取，没有利用响应 ETag 驱动刷新。

这些问题必须按协议语义修复，不能增加模型名特判或兼容分支。

## 3. 权威证据

### 3.1 本地桌面版本

`/home/zyy/桌面/Codes/codex-desktop-linux/flake.nix` 锁定：

```text
codexVersion = "26.707.41301"
```

桌面包用于确认发布版本；协议行为以对应日期的官方 Rust 源码为主，因为桌面 JavaScript 包并不拥有 Responses 传输状态机。

### 3.2 官方源码

官方源码基线：

```text
5c19155cbd93bfa099016e7487259f61669823ff
2026-07-11T04:15:42Z
Add ordinals to paginated rollout records (#32332)
```

主要证据：

| 主题 | 官方文件 |
| --- | --- |
| WebSocket 会话与增量请求 | `codex-rs/core/src/client.rs` |
| WebSocket 协议、握手和包裹错误 | `codex-rs/codex-api/src/endpoint/responses_websocket.rs` |
| HTTP/SSE 响应解析 | `codex-rs/codex-api/src/sse/responses.rs` |
| 请求元数据 | `codex-rs/core/src/responses_metadata.rs` |
| 限流解析 | `codex-rs/codex-api/src/rate_limits.rs` |
| 请求与流默认阈值 | `codex-rs/model-provider-info/src/lib.rs` |
| WebSocket 行为测试 | `codex-rs/core/tests/suite/client_websockets.rs` |

固定版本链接：

- [WebSocket 会话状态](https://github.com/openai/codex/blob/5c19155cbd93bfa099016e7487259f61669823ff/codex-rs/core/src/client.rs#L289-L354)
- [增量输入判定与请求构造](https://github.com/openai/codex/blob/5c19155cbd93bfa099016e7487259f61669823ff/codex-rs/core/src/client.rs#L1167-L1254)
- [新连接清除增量状态](https://github.com/openai/codex/blob/5c19155cbd93bfa099016e7487259f61669823ff/codex-rs/core/src/client.rs#L1310-L1345)
- [WebSocket 响应与错误映射](https://github.com/openai/codex/blob/5c19155cbd93bfa099016e7487259f61669823ff/codex-rs/codex-api/src/endpoint/responses_websocket.rs)
- [限流解析](https://github.com/openai/codex/blob/5c19155cbd93bfa099016e7487259f61669823ff/codex-rs/codex-api/src/rate_limits.rs)

## 4. 当前工程链路

### 4.1 入站请求

`backend/src/api/client/responses.rs`：

- 接收原始 JSON object。
- 剥离本地传输字段 `use_websocket`。
- 提取 Codex 上下文字段供请求头使用，同时保留原始 body。
- 必要时向 `client_metadata` 写入受控的 `x-openai-subagent`。

`backend/src/upstream/openai/protocol/responses.rs` 以 `Map<String, Value>` 作为上游 body 的唯一真相源，并启用 serde_json `preserve_order`。这是正确方向：未知字段可透传，协议升级不需要先扩充本地 DTO。

但 `CodexResponsesRequest::from_body()` 仍会为缺失字段注入 `input=[]`、`stream=true`、`store=false`。即使这些值通常符合 Codex 客户端行为，也不应写回 wire body；代理可以在本地读取协议默认值，但必须保留字段“缺失”的原始状态。

### 4.2 调度与会话恢复

`backend/src/dispatch/affinity/resolve.rs` 和 `backend/src/dispatch/recovery/implicit_resume.rs`：

- 根据 `prompt_cache_key`、窗口和账号建立本地 conversation identity。
- 代理生成隐式续接时保存完整输入快照。
- 只有存在 `ImplicitResumeSnapshot` 时才会清除旧 history 并完整重放。
- 亲和账号被禁用或封禁时执行 cascading ban defense，但没有快照就无法安全清除 history。

旧文档所述“客户端显式 previous ID 会被无条件剥离”已经不符合当前代码。现在显式 ID 没有快照时不会被伪恢复，这是已经修正的部分。

但隐式续接本身不符合当前确认的透明边界：客户端发送完整 input 且没有 previous ID 时，代理不应自行生成 previous ID、裁剪 input、插入 reasoning replay，再在失败后尝试恢复原请求。应删除这条优化链路，完整请求直接完整转发。

### 4.3 账号池与保护链路

当前 dispatch 已经会对以下账号级结果更新状态、排除当前账号并继续调度：

- 429 和 quota exhausted。
- token 失效、账号禁用或封禁。
- Cloudflare challenge 和 path block。
- 部分 model unsupported。

账号保护也已有正确基础：cookie 以账号 ID 隔离存储，`set-cookie` 被代理截留；quota cooldown、Cloudflare cooldown、账号状态和 WebSocket pool 都按账号管理；选中账号的凭据和 account ID 由 transport 重新生成，而不是透传客户端 Authorization。

但当前循环不是完整的候选遍历：

- `candidates::filter()` 会按当前最高可用订阅层级收窄，并把瞬时达到并发上限的账号排除；它适合“选下一个”，不能直接作为“本请求全部候选”的快照。
- `MAX_QUOTA_VERIFY_ATTEMPTS=5` 会在第 5 个 stale-quota 账号后直接返回。
- `model_unsupported_retry_used` 是单个布尔值，第二个账号仍不支持时直接返回。
- retryable 5xx 只在同一账号立即重试 2 次，之后走 generic error 并返回。
- 其他可恢复 transport error 会直接返回，不会排除当前账号。
- empty response 最多重试 2 次，但没有把产生空响应的账号加入排除集，可能重复选择同一账号。
- history error 没有临时 implicit snapshot 时直接返回，也无法在其他账号上做完整重放。
- stream/complete 循环持有同一个可变 `request`，history 清理和恢复 patch 会原地累积；后续账号没有从不可变基线重新派生，存在 attempt 间状态穿透。
- Cloudflare challenge/path-block tracker 保持账号维度；这是有意设计，跨账号风险信号不得改写候选顺序或提前中断本请求。

目标实现必须先由 pool/scheduler 生成稳定的候选 ID 快照，再由 dispatch 维护逐账号 attempt ledger。快照包含所有基础可用层级，tier/affinity/策略只负责排序；账号凭据和运行状态在真正租用时重新读取，避免长请求持有过期 token。ledger 中每个候选只能进入一次账号级尝试，单账号内部允许独立的有界 transport retry；瞬时 Busy 候选先延后再回访。最终错误必须能证明候选总数、已尝试数和被状态排除数相等，而不是根据某个固定计数猜测“应该没有账号了”。

每个 attempt 都从不可变的客户端原始请求或完整 replay snapshot 构造新请求，并重新绑定所选账号的 Authorization、account ID、cookie、Cloudflare 上下文、WebSocket 和冷却状态。前一 attempt 的可变 request、turn state、连接 metadata 或加密 reasoning 不能被下一账号继承。

### 4.4 上游传输

`backend/src/upstream/openai/transport/client.rs` 与 `client_sse.rs`：

- HTTP/SSE 发送原始 body，并按传输要求设置 `stream`。
- WebSocket 在 body 外增加 `type: response.create`。
- 有 `previous_response_id` 时强制使用 WebSocket，不允许 HTTP fallback。
- 普通 `use_websocket=true` 请求在特定失败后允许回退 HTTP/SSE。

`response_upstream_request()` 当前会把显式 wire `prompt_cache_key` 改成账号作用域 conversation identity，并把 session/thread/window 等 reserved identity 写入 `client_metadata`。账号作用域方向符合账号保护，但当前实现没有独立安全策略：缺失 key 会先被生成，多个语义不同的 identity 被合并，同一个 installation ID 跨全部账号复用，账号作用域哈希也没有服务端密钥。应保留保护目标，重写实现边界。

`backend/src/upstream/openai/transport/websocket_pool.rs` 按 base URL、账号和本地 conversation identity 管理连接，但 `PooledWebSocketConnection` 当前只有：

```text
websocket
metadata
created_at
```

它没有保存这条连接实际缓存的最后一个 response ID。

### 4.5 响应与记录

- `protocol/events.rs`：usage 与固定限流族。
- `protocol/responses.rs`：SSE 收集、完成响应重构、续接元数据。
- `transport/response_meta.rs`：turn state、cookie、限流头和首输出时间。
- `dispatch/affinity/resolve.rs`：response ID affinity 与 reasoning replay。
- `dispatch/recording.rs`、`telemetry/recorder.rs`：使用和错误记录。

API 响应层目前主要重建 content type 与 JSON/SSE body，没有把安全的上游 response headers 带回客户端。`set-cookie` 必须被账号层截留，但 request ID、effective model、turn state、限流等端到端 header 应建立明确 allowlist。

### 4.6 Wire 改写清单

| 当前行为 | 分类 | 结论 |
| --- | --- | --- |
| 替换 Authorization、设置上游 account ID | 账号必需 | 保留 |
| 管理上游 cookie、Cloudflare cooldown、账号禁用 | 风控必需 | 保留，cookie 不下发客户端 |
| 每次 attempt 重建账号 envelope | 账号隔离必需 | 保留；禁止复用前一账号状态 |
| 剥离 `use_websocket` | 本地 transport hint | 保留 |
| JSON 重新序列化并保持字段顺序 | transport 必需 | 允许，字段语义必须相同 |
| WebSocket 增加 `type=response.create` | 协议 envelope | 保留 |
| 模型别名映射 | 显式代理能力 | 仅对已公开配置的 alias 允许 |
| 缺省注入 input/stream/store | 业务 body 改写 | 删除；默认值只在本地解释 |
| 缺失时生成 `prompt_cache_key` | 非必要业务 body 改写 | 删除；缺失继续缺失 |
| 显式 identity 原样跨账号 | 账号关联风险 | 禁止；用带密钥、按账号和字段域分离的伪名替换 |
| 合并 session/thread/request identity | 协议语义破坏 | 拆分后分别做账号作用域转换 |
| 全池复用 installation ID | 账号关联风险 | 改成稳定的 per-account installation pseudonym |
| 覆盖普通 client metadata | 业务 body 改写 | 禁止；只处理 reserved identity key |
| 添加纯代理 telemetry metadata | transport metadata | 只允许保留在代理命名空间且不得覆盖 |
| implicit resume 改写 input/previous ID | 业务语义改写 | 删除 |
| history-bound 请求跨账号发送原 ID | 历史所有权破坏 | 改成基于完整上下文的 full replay |
| 确定性客户端 4xx 广播到全部账号 | 账号保护破坏 | 禁止；精确错误直接透传 |
| 为非流式响应添加 `output_text` | 成功响应改写 | 删除；只返回上游终态对象 |
| 上游 error 改成 `stream_disconnected` | 错误响应改写 | 删除；保留 status/type/code/message/param |
| 完全无上游终态时合成代理失败事件 | transport 必需 | 允许，但必须标识 `source=proxy` |

## 5. 官方 WebSocket 状态机

最新官方实现的核心所有权是：

```text
ModelClientSession
  -> WebsocketSession.connection
  -> WebsocketSession.last_request
  -> WebsocketSession.last_response_rx
       -> LastResponse { response_id, items_added }
```

规则如下：

1. 只有 `response.completed` 才产生 `LastResponse`。
2. 下一次请求必须与上一次请求的非输入属性完全匹配。
3. 当前 input 必须以前次 input 加服务端完成输出为前缀，才能只发送增量 input。
4. 满足条件时才写 `previous_response_id`。
5. 连接关闭或创建新连接前，立即清除 `last_request`、`last_response_rx` 和 warmup 状态。
6. 错误后的下一次请求使用新连接和完整 `response.create`，不携带旧 ID。

非输入属性比较包含：

```text
model
instructions
tools
tool_choice
parallel_tool_calls
reasoning
store
stream
include
service_tier
prompt_cache_key
text
```

官方有意忽略 `client_metadata` 和 `stream_options`，因为它们不改变被 previous response 引用的上下文。

对应测试已覆盖：

- 完成后前缀匹配，使用 `previous_response_id`。
- 非输入字段变化，发送完整请求且不带 previous ID。
- 前次流错误，下一次新连接发送完整请求且不带 previous ID。

## 6. P0：连接本地 previous response

### 6.1 当前缺陷

本地 affinity 和 pool key 只能证明“这次请求属于同一会话和账号”，不能证明“当前 socket 仍持有这个 response ID”。以下路径都可能失去上游连接本地缓存：

- socket EOF、Close、RST 或 pump 标记关闭。
- 55 分钟主动过期。
- stale-reuse 后丢弃旧连接并新建。
- pool Busy 时旁路新建连接。
- 进程重启或实例切换。
- 客户端取消导致 socket 被丢弃。

当前新连接仍会原样发送请求 body，因此会出现：

```text
store=false
previous_response_id=<W1 only>
websocket_pool_kind=new
actual socket=W2
```

服务端正确返回 `previous_response_not_found`。

### 6.2 生产证据

2026-07-11 通过 `ssh oci` 对 `https://codex-proxy.aivify.cc/` 做了只读复核：

```text
Nginx Proxy Manager: /data/nginx/proxy_host/26.conf
forward: 127.0.0.1:8082
container: codex-proxy-rs
image: ghcr.io/zyycn/codex-proxy-rs:1.0.2
runtime data: /srv/codex-proxy-rs/.runtime
```

日志中 `previous_response_not_found` 有 549 行，但每个失败通常同时写一条 WARN 和一条最终 ERROR，不能把 grep 行数当请求数。以 `live response stream finalized` 和 `request_id` 去重，并与 SQLite `ops_error_logs` 交叉确认后的结果是：

| 指标 | 结果 |
| --- | ---: |
| 独立失败请求 | 211 |
| 失效 previous response ID | 16 |
| 涉及账号 | 13 |
| 2026-07-10 | 116 |
| 2026-07-11（截至 18:59:57 北京时间） | 95 |
| `gpt-5.6-sol` | 211 |
| WebSocket | 211 |
| 每请求上游账号 attempt | 1（211/211） |
| pool new | 206 |
| pool bypass | 5 |
| ops_error_logs | 211 |
| usage_records | 0 |

失败延迟说明服务端会快速拒绝无效 ID：

| 指标 | latency_ms |
| --- | ---: |
| min | 310 |
| p50 | 549 |
| p95 | 1826 |
| p99 | 2850 |
| max | 8138 |

16 个失效 ID 均能在 `usage_records.response_id` 找到原始成功响应，且全部满足：

```text
origin model = gpt-5.6-sol
origin transport = websocket
origin store = false
failure account = origin account
```

原响应连接中 14 个为 reuse、2 个为 new；后续 211 个失败全部在 new 或 bypass 连接发生。因此账号轮换不是这批生产事故的根因，连接本地缓存丢失才是共同条件。但每个失败请求也都只尝试了一个账号，说明 history error 分支没有发挥账号池；修复不能简单把旧 ID 继续发给下一账号，必须先从完整快照重建干净请求。

所有 211 个失败 wire request 都包含 previous ID，`store=false`，input item 数为 1 到 8（p50=4，p95=7），符合增量请求形态。现有日志没有记录 previous ID 是客户端显式提供还是代理 implicit resume 生成，因此后续必须记录 provenance，不能靠 input 长度推断。

用户给出的 response ID 的完整时间线：

| 北京时间 | request ID | 事实 |
| --- | --- | --- |
| 22:58:30 | `req_3090a40b-200e-4997-a910-01d522974a59` | reuse socket 成功完成，产生 `resp_0de25...fb6aa5f`；store=false，610 个 input item，首输出 5599ms |
| 22:58:31 | `req_aee4bb05-d67e-454e-9913-10a5db5afd64` | 同账号、同 conversation/pool key 复用 socket；HTTP 200 已交给客户端，但之后没有流终态记录 |
| 23:00:21 | `req_20d5d48a-7f95-4bd8-a521-694af6ac939d` | 同账号改为 new socket，携带旧 ID 和 5 个增量 item |
| 23:00:22 | 同上 | 上游返回 400 `previous_response_not_found` |
| 23:34:52 | 多次客户端重试 | 同一 ID 累计 19 个独立失败请求 |

这条链路说明：22:58:31 的客户端取消或未完成流使原 socket 未回池，下一次只能创建新 socket；代理只按 account/conversation 找池，无法知道旧 ID 已随 socket 消失。

生产还证实了错误透明性问题。上游原始错误是：

```json
{
  "type": "error",
  "error": {
    "type": "invalid_request_error",
    "code": "previous_response_not_found",
    "message": "Previous response with id 'resp_...' not found.",
    "param": "previous_response_id"
  },
  "status": 400
}
```

客户端最终看到的却是 HTTP 200 流内 `response.failed`，code 变成 `stream_disconnected`，原始 JSON 被嵌入 message；`ops_error_logs.upstream_code` 也只记录 `stream_disconnected`。这是确定的响应改写，不符合透明代理原则。

这证明故障条件是“连接本地 ID 被发到新连接”，不能据此增加 `gpt-5.6-sol` 特判。

### 6.3 目标不变量

1. 代理已知由 `store=false` 响应产生的 previous ID，只能发到明确持有该 ID 的同一条 socket。
2. 新连接的 continuation state 必须为空。
3. 只有成功的 `response.completed` 才更新连接的 latest response ID。
4. `response.incomplete`、`response.failed`、错误、取消和未完成 EOF 都必须使本轮无法成为续接基线。
5. 正常路径原样转发客户端请求，不主动把完整请求优化成增量请求。
6. 原账号或原 socket 不可用时必须继续账号轮换；有完整快照时清除旧 ID 和账号绑定状态，在本请求剩余的每个候选账号执行 full replay，直至成功或候选耗尽。
7. stale-reuse 失败后不得把原增量 payload 或同一 previous ID 原样发给新连接/新账号。
8. full replay 只能发生在任何真实输出提交给客户端之前，并且每次派生都必须可审计。
9. `store=true` 的服务端持久化语义要与连接本地 fast path 分开处理，不能共用一个布尔判断。

必须显式记录 ID 来源，不能仅根据当前请求的 `store` 值推断前一响应是否持久化：

```text
KnownConnectionLocal  # 代理观察到前一响应 store=false，只能在原 socket 使用
KnownPersisted        # 代理观察到前一响应已持久化，可在新连接尝试 hydration
ExternalUnknown       # 客户端外部提供，代理不知道来源
```

`KnownConnectionLocal` 丢失原 socket 后，使用完整快照在原账号的新连接 full replay；若该账号同时发生账号级故障，再依次重放到其余候选账号。`KnownPersisted` 可以先在符合账号所有权的连接尝试 hydration，失败后同样走 full replay 和完整候选遍历。`ExternalUnknown` 没有代理历史时只能原样尝试；若上游返回 not found，必须原样返回，不能删除 ID 后把不完整 input 当新会话发送，更不能把未知 ID 广播给所有账号。

账号轮换需要新的完整恢复快照，而不是沿用当前只保存 reasoning/function call 的 replay cache：

```text
ResponseReplaySnapshot
  response_id
  owner_account_id
  source_store
  request_properties
  full_input_through_completed_response
  owner_turn_state
  created_at
```

快照应放在 Redis 独立 TTL key 中，与 affinity 同步提交和删除，不写 PostgreSQL，不进入日志。快照只保存恢复所需的业务上下文，不保存 access token、account cookie、上游 account ID、指纹或 Cloudflare 凭据。正常请求仍使用客户端原始 body；只有账号/连接恢复分支读取快照并派生 full replay。跨账号时只剥离明确账号绑定的 `previous_response_id`、turn state 和加密 reasoning 内容，保留 reasoning summary、tool call、tool output 及所有其他业务字段。

当前 `ImplicitResumeSnapshot` 只在代理主动做隐式续接时保存本次完整 input，无法覆盖客户端显式增量请求，也无法跨请求生存；它不能承担账号轮换恢复，应由上述完整快照替代。

### 6.4 推荐状态

连接对象应直接拥有 continuation state：

```text
PooledWebSocketConnection
  websocket
  metadata
  created_at
  continuation
    latest_response_id
    owner_account_id
    source_store
    replay_snapshot_key
```

同 socket 快路径只校验精确 latest response ID；代理不替客户端判断或改写其他业务字段。需要换 socket 或换账号时，每个候选都通过 replay snapshot 构造独立的派生请求，不修改保留的客户端原始 request，也不复用上一个 attempt 的派生 request。

不要把连接代次或 latest response ID 写入 Redis。Redis 无法证明当前进程中的具体 socket 仍存活，反而会制造第二真相源。

## 7. P0：预取与超时

### 7.1 当前行为

`backend/src/upstream/openai/transport/websocket_frames.rs` 的 `prefetch_stream_frames_until_output_or_terminal()` 已用于 fresh、pooled 和旁路流式路径。

它会：

1. 暂存 `response.created`、`response.in_progress` 等结构事件。
2. 继续等待文本、reasoning、工具参数等真实输出，或等待终态。
3. 从发送请求开始应用默认 20 秒绝对 `first_token_timeout`。
4. 20 秒内即使不断收到生命周期事件，只要没有真实输出仍会失败。

最新官方 WebSocket 实现不会对每个流做这种全局预取。请求发送成功后返回流，由 5 分钟逐帧 idle timeout 保护活跃流。

### 7.2 延迟证据

最近三天 5722 条 `gpt-5.6-sol` 成功记录：

| 指标 | first_token_ms |
| --- | ---: |
| p50 | 2716ms |
| p95 | 6929ms |
| p99 | 11275ms |
| 大于 15 秒 | 21 |
| 大于 20 秒 | 9 |
| 最大值 | 37334ms |

这些数据不能精确等同于 WebSocket 内部计时点，但足以否定“20 秒内没有真实输出就是坏连接”。

### 7.3 正确拆分

必须区分三个概念：

| 概念 | 含义 | 建议 |
| --- | --- | --- |
| initial activity timeout | 请求发出后迟迟没有任何有效上游活动 | 可保留短超时，但收到合法生命周期事件后应重置 |
| stream idle timeout | 活跃流在两帧之间长时间无数据 | 保持 5 分钟，与官方默认一致 |
| failover commit boundary | 哪一帧发给客户端后就不能做账号轮换 full replay | 在首个不可逆业务输出前允许恢复，提交后禁止重放 |

正常请求和客户端显式 previous ID 不应被全局延迟到真实输出。账号 failover 可以在首个不可逆业务输出前短暂暂存纯 lifecycle 帧，但只等待“足以判断能否提交”的边界，不能等待首个文本 token，更不能受 20 秒绝对首输出阈值约束。

默认 `first_token_timeout_ms` 应取消，或重定义为 activity-aware 的诊断阈值；不能继续作为 20 秒绝对熔断。

## 8. 请求格式与身份

### 8.1 已对齐部分

- HTTP body 保留未知字段和客户端显式业务字段。
- WebSocket body 增加 `type: response.create`。
- `OpenAI-Beta: responses_websockets=2026-02-06` 与最新官方一致。
- 支持 `x-codex-turn-state`、turn metadata、beta features、timing metrics、window、parent thread、installation ID 和受控 subagent。
- WebSocket payload 会写 `x-codex-ws-stream-request-start-ms`。

官方 Codex 构造请求时还会设置 `store=false`、`stream=true`、`tool_choice=auto`、`parallel_tool_calls`、`include=[reasoning.encrypted_content]` 等值。它是客户端请求构造器，不是代理改写清单；代理必须转发客户端实际提供的字段，不能照抄这些默认值到缺失字段。

### 8.2 身份字段被合并

官方元数据明确区分：

```text
installation_id
session_id
thread_id
turn_id
window_id
```

官方请求头映射是：

```text
x-client-request-id = thread_id
session-id = session_id
thread-id = thread_id
```

当前 `CodexRequestContext` 只有 `session_id`，并把它同时写到三个位置。`response_client_metadata()` 也把 `thread_id` 强制覆盖成 `session_id`。

目标设计：

1. `InboundIdentity` 分开保存客户端原始 `session_id`、`thread_id`、`client_request_id`、`turn_id`、window、parent thread 和 prompt cache key，只存在于受控请求上下文/恢复快照。
2. `AccountScopedIdentity` 按字段 domain 和账号分别派生 wire 值，不能用一个 conversation hash 覆盖所有字段。
3. 派生使用持久化服务端密钥的 HMAC，而不是可离线枚举的裸 SHA-256；密钥不进日志、不进响应，换账号必然得到不同伪名。
4. installation ID 仅由持久化身份密钥和账号 ID 稳定派生；不保留全池共用值，也不维护本地基础 installation ID 文件。
5. 客户端未提供的 body identity 默认保持缺失；仅 transport 必需的 request ID 可以使用代理本轮 request ID，不能回写成客户端 body 字段。
6. `client_metadata` 的普通字段原样保留；reserved identity key 由安全策略替换为账号作用域值，并记录 `identity_transform=account_scoped`，但不记录原值。
7. 本地 pool/affinity identity、客户端原始 identity 和上游账号作用域 identity 是三个不同类型，禁止用 `String` 在层间混传。

### 8.3 prompt_cache_key 被改写

当前 transport 会把 `prompt_cache_key` 改写为账号作用域的本地 conversation identity。这个保护方向正确，但实现仍有三个问题：

- `ensure_prompt_cache_key()` 会在客户端缺失时主动生成业务字段。
- 裸 SHA-256 同时承担本地索引和 wire 伪名，没有密钥和类型边界。
- conversation/session/thread/window 被压成同一个 identity，协议语义与保护语义混在一起。

目标设计应保留三个独立字段：

```text
client_prompt_cache_key                # 客户端原值，仅受控本地使用
local_conversation_identity            # pool、affinity、恢复索引
wire_account_scoped_prompt_cache_key   # HMAC(field, account, client value)
```

客户端显式提供 key 时，上游只看到当前账号的稳定伪名；客户端未提供时 wire body 继续缺失。连接池和 affinity 使用独立 local identity，不能为了内部索引而给上游 body 生成 key。账号 failover 必须从 `client_prompt_cache_key` 为新账号重新派生，不能复用前一账号的 wire key。

HMAC identity secret 在首次启动时生成并以仅运行用户可读的权限持久化到 data directory；启动时无法安全读取/创建就 fail closed。它不能放进日志、PostgreSQL 业务表或客户端可见配置。镜像替换必须继续挂载同一 data directory，否则 wire identity 会改变，当前 Redis continuation 快照应一并失效，不能保留旧伪名兼容分支。

### 8.4 请求头差异

| 请求信息 | 当前 | 最新官方 | 处理建议 |
| --- | --- | --- | --- |
| Responses WS beta | 已支持 | 已支持 | 保持 |
| session/thread/client request ID | 合并成账号作用域 conversation ID | 分离 | 按字段分别做账号作用域伪名化 |
| installation ID | 持久密钥 + 账号 ID 派生 | 单客户端稳定 | 保持 per-account 稳定伪名，不落地基础 installation ID 文件 |
| `x-openai-subagent` | 白名单支持 | 支持 | 保持白名单 |
| `x-openai-memgen-request` | 缺失 | memory consolidation 使用 | 对受信客户端/内部 route 做 allowlist 透传，不改 body |
| Responses Lite header | 缺失 | 客户端与 Lite body 同步发送 | 受控透传客户端 marker，代理不重建 Lite body |
| WS traceparent/tracestate metadata | 缺失 | 支持 | 客户端 trace 只做本地关联；每个账号 attempt 生成独立合法 trace context |
| `x-oai-attestation` | 缺失 | 官方按请求生成 | 不伪造；仅在有真实 attestation provider 时实现 |
| Zstd 请求压缩 | 缺失 | ChatGPT backend 可启用 | P2 性能项，先做能力探测与指标 |

## 9. 响应驱动的数据与判断

### 9.1 当前覆盖情况

| 数据 | 当前处理 | 最新官方用途 | 结论 |
| --- | --- | --- | --- |
| `x-codex-turn-state` | HTTP/WS 捕获并续传 | 同一 turn sticky routing | 已对齐 |
| `set-cookie` | 捕获并进入 Cloudflare/cookie 链路 | 会话恢复 | 已对齐 |
| `x-request-id`、`cf-ray` | 部分捕获 | 诊断与反馈 | 扩充字段 |
| usage | 输入、输出、缓存、reasoning、图片 token | 计量 | 基本对齐 |
| completed response ID | 写 affinity/replay | WS 增量续接 | 所有权仍错误 |
| `response.incomplete` | 被当作完成元数据 | 官方作为错误，不推进续接 | P1 修复 |
| `x-models-etag` | 未捕获 | 模型目录刷新 | P1 增加 |
| `openai-model` | 未捕获 | 服务端实际选择模型 | P1 增加 |
| `x-reasoning-included` | 未捕获 | 响应能力/解析提示 | P1 增加 |
| model verifications | 未解析 | 模型验证状态 | 按客户端契约保留 |
| turn moderation metadata | 未解析 | 审核信息 | 保留并定义展示边界 |
| safety buffering | 未解析 | 安全缓冲与重试模型提示 | 保留并定义行为 |
| `end_turn` | 反序列化后丢弃 | turn 生命周期 | 向内部流事件暴露 |

### 9.2 `response.incomplete` 不能成为续接基线

`completed_response_metadata()` 当前匹配：

```text
response.completed | response.incomplete
```

随后 `record_response_affinity()` 会把该 ID 写入 affinity 和 reasoning replay。这与官方状态机相反。

修复要求：

1. `CompletedResponseMetadata` 只由 `response.completed` 产生。
2. `response.incomplete` 可按 Responses API 契约透传给客户端，但不能写 continuation、affinity 或 replay。
3. 流式与非流式路径共用同一终态分类，禁止各自维护不同规则。

### 9.3 有效模型

服务端可能通过 `openai-model` 返回实际执行模型，例如安全路由后的模型。当前 usage、账号模型统计和日志主要使用请求模型或本地 display model。

应新增 `effective_model`：

- 调度仍按 requested model 选账号。
- 计量和模型分布优先记录 server effective model。
- 同时保留 `requested_model`，便于审计路由变化。
- 不把 effective model 回写请求或影响本轮重试选择。

### 9.4 模型 ETag

当前模型目录依次请求：

```text
/codex/models?client_version=...
/models
/sentinel/chat-requirements
```

最新官方会从 Responses HTTP/WS 响应捕获 `x-models-etag` 并发出模型目录变更事件。建议让模型服务保存 ETag；发生变化时异步刷新 Redis 模型快照，避免每次响应同步请求模型端点。

## 10. 限流差异

当前 `ParsedRateLimits` 只包含：

```text
primary
secondary
code_review
```

最新官方会：

1. 枚举所有 `x-<limit-id>-primary-*` header family。
2. 保存 `limit_id` 和 `limit_name`。
3. 解析 credits 的 `has_credits`、`unlimited`、`balance`。
4. 从 `codex.rate_limits` 事件读取 `plan_type` 和 metered limit。
5. 解析 promo message 与 rate-limit-reached-type。

改造后应以 `limit_id` 为键保存任意窗口，core 和 code review 只是普通已知 ID，不再写固定分支。数据库/Redis 结构若已能保存 JSON snapshot，应直接扩充领域类型，不新增旧格式兼容路径。

## 11. 错误与诊断

### 11.1 WebSocket 包裹错误

官方会把 WebSocket error event 中的 primitive `headers` map 完整转换为 HTTP HeaderMap，再走统一 TransportError。它还对精确 code `websocket_connection_limit_reached` 做专门重连判断。

当前 `ClassifiedWebSocketError` 只有：

```text
status_code
```

随后 transport 只提取 retry-after，并用 `CodexUpstreamDiagnostics::with_status()` 构造诊断。因此会丢失：

- 精确 `error.code` 和 `error.type`。
- `x-request-id` / `x-oai-request-id`。
- `cf-ray`。
- `x-openai-authorization-error`。
- base64 `x-error-json` 中的认证错误 code。
- 包裹事件中的 set-cookie 和完整限流头。

目标错误类型至少包含：

```text
status
code
error_type
message
headers
retry_after
request_id
cf_ray
```

`previous_response_not_found`、无 tool output、`invalid_encrypted_content` 和连接 60 分钟上限必须由精确 code 分类，不再扫描 message/body。

### 11.2 请求 ID

`CodexUpstreamDiagnostics` 当前缺少官方使用的 `x-oai-request-id`。解析顺序应为：

```text
x-request-id
x-oai-request-id
x-openai-request-id
openai-request-id
request-id
```

认证错误 header 只进入受控诊断字段，不将原始敏感 body 写入普通日志。

### 11.3 错误分类与账号池动作

中间账号的错误是 dispatch 决策输入，不是立即返回给客户端的结果。目标分类如下：

| 错误来源 | 示例 | 账号池动作 | 最终客户端语义 |
| --- | --- | --- | --- |
| 确定性请求错误 | schema/param 非法、上下文超限、缺 tool output | 不广播到其他账号 | 原样保留上游 status/type/code/message/param/body |
| 外部未知 history | 无快照的 `previous_response_not_found` | 不把未知 ID 广播到其他账号 | 原样返回上游 400 |
| 可恢复 managed history | connection-local ID 丢失、invalid encrypted content | 从完整快照派生 full replay；账号级失败时继续剩余候选 | 成功响应，或候选耗尽后的统一错误 |
| 节点级认证 | 精确 token invalid/expired/revoked | 更新账号状态并排除，继续全部剩余候选 | 候选耗尽后返回 `no_available_accounts` |
| 高风险账号状态 | account deactivated/banned | 隔离当前账号与连接，重建干净 envelope 后继续当前设置所选调度策略 | 候选耗尽后返回 `no_available_accounts` |
| 账号额度/单节点风控 | 429、quota、Cloudflare challenge/path block | 写冷却/状态并排除，使用干净 envelope 继续遍历当前设置所选调度策略排序后的剩余候选 | 候选耗尽后返回 `no_available_accounts` |
| 账号模型能力 | 精确 model unsupported | 记录该账号能力并排除，继续全部剩余候选 | 全部候选均不支持时返回稳定 `model_not_supported` |
| 可恢复 5xx/transport | connect、reset、502/503/504 | 先做单账号有界重试；仍失败则仅在本请求排除并继续全部剩余候选，不持久禁用账号 | 候选耗尽后返回稳定 upstream unavailable |
| 多账号同类风控 | 多个账号在干净重放后出现 challenge/path-block/new-ban | 各自隔离，不传播账号绑定状态，继续当前设置所选调度策略 | 候选耗尽后返回脱敏汇总错误 |
| 明确共享上游故障 | global overload/maintenance 精确 code | 仅记录当前请求证据，不打开 endpoint circuit，不修改账号状态 | 候选耗尽后保留 upstream unavailable 语义 |
| 输出已提交后的流错误 | 任意晚期 EOF/error | 禁止盲目 full replay | 保留原上游失败；仅无等价上游事件时合成 proxy failure |

账号池终态只能由 attempt ledger 证明候选已耗尽后产生。全为 model unsupported 时返回 `model_not_supported`，全为可恢复 5xx/transport 时返回 upstream unavailable，node-local 认证、额度或混合账号状态耗尽时返回 `no_available_accounts`。错误应携带脱敏失败类别汇总和最后一个上游 request ID 供诊断，但不能冒充某个中间账号的原始响应。客户端主动取消、确定性请求错误、输出已提交和进程关闭是独立终止条件，不应伪装成账号池耗尽。

## 12. 阈值对照

| 项目 | 当前工程 | 最新官方默认 | 结论 |
| --- | ---: | ---: | --- |
| WebSocket 最大寿命 | 55 分钟 | 服务端 60 分钟精确错误 | 55 分钟预留合理，保留 |
| WS 建连超时 | 依赖底层连接行为 | 15 秒 | 增加显式配置与指标 |
| 首次无活动等待 | 20 秒 | 无独立短阈值 | 改成 activity-aware |
| 首真实输出绝对超时 | 20 秒 | 无 | 默认取消 |
| 活跃流逐帧 idle | 5 分钟 | 5 分钟 | 对齐 |
| fresh WS 首输出重试 | 2 次 | stream retries 5 | 不直接照抄；先修复错误分类和提交边界 |
| 同账号 5xx 重试 | 2 次，无退避，结束后立即返回 | request retries 4，200ms 指数退避和抖动 | 加退避；单账号预算用尽后继续下一候选 |
| quota verify 账号数 | 固定最多 5 | 不适用 | 删除固定上限，遍历本请求完整候选集 |
| model unsupported 账号数 | 最多 2 | 不适用 | 删除布尔闸门，遍历本请求完整候选集 |
| 空响应重试 | 请求级固定 2 次 | 不适用 | 当前账号记一次并排除，继续完整候选；删除固定请求级上限 |
| 单账号 pool slot | 8 | session 单连接模型 | 代理特有，基于 Busy/饱和数据调整 |
| ping / ping timeout | 25 秒 / 5 秒 | 库和服务端管理 | 可保留，记录误杀率 |

官方 `request_max_retries=4` 和 `stream_max_retries=5` 是 Codex 客户端策略，不是代理应无条件复制的数值。代理还会轮换账号，必须把单账号网络预算与账号候选预算分开，避免次数相乘，也不能让网络重试上限截断候选遍历。

推荐重试规则：

1. 请求开始时冻结候选账号快照，并按亲和/调度策略排序；后续只从该快照取未尝试账号。
2. 同账号 5xx 使用 200ms 起步的有界指数退避和 0.9 到 1.1 抖动；预算用尽后排除当前账号并继续下一候选。
3. 对 429 使用服务端 retry-after 和账号冷却，不做同账号快速重试，立即继续下一候选。
4. quota verify、model unsupported、empty response、token invalid/expired、Cloudflare 和 new-ban 都计入当前账号的 attempt，不能使用小于候选数的全局固定上限。
5. managed previous history 错误在每个候选上只允许一次有完整快照证明的 full replay；不能复用旧 ID，也不能重复使用上一个账号的派生 request。
6. 确定性客户端 4xx 和外部未知 history 不做账号 fanout，避免用无效请求伤害整个账号池。
7. 一旦真实输出已经提交给客户端，不做可能重复输出或副作用的 full replay。
8. 客户端取消、确定性请求错误、输出已提交、进程关闭或候选 ledger 已耗尽可以结束 dispatch；普通 node-local 重试计数不能提前返回客户端错误。

## 13. 其他上游差异

### 13.1 Responses Lite

最新官方按模型目录的 `use_responses_lite` 同时改变 body 与 header：

- tools 和 developer instructions 移入 input。
- 顶层 instructions 置空。
- 顶层 tools 清空。
- reasoning context 使用 all-turns 语义。
- 增加 `x-openai-internal-codex-responses-lite: true`。

这不是代理应该执行的单个 header 功能。客户端若已经按 Responses Lite 契约构造 body，代理只对受信客户端透传 marker 和原始 body；代理不能根据模型目录自行搬移 instructions/tools 或改变 reasoning context，否则会同时破坏透明性和账号 failover 的等价重放。

### 13.2 memory consolidation

最新官方在 memory consolidation 请求上同时发送受控 subagent 和 `x-openai-memgen-request: true`。当前白名单已经允许 `memory_consolidation`，但缺少 companion header。只有受信客户端明确提交该请求类型时才成对透传；普通客户端提供的同名内部 header 必须剥离，代理不改写 body 来伪造 memory consolidation。

### 13.3 trace context

最新官方把每次 WS 请求的 traceparent/tracestate 写进 `client_metadata`，因为握手头不能表达同一连接上的每一轮 trace。代理收到的客户端 trace 只用于本地链路关联，不能把同一 trace ID 透传到多个上游账号。每个账号 attempt 应生成独立的合法 W3C trace context，并在本地用 request ID 关联；tracestate 中不可信 vendor 数据剥离，日志和 audit artifact 继续脱敏。

### 13.4 attestation

`x-oai-attestation` 是官方按请求生成的证明，不是固定指纹头。代理没有真实 attestation provider 时应保持缺失，不得伪造静态值或盲目转发不可信客户端值。

### 13.5 请求压缩

官方可对 ChatGPT Responses 请求启用 Zstd。该项只影响带宽和 CPU，不影响 P0 正确性。实施前应确认服务端 capability、正文阈值、压缩失败回退和端到端延迟收益。

## 14. 实施顺序

### 阶段一：P0 账号池耗尽、账号隔离与 continuation 所有权

修改：

- `backend/src/fleet/pool/mod.rs`
- `backend/src/fleet/scheduler/candidates.rs`
- `backend/src/dispatch/service.rs`
- `backend/src/dispatch/stream/lifecycle.rs`
- `backend/src/dispatch/upstream_call.rs`
- `backend/src/dispatch/recovery/exhaustion.rs`
- `backend/src/dispatch/recovery/cloudflare.rs`
- `backend/src/upstream/openai/transport/websocket_pool.rs`
- `backend/src/upstream/openai/transport/websocket.rs`
- `backend/src/upstream/openai/transport/websocket_frames.rs`
- `backend/src/upstream/openai/protocol/websocket.rs`
- `backend/src/dispatch/affinity/types.rs`
- `backend/src/dispatch/affinity/store.rs`
- `backend/src/dispatch/recovery/implicit_resume.rs`
- `backend/src/dispatch/errors.rs`

完成项：

1. pool/scheduler 一次返回跨全部可用层级的有序候选 ID 快照；tier/affinity 只排序，瞬时 Busy 延后回访，dispatch 使用 attempt ledger 证明全部候选已处理。
2. 删除 `MAX_QUOTA_VERIFY_ATTEMPTS` 和 model unsupported 单次换号闸门；账号级错误、5xx/transport retry exhausted 和 empty response 都继续剩余候选。
3. 风控只修改当前账号的状态并清理其连接绑定信息；不引入 request risk guard、endpoint circuit 或换号次数/时间预算。
4. 每个 attempt 从不可变原始请求/完整快照重新构造，并绑定该账号独立的凭据、cookie、风控状态和连接。
5. 连接对象拥有 continuation state，发送前验证 previous ID、owner account、socket 和请求形态；新连接不携带 connection-local ID。
6. Redis 保存唯一的完整 replay snapshot；删除主动 implicit resume 优化及其临时快照，不保留两套恢复模型。
7. history failover 只从完整快照派生，跨账号剥离旧 ID、turn state 和账号绑定加密内容，业务上下文保持等价。
8. 删除全局 output-prefetch，拆分 activity timeout 与 commit boundary；提交前可 failover，提交后禁止盲目重放。

### 阶段二：终态和结构化错误

修改：

- `backend/src/upstream/openai/protocol/responses.rs`
- `backend/src/upstream/openai/protocol/websocket.rs`
- `backend/src/upstream/openai/transport/diagnostics.rs`
- `backend/src/dispatch/affinity/resolve.rs`
- `backend/src/api/client/errors.rs`

完成项：

1. completed 与 incomplete 使用判别明确的终态类型。
2. exact error code 和 primitive headers 全链路保留。
3. 中间账号错误只进入 attempt ledger；候选耗尽才生成稳定的 `no_available_accounts`/upstream unavailable。
4. 可由客户端修复的上游 4xx 原样保留，外部未知 history 不做账号 fanout。
5. 请求 ID、认证错误和 Cloudflare trace 统一提取。

### 阶段三：身份、响应元数据与限流

修改：

- `backend/src/upstream/openai/transport/client.rs`
- `backend/src/upstream/openai/transport/client_sse.rs`
- `backend/src/upstream/openai/transport/response_meta.rs`
- `backend/src/upstream/openai/protocol/events.rs`
- `backend/src/models/`
- `backend/src/dispatch/recording.rs`
- `backend/src/telemetry/`

完成项：

1. session/thread/turn/client request ID 分离。
2. local conversation identity、客户端原始 identity 与 wire account-scoped identity 使用不同强类型。
3. 用持久 HMAC 密钥分别派生 prompt/session/thread/window/installation 伪名，删除裸哈希与全池 installation ID。
4. 客户端普通 body 保持不可变，账号 envelope、reserved identity transform、代理 trace 与普通客户端 metadata 分层构造。
5. 捕获 ETag、effective model、reasoning included、end_turn 和安全元数据。
6. 任意 limit ID、credits 和 plan type 进入统一限流领域模型，并反馈账号保护状态。

### 阶段四：P2 协议能力

按独立评审实施 Responses Lite、memgen、trace context、attestation provider 和请求压缩。不得为未启用能力保留半成品兼容代码。

## 15. 测试矩阵

所有测试放在 `backend/tests`，禁止写入 `backend/src`。

### 15.1 continuation

| 场景 | 预期 |
| --- | --- |
| 同 socket，完成响应后前缀扩展 | 发送 delta 和匹配的 previous ID |
| 请求非输入字段变化 | 同 socket 发送完整 input，不带 previous ID |
| 完成后主动丢弃 socket | 新连接不带 connection-local ID |
| stale-reuse 写失败 | 不把原增量 payload 发到新连接 |
| pool Busy | 旁路连接不携带池连接的 previous ID |
| `response.incomplete` | 不更新 latest ID、affinity 或 replay |
| completed 后客户端取消下一轮 | 被取消轮不推进 continuation |
| managed connection-local ID、连接缺失 | 从 Redis 完整快照重放，不带旧 ID/turn state/账号绑定加密内容 |
| managed ID 的快照意外缺失 | 视为内部不变量破坏，不把残缺请求发往任何账号 |
| 外部来源未知的显式 previous ID | 新连接尝试一次；not found 时原样返回，不广播未知 ID |
| 原账号新连接 full replay 仍发生账号级失败 | 排除原账号，并从同一业务快照依次派生到全部剩余候选 |

### 15.2 账号池与账号保护

| 场景 | 预期 |
| --- | --- |
| 高优先级层级账号全部失败 | 继续尝试快照中的低优先级可用层级，不因 tier 收窄提前结束 |
| 某候选瞬时 slot/connection Busy | 先尝试其他账号，再回访该候选，不设代理自定义换号 deadline |
| 候选快照有 N 个账号，第一个 429 | 写该账号 cooldown，随后尝试其余 N-1 个 |
| 连续 quota verify limit reached 超过 5 个 | 不受固定 5 次限制，继续到候选耗尽或成功 |
| 连续 model unsupported 超过 2 个 | 不受布尔闸门限制，逐账号记录能力并继续 |
| 某账号 retryable 5xx | 完成有退避的单账号重试；仍失败则排除并尝试下一账号 |
| 某账号 transport reset/connect error | 有界同账号重试后继续下一账号，不立即返回 |
| 某账号返回空响应 | 记录失败并排除，继续下一账号，不重复选择同一账号 |
| token invalid/expired | 更新账号状态、清理/隔离连接并继续下一账号 |
| account deactivated/banned | 隔离该账号与其连接状态，重建干净 envelope 后继续遍历当前设置所选调度策略排序后的剩余候选 |
| Cloudflare challenge/path block | 捕获该账号 cookie 与冷却状态，排除并继续下一账号 |
| 第一个账号高风险、第二个账号在干净 envelope 下成功 | 正常返回成功，两个 attempt 间无 cookie/session/identity 穿透 |
| 多个账号在干净重放后出现同类高风险信号 | 逐个隔离已证实失败的账号，继续遍历当前设置所选调度策略排序后的剩余候选，不触发请求级熔断 |
| 同一账号内部重复同类高风险帧 | 只更新该账号状态，不影响其他候选的排序或健康状态 |
| 多账号连续 quota/model unsupported | 继续完整候选遍历 |
| 混合账号级错误，最后一个成功 | 客户端只看到成功结果，中间错误只进入脱敏 attempt trace |
| 全部候选发生 node-local 失败 | `attempted + state_excluded = candidate_count` 后才返回稳定池耗尽错误 |
| 确定性 invalid request 4xx | 只请求一个账号，原样返回错误，避免污染账号池 |
| 每次跨账号 attempt | Authorization/account ID/cookie/socket/turn state 均来自或匹配当前账号 |
| 跨账号 full replay | 业务 input/tools/tool outputs 保持等价，旧账号绑定字段全部不存在 |
| 客户端取消 | 终止请求但不伪报账号池耗尽，不错误修改未尝试账号状态 |

### 15.3 流与超时

| 场景 | 预期 |
| --- | --- |
| 生命周期事件持续到 25 秒才有输出 | 不触发 20 秒绝对首输出失败 |
| 发送后完全无活动 | activity timeout 生效 |
| 首输出后 5 分钟无帧 | stream idle timeout 生效 |
| 结构事件后 history error 且有快照 | 客户端未见不可逆输出时按候选顺序完整重放 |
| 真实输出后上游错误 | 禁止 full replay，保留原上游错误语义 |
| `websocket_connection_limit_reached` | 精确 code 触发新连接，清空增量状态 |

### 15.4 请求与响应元数据

- distinct session/thread/client request ID 不互相覆盖，并分别派生账号作用域值。
- 显式 prompt cache key 的原值不出现在上游 wire；同账号伪名稳定，换账号伪名不同，缺失时不生成。
- 同一个代理 identity secret 为不同账号派生不同且各自稳定的 installation ID。
- `x-models-etag` 变化触发一次异步模型刷新。
- `openai-model` 写入 effective model，同时保留 requested model。
- WS error headers 中 string、number 和 bool 均能安全映射。
- `x-oai-request-id`、auth error 和 `x-error-json` 可解析且日志不泄露原 body。
- 任意 limit ID 和 credits 在 HTTP header 与 WS event 两种路径一致。

## 16. 可观测性

每次 WebSocket 请求至少记录：

```text
candidate_count
attempted_account_count
state_excluded_account_count
pool_exhausted
attempt_index
attempt_account_id_hash
attempt_failure_class
account_state_transition
account_envelope_rebuilt
identity_transform
pool_decision
connection_id_hash
connection_age_ms
continuation_requirement
continuation_match
previous_response_id_present
previous_response_provenance
request_shape_match
replay_snapshot_available
derived_request_reason
recovery_action
commit_boundary
first_activity_ms
first_output_ms
upstream_request_id
upstream_error_code
effective_model
```

禁止记录 access token、cookie、完整 previous ID、prompt cache key、完整 input/tools/client metadata 或 attestation。

内部重连、full replay、模型刷新和 quota verify 不得产生额外 `usage_records`；usage 只代表真实 `/v1/...` 客户端调用的最终结果。

## 17. 验收标准

只有全部满足才算改造完成：

- 代理已知源自 `store=false` 响应的旧 ID 不会出现在新 socket 的上游 payload。
- continuation state 由具体连接拥有，连接销毁时同步销毁。
- 只有 `response.completed` 推进续接状态。
- 代理管理的每条 continuation 都有 Redis 完整 replay snapshot，快照不包含任何账号凭据或 cookie。
- 显式增量请求没有完整历史时不会伪恢复，也不会把未知 ID 广播到账号池。
- 普通业务 body 原样转发；reserved identity 做可审计的账号作用域转换，只有账号 failover 分支派生 full replay。
- 每次 attempt 都重建账号 envelope，前一账号的 Authorization、account ID、cookie、turn state、socket 和加密 reasoning 不会进入下一账号。
- prompt/session/thread/window/installation 等关联标识按账号隔离，普通 metadata 不被风控转换误伤。
- node-local token 失效、额度、模型能力、可恢复 5xx/transport、空响应和高风险信号会隔离当前账号并继续遍历当前设置所选调度策略排序后的剩余候选；只有确定性请求错误、外部未知 history 无完整快照或输出已提交可以停止跨账号重放。
- 不存在 request risk guard、endpoint circuit、候选次数上限或换号时间预算；风险剖离只保护已失败账号，不改写当前设置所选策略的候选顺序。
- quota verify、model unsupported 或普通重试计数不会在候选耗尽前结束 dispatch。
- 最终池耗尽错误有 `candidate_count = attempted + state_excluded` 的 ledger 证据。
- 确定性客户端 4xx 原样返回且不会污染其余账号。
- 全局 output-prefetch 与 20 秒绝对首输出熔断已移除。
- exact upstream error code 和 headers 全链路保留。
- session/thread/request identity 不再合并。
- effective model、models ETag 和任意限流族有明确消费方。
- 已提交真实输出后不重试。
- 所有新增测试位于 `backend/tests`。
- `cargo fmt --manifest-path backend/Cargo.toml -- --check` 通过。
- `cargo clippy --manifest-path backend/Cargo.toml --all-targets --all-features -- -D warnings` 通过。
- `cargo test --manifest-path backend/Cargo.toml` 通过。

## 18. 真实链路验证

先在 next 实例开启脱敏后的 WebSocket audit：

```bash
CODEX_PROXY_WS_AUDIT_DIR=.runtime/ws-audit
```

至少验证：

1. 同 socket 连续两轮正常增量。
2. 完成后主动 discard，再发送代理已知 connection-local previous ID。
3. 在新连接发送外部来源未知、可能已持久化的 previous ID。
4. 完成后主动 discard，使用 Redis 完整快照在原账号新连接重放。
5. 原账号分别制造 429、quota、auth、首次 Cloudflare、model unsupported 和 retryable 5xx，验证每类单节点错误都会进入下一候选。
6. 让前 N-1 个候选发生 node-local 失败、最后一个成功，确认客户端只看到成功且 attempt ledger 完整。
7. 让全部候选发生 node-local 失败，确认最后一个候选结束前不会返回客户端错误。
8. 让两个独立账号在干净 envelope 下出现同类 challenge/path-block，确认两者各自隔离且第三个账号仍由当前设置所选调度策略正常选中。
9. 跨账号 full replay 抓取脱敏 wire，确认业务上下文等价且无旧账号 ID、turn state、cookie、account ID 或加密 reasoning。
10. 对同一客户端 identity 分别选两个账号，确认 prompt/session/thread/window/installation wire 伪名均不同且普通 metadata 相同。
11. pool Busy 与 stale-reuse。
12. 生命周期活动超过 20 秒后才出现首输出。
13. 精确 `websocket_connection_limit_reached`。
14. HTTP/SSE 与 WS 的 effective model、ETag、限流和 request ID 一致性。

每个场景记录下游 request ID、上游 request ID、pool decision、是否实际发送上游、恢复动作、首活动和首输出时间。确认问题后先修复并重跑当前场景，再进入下一场景。

## 19. 参考工程取舍

`sub2api` 的可取原则是：只有保存了完整 replay input 才允许清除 previous ID 后恢复；本工程还必须在此基础上遍历全部账号候选，而不是同账号恢复失败后立即结束。

`CLIProxyAPI` 的可取原则是：连接失效时增量状态同时失效，下一次使用完整 transcript。

本工程最终只保留一套 Redis `ResponseReplaySnapshot`。现有进程内 `ImplicitResumeSnapshot` 和不完整 reasoning replay cache 不能作为并存兼容路径，应在迁移完成后删除。也不应复制依赖消息文本的启发式分类；连接状态、错误 code、账号失败类别和 attempt ledger 必须使用强类型表达。

## 20. 最终建议

先完成以下最小正确闭环：

```text
immutable client request
  -> ordered candidate snapshot
  -> account-local risk stripping
  -> per-account isolated envelope
  -> per-account identity pseudonyms
  -> bounded same-account transport retry
  -> exclude failed account and continue
  -> full replay snapshot for history failover
  -> success or proven candidate exhaustion

connection-owned continuation state
  + send-before-match validation
  + completed-only continuation update
  + new socket/new account clears bound state
  + exact upstream error classification
  + remove global output-prefetch
```

## 21. 实施状态

P0/P1 已按上述边界实施：完整候选 ledger 使用运行时设置当前选择的策略顺序（Smart / QuotaResetPriority / RoundRobin / Sticky），没有换号次数/时间预算或风险熔断；managed history 换号只从 Redis 完整快照重放，external unknown history 不广播；WebSocket continuation 由连接拥有，只有 `response.completed` 推进状态。

请求 body 不再注入代理默认值，identity 和 installation ID 使用持久 HMAC 密钥按字段域/账号隔离；`response.incomplete` 不写续接基线；HTTP/SSE/WS 捕获并消费 effective model、models ETag、reasoning included 和任意 limit ID/credits/plan/promo/reached type。P2 协议能力仍保持独立评审边界。
