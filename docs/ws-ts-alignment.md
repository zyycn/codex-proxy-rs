# WebSocket 真实链路与 TS 版本对齐

这份记录用于后续排查 Rust 版 `codex-proxy-rs` 的真实链路、首 token、WebSocket 连接复用和 `/v1/responses` / `/v1/chat/completions` 流式行为。对齐参考是 TS 版本 `/home/zyy/Codes/codex-proxy`。

## 当前判断

Rust 版已经有 WebSocket pool，不是每次请求都必然新建连接：

- `src/runtime/services.rs` 会在 `config.ws_pool.enabled` 时创建 `CodexWebSocketPool`。
- `src/upstream/transport/websocket_pool.rs` 默认启用 max age、maintenance ping 和 liveness timeout。
- `src/upstream/transport/client.rs` 的 HTTP SSE 链路也使用可复用 `reqwest` client。

因此真实链路仍然慢时，优先看事件语义、pool key、连接回收和首 token 统计，不要先把问题归因成“没有复用连接”。

## 2026-06-29 后半段真实日志审计

审计范围：

- 日志：`.runtime/logs/codex-proxy-rs.2026-06-29.log` 中 `16:30:00` 之后的真实请求。
- 数据库：`usage_records.created_at >= 2026-06-29T08:30:00Z` 的记录。

后半段日志可以确认连接池已经在真实链路生效：

- 日志里有 46 次 `websocket pool decision`：`reuse` 24 次，`new` 17 次，`bypass(busy)` 4 次，`retry_after_stale_reuse` 1 次。
- usage 里有 49 条 `/v1/responses` 相关记录：33 条成功，15 条 502，1 条 429。
- 33 条成功里，20 条是 WebSocket reuse，7 条是 WebSocket new，3 条是 WebSocket bypass busy，3 条是 HTTP SSE。

这批 502 的直接形态不是“没有复用连接”，也不能简单归因为“复用连接坏了”：

- 15 条 502 全部是 `transport=websocket`、`failureEvent=response.failed`、`upstreamCode=stream_disconnected`。
- 失败详情都是 `websocket receive idle timeout after 20s`。
- 其中 5 条发生在 `websocketPool.kind=reuse`，10 条发生在 `websocketPool.kind=new`。
- 这些 502 都已有 `firstTokenMs`，说明不是首帧前失败，而是已经开始输出后，中途超过 20s 没有收到下一帧。

这里的 `response.failed` 需要特别区分来源。代码会在流式 body 出错且没有 terminal event 时，通过 `premature_close_failed_event` 合成 `response.failed(stream_disconnected)`。所以后半段日志里的 `live upstream stream ended with response.failed` 不一定代表上游原样发送了 `response.failed`，对这批样本来说更准确的描述是：代理在 WebSocket 收包 idle timeout 后合成了断流 terminal 事件。

后半段还暴露了两个观测问题：

- HTTP trace 的 `completed HTTP request status=200` 只表示流式响应已经开始返回，并不代表上游流最终成功。比如同一个 request 可能在 trace 里先记 200，几十秒后 usage 再记 502。
- `live upstream stream ended with response.failed` WARN 只有 `account_id/event/code`，没有 `request_id`、`response_id`、transport、pool decision 和具体 timeout detail，必须靠时间和 usage 反查，排障成本高。

性能样本也能说明 reuse 有效果，但不是整体耗时的唯一因素：

- 成功的 WebSocket reuse 首 token 平均约 1.9s。
- 成功的 WebSocket new 首 token 平均约 11.5s。
- 成功的 HTTP SSE 首 token 平均约 25.1s。
- 成功请求的整体耗时仍可能很高，主要还受模型推理、输出长度、上下文和缓存规模影响。

本次真实日志下，当前最高优先级应调整为：

1. 先核对并调整活跃 WebSocket 流的 20s receive idle timeout。TS 版本只有打开连接的 20s open timeout，active stream 没有 Rust 这种每帧 20s receive timeout；persistent pool 的 liveness 也跳过 busy 连接。
2. 明确区分“上游原始 `response.failed`”和“代理合成 `stream_disconnected`”，不要把两者写成同一种链路错误。
3. 保留并继续修正 `response.failed` 分类语义，避免真正的上游 terminal failed 被错误升级成 503 或错误丢弃 WebSocket。

## 协议语义差异

`response.failed` 当前是最需要先对齐的点。

TS 版本行为：

- `src/proxy/ws-transport.ts` 只把 allowlist 里的错误码转换成 `CodexApiError`。
- allowlist 之外的 `response.failed` 保持 SSE 事件透传，并作为 terminal frame 正常结束当前响应。
- `src/proxy/ws-pool.ts` 的 persistent WS 在 `response.completed` / `response.failed` / `error` 后关闭当前响应流，但保留 WebSocket 供下一次复用。

本轮修复前 Rust 版本风险：

- `src/upstream/protocol/websocket.rs` 会把未命中具体错误码的 `response.failed` 默认归类为 503。
- WebSocket 错误码 allowlist 明显宽于 TS 版，包含 `invalid_request`、`invalid_encrypted_content`、`server_error` 等历史扩展码，容易把普通 terminal failure 提升为账号轮换或 5xx。
- `src/upstream/transport/websocket.rs` 会吞掉未分类 `type: "error"` 事件，导致应该作为 terminal SSE 的错误无法进入 SSE failure / history recovery 路径。
- `src/upstream/transport/websocket.rs` 在分类到上游错误后会 `discard_stream_websocket`，导致当前 socket 被丢弃。
- 如果上游真的发送未分类 `response.failed`，这会把普通上游 terminal failed 放大成代理错误、usage 502/503，并增加后续重建连接成本。注意：2026-06-29 后半段日志里的 `stream_disconnected` 主要是代理因 20s receive idle timeout 合成的 failure，不应和这个语义分类问题混为一类。

本轮已落地：

- 移除未分类 `response.failed` 默认 503。
- 移除无实际差异的 WebSocket error classification profile。
- WebSocket 错误码分类收窄到 TS 版 allowlist：`usage_limit_reached`、`rate_limit_exceeded`、`rate_limit_reached`、`quota_exhausted`、`payment_required`、`unauthorized`、`token_invalid`、`token_expired`、`account_deactivated`、`forbidden`、`account_banned`、`banned`、`previous_response_not_found`；连接级 `websocket_connection_limit_reached` 仍按 503 处理。
- 未分类 `response.failed` 和未分类 `error` 都透传为 terminal SSE；普通 terminal frame 完成当前响应后，池化 WebSocket 回池供下一次复用。
- 活跃 WebSocket 流收到首个公开响应帧后，不再应用每帧 20s receive idle timeout；首帧前仍保留 20s 保护，复用连接首帧前失效仍按 stale reuse 重试新连接。

目标语义：

- allowlist 内可恢复错误：转成上游错误，允许账号轮换、strip previous response id 或 retry。
- 连接级错误：丢弃 WebSocket。
- 普通 terminal failed：透传 SSE terminal 事件，结束当前响应流，尽量保留 WebSocket。
- `/v1/responses` 和 `/v1/chat/completions` 必须共享这套判断。

## 连接复用与并发隔离

TS 版本 pool key 是账号加 conversation：

```text
${entryId}:${conversationId}
```

Rust 版本 pool key 是：

```text
base_url + account_id + prompt_cache_key/client_conversation_id/previous_response_id
```

正常有 `prompt_cache_key` 或 `client_conversation_id` 时，两边语义基本一致。只有 `previous_response_id` 时，Rust 会把 response id 当 conversation 标识使用，需要确认 session affinity 是否已经能继承原 conversation identity。

并发场景下应满足：

- 同一个账号、同一个 conversation：同一时间只能有一个请求占用同一个 WS slot，不能串流。
- 同一个账号、不同 conversation：pool key 不同，不能串 conversation。
- 不同账号：pool key 必须隔离，不能串账号。
- 复用失败如果发生在首帧前，应能按“stale reused connection”语义打开新连接重试；已经开始输出的流不能静默换连接。

## 请求头差异

本轮按 `/home/zyy/桌面/Codes/codex-proxy` 当前源码和 `docs/flows.all.txt` 抓包重新核对，结论需要区分 TS 参考实现和官方 Codex Desktop 真实链路。

TS 版本当前在 WebSocket 和 HTTP SSE 链路中使用：

```text
x-client-request-id: <conversationId>
session_id: <conversationId>
```

官方 Codex Desktop 抓包中的 WebSocket 请求使用：

```text
x-client-request-id: <redacted>
session-id: <redacted>
thread-id: <redacted>
```

Rust 版本当前上游请求头使用：

```text
x-client-request-id: <session_id 或 request_id>
session-id: <session_id>
thread-id: <session_id>
```

同时 Rust 在 `client_metadata` 内写入 `session_id` / `thread_id`。

因此 header 不能简单按 TS 版 `session_id` 回退。TS 参考实现和官方客户端抓包在会话 header 命名上不一致；Rust 当前更贴近官方抓包，并且现有 header 测试已断言不发送 `session_id` 请求头。后续若要调整，应先用真实链路 A/B 验证，而不是仅以 TS 源码为准。

## 首 Token 统计

首 token 耗时应按流式链路真实事件计算：

- 起点：请求进入代理并开始上游流式请求前的时间。
- 终点：第一个有效输出事件到达代理的时间。
- 不应使用数据库写入时间、请求完成时间或 terminal frame 时间倒推。

需要分别覆盖：

- HTTP SSE 首个输出事件。
- WebSocket 首个输出事件。
- 只有 metadata / rate limit / created / in_progress 事件时不能误记首 token。
- 上游失败且没有任何有效输出时，首 token 应为空，而不是写 0 或整体耗时。

## 可观测性要求

真实链路排查时，日志和 usage 至少要能关联这些字段：

- `request_id`
- `response_id`
- 账号 ID 或邮箱
- 请求模型、上游模型、端点
- transport：`http_sse` 或 `websocket`
- pool decision：fresh、reused、bypass、discard、release
- upstream code / error code
- first token ms、total latency ms

没有这些字段时，管理端请求明细、后端日志和真实客户端现象很难对上。

## 修复优先级

1. 已完成：核对活跃 WebSocket 流的 `WEBSOCKET_RECEIVE_IDLE_TIMEOUT=20s`，对齐 TS 版本 active stream 不做每帧 20s 超时的行为；首帧前仍保留 stale reuse 保护。
2. 已完成：把代理合成的 `stream_disconnected` 明确标记为 synthetic/proxy-side failure，并在 usage/log 中保留原始错误 detail。
3. 已完成：给 `live upstream stream ended with response.failed` 增加 `request_id`、`response_id`、transport、pool decision、first token、latency 和 failure detail。
4. 已完成：修 `response.failed` 分类，去掉“未分类 response.failed 默认 503”的行为，并收窄到 TS allowlist。
5. 已完成：调整 WS terminal frame 回收策略；普通 terminal 释放当前 stream 并回池，分类上游错误才 discard。
6. 已完成：已补 Responses WebSocket terminal 语义测试，并补 `/v1/chat/completions` 路由级回归，确认未分类 `response.failed` 不触发账号轮换或 WS discard 放大。
7. 已验证：Rust 在只有 `previous_response_id` 时会通过 session affinity 继承原 conversation identity，并已有 `responses_websocket_should_route_previous_response_id_to_recorded_account` 回归覆盖。
8. 已完成：基于 `docs/flows.all.txt` 做真实 header audit；官方客户端使用 `session-id` / `thread-id`，Rust 当前保持该形态，不补齐 TS 版 `session_id` 请求头。
9. 已完成：补首 token 统计判定和测试，覆盖 HTTP SSE、WebSocket、metadata-only 前缀事件、首帧前失败和已有输出后断流；`response.created`、`response.in_progress`、`response.metadata`、`codex.rate_limits` 和上游失败 terminal 不会误写 `firstTokenMs`。
10. 已完成：补 `live response stream finalized` 结构化日志，作为真正代表流结束状态的日志；字段包含 request/response、transport、pool decision、first token、latency、完成/失败状态和失败细节，不再只依赖 HTTP trace 的 `completed HTTP request status=200`。

## 真实链路回归

每次改动后至少跑这些场景：

- 官方 Codex 客户端走 `/v1/responses`，连续多轮自然语言长文本请求。
- OpenAI 兼容客户端走 `/v1/chat/completions`，开启 stream。
- 同一账号同一会话连续请求，观察是否出现 reused，首 token 是否下降。
- 同时多个客户端并发请求，确认账号、conversation、响应流不串。
- 人为触发或观察上游 `response.failed`，确认客户端收到的是正确 SSE terminal 语义，usage 不误写为代理 502/503。
- 对比管理端请求明细、后端日志和客户端侧耗时，确认 request id 能串起来。

### 2026-06-29 额度、调度、缓存真实链路

运行环境：本地 `target/debug/codex-proxy-rs`，`config.yaml`，SQLite `.runtime/data/codex-proxy-rs.sqlite`，日志 `.runtime/logs/codex-proxy-rs.2026-06-29.log`。本轮使用管理端 API Key 创建临时客户端 key，真实请求不记录明文 key。

本地回归：

- `cargo test --test main quota -- --nocapture`：39 passed。
- `cargo test --test main usage_logging -- --nocapture`：15 passed。
- `cargo test --test main responses_websocket -- --nocapture`：30 passed。
- `cargo test --test main chat_completions_should_mark_quota_exhausted_after_402_and_fallback -- --nocapture`：1 passed。
- `cargo build`：通过。

真实链路结果：

- 启动恢复：数据库中 `acct_a73f281b0a2c4fdebae8b17ab88b2270` 原为额度耗尽，启动后恢复为 `status=quota_exhausted`、`quota_limit_reached=1`、`remaining=0`、`cooldown_until=2026-07-29T08:45:10+00:00`；本轮真实请求没有再调度到该账号。
- HTTP SSE：`real_quota_sse_20260629_2057` 成功完成，transport=`http_sse`，`firstTokenMs=2623`，usage=`20/57/77`，账号 `acct_fa9f0172d2084eaf86ddbd0e5d0c1c16` 写入 `x-codex-primary-used-percent=5`，`quota_fetched_at/updated_at` 更新到本轮请求结束时间。
- WebSocket fallback 日志：`real_quota_ws_20260629_2055` 上游 WS 握手返回 `tls handshake eof`，触发 fallback warn；日志字段已包含 `request_id`、`account_id`、`transport=websocket`、`fallback_transport=http_sse`、`fallback_reason=websocket_error` 和 error。
- WebSocket 成功：`real_chain_ws_20260629_2109` 成功完成，transport=`websocket`，pool=`new`，`firstTokenMs=7052`，usage=`85/3526/3611`；账号 `acct_fa9...` 额度从 5% 被动更新到 14%。
- 缓存命中：相同长上下文和同一 `prompt_cache_key` 连续请求中，`real_cache_repeat_20260629_1_ws` 为 transport=`websocket`、pool=`new`、`cachedTokens=0`；`real_cache_repeat_20260629_3_ws` 回到同账号并复用连接，transport=`websocket`、pool=`reuse`、`cachedTokens=15104`、`firstTokenMs=2738`，账号 `acct_81aaba2a4e084162924b17d6f55e8a10` 额度从 6% 更新到 9%。
- `/v1/chat/completions`：`real_chat_stream_20260629_2114` 成功完成，kind=`v1.chat`，transport=`http_sse`，`firstTokenMs=8824`，usage=`84/752/836`，账号 `acct_fa9...` 额度继续更新到 20%。

真实链路观察：

- WS 和 HTTP SSE 成功路径都会写 usage，并会把上游 rate limit headers 被动同步回账号 quota。
- 额度耗尽账号会恢复为 `quota_exhausted` 并跳过调度。
- 同账号、同 `prompt_cache_key`、WS pool reuse 时可以观察到 prompt cache 命中；跨账号或 WS 失败 fallback 的样本不能用来判断缓存未命中。
- 强制 HTTP SSE 并携带 `previous_response_id` 会被上游拒绝：`Unsupported parameter: previous_response_id`。真实连续上下文应走默认/WS 路径。

### 2026-06-29 TS 调度策略对齐

本轮重新对齐 TS 版本 `/home/zyy/桌面/Codes/codex-proxy` 的三个调度模式：

- `least_used` / 智能分配：按 TS 比较器选择候选账号，顺序为 quota exhausted 靠后、两个账号都有 `window_reset_at` 时更早 reset 优先、更低 `request_count` 优先、最后按 LRU 选择更久未使用账号；只在完整比较结果相同的首组选项内轮转。
- `round_robin` / 轮询：候选账号顺序使用账号恢复/插入顺序，不再被账号 ID 排序覆盖；cursor 语义对齐 TS：选择前对当前候选数量取模，选择后只自增。
- `sticky` / 粘性：优先最近使用账号；当 `last_used` 完全相同时保留候选顺序，冷启动时选择候选首项。
- `least_used` 和 `round_robin` 共用同一个 rotation cursor；切换调度策略时重置 cursor。
- `prompt_cache_key` 不新增独立账号硬绑定。TS 版本也是通过 `previous_response_id` / implicit resume 命中 session affinity 后把账号作为 preferred hint；Rust 保持同一语义。缓存命中仍要求后续请求落在同账号，并且 WS pool key 的账号 + conversation identity 一致。

本地回归：

- `cargo test --test main account_pool -- --nocapture`：38 passed。
- `cargo test --test main responses_websocket -- --nocapture`：30 passed。
- `cargo test --test main responses_http -- --nocapture`：23 passed。
- `cargo build`：通过。
- `cargo clippy --all-targets --all-features --locked -- -D warnings`：通过。
- `pnpm -C web build`：通过。

真实链路回归：

- 运行环境：本地 `target/debug/codex-proxy-rs`，审计目录 `.runtime/real-chain-scheduler-20260629_214555/ws-audit`，日志 `.runtime/logs/codex-proxy-rs.2026-06-29.log`。本轮使用管理端 API Key 创建临时客户端 key，结束后已删除临时 key，并将 `rotationStrategy` 恢复为 `least_used`。
- `least_used`：`real_sched_least_20260629_214555` 走 `/v1/responses` stream，status=200，选中 `acct_81aaba2a4e084162924b17d6f55e8a10`，transport=`websocket`，pool=`new`，`firstTokenMs=8317`，usage=`70/503/0`。该账号是本轮候选中更符合 TS 智能分配比较器的低使用/LRU 账号。
- `round_robin`：依次执行 `real_sched_rr1_20260629_214555`、`real_sched_rr2_20260629_214555`、`real_sched_rr3_20260629_214555`，均 status=200，选中账号顺序为 `acct_7951dd589a5c4046afdd7080d58f8501` -> `acct_81aaba2a4e084162924b17d6f55e8a10` -> `acct_fa9f0172d2084eaf86ddbd0e5d0c1c16`。其中第一轮后 `acct_795...` 根据上游 quota header 被更新为 `status=quota_exhausted`、`quota_limit_reached=1`、`quota_cooldown_until=2026-07-29T03:09:21+00:00`，后续轮询跳过该账号。
- `sticky` + 缓存：`real_sched_sticky_cache1_20260629_214555` 和 `real_sched_sticky_cache2_20260629_214555` 均选中 `acct_fa9f0172d2084eaf86ddbd0e5d0c1c16`，验证 sticky 不切换账号。第一次 pool=`new`、`firstTokenMs=7532`、usage=`6581/2031/0`；第二次 pool=`reuse`、`firstTokenMs=1575`、usage=`6581/1737/6400`，同账号、同 `prompt_cache_key`、WS 复用时缓存命中。

真实链路观察：

- 三个调度策略均能完成真实 `/v1/responses` stream，usage 记录中的 `account_id`、`transport`、`websocketPool.kind` 和日志 `live response stream finalized` 一致。
- 额度耗尽会实时写回账号状态，并从后续调度候选中排除。
- 切换账号会丢缓存的根因不是 `prompt_cache_key` 丢失，而是缓存依赖同账号和同 conversation/WS pool identity；sticky 场景已验证同账号复用下缓存命中。

#### 智能分配二次审计

2026-06-29 重新按 TS `src/auth/rotation-strategy.ts` / `src/auth/account-lifecycle.ts` 审计 Rust `AccountPool::select_least_used`：

- 比较器顺序一致：quota limited 靠后 -> 两边都有 `window_reset_at` 时更早 reset 优先 -> 更低 `request_count` 优先 -> 更早 `last_used` 优先。
- 缺失 `window_reset_at` 的账号不会被惩罚；只要另一侧没有同时具备可比较窗口，就回落到 `request_count`。
- 首组选项完全相等时使用共享 rotation cursor 轮转，候选数组/账号池插入顺序不被排序副作用改变。
- acquire 前候选过滤和 TS 对齐到同一层级：active、并发槽位、exclude、Cloudflare 冷却、quota limited 跳过、模型计划限制、tier priority，再进入 `least_used`。
- 已补 Rust 回归覆盖 TS 边界用例：缺失窗口、双边无窗口、quota limited 内部按 reset 排序、least_used 不改变候选顺序；`cargo test --test main account_pool -- --nocapture`：42 passed。

## 源码锚点

Rust 版：

- `src/runtime/services.rs`
- `src/upstream/protocol/websocket.rs`
- `src/upstream/transport/websocket.rs`
- `src/upstream/transport/websocket_pool.rs`
- `src/upstream/transport/client.rs`
- `tests/upstream/transport/headers.rs`

TS 版：

- `/home/zyy/Codes/codex-proxy/src/proxy/ws-transport.ts`
- `/home/zyy/Codes/codex-proxy/src/proxy/ws-pool.ts`
- `/home/zyy/Codes/codex-proxy/src/proxy/codex-api.ts`
- `/home/zyy/Codes/codex-proxy/src/routes/shared/proxy-ws-context.ts`
- `/home/zyy/Codes/codex-proxy/src/logs/stream-close-event.ts`
