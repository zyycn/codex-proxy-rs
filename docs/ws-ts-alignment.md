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

TS 版本在 WebSocket 和 HTTP SSE 链路中使用：

```text
session_id: <conversationId>
x-client-request-id: <conversationId>
```

Rust 版本当前上游请求头使用：

```text
session-id: <session_id>
thread-id: <session_id>
```

同时 Rust 在 `client_metadata` 内写入 `session_id` / `thread_id`。

这里不能只看本地测试是否通过，需要做一次真实 header audit。若没有明确兼容原因，应优先贴近 TS 版和官方客户端链路，避免因为 header 命名差异影响上游会话亲和、缓存或连接复用。

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
2. 把代理合成的 `stream_disconnected` 明确标记为 synthetic/proxy-side failure，并在 usage/log 中保留原始错误 detail。
3. 给 `live upstream stream ended with response.failed` 增加 `request_id`、`response_id`、transport、pool decision、first token、latency 和 failure detail。
4. 已完成：修 `response.failed` 分类，去掉“未分类 response.failed 默认 503”的行为，并收窄到 TS allowlist。
5. 已完成：调整 WS terminal frame 回收策略；普通 terminal 释放当前 stream 并回池，分类上游错误才 discard。
6. 部分完成：已补 Responses WebSocket terminal 语义测试；`/v1/chat/completions` 仍需补同一套路由级回归。
7. 补 pool key/session affinity 单测，尤其是只有 `previous_response_id` 的连续请求。
8. 做真实 header audit，再决定是否把 `session-id` / `thread-id` 改为或补齐 `session_id`。
9. 补首 token 统计测试，分别覆盖 HTTP SSE、WebSocket、metadata-only 前缀事件、首帧前失败和已有输出后断流。
10. 增加 stream close / pool decision 的结构化日志字段，并补一条真正代表流结束状态的日志；不要只依赖 HTTP trace 的 `completed HTTP request status=200`。

## 真实链路回归

每次改动后至少跑这些场景：

- 官方 Codex 客户端走 `/v1/responses`，连续多轮自然语言长文本请求。
- OpenAI 兼容客户端走 `/v1/chat/completions`，开启 stream。
- 同一账号同一会话连续请求，观察是否出现 reused，首 token 是否下降。
- 同时多个客户端并发请求，确认账号、conversation、响应流不串。
- 人为触发或观察上游 `response.failed`，确认客户端收到的是正确 SSE terminal 语义，usage 不误写为代理 502/503。
- 对比管理端请求明细、后端日志和客户端侧耗时，确认 request id 能串起来。

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
