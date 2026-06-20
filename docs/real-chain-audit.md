# 真实链路审计记录

日期：2026-06-19

本文档记录当前 `codex-proxy-rs` 使用真实账号、真实导入文件、真实 OpenAI/Codex 上游时暴露出来的链路问题。目标是把问题先固定下来，再按证据修复，直到全链路真实场景验证通过。

## 范围

本轮只验证 OpenAI/Codex 相关链路：

- 账号导入：原生格式、sub2api 格式
- refresh token 刷新：手动刷新、后台恢复调度
- 账号池选择：active、expired、refreshing 的运行时行为
- 请求发送：`/v1/models`、`/v1/responses`、`/v1/chat/completions`
- 上游传输：HTTP SSE、WebSocket、WebSocket fallback
- 特殊入口：`/v1/responses/review`、`/v1/responses/compact`
- 日志排查：文件日志、管理端结构化事件日志、请求 ID 串联

不验证通用 proxy/VPN，也不纳入 Anthropic、Gemini、OpenRouter、Ollama 等非 OpenAI provider。

## 测试环境

- 服务地址：`http://127.0.0.1:8080`
- 运行数据：`.runtime/data/codex-proxy-rs.sqlite`
- 运行日志：`.runtime/logs/codex-proxy-rs.2026-06-19.log`
- 管理端登录：本地 admin session cookie
- 客户端 key：本地临时文件读取，未写入本文档
- 导入文件：
  - `/home/zyy/下载/accounts-export-2026-06-19.json`
  - `/home/zyy/下载/accounts-export-2026-06-19 (1).json`

## 账号状态证据

第一个 sub2api 导入文件包含两个账号，导入后更新已有账号，而不是跳过：

```text
10e584b410a5f1ab  tami.crewless982@passinbox.com   active
64e49d697f15dd02  dnage.happily799@passinbox.com  expired
```

`64e49d697f15dd02` 的 refresh token 已不可用。手动刷新返回：

```json
{
  "result": "dead",
  "previousStatus": "active",
  "status": "expired",
  "error": "refresh token is invalid or expired"
}
```

第二个 sub2api 导入文件包含一个账号：

```text
38907867f4c6c36e  hi.thickness354@passinbox.com  active
```

该账号的 `accessToken` / `access_token` 字段为空，但 `token` 字段存在，长度约 1973；`refreshToken` 存在。导入后该账号短暂进入 active 池，但真实手动刷新返回：

```json
{
  "result": "dead",
  "previousStatus": "active",
  "status": "expired",
  "error": "refresh token is invalid or expired"
}
```

最终稳定状态：

```text
pool: total=3 active=1 expired=2 refreshing=0 disabled=0 banned=0

10e584b410a5f1ab  tami...   active
64e49d697f15dd02  dnage...  expired
38907867f4c6c36e  hi...     expired
```

## 已验证通过的真实请求

### `/v1/models`

请求 ID：`deep_real_models_get_probe`

结果：HTTP 200，返回 3 个模型：

```text
codex-auto-review
gpt-5.4-mini
gpt-5.5
```

### `/v1/responses` HTTP 非流

请求 ID：`deep_real_responses_http_nonstream`

结果：HTTP 200，返回 `http-json-ok`，包含 usage。

管理端结构化日志：

```text
kind=v1.response
level=info
accountId=10e584b410a5f1ab
route=/v1/responses
statusCode=200
message="v1 responses completed"
metadata.usage 存在
metadata.rateLimitHeaders 存在
```

### `/v1/responses` HTTP stream

请求 ID：`deep_real_responses_http_stream`

结果：HTTP 200，SSE 包含：

```text
response.output_text.delta
response.completed
data: [DONE]
```

管理端结构化日志包含 `completed=true`、`responseId`、usage 和 rate-limit headers。

### `/v1/responses` WebSocket 非流

请求 ID：`deep_real_responses_ws_nonstream`

客户端结果：HTTP 200，返回 `ws-json-ok`。

文件日志显示真实过程并非纯 WebSocket 成功：

```text
websocket response failed; falling back to HTTP SSE
error="websocket request failed: websocket receive idle timeout after 20s"
```

管理端结构化日志只记录最终成功，没有记录 `transport=websocket`、`fallback=http_sse` 或原始 WebSocket 失败原因。

### `/v1/chat/completions` 非流

请求 ID：`deep_real_chat_nonstream`

结果：HTTP 200，返回 OpenAI chat completion JSON，内容为 `chat-json-ok`。

### `/v1/responses/review`

请求 ID：`deep_real_review_nonstream`

结果：HTTP 200，返回 `review-ok`。

管理端结构化日志存在，但 `route` 记录为 `/v1/responses`，无法从日志区分它来自 `/v1/responses/review`。

### `/v1/responses/compact`

请求 ID：`deep_real_compact_json`

结果：HTTP 200，返回 `response.compaction`，包含 `compaction_summary.encrypted_content`。

## 已复现的问题

### 1. sub2api 导入会盲信导出状态

状态：已做最小修复并通过定向自动化验证。

复现路径：

1. 导入 `/home/zyy/下载/accounts-export-2026-06-19 (1).json`
2. 账号 `38907867f4c6c36e` 的导出状态为 `active`，`token` 字段存在
3. `/api/admin/auth/status` 可选中该账号作为当前 user

风险：

- 已删除、封禁或 refresh token 已失效的账号会短暂污染 active 池
- 用户请求可能先拿到不可发送账号，并依赖请求失败后再纠正状态
- 后续只能依赖请求失败或刷新失败再纠正状态

期望：

- sub2api 适配器导入时至少应校验 JWT `exp`，过期 token 不应以 `active` 入库或展示
- 导入后需要可选健康探测或刷新调度，尽快把已删除/封禁/RT 失效账号落到非 active 状态
- 如果 refresh token 不可刷新，应稳定落为 `expired` 或更精确的 `banned` / `disabled`

### 2. 请求 fallback 只记录最终成功账号

复现请求 ID：`deep_route_empty_at_deleted_then_fallback`

路径：

1. 将 `38907867f4c6c36e` 重新导入为 `active`
2. 立即请求 `/v1/responses`
3. 客户端最终 HTTP 200，使用 `10e584b410a5f1ab` 成功返回
4. 请求后 `38907867f4c6c36e` 被打回 `expired`

问题：

- 管理端结构化日志只记录最终成功账号 `10e584b410a5f1ab`
- 没有记录第一次尝试的账号、失败类型、上游状态码或 fallback 决策

期望：

- 每次账号尝试都应可追踪
- fallback 链路应记录：
  - requestId
  - attemptedAccountId
  - attemptIndex
  - failureClass
  - upstreamStatus
  - upstreamCode
  - fallbackNextAccountId

### 3. 后台 refresh recovery 缺少账号级失败原因

真实观察：

```text
token 刷新定时器已调度 scheduled=1 immediate=2 recovery_scheduled=0 replaced=1
token 刷新定时器已调度 scheduled=1 immediate=0 recovery_scheduled=2 replaced=1
```

两个坏 RT 账号会周期性进入 `refreshing`，随后回到 `expired`，`updatedAt` 被推进。

问题：

- 文件日志只有聚合计数
- 没有账号 ID、失败原因、上游错误分类
- 管理端事件日志没有 refresh attempt 记录

期望：

- refresh 调度每个账号尝试都应有结构化记录
- 至少记录账号 ID、触发类型、结果、失败原因、下次恢复时间

### 4. WebSocket stream 返回失败但缺管理事件日志

请求 ID：`deep_real_responses_ws_stream`

客户端结果：

```text
HTTP 200
event: response.failed
data: {"error":{"code":"codex_api_error","message":"Upstream Codex request failed","type":"server_error"}}
data: [DONE]
```

问题：

- 管理端 `/api/admin/logs?requestId=deep_real_responses_ws_stream` 没有记录
- 文件日志只有 HTTP request completed，缺少上游失败细节
- 客户端 HTTP status 仍为 200，只能从 SSE event 判断失败

期望：

- stream 内部失败也应写入 `v1.response` error 事件
- metadata 应记录 `transport=websocket`、失败阶段、upstream code/message

当前状态：

- runtime 已能记录进入 live stream 后的 completed / failed 事件。
- 创建 live stream 前的首帧 `response.failed` 已通过 `record_prefetched_response_stream_failure_event` 写入 `v1.response` error 事件。
- 本次补齐 stream 启动阶段终态 dispatch error：账号耗尽、dirty quota 校验达到最大次数、模型不支持重试耗尽、首帧前空响应 / 缺 completion / SSE 解析失败都会写入 `kind=v1.response`、`level=error`，metadata 包含 `stream=true`、`transport`、`failureClass`、`exhaustedCount`、`upstreamError` 等字段。
- 定向用例已固定 HTTP SSE stream 的 429 fallback exhausted：客户端仍返回 SSE `response.failed`，管理端事件记录 `statusCode=429`、`failureClass=rate_limited`。

本次对照参考项目只作为排查线索：参考项目有 `recordStreamCloseEvent` / audit log 思路，会把 streaming 异常携带 requestId、path、model、account 写入排查日志；本项目不直接照搬旧日志体系，只补齐本项目 `event_logs`。WebSocket 真实链路还需要用 `gpt-5.5` 复测确认同类事件落库。

### 5. WebSocket 非流 fallback 不可观测

请求 ID：`deep_real_responses_ws_nonstream`

文件日志有：

```text
websocket receive idle timeout after 20s
falling back to HTTP SSE
```

管理端结构化日志没有 fallback 信息。

期望：

- 成功日志中记录实际 transport
- 发生 fallback 时记录原始 transport、fallback transport 和失败原因

### 6. 上游 400 被包装成泛化 502

请求 ID：`deep_real_upstream_400_empty_input`

请求体：

```json
{"model":"gpt-5.5","input":[],"stream":false,"use_websocket":false}
```

客户端结果：

```json
{
  "error": {
    "code": "upstream_error",
    "message": "Upstream Codex request failed",
    "type": "server_error"
  }
}
```

问题：

- HTTP 502 掩盖了真实上游 400
- 管理端结构化日志没有记录
- 文件日志也没有上游响应体

已知真实上游语义是缺少 `input`、`previous_response_id`、`prompt` 或 `conversation_id`。

期望：

- 上游 4xx 应保留错误 code/message 到日志
- 是否透传给客户端需要单独设计，但排查日志必须能还原上游响应

### 7. Chat stream 语义不符合 OpenAI SSE

请求 ID：`deep_real_chat_stream`

请求体包含：

```json
{"stream":true}
```

结果：

- HTTP 200
- 返回普通 JSON chat completion
- 不是 SSE chunk

期望：

- 如果声明支持 OpenAI 兼容 `/v1/chat/completions`，`stream:true` 应返回 SSE
- 如果暂不支持，应明确返回 400 或在 README/API 文档中说明限制

当前状态：

- 已按 TS 版本对齐：`stream:true` 复用 Responses stream 调度链路，再翻译为 OpenAI `chat.completion.chunk` SSE。
- 输出包含 assistant role chunk、文本 delta、final usage chunk 和 `data: [DONE]`。
- 启动阶段或中途失败按 TS chat stream 写 `data: {"error":{"message":...,"type":"stream_error"}}`。
- 已用定向自动化测试固定，并已用真实 `gpt-5.5` 复测通过。

### 8. Chat、compact、models 缺少结构化事件日志

以下 requestId 均无管理端事件日志：

```text
deep_real_chat_nonstream
deep_real_chat_stream
deep_real_compact_json
deep_real_models_get_probe
```

问题：

- 从管理端无法串联请求、账号、usage、上游状态
- compact 有真实上游调用和 usage，但没有进入事件日志
- models 是管理排障重要入口，也没有事件记录

期望：

- 至少 chat、compact 应写入结构化日志
- models 可记录 probe 成功/失败和账号 ID

当前状态：

- Chat 非流已补齐结构化事件：`kind=v1.chat`、`route=/v1/chat/completions`、`metadata.apiKind=chat`、`metadata.transport`、`metadata.usage`。
- compact 成功路径已补齐结构化事件：`kind=v1.response`、`route=/v1/responses/compact`、`metadata.compact=true`、`metadata.usage`、rate-limit headers。
- compact 成功 body 中的 `usage` 会用现有 `extract_usage` 提取并同步账号 usage 统计。
- `/v1/models` 仍是本地模型目录响应，尚未接入结构化事件；后续应单独设计，不和上游请求事件混在一起。

## 已修复项

### Responses WebSocket 压缩帧解码

状态：已修复并用真实 `gpt-5.5` WebSocket stream 验证。

复现请求：

```text
早期请求：real_chain_gpt55_ws_stream_0208 / real_chain_gpt55_ws_stream_0210
修复后请求：3f90f64e-9a3e-465c-8988-e38e2d5a0d47
```

真实问题：

- WebSocket opening 能完成 101，说明账号、基础认证和基础 WS 握手不是主要问题。
- 收到上游帧后客户端失败，管理端记录：

```text
websocket request failed: websocket transport error: UTF-8 encoding error: invalid utf-8 sequence of 1 bytes from index 65
```

对照抓包与原版：

- `docs/openai-res.txt` 中真实 WS opening header 只有最小集合：
  `Host`、`Connection`、`Upgrade`、`Sec-WebSocket-Version`、`Sec-WebSocket-Key`、
  `chatgpt-account-id`、`authorization`、`user-agent`、`originator`、`openai-beta`、
  `x-client-request-id`、`session-id`、`thread-id`、`x-codex-window-id`、
  `x-codex-turn-metadata`、`sec-websocket-extensions`。
- 曾尝试按这次抓包收敛 WS header 后仍复现 `invalid utf-8`，说明根因不是多余 header。
- 当前 Rust WS header 先按 TS 版本从统一 Codex header map 投影，过滤 HTTP/SSE 专用 `content-type`、`accept`，保留 TS 使用的 `session_id`，不再维护手写最小白名单。
- 原版 Codex Rust 使用 OpenAI fork 的 `tungstenite`，带 `deflate` 扩展；本项目已切到 fork 的 `tokio-tungstenite`，由 `WebSocketConfig.extensions.permessage_deflate` 负责协商和解码。

根因：

- 真实上游协商 `permessage-deflate; client_max_window_bits` 后可能使用 server context takeover。
- 旧实现每条消息都新建 deflate decoder，只能覆盖单消息/无上下文场景；真实连续压缩帧会导致解码错位，
  最终被 tungstenite 当作无效 UTF-8 文本帧。

修复内容：

- WS opening 业务头回到 TS 语义：从统一 Codex header map 派生，过滤 HTTP/SSE 专用的
  `content-type`、`accept`，保留 `session_id`，不再维护手写最小白名单。
- 自写 `PerMessageDeflateStream` 已删除，WebSocket 握手和 `permessage-deflate` 协商/解压改由
  OpenAI fork `tokio-tungstenite` 负责。
- 测试 helper 也使用同一 fork 配置接受 `permessage-deflate`，避免只测试项目自写解压器。

验证：

```text
cargo check -p codex-proxy-runtime -p codex-proxy-server
cargo test -p codex-proxy-adapters --test codex
```

真实请求结果：

```text
HTTP 200
event: response.completed
data: [DONE]

管理端日志：
kind=v1.response
level=info
statusCode=200
metadata.transport=websocket
metadata.completed=true
metadata.usage 存在
metadata.rateLimitHeaders 存在
```

### WebSocket payload 与 TS 对齐

状态：已修复并完成真实链路复测。

真实问题：

- Rust 之前在 `instructions` 为空字符串时省略 WebSocket `response.create.instructions`。
- 真实上游返回 400：`Instructions are required`。
- TS `createResponseViaWebSocket` 明确使用 `instructions: request.instructions ?? ""`，因此空字符串也必须发送。

本轮对齐内容：

- `instructions` 永远出现在 WebSocket `response.create` payload 中。
- WebSocket payload 字段顺序按 TS `wsRequest` 的对象插入顺序：
  `type`、`model`、`instructions`、`input`、`store`、`stream`，再追加可选字段。
- `reasoning`、`tools`、`include` 只在 TS 会赋值时发送；不再为了旧 Rust 快照发送
  `reasoning:null`、`tools:[]`、`include:[]`。
- `tool_choice` 和 `parallel_tool_calls` 保持 TS 默认：`auto` 和 `true`。

真实复测：

```text
/v1/responses WebSocket JSON: c3521027-8209-4326-9fab-5a7f9907696d
结果：HTTP 200，status=completed，usage 正常，metadata.transport=websocket

/v1/responses WebSocket SSE: d96acb51-cb10-4203-a33c-df4592ab4a6c
结果：HTTP 200，response.completed + [DONE]，metadata.transport=websocket
```

### refresh token 永久失效后的状态语义

状态：已根据真实链路证据调整为两阶段语义。请求链路只证明 access token 不可用时先落 `expired`；refresh token 本身被上游确认 invalid/expired 后，终态改为 `disabled`，避免坏 RT 被重启或后台周期调度继续刷新。

TS 基线：

- `invalid_grant` / `invalid_token` / `access_denied` / `refresh_token_expired` 属于 refresh token 永久错误，但 TS 的终态是 `expired`。
- `account has been deactivated` / `refresh_token_reused` / `banned` 才落 `banned`。
- `disabled` / `banned` 会被刷新调度跳过；`expired` 且有 refresh token 的账号会被安排 recovery。

Rust 当前内容：

- 请求链路 401 的 `token_revoked` / `token_invalid` / `invalidated oauth token` 仍映射为 `expired`，让 RT 有一次自救机会。
- `RefreshFailure::InvalidGrant` 映射为 `disabled`，表示 RT 已确认不可刷新，不再参与启动 recovery 或周期调度。
- `account_deactivated` / `banned` 继续映射为 `banned`。
- 状态更新和客户端返回状态码分开处理：TS 的 401 deactivated 会把账号标为 `banned`，但对客户端返回仍保留 401；403 banned 才返回 403。
- 新增 `accounts.next_refresh_at`，用于持久化下一次允许刷新时间。
- 刷新成功后写入下一次 refresh 时间；传输失败时写入 recovery 时间；确认 RT 永久失败时清空该时间并落 `disabled` / `banned` 等终态。
- 账号级 timer 现在记录 `scheduled_at` 和 trigger，相同未来 timer 不再每分钟重复替换和重复计入 `scheduled`。

历史验证记录：

```text
前一轮 disabled 方案下，重启前一轮真实刷新后：
pool: total=3 active=0 expired=0 refreshing=0 disabled=3 banned=0

再次重启后立即查询：
pool: total=3 active=0 expired=0 refreshing=0 disabled=3 banned=0
```

说明：

- 这证明 disabled 方案能避免重启后继续刷新坏 RT。
- TS 当前语义仍是 invalid_grant -> expired；本项目由于新增了持久化 `next_refresh_at` 和周期调度，继续照搬会造成坏 RT 反复进入 refreshing，因此这里是有证据的偏离。
- 当前没有 active 账号，后续真实请求链路需要重新导入有效账号后继续验证。

### Responses 字符串 input 上游 400

状态：已做最小修复，待有效账号恢复后继续真实复测。

复现请求 ID：`deep_real_gpt55_http_json_string_005809`

请求体：

```json
{"model":"gpt-5.5","instructions":"Return only the requested text.","input":"Reply exactly: gpt55 http json string ok","stream":false,"use_websocket":false}
```

真实结果：

```text
HTTP 502（客户端泛化）
管理端事件日志记录上游真实状态 400
```

上游 body：

```json
{
  "error": {
    "message": "Invalid type for 'input[0]': expected an input item, but got a string instead.",
    "type": "invalid_request_error",
    "param": "input[0]",
    "code": "invalid_type"
  }
}
```

对照抓包：

- `docs/openai-res.txt` 里的原版 Codex Desktop WebSocket 请求体使用 `type:"response.create"`。
- `input` 是 Responses input item 数组，不是裸字符串。
- 原版 Rust `codex-api` 的 `ResponsesApiRequest.input` 类型也是 `Vec<ResponseItem>`，测试样例使用 `ResponseItem::Message { content: vec![ContentItem::InputText { ... }] }`。

修复内容：

- OpenAI Responses 入口收到 `input` 字符串时，不再把字符串原样放进 Codex 上游 `input[0]`。
- 字符串会规范化为：

```json
{
  "type": "message",
  "role": "user",
  "content": [{"type": "input_text", "text": "..."}]
}
```

已验证的相邻真实结果：

```text
deep_real_gpt55_http_json_chatshape_005809      HTTP 200
deep_real_gpt55_http_json_messageitem_005809    HTTP 200
deep_real_gpt55_http_stream_chatshape_005809    HTTP 200
```

待复测：

- 重新导入有效账号后，用 `gpt-5.5` 复测字符串 input 应返回 200。

### 请求链路 token_revoked 状态分类

复现请求 ID：`deep_real_gpt55_http_json_string_0136`

请求体：

```json
{"model":"gpt-5.5","instructions":"Return only the requested text.","input":"Reply exactly: gpt55 http json string ok","stream":false,"use_websocket":false}
```

真实结果：

```text
HTTP 401
All accounts exhausted (3 expired)
上游错误 code: token_revoked
上游错误 message: Encountered invalidated oauth token for user, failing request
```

请求后账号池：

```text
total=3 active=0 expired=3 disabled=0 banned=0
```

问题判断：

- 请求链路 401 的 `token_revoked` / `token_invalid` / `invalidated oauth token` 应先落 `expired`，让 refresh token 有一次自救机会。
- `account_deactivated` / `banned` 落 `banned`。
- 401 deactivated 即使落 `banned`，客户端响应也应保留上游 401；403 banned 保留 403，不能只根据账号终态统一返回 403。
- 不把普通 401 token invalid 类错误直接归为 `disabled`；只有 refresh token 自身确认 invalid/expired 后才归为 `disabled`。
- 非流式 Responses 的账号全部耗尽路径当时没有写管理端事件日志，本次 `/api/admin/logs?requestId=deep_real_gpt55_http_json_string_0136` 返回空列表；当前代码已补齐终态 dispatch error 事件，复用现有 `failureClass` / `exhaustedCount` / `upstreamError` 元数据。
- compact 终态失败路径已补齐管理端事件日志：`kind=v1.response`、`route=/v1/responses/compact`、`level=error`、`metadata.failed=true`、`metadata.failureClass`、`metadata.exhaustedCount`、`metadata.upstreamError`。

### 刷新调度持久化

范围说明：本节讨论 access token 自动刷新调度，也就是 `next_refresh_at`。TS active quota refresher 对 `/usage` 主动校验的 30 分钟 per-account 节流是进程内 `Map`，没有持久化；不能把这两条调度链路混在一起判断。

真实链路要求：

- 刷新成功后必须持久化下一次允许刷新时间。
- 在 `next_refresh_at` 到达之前，后台扫描和进程启动扫描都不应再次执行刷新。
- `disabled` / `banned` 账号重启后不能重新进入刷新调度；`expired` 且有 refresh token 的账号仍可进入 recovery 调度。

已确认代码路径：

- `accounts.next_refresh_at` 已落 SQLite schema，并有轻量迁移补列。
- token refresh 后台任务扫描会跳过未来的 `next_refresh_at`。
- 账号级 timer 调度会把未来刷新时间写入 `next_refresh_at`。
- 传输失败只写入短恢复时间；永久失败清空 `next_refresh_at` 并落终态。

发现的问题：

- 管理端手动刷新、手动创建覆盖已有账号、健康检查刷新成功时，`AccountClaimsUpdate.next_refresh_at` 仍写入 `None`。
- 这会导致“刷新成功后的下一次刷新时间”没有在所有刷新路径持久化，重启后只能依赖 access token 过期时间重新计算，不利于审计。

修复方向：

- `AdminAccountService` 保存 `refresh_margin_seconds`。
- 所有刷新成功并写入新 access token 的管理端路径，都写入 `expires_at - refresh_margin_seconds` 作为 `next_refresh_at`。

### sub2api 过期 JWT 导入准入

修复内容：

- sub2api 导入时不再盲信导出文件里的 `active` 状态。
- 当导入来源为 sub2api 且 token JWT 不可解析或已过期时，账号以 `expired` 状态入库。
- 原生导入行为不跟着改变，避免把格式适配器问题扩散到原生格式。

验证：

```bash
cargo test -p codex-proxy-server --test admin_accounts_routes admin_accounts_import_should_expire_sub2api_account_when_token_is_expired
```

### `/v1/responses/review` 事件日志 route

修复内容：

- server 层把真实入口 route 传入 runtime。
- runtime 的非流式和流式 Responses 事件日志都记录传入的真实 route，不再写死 `/v1/responses`。
- `/v1/responses/review` 的 review 语义仍通过 metadata/subagent 处理，日志 route 保留 HTTP 入口。

验证：

```bash
cargo test -p codex-proxy-server --test openai_chat_upstream responses_review_route_should_record_review_route_in_event_log
```

## 修复优先级

### P0：账号状态准入

- sub2api 适配器不允许过期或无法解析 `exp` 的 token 直接以 active 入库
- 导入 update 时需要同步清理或设置运行池状态
- 导入后应尽快触发 refresh/health check，避免已删除/封禁账号长时间污染 active 池

### P0：请求尝试和 fallback 日志

- 每次上游账号尝试都需要结构化事件或 metadata
- 成功响应不能只记录最终账号，应能反查失败尝试
- stream 启动阶段终态失败已补齐管理端事件日志；逐次账号尝试和 fallback 决策仍需更细粒度事件

### P1：上游错误保真

- 保留上游 status、code、message、response body 摘要
- 客户端错误和内部日志可以分层，但日志不能丢失真实原因

### P1：WebSocket transport 可观测

- 成功日志记录 `transport`
- fallback 日志记录 `fromTransport`、`toTransport`、`reason`
- WebSocket stream 失败需要继续真实复测，确认启动阶段和 live 阶段都能落到 event log

### P1：Chat stream 行为

- 已实现 OpenAI SSE chat stream，按 TS 版本转换 Codex SSE 为 `chat.completion.chunk`
- 待有效账号恢复后复测真实 `gpt-5.5` stream 请求

### P2：chat、compact、models 事件日志覆盖

- chat completion 成功事件已记录 accountId、route、model、usage；非流失败耗尽路径已补齐 `kind=v1.chat`、`level=error`、`statusCode`、`failureClass`、`exhaustedCount`、`upstreamError`、`transport`
- Responses 非流终态失败事件已补齐 route、accountId、statusCode、failureClass、exhaustedCount、upstreamError；已用 429 fallback exhausted 定向用例验证响应路径。
- Responses stream 启动阶段终态失败事件已补齐 `stream=true`、`transport`、`failureClass`、`exhaustedCount`、`upstreamError`；已用 429 fallback exhausted 定向用例验证 SSE 错误响应和事件日志一致。
- compact 成功事件已记录 accountId、usage、route；终态失败耗尽事件已记录 route、accountId、statusCode、failureClass、upstreamError
- models probe 记录状态即可，不应记录敏感头

## 本次问题对照记录

参考项目路径：

```text
/home/zyy/桌面/Codes/codex-proxy
```

说明：参考项目不作为正确性标准。只有当本项目真实链路已经复现问题时，才把它作为差异排查材料；是否修、怎么修，以本项目 OpenAI/Codex-only 的目标和真实测试结果为准。

本次对照到的差异：

- `src/routes/responses.ts` 在 ingress 日志里使用 `path: c.req.path`，这解释了为什么它能保留 `/v1/responses/review` 入口。
- `src/routes/responses.ts` 通过 `c.req.path === "/v1/responses/review"` 判断 review 入口，仅把 review 语义写进请求 metadata，不改变原始路由路径。
- `src/routes/messages.ts` 也使用 `path: c.req.path` 记录 chat 入口，可作为“入口层知道真实路由，下游不应写死”的排查线索。
- `src/services/account-import.ts` 在导入 token 前会走 token 校验或 refresh token 换取后校验；本项目当前不迁移其历史兼容层，但 sub2api 导入不能盲信导出的 active 状态，这一点已由本项目真实导入测试独立证明。

本项目当前修复方向：server 层把真实入口路由传给 runtime，runtime 事件日志按真实路由写入；导入适配器按格式做最小状态准入，不引入旧项目的多 provider 或迁移兼容逻辑。

## 后续对照参考项目

本仓库和参考项目均存在 codegraph 数据库：

```text
.codegraph/codegraph.db
/home/zyy/桌面/Codes/codex-proxy/.codegraph/codegraph.db
```

下一步对照重点：

- 导入适配器如何处理空 access token、坏 refresh token、账号禁用
- 请求 fallback 是否保留每次尝试的信息
- WebSocket stream 失败如何分类和记录
- chat stream 真实链路是否按 OpenAI SSE 稳定输出
- compact/review 是否有独立日志语义

参考项目只能作为线索，不能直接搬迁迁移层或历史兼容测试；最终以本项目 OpenAI/Codex 真实链路为准。

## 2026-06-20 真实链路复测

本轮使用当前源码重新构建并启动 `target/debug/codex-proxy-server`。当时运行态 fingerprint 曾切到 macOS 解包值，但该结论已被后续 TS 对齐修正；当前默认基线应以 TS `config/default.yaml` 为准：

```text
Codex Desktop/26.519.81530 (darwin; arm64)
build=3178
chromium=146
```

启动后账号池：

```text
total=4 active=1 disabled=3 expired=0 refreshing=0 banned=0
```

启动日志显示：

```text
token 刷新定时器已调度 scheduled=1 immediate=0 recovery_scheduled=0 replaced=0
```

说明前一轮 disabled 方案下，启动没有把 3 个 disabled 账号重新拉入刷新。当前 TS 对齐实现应重新验证 `expired` recovery 与 `next_refresh_at` 行为。

本轮临时 client key 已在验证后删除，明文文件也已从 `.runtime/real-chain-run-20260620/` 删除。

### 本轮请求结果

所有请求均使用 `gpt-5.5` 和自然中文输入，request id 均为 UUID。

| 场景 | requestId | 客户端结果 | 结构化日志 |
| --- | --- | --- | --- |
| `/v1/models` | `9bf94caf-dd87-42db-97a0-7fb5362a4408` | HTTP 200，返回 `codex-auto-review`、`gpt-5.4-mini`、`gpt-5.5` | 无 `event_logs` |
| `/v1/responses` HTTP JSON | `bf4217c8-2721-4677-a98c-187905951730` | HTTP 200，`status=completed`，有 `output_text` 和 usage | 有 `v1.response`，route 正确，usage/rate-limit 存在 |
| `/v1/responses` HTTP SSE | `1a2fcdeb-1713-413f-82bf-a76af20c01b1` | HTTP 200，SSE 到 `response.completed` 和 `[DONE]` | 有 `v1.response`，`stream=true`、`completed=true`、usage/rate-limit 存在 |
| `/v1/responses` WebSocket JSON | `c3521027-8209-4326-9fab-5a7f9907696d` | HTTP 200，`status=completed`，有 `output_text` 和 usage | 有 `v1.response`，`transport=websocket`、usage/rate-limit 存在 |
| `/v1/responses` WebSocket SSE | `d96acb51-cb10-4203-a33c-df4592ab4a6c` | HTTP 200，SSE 到 `response.completed` 和 `[DONE]` | 有 `v1.response`，`transport=websocket`、`stream=true`、`completed=true`、usage/rate-limit 存在 |
| `/v1/chat/completions` JSON | `28041c82-2e22-4a19-ab53-b769719e0deb` | HTTP 200，返回 chat completion JSON 和 usage | 无 `event_logs` |
| `/v1/chat/completions` 声明 `stream:true` | `d10681f3-c9cd-4548-ab8e-1b2e5b7379d1` | HTTP 200，返回 OpenAI `chat.completion.chunk` SSE 和 `[DONE]` | 有 `v1.chat`，`metadata.apiKind=chat`、`metadata.transport=http_sse` |
| `/v1/responses/review` | `ec689b00-421b-4599-a29f-afcf256ae6bf` | HTTP 200，`status=completed`，有 `output_text` 和 usage | 有 `v1.response`，route 已正确记录为 `/v1/responses/review` |
| `/v1/responses/compact` | `213e0e53-1bab-4569-aa55-75d1c91770cc` | HTTP 200，返回 `response.compaction` 和 usage | 无 `event_logs` |

文件日志中，本轮两个 WebSocket 请求未再出现旧的 `invalid utf-8`、`idle timeout` 或 `falling back to HTTP SSE`。

### Chat stream 修复后真实复测

请求 ID：`9805212d-401f-4a10-a8f1-3d5bc422299e`

请求：

- 模型：`gpt-5.5`
- 路径：`/v1/chat/completions`
- 参数：`stream:true`
- 输入：自然中文请求

客户端结果：

```text
HTTP 200
content-type: text/event-stream
包含 OpenAI chat.completion.chunk
包含 assistant role chunk
包含 content delta
包含 final usage chunk
包含 data: [DONE]
未出现 error SSE
```

响应尾部 usage：

```json
{
  "prompt_tokens": 27,
  "completion_tokens": 52,
  "total_tokens": 79,
  "prompt_tokens_details": {
    "cached_tokens": 0
  },
  "completion_tokens_details": {
    "reasoning_tokens": 16
  }
}
```

管理端结构化日志：

```text
修复前：
kind=v1.response
level=info
statusCode=200
metadata.stream=true
metadata.completed=true
metadata.usage 存在
metadata.responseId 存在
```

后续修复：

- 当前代码已按 TS `tag: "Chat"` 语义补齐事件分类：chat stream 记录为 `kind=v1.chat`。
- top-level `route` 继续记录 `/v1/chat/completions`。
- `metadata.route` 和 `metadata.apiKind=chat` 已补齐，便于从管理日志直接区分 chat stream 与 responses stream。
- Chat 模型选项已按 TS 对齐：`reasoning_effort` / `service_tier` 使用“显式字段 > 模型后缀 > 配置默认值”，`fast` 发送上游前规范为 `priority`，stream 输出是否包含 reasoning 按翻译后的 `reasoning.effort` 判断。
- Chat stream 响应中的模型名使用 `build_display_model_name(parse_model_name(req.model))`，与 TS `buildDisplayModelName(parseModelName(req.model))` 一致。
- 已用定向自动化测试固定。

二次真实复测：

请求 ID：`dc22ea52-9f74-428f-8fe5-3b1a3def0114`

结果：

```text
HTTP 200
content-type: text/event-stream
包含 OpenAI chat.completion.chunk
包含 assistant role chunk
包含 content delta
包含 final usage chunk
包含 data: [DONE]
未出现 error SSE
```

管理端结构化日志：

```text
kind=v1.chat
level=info
route=/v1/chat/completions
statusCode=200
metadata.route=/v1/chat/completions
metadata.apiKind=chat
metadata.stream=true
metadata.completed=true
metadata.usage 存在
metadata.responseId 存在
```

产物目录：`.runtime/real-chain-run-20260620/chat-stream-log-after-new-binary/`

### 本轮仍存在的问题

1. `/v1/models` 仍没有结构化事件日志；chat 非流成功/失败耗尽、Responses 非流终态失败、compact 成功/终态失败事件已补齐。
2. Chat `stream:true` 已按 TS 对齐为 OpenAI SSE，并通过真实链路复测；结构化日志分类和 metadata 字段已验证。
3. WebSocket 非流成功事件已补齐 `metadata.transport=websocket`，并已用真实 `gpt-5.5` 链路复测确认。
4. `/health` 当前返回前端 SPA HTML，不是专用健康检查 JSON；这不阻塞上游链路，但会影响脚本化探活。

### 最新原生导入复测：token invalidated 阻断全链路

导入文件：

```text
/home/zyy/下载/sub2api-accounts-2026-06-19T17-53-40-679Z.json
```

导入结果：

```text
sourceFormat=native
imported=1
skipped=0
导入后账号池：active=1 disabled=3
```

说明：

- 该文件当前被识别为本项目原生格式，不是 sub2api 格式。
- 导入后恢复的 active 账号是已有账号的 update，不新增账号。
- 临时 client key 已在本轮请求后删除；本文档不记录明文 key、token、refresh token 或 cookie。

本轮请求均使用 `gpt-5.5` 和自然中文输入：

| 场景 | requestId | 客户端结果 | 结构化日志 |
| --- | --- | --- | --- |
| `/v1/models` | `f33d7261-f613-4ae9-9c4b-30f92a173ad6` | HTTP 200，返回 `codex-auto-review`、`gpt-5.4-mini`、`gpt-5.5` | 无 `event_logs` |
| `/v1/responses` HTTP JSON | `3a6e97cf-8ddc-402e-9b1d-0d85e7d975bf` | HTTP 401 | 有 `v1.response` error，`failureClass=expired`，`transport=http_sse` |
| `/v1/responses` HTTP SSE | `afdf9d34-b76f-4dc2-83fe-8c61fd253202` | HTTP 200，SSE 内 `response.failed` + `[DONE]` | 有 `v1.response` error，`failureClass=no_active_account` |
| `/v1/responses` WebSocket JSON | `4763c753-a4c8-484f-afaf-2f3a6b2740a2` | HTTP 503 | 有 `v1.response` error，`transport=websocket`，`failureClass=no_active_account` |
| `/v1/responses` WebSocket SSE | `b7974ac0-aab3-4fe3-9417-537dcbca08fd` | HTTP 200，SSE 内 `response.failed` + `[DONE]` | 有 `v1.response` error，`transport=websocket`，`failureClass=no_active_account` |
| `/v1/chat/completions` JSON | `51e0c622-3752-4c28-88f2-6ed98e2b9e7a` | HTTP 503 | 有 `v1.chat` error，`failureClass=no_active_account` |
| `/v1/chat/completions` stream | `0b443d81-7f50-4b7a-a6b3-e89391c777a1` | HTTP 200，SSE 内 `stream_error`，未见 `[DONE]` | 有 `v1.chat` error，`failureClass=no_active_account` |
| `/v1/responses/review` | `da8a1046-eefc-4a1d-953d-cb355891a1a2` | HTTP 503 | 有 `v1.response` error，route=`/v1/responses/review` |
| `/v1/responses/compact` | `d43390fa-1f93-4d84-87a5-8e2a94beb740` | HTTP 503 | 有 `v1.response` error，`compact=true` |

第一个真实上游业务请求返回：

```json
{
  "error": {
    "message": "Your authentication token has been invalidated. Please try signing in again.",
    "type": "invalid_request_error",
    "code": "token_invalidated",
    "param": null
  },
  "status": 401
}
```

请求后账号池：

```text
active=0
expired=1
disabled=3
```

Cookie 观察：

```text
account_cookies 仍只有历史 __cf_bm 记录
未捕获新的 cf_clearance
updated_at 未变化
```

结论：

- 这份最新原生导入文件能恢复 active 状态，但真实上游已判定 access token invalidated。
- 本轮不能证明 WebSocket / HTTP SSE 成功链路，因为第一个业务请求后已经没有 active 账号。
- 当前错误事件日志覆盖比前序版本更完整：除 `/v1/models` 外，Responses、chat、review、compact 的 no-active-account 都能落入 `event_logs`。
- Chat stream 的 no-active-account 响应只返回 `data: {"error": ...}`，没有 `[DONE]`；是否需要和 Responses stream 的错误收尾对齐，后续需要对照 TS 版本确认。

### 手动刷新失效 RT 复测

对最新导入恢复的账号执行管理端手动刷新：

```text
POST /api/admin/accounts/{account_id}/refresh
```

结果：

```json
{
  "result": "dead",
  "previousStatus": "expired",
  "status": "expired",
  "error": "refresh token is invalid or expired"
}
```

刷新后账号池仍为：

```text
expired=1
disabled=3
```

新发现：

- 后台 token refresh 任务在永久失败状态下会清理 `next_refresh_at`。
- 管理端手动刷新失败路径只调用 `set_status`，没有清理 `next_refresh_at`。
- 本轮真实刷新失败后，该账号仍保留未来的 `next_refresh_at=2026-06-29T17:48:34Z`。

判断：

- 这是管理端刷新路径和后台刷新路径的行为不一致。
- 若 refresh token 已确认永久失效，继续保留未来 `next_refresh_at` 会让管理端状态看起来像仍有下一次计划刷新，不利于排查。
- 修复方向应是管理端手动刷新遇到永久失败状态时同步清理 `next_refresh_at`，并与后台刷新任务保持一致。

修复结果：

- 管理端手动刷新失败路径已在非 active 终态下清理 `next_refresh_at`。
- 管理端 health-check 的 refresh-token 探测失败路径同步采用相同语义。
- 重新启动新二进制后复验同一失效 RT，`next_refresh_at` 已能清空：

```text
result=dead
previousStatus=expired
status=expired
error="refresh token is invalid or expired"
next_refresh_at=NULL
```

后续真实复验进一步确认：仅清理 `next_refresh_at` 仍会让 expired 账号被后台 recovery 再次捞起刷新；因此最终状态语义调整为“确认 RT 永久失效后落 `disabled`”。新的预期是：

```text
result=dead
previousStatus=expired
status=disabled
error="refresh token is invalid or expired"
next_refresh_at=NULL
```

新二进制启动后的最终复验：

```text
启动 recovery 触发同一坏 RT 刷新
账号最终状态：disabled
账号池：disabled=4
acct_be7.next_refresh_at=NULL
短时间观察未再进入 refreshing
```

验证命令：

```bash
cargo fmt --check
cargo check -p codex-proxy-runtime -p codex-proxy-server
```

## 2026-06-20 accounts-export-2026-06-20 真实链路复测

本轮使用文件：

```text
/home/zyy/下载/accounts-export-2026-06-20.json
```

导入结果：

```text
sourceFormat=sub2api
imported=1
skipped=0
```

导入后账号池：

```text
active=1
disabled=4
total=5
```

说明：

- 文件顶层结构是 `accounts`，账号数量为 1；当前导入器识别为 `sub2api`。
- 本轮未输出 access token、refresh token、cookie、client key 明文。
- 本轮运行目录：`.runtime/real-chain-run-20260620/import-150350/`。
- 后续确认这里的 `total=5` 是导入去重缺陷造成：同一 ChatGPT `accountId=99ad8af8-08ea-444d-8975-d84b410a27af` 因导入文件本地 `id` 不同而新增了第二条记录，没有更新旧记录。

去重修复后，运行库已删除新插入的重复行并用同一文件重新导入，最终账号池为：

```text
active=1
disabled=3
total=4
```

最终只保留一条匹配记录：

```text
id=acct_be7f5c37f60b44ff8058b1b9b164fd42
account_id=99ad8af8-08ea-444d-8975-d84b410a27af
user_id=user-nOaewfOy8r1MGUpmQbKMu1Xv
status=active
```

### 请求覆盖

全部请求使用 `gpt-5.5`，请求内容使用自然中文业务文本，不包含测试提示语。

| 场景 | HTTP 状态 | transport | 终态 | usage | rate-limit headers |
| --- | ---: | --- | --- | --- | ---: |
| `/v1/models` | 200 | 本地目录 | 成功，目录含 `gpt-5.5` | 不涉及 | 不涉及 |
| `/v1/responses` JSON | 200 | `http_sse` | 成功 | `23/86` | 9 |
| `/v1/responses` SSE | 200 | `http_sse` | 成功，SSE 有 `[DONE]` | `26/123` | 9 |
| `/v1/responses` WS JSON | 200 | `websocket` | 成功 | `21/103` | 3 |
| `/v1/responses` WS SSE | 200 | `websocket` | 成功，SSE 有 `[DONE]` | `26/157` | 3 |
| `/v1/chat/completions` JSON | 200 | `http_sse` | 成功 | `32/178` | 9 |
| `/v1/chat/completions` stream | 200 | `http_sse` | 成功，SSE 有 `[DONE]` | `36/134` | 9 |
| `/v1/responses/review` | 200 | `http_sse` | 成功 | `56/1263` | 9 |
| `/v1/responses/compact` | 200 | 未记录 | 成功 | `63/58` | 9 |

请求后账号池仍为：

```text
active=1
disabled=4
```

结论：

- 这份导入文件中的账号可以完成真实上游链路，HTTP/SSE、WebSocket JSON、WebSocket SSE、chat、review、compact 均成功。
- WebSocket 链路的管理端事件日志明确记录 `transport=websocket`、usage 和 rate-limit headers，说明本轮不是落回 HTTP 路径。
- 请求后 active 账号没有被错误降级，坏 RT 账号也没有被重新调度为 active。

### 日志观察

结构化事件日志覆盖情况：

- Responses HTTP/WS、chat JSON/stream、review、compact 均有 `event_logs`。
- 成功日志包含 route、model、statusCode、latencyMs、usage、rate-limit headers。
- 流式成功日志包含 `completed=true`。
- 本轮没有 failureClass。

待观察点：

- compact 成功日志有 usage 和 rate-limit headers，但 metadata 中没有 `transport` 字段；如果后续需要从日志快速区分 compact 的上游发送路径，应补齐该字段。
- 本轮没有生成新的 WS opening audit JSON 文件；当前只能从结构化事件日志确认 `transport=websocket`，不能用新审计文件复核 wire-level opening header。
