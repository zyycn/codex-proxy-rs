# 官方 Codex 请求链路解包记录

本记录基于官方 macOS Codex Desktop 包解包结果，用于校准 `codex-proxy-rs` 的 Codex 上游请求实现。

- 官方包路径：`/tmp/codex-desktop-official/dmg-extract/Codex Installer/Codex.app`
- App 版本：`26.616.81150`
- Build：`4306`
- app.asar 解包目录：`/tmp/codex-desktop-official/app-asar`
- 原生二进制：`Contents/Resources/codex`，Mach-O arm64，当前 Linux 环境不能直接运行，只能用静态字符串和 JS bundle 取证。

## 总结

官方 Desktop 的主对话请求链路不是 webview 直接 `fetch /codex/responses`。webview 负责 UI、设置和额度展示；认证头、远端控制、真实 Codex 请求链路主要在 Electron main bundle 和 `Resources/codex` 原生二进制里。

当前可确认的官方形态：

- 对话主通道同时存在 `responses_http` 和 `responses_websocket` 两类 transport。
- WebSocket beta 头为 `OpenAI-Beta: responses_websockets=2026-02-06`。
- WebSocket opening 请求头会携带 `Authorization`、`ChatGPT-Account-Id`、`originator`、`User-Agent`、`x-client-request-id`、`session-id`、`thread-id`、`x-codex-window-id`、`x-codex-turn-metadata`。
- 原生二进制包含 `x-client-request-id`、`x-oai-attestation`、`x-responsesapi-include-timing-metrics`、`x-codex-installation-id`、`x-codex-turn-state` 等请求相关字段。
- 请求体/WS payload 关键字段包含 `model`、`instructions`、`input`、`parallel_tool_calls`、`previous_response_id`、`store`、`include`、`client_metadata`。
- 官方响应处理以 `response.completed` 作为完成边界；二进制里有 `stream closed before response.completed` 和 `remote compaction v2 stream closed before response.completed`。
- 官方 WebSocket 失败存在回退路径，二进制里有 `websocket connection is unavailable`、`Falling back from WebSockets to HTTPS transport.`、`try_switch_fallback_transport`。
- Cloudflare/cookie 风控存在官方实现，二进制里有 `ChatGptCloudflareCookieStore`、`set_cookies`、`cookies`。

## 官方证据

### Electron main

文件：`/tmp/codex-desktop-official/app-asar/.vite/build/main-dSxbxAhH.js`

关键函数：

- `v_({ action, appServerClient, desktopOriginator, headers, refreshToken })`：调用 `appServerClient.getAuthToken`。
- `y_(headers, token, { desktopOriginator })`：写入认证和 surface 头。
- `x_(token)`：解析 JWT 中 `https://api.openai.com/auth.chatgpt_account_id`。
- `S_()`：生成 `Codex Desktop/<appVersion> (<platform>; <arch>)`。

可确认请求头：

- `Authorization: Bearer <token>`
- `ChatGPT-Account-Id: <jwt auth.chatgpt_account_id>`
- `originator: <desktopOriginator>`
- `User-Agent: Codex Desktop/<version> (<platform>; <arch>)`

同一 bundle 还包含 remote control WebSocket：

- path：`/codex/remote/control/client`
- remote control 同样通过 `getAuthHeaders` 获取认证头。
- 解析入站头时同时接受 `ChatGPT-Account-Id` 和 `chatgpt-account-id`。

### 原生 codex 二进制

文件：`/tmp/codex-desktop-official/dmg-extract/Codex Installer/Codex.app/Contents/Resources/codex`

`strings` 可见的请求链路证据：

- `responses_websockets=2026-02-06`
- `OpenAI-Beta`
- `x-client-request-id`
- `x-oai-attestation`
- `x-responsesapi-include-timing-metrics`
- `/responses/compact`
- `x-codex-installation-id`
- `responses_http`
- `responses/responses`
- `responses_websocket`
- `websocket connection is unavailable`
- `x-codex-turn-state`
- `auth_manager_missing`
- `recovery_succeeded`
- `recovery_failed_permanent`
- `recovery_failed_transient`
- `stream closed before response.completed`
- `model`
- `instructions`
- `input`
- `parallel_tool_calls`
- `previous_response_id`
- `ResponsesWsRequest`
- `response.create`
- `store`
- `include`
- `client_metadata`

这些是二进制字符串证据，不等价于完整源码控制流；但足以证明官方包中存在这些 transport、请求字段、恢复状态和完成边界。

追加取证到的更细字段：

- 官方二进制包含源码路径字符串：`codex-api/src/endpoint/responses_websocket.rs`、`codex-api/src/endpoint/responses.rs`、`codex-api/src/endpoint/compact.rs`、`codex-api/src/endpoint/models.rs`、`codex-client/src/request.rs`、`codex-client/src/default_client.rs`、`core/src/client.rs`。
- WebSocket 相关符号包含 `ResponsesWebsocketClient`、`ResponsesWebsocketConnection`、`merge_request_headers`、`serialize_websocket_request`、`send_websocket_request`、`run_websocket_response_stream`、`map_wrapped_websocket_error_event`。
- 官方连接日志包含 `responses_websocket.connect`、`responses_websocket.stream_request`、`responses.stream_request`、`transport`、`pre_compression_bytes`、`post_compression_bytes`、`compression_duration_ms`。
- 官方二进制静态字符串还能看到 `tokio_tungstenite::tls::encryption::rustls`、`tungstenite::handshake`、`reqwest`、`h2`、`gzip`、`deflate` 等传输栈痕迹。这只能证明 Desktop 包内存在这些 Rust HTTP/WS/TLS/压缩组件，不能单独还原完整调用图。
- 官方协议能力包含 `requestAttestation`，注释为允许 client 接收 `attestation/generate` 请求并为上游生成 `x-oai-attestation`。
- 官方 WebSocket 头相关字段包含 `ws_request_header_traceparent`、`ws_request_header_tracestate`、`ws_request_header_x_openai_internal_codex_responses_lite`。
- Desktop 原生二进制还出现 `add_responses_lite_header`、`stamp_ws_stream_request_start_ms`、`x-openai-internal-codex-responses-lite`、`x-codex-ws-stream-request-start-ms`、`x-codex-beta-features`。公开 `openai/codex` 同名源码只能作为辅助解释：
  - `stamp_ws_stream_request_start_ms` 会向 WebSocket payload `client_metadata` 写入当前 Unix 毫秒字符串 `x-codex-ws-stream-request-start-ms`。
  - `add_responses_lite_header` 在辅助源码里依赖模型信息 `use_responses_lite = true`，这不能单独证明 Desktop 当前所有触发条件。
- 官方错误文案包含 `Responses websocket connection limit reached (60 minutes). Create a new websocket connection to continue.`，说明 WebSocket 连接有 60 分钟生命周期或服务端连接限制。
- 官方安全拦截文案包含 `This request has been flagged for possible cybersecurity risk.`，属于请求内容/工具调用侧风控信号，不是普通额度不足。

### 运行态 WebSocket 抓包

文件：本地取证 `docs/flows.all.txt` 和已提交的 `docs/openai-res.txt`。`flows.all.txt` 是完整抓包，内容较大且包含大量请求正文，不作为普通文档资产提交。

确认点：

- Flow #41/#44/#48/#59/#60 均为 `GET /backend-api/codex/responses` WebSocket opening。
- opening 基础头包含 `Host`、`Connection: Upgrade`、`Upgrade: websocket`、`Sec-WebSocket-Version: 13`、`Sec-WebSocket-Key`。
- opening 业务头包含 `chatgpt-account-id`、`authorization`、`user-agent: Codex Desktop/...`、`originator: Codex Desktop`、`openai-beta: responses_websockets=2026-02-06`。
- opening 会话头包含 `x-client-request-id`、`session-id`、`thread-id`、`x-codex-window-id`、`x-codex-turn-metadata`，随后是 `sec-websocket-extensions: permessage-deflate; client_max_window_bits`。
- payload 的 `client_metadata` 可见 `session_id`、`thread_id`、`x-codex-installation-id`、`x-codex-window-id`、`x-codex-turn-metadata` 和 `x-codex-ws-stream-request-start-ms`。
- 服务端会下发内部 `codex.rate_limits` 事件；本次 free 样本为 `plan_type = "free"`、`primary.window_minutes = 43200`、`secondary = null`、`additional_rate_limits = null`、`credits = null`。

证据边界：本抓包只覆盖 WebSocket opening 和 WebSocket 事件；HTTP SSE 是否完全同头只能结合官方二进制的 shared request header 线索和当前项目的 shared header builder 推断。

### Webview 额度逻辑

相关文件：

- `webview/assets/thread-context-inputs-B6tQCr7t.js`
- `webview/assets/use-rate-limit-DfBgdYGx.js`
- `webview/assets/rate-limit-rows-B5kE6YOz.js`
- `webview/assets/usage-settings-BMxGTDii.js`
- `webview/assets/codex-api-AUWtp9Y7.js`

确认点：

- `thread-context-inputs` 查询 `/wham/usage`，query key 为 `rate-limit-status`，失败 401/403/404 时返回 `null`。
- `thread-context-inputs` 对 `rate-limit-status` 设置 `refetchInterval = ONE_MINUTE`，且 `refetchIntervalInBackground = false`，说明 Desktop webview 打开且不在后台时会约每分钟刷新一次 usage。
- `use-rate-limit` 把 `rate_limit.primary_window`、`secondary_window` 转成 `primary`、`secondary` bucket，并遍历 `additional_rate_limits`。每个 additional item 会按 `limit_name` 建一个独立 snapshot。
- `rate-limit-rows` 将每个 snapshot 的 `primary` 和 `secondary` 按 `windowDurationMins` 从小到大排序为多行，5 小时、周、月分别映射成 `five-hour`、`weekly`、`monthly` key。
- `usage-settings` 识别：
  - `300` 分钟：`5 hour usage limit`
  - `10080` 分钟：`Weekly usage limit`
  - `1440` 分钟：`Daily usage limit`
  - `30 * 1440` 分钟：`Monthly usage limit`
- 月限额优先使用 `spend_control.individual_limit`；没有该字段时才从 30d rate-limit row 中取。
- `codex-api` 有 `/wham/rate-limit-reset-credits` 和 `/consume`，用于额度 reset credit 的读取/消费。

结论：5h 和 7d/周限额可以同时出现，通常是同一个 snapshot 的 `primary_window` 与 `secondary_window`。30d/月限额是单独抽出来展示；free 账号常见只有月/30d 限额。

#### 限额组合还原

官方 webview 不是只取一个“当前限额”。它先把 `/wham/usage` 归一成 snapshot 列表：

- core snapshot：来自顶层 `rate_limit`，包含 `primary` 和 `secondary` 两个可选窗口。
- additional snapshots：来自 `additional_rate_limits[]`，每个有 `limit_name` 的条目各自拥有 `primary` 和 `secondary`。
- 月限额：展示层优先读取 `spend_control.individual_limit`；如果没有该字段，才在 core rows 里寻找 30d window。
- workspace monthly：企业/workspace 路径还可能从 workspace monthly usage 单独构造一个 monthly row。

因此官方 UI 里出现的组合是合理的：

- `5h + 7d`：通常是同一个 snapshot 的 primary/secondary 窗口，官方会显示成两行 short-term rows。
- `30d`：如果来自 `spend_control.individual_limit`，官方会作为 monthly usage limit 单独显示，并把 core 里的 30d row 从通用 rows 里过滤掉。
- `free`：常见只看到 monthly/30d，是因为短周期窗口可能不存在，或者月限额由 spend control 提供。
- model/additional：例如 `gpt-5.3-codex-spark` 这类 additional limit 会按 `limit_name` 单独组成 snapshot，和 core 限额不是同一行。

官方阻断判断也不是单看 `rate_limit.limit_reached`：`rate_limit.allowed === false`、`credits` 不可用、`spend_control.reached === true` 都会进入“额度不可用”的判断路径。additional limit 的阻断是按对应 `limit_name` 独立判断，不能简单等同于整个账号不可用。

### 官方响应事件

文件：

- `app-asar/.vite/build/worker.js`
- `app-asar/.vite/build/workspace-root-drop-handler-DYf1cfzL.js`

可见的 OpenTelemetry/事件处理名单：

- operation：`responses.create`、`chat.completions.create`、`embeddings.create`
- stream events：`response.created`、`response.in_progress`、`response.failed`、`response.completed`、`response.incomplete`、`response.queued`
- delta/item events：`response.output_text.delta`、`response.output_item.added`、`response.function_call_arguments.delta`、`response.function_call_arguments.done`、`response.output_item.done`

官方 worker 会从流事件里收集 response id、model、timestamp、usage、finish reason、text 和 tool calls。

## 当前项目对齐

### Transport 选择

当前实现：`src/upstream/protocol/responses.rs`

- `force_http_sse = true` -> `HttpSse`
- `previous_response_id.is_some()` -> `WebSocketRequired`
- `use_websocket = true` -> `WebSocketPreferred`
- 其他情况 -> `HttpSse`
- `WebSocketRequired` 不允许 HTTP SSE fallback。

这和官方二进制里 `previous_response_id`、`responses_websocket`、`try_switch_fallback_transport` 的证据方向一致。

### HTTP SSE

当前实现：`src/upstream/transport/client.rs`

- POST `/codex/responses`
- `Accept: text/event-stream`
- body 为 `CodexResponsesRequest`
- 非 2xx 转为 `CodexClientError::Upstream`，保留 status、body、`retry-after`、`set-cookie`
- 非流式读取完整 SSE 文本并提取 usage
- 流式返回 `bytes_stream`

### WebSocket

当前实现：

- `src/upstream/transport/websocket.rs` 把 base URL 的 `/codex/responses` 转成 `wss://.../codex/responses` 或 `ws://.../codex/responses`。
- `src/upstream/transport/client.rs` 复用同一套请求头构造 WebSocket opening headers。
- WebSocket 请求会写 audit artifact，包含 opening snapshot 和 payload snapshot。
- 如果启用了 pool，会按账号/session 复用连接。
- opening request 固定补齐 `Host`、`Connection: Upgrade`、`Upgrade: websocket`、`Sec-WebSocket-Version: 13`、`Sec-WebSocket-Key`，业务头随后追加，最后追加 `sec-websocket-extensions: permessage-deflate; client_max_window_bits`。
- 实际握手由 `tokio-tungstenite`/`tungstenite` 执行，TLS 走 rustls；HTTP/SSE 走 `reqwest` + rustls。
- 首个文本帧是 `response.create`，payload 由 `CodexResponsesRequest` 序列化而来。
- WebSocket opening 会移除 `content-type` 和 `accept`，因为这两个是 HTTP/SSE 请求头，不属于 WS opening 业务头。
- 连接池 key 为 `base_url + account_id + conversation_id`，conversation id 优先取 `prompt_cache_key`，其次 `client_conversation_id`，再其次 `previous_response_id`。
- 官方二进制有 60 分钟连接限制文案；当前项目连接池默认 `max_age_ms = 3_300_000`（55 分钟），低于官方限制。连接在 acquire、put 和后台 maintenance sweep 时都会按 `max_age` 回收，避免复用接近官方 60 分钟上限的 socket。

当前 WebSocket fallback：

- `WebSocketPreferred` 发生连接/transport 类错误时允许 fallback 到 HTTP SSE。
- `WebSocketRequired` 不 fallback。
- 上游明确 HTTP 状态错误不会被当作普通 WS 连接错误降级。

这对应官方二进制里的：

- `websocket connection is unavailable`
- `Falling back from WebSockets to HTTPS transport.`
- `try_switch_fallback_transport`

### 请求头

当前实现集中在 `src/upstream/transport/client.rs` 和 `src/upstream/transport/headers.rs`。不同请求类型不是同一组头：

| 请求类型 | 路径 | 传输 | 当前头部规则 |
| --- | --- | --- | --- |
| Responses HTTP SSE | `/codex/responses` | `reqwest` POST | 指纹基础头 + `content-type: application/json` + 可选 `cookie` + `accept: text/event-stream` + `openai-beta: responses_websockets=2026-02-06` + `x-openai-internal-codex-residency: us` + `x-client-request-id` + `session-id` + `thread-id` + Codex 上下文头 |
| Responses WebSocket | `/codex/responses` | `tokio-tungstenite` GET upgrade + 首帧 `response.create` | 先按 Responses HTTP 头构造，再移除 `content-type` 和 `accept`，追加标准 WebSocket upgrade 头和 `sec-websocket-extensions` |
| Compact | `/codex/responses/compact` | `reqwest` POST | 指纹基础头 + `content-type` + 可选 `cookie` + `openai-beta` + `x-openai-internal-codex-residency` + 新生成的 `x-client-request-id` + 可选 `x-codex-installation-id` |
| Models/探测 | `/codex/models`、`/models`、`/sentinel/chat-requirements` | `reqwest` GET | 指纹基础头 + 可选 `cookie` + `accept: application/json` + `accept-encoding: gzip, deflate` + 可选 `x-codex-installation-id` |
| Usage/配额 | `/api/codex/usage`、`/wham/usage`、`/codex/usage` | `reqwest` GET | `user-agent` + `authorization` + `originator` + 可选 `chatgpt-account-id` + 可选 `cookie` + `accept: application/json` |

指纹基础头：

- `authorization`
- `chatgpt-account-id`
- `originator`
- `user-agent`
- `sec-ch-ua`
- fingerprint 默认 header 和 header order，例如 `accept-encoding`、`accept-language`、`sec-ch-ua-mobile`、`sec-ch-ua-platform`、`sec-fetch-*`

Responses/WS 的 Codex 上下文头：

- `x-client-request-id`：主 Responses 请求优先使用 conversation identity，没有 session 时使用本次 `request_id`；Compact 每次新生成 UUID。
- `x-codex-installation-id`：有 installation id 时写入。
- `session-id` / `thread-id`：仅主 Responses 请求有 conversation identity 时写入；当前运行态 WS 抓包里二者同值。
- `x-codex-window-id`：由请求 `codex_window_id` 或会话身份推导。
- `x-codex-turn-state`：显式续链或历史恢复时携带。
- `x-codex-turn-metadata`、`x-codex-beta-features`、`x-responsesapi-include-timing-metrics`、`version`、`x-codex-parent-thread-id`：客户端 body/header 透传后写入。
- `x-openai-subagent`：只接受 `review`、`compact`、`memory_consolidation`、`collab_spawn`。

Compact 只保留 `x-client-request-id` 和 `x-codex-installation-id`，不带会话/线程头。

WebSocket payload metadata：

- 当前项目会在 Responses payload `client_metadata` 写入 `session_id`、`thread_id`、`x-codex-installation-id`、`x-codex-window-id`、`x-codex-turn-metadata`、`x-codex-parent-thread-id`。
- 当前项目会在每次 WebSocket 请求发出前额外向 `client_metadata` 写 `x-codex-ws-stream-request-start-ms`，值为当前 Unix 毫秒字符串；HTTP SSE 不写该字段。

Desktop 二进制还出现 `x-oai-attestation`、`requestAttestation`、`ws_request_header_traceparent`、`ws_request_header_tracestate`、`x-openai-internal-codex-responses-lite`。这些字段不能按固定值补齐，需要满足各自触发条件。

当前需要继续取证的头部差距：

- `x-oai-attestation` 来自 app-server 协议的 `attestation/generate` 能力。Desktop main 在 macOS arm64 会加载 `native/devicecheck.node` 生成 token；公开 `openai/codex` README 只作为辅助资料，说明 client 返回 `{ "token": "v1.<opaque>" }` 后，上游 envelope 形如 `{ "v": 1, "s": 0, "t": "v1.<opaque>" }`，没有 opt-in client 时省略该头。
- `traceparent` / `tracestate` 是 OTEL 链路追踪头，官方只确认 WebSocket request header 字段存在，未确认是否每个请求都带。
- `x-openai-internal-codex-responses-lite` 与 `ws_request_header_x_openai_internal_codex_responses_lite` 在 Desktop 二进制中能确认存在；公开 `openai/codex` 辅助源码显示它们依赖模型信息 `use_responses_lite`，但不能单独作为 Desktop 实现依据。当前项目不应无条件添加。

### 请求携带的数据

当前请求体：`src/upstream/protocol/responses.rs`

核心字段：

- `model`
- `instructions`
- `input`
- `stream`
- `store`
- `reasoning`
- `tools`
- `tool_choice`
- `parallel_tool_calls`
- `text`
- `service_tier`
- `previous_response_id`
- `prompt_cache_key`
- `include`
- `client_metadata`

不会序列化但影响 header/transport 的字段：

- `use_websocket`
- `force_http_sse`
- `turn_state`
- `turn_metadata`
- `beta_features`
- `version`
- `include_timing_metrics`
- `codex_window_id`
- `parent_thread_id`

OpenAI 入口支持从 body 或 header 读取这些 Codex 上下文字段：`src/proxy/openai/responses.rs`。

### SSE / WS 响应处理

当前实现：

- `src/upstream/protocol/responses.rs` 将 SSE 收集为 `Completed`、`Failed`、`MissingCompleted`、`Empty`。
- `response.output_text.delta` 会累积文本。
- `response.completed` 是成功边界。
- `error` 或 `response.failed` 是失败边界。
- 缺少 `response.completed` 时返回 `MissingCompleted`。

WebSocket 事件：

- `src/upstream/protocol/websocket.rs` 会跳过内部 `codex.rate_limits` 事件，不直接下发给 OpenAI 客户端。
- 对 `response.completed`、`response.created`、`response.output_text.delta` 和各类 output item 做官方形状校验。
- `src/upstream/protocol/events.rs` 解析 `codex.rate_limits`，转换成 `x-codex-primary-*`、`x-codex-secondary-*`、`x-codex-code-review-*` 等 rate-limit header pairs。

调度层：

- `src/proxy/dispatch/responses.rs` 在非流式和流式完成时记录 usage、event log、transport、rateLimitHeaders。
- live stream 结束后再次按 `response.completed` 判定完成状态；未完成会记录 `missing_completed`。
- live WebSocket 捕获到的 rate-limit 更新和 turn-state 更新会在 finalize 阶段合并。
- WebSocket 请求 payload 会在发送前写入 `client_metadata.x-codex-ws-stream-request-start-ms`，用于和 Desktop 二进制里的 `stamp_ws_stream_request_start_ms` 证据对齐。

### 风控和恢复

官方证据：

- `ChatGptCloudflareCookieStore`
- `set_cookies`
- `cookies`
- `auth_manager_missing`
- `recovery_succeeded`
- `recovery_failed_permanent`
- `recovery_failed_transient`

当前实现：

- `src/proxy/dispatch/cloudflare.rs` 从 `chatgpt.com` cookie store 为 `/codex/responses`、`/codex/responses/compact`、`/codex/usage` 拼接 cookie。
- 捕获上游 `set-cookie` 并按账号持久化。
- 403 body 命中 `cf-mitigated`、`cf-chl-bypass`、`_cf_chl`、`cf_chl`、`attention required`、`just a moment` 时进入 Cloudflare challenge cooldown。
- 404 空 body 作为 path-block，立即删除账号 cookie，多次后禁用账号。
- 调度层会在 Cloudflare challenge/path-block 后换号或返回明确错误。
- 官方二进制出现 `This request has been flagged for possible cybersecurity risk.`。当前项目把 `cyber_policy` 归为请求内容/安全策略类 400，不作为 quota、Cloudflare 或账号封禁信号。
- 官方错误枚举还包含 `context_window_exceeded`、`usage_limit_exceeded`、`server_overloaded`、`cyber_policy`、`response_stream_connection_failed`、`response_stream_disconnected` 等 CodexErrorInfo 类别。当前项目按 HTTP status、SSE failure code/message 和 WS wrapped error 分类；其中 WS `cyber_policy`/`invalid_prompt`/`context_length_exceeded`/`invalid_request` 会映射 400，SSE `cyber_policy`/`invalid_prompt`/`context_window*`/`bad_request` 也会映射 400。

### 额度和限流

官方 webview 使用 `/wham/usage` 获取额度；原生二进制有 `parse_rate_limit_event` 和 `codex.rate_limits`。

当前实现：

- `/wham/usage` 响应归一化在 `src/upstream/accounts/quota/mod.rs`。
- `rate_limit.primary_window` / `secondary_window` -> `snapshots[].primary` / `snapshots[].secondary`
- `additional_rate_limits[]` -> 独立 `snapshots[]`，`source = "additional"`，保留 `limit_name` / `metered_feature`
- `spend_control.individual_limit` -> `monthly_limit`，优先作为月限额展示；没有该字段时才从 core 30d window 生成 `monthly_limit`
- WebSocket 内部 `codex.rate_limits` 事件由 `src/upstream/protocol/events.rs` 解析。

本项目对齐官方后的展示约定：

- `5小时限额` 和 `周限额` 归入 `shortTerm` group，可以同时出现。
- `月限额` 归入 `monthly` group，优先来自 `spend_control.individual_limit`，再回退到 30d rate-limit window。
- additional/model 限额不覆盖 core 限额，作为独立 window 行保留；如果同一个 additional limit 同时有 5h/7d，也会拆成两行。
- WebSocket `codex.rate_limits` 被动刷新只携带短周期窗口时，输出同样的 `snapshots` 结构，并保留已有 `monthly_limit`、`credits`、`spend_control` 和 additional snapshots，避免把 `/wham/usage` 拿到的月限额或 additional 限额冲掉。

#### 配额刷新触发

当前项目里配额刷新不是单一入口：

- 管理端主动刷新：`GET /api/admin/accounts/quota?id=...` 会调用 `fetch_usage`，拉取实时 usage 后归一化并写回 `quota_json`。
- 管理端健康检查：`health_check_accounts` 也会调用 `fetch_usage`，但只判断 alive/dead，不写 quota。
- 账号导入：导入时如果缺 plan/quota 或导入数据要求补全，会调用 `fetch_usage` 补齐。
- 业务请求前验证：如果账号 `quota_verify_required = true`，调度层在真正发 `/codex/responses` 前会先调用 `fetch_usage`。如果仍然 `limit_reached`，会释放该账号、换号，最多尝试 5 个账号。
- 业务响应后被动刷新：成功响应的 HTTP/WS rate-limit header，以及 WS 内部 `codex.rate_limits` 事件，会同步到账号池和持久化 quota。
- 业务失败触发锁定：上游 HTTP 429 会写入 `quota_cooldown_until`；HTTP 402 或 SSE `quota_exceeded` / `insufficient_quota` 会把账号置为 `QuotaExhausted`。
- 后台定时刷新：`QuotaRefreshTask` 默认按配置间隔扫描 active 且 `quota_limit_reached || quota_verify_required` 的账号；单个账号至少间隔 30 分钟，同批请求之间默认错峰 3 秒。

Usage 请求路径按 base URL 选择：

- base URL 带 `/backend-api` 时：优先 `/wham/usage`，再 `/codex/usage`。
- 其他 base URL：优先 `/api/codex/usage`，再 `/codex/usage`。

调用方差异：

- 业务请求前验证、管理端主动刷新、管理端健康检查和后台定时刷新都会按账号为 `/codex/usage` 读取 Cloudflare cookie，并传给 usage 请求头。
- 账号导入补全 usage 发生在新账号入库前，通常没有可复用的本地 cookie；当前仍按无 cookie usage 请求处理。
- `usage_request_headers` 当前支持可选 `cookie`，但是否携带取决于调用方传入。

### 真实数据验证

2026-06-25 使用本地 `.runtime` active free 账号调用 `GET /api/admin/accounts/quota`，经项目真实解密、请求官方 `/wham/usage`、归一化并落库。

官方实时响应形态：

- `plan_type = free`
- `rate_limit.primary_window.limit_window_seconds = 2592000`
- `rate_limit.primary_window.used_percent = 6`
- `rate_limit.secondary_window = null`
- `spend_control.individual_limit = null`
- `credits.has_credits = false`

项目归一化结果：

- `snapshots[0].source = "core"`
- `snapshots[0].primary.window_minutes = 43200`
- `snapshots[0].secondary = null`
- `monthly_limit.source = "rate_limit"`
- 管理端账号列表返回 `quota.windows[0].group = "monthly"`、`labelDisplay = "月限额"`、`windowSeconds = 2592000`

2026-06-26 使用临时 18080 本地实例复用 `.runtime` 数据，登录管理端后调用 `GET /api/admin/accounts/quota?id=acct_fbed55cb574b4aee81760d0e8ac84ba6`。请求真实官方 `/wham/usage` 返回 200，`quota_fetched_at` 从 `2026-06-25T16:32:12.697845388+00:00` 更新为 `2026-06-25T16:53:08.898420541+00:00`。

本地 `account_cookies` 当前没有该 active 账号的 `cf_clearance`，所以这次真实链路只验证 usage 主刷新路径未被 cookie 改动破坏；cookie 头携带行为由 `tests/admin/accounts/quota.rs` 和 `tests/upstream/accounts/quota_refresh.rs` 的 route/mock 请求头测试覆盖。

结论：本地真实 free 账号确认为 30d/月限额单独展示。当前本地没有 active 的 5h+7d 账号可实时验证组合返回；该组合路径由官方 bundle 证据和项目测试覆盖。

## 当前差距

- `x-oai-attestation` 已确认是 host/app-server 动态协作生成，不是常量；当前项目没有 Desktop host 的 DeviceCheck/signals 链路，不实现该头。
- `traceparent` / `tracestate` 只确认官方 WebSocket request header 字段存在，当前项目未实现。
- `x-openai-internal-codex-responses-lite` 和 `ws_request_header_x_openai_internal_codex_responses_lite` 已在 Desktop 二进制中确认存在；触发条件仍以 Desktop 为准，公开 `openai/codex` 只能辅助判断，当前项目暂不实现。
- 原生二进制不能在当前 Linux 环境执行，无法做动态断点或真实 app-server 调用链追踪。
- 官方包里没有 source map，JS bundle 为压缩产物；函数名和局部变量名不能视为稳定 API。
- WebSocket 降级策略目前按现有项目实现理解为连接/transport 错误可降级，显式上游状态错误不降级；官方二进制字符串能证明有降级路径，但不能单独证明完整条件矩阵。
- 官方有 WebSocket 60 分钟连接限制文案；当前项目用 55 分钟默认 pool TTL 主动规避该上限，但没有实现从上游错误中动态调整 TTL。
- 官方有 `cyber_policy` 风控语义；当前项目已将 WS/SSE 中的 `cyber_policy` 归入 400 类请求失败，但没有创建单独的账号状态或轮换策略。

## 实现建议

短期不建议伪造 `x-oai-attestation`。如果后续真实链路出现必须 attestation 的 403/401，再单独做动态取证或抓官方运行态请求，确认该头来源后实现。

当前项目更应该保持：

- `responses_websockets=2026-02-06` beta 头。
- `x-client-request-id`、`x-codex-installation-id`、`x-codex-window-id`、`x-codex-turn-state`、`x-codex-turn-metadata` 的透传。
- `session-id`、`thread-id` 请求头，以及 payload `client_metadata.session_id`、`client_metadata.thread_id`。
- `previous_response_id` 强制 WebSocket。
- `WebSocketPreferred` 可降级 HTTP SSE，`WebSocketRequired` 不降级。
- `codex.rate_limits` 内部事件不下发给客户端，但用于刷新账号限额状态。
- Cloudflare cookie 捕获、重放、challenge cooldown、path-block 清 cookie/禁用账号。
- usage 请求头保持轻量：不要把 Responses 专用 beta/residency/turn header 全部塞到 `/api/codex/usage`。
