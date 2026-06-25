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
- 请求头会携带 `Authorization`、`ChatGPT-Account-Id`、`originator`、`User-Agent`。
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

### Webview 额度逻辑

相关文件：

- `webview/assets/thread-context-inputs-B6tQCr7t.js`
- `webview/assets/use-rate-limit-DfBgdYGx.js`
- `webview/assets/rate-limit-rows-B5kE6YOz.js`
- `webview/assets/usage-settings-BMxGTDii.js`
- `webview/assets/codex-api-AUWtp9Y7.js`

确认点：

- `thread-context-inputs` 查询 `/wham/usage`，query key 为 `rate-limit-status`，失败 401/403/404 时返回 `null`。
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

当前 WebSocket fallback：

- `WebSocketPreferred` 发生连接/transport 类错误时允许 fallback 到 HTTP SSE。
- `WebSocketRequired` 不 fallback。
- 上游明确 HTTP 状态错误不会被当作普通 WS 连接错误降级。

这对应官方二进制里的：

- `websocket connection is unavailable`
- `Falling back from WebSockets to HTTPS transport.`
- `try_switch_fallback_transport`

### 请求头

当前实现：`src/upstream/transport/client.rs`

基础 header 来自 fingerprint：

- `authorization`
- `chatgpt-account-id`
- `originator`
- `user-agent`
- `sec-ch-ua`
- fingerprint 默认 header 和 header order

Responses 请求额外写入：

- `content-type: application/json`
- `cookie`
- `accept: text/event-stream`
- `openai-beta: responses_websockets=2026-02-06`
- `x-openai-internal-codex-residency: us`
- `x-client-request-id`
- `x-codex-installation-id`
- `session_id`
- `x-codex-window-id`
- `x-codex-turn-state`
- `x-codex-turn-metadata`
- `x-codex-beta-features`
- `x-responsesapi-include-timing-metrics`
- `version`
- `x-codex-parent-thread-id`
- 条件写入 `x-openai-subagent`

官方二进制还出现 `x-oai-attestation`。当前项目没有构造该头；仅凭静态字符串无法确认其生成条件、签名来源和是否每次请求都必需，不能用固定值或伪造值补齐。

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

结论：本地真实 free 账号确认为 30d/月限额单独展示。当前本地没有 active 的 5h+7d 账号可实时验证组合返回；该组合路径由官方 bundle 证据和项目测试覆盖。

## 当前差距

- `x-oai-attestation` 只确认官方二进制里存在，未确认生成算法、触发条件、签名材料和失败行为；当前项目不实现该头。
- 原生二进制不能在当前 Linux 环境执行，无法做动态断点或真实 app-server 调用链追踪。
- 官方包里没有 source map，JS bundle 为压缩产物；函数名和局部变量名不能视为稳定 API。
- WebSocket 降级策略目前按现有项目实现理解为连接/transport 错误可降级，显式上游状态错误不降级；官方二进制字符串能证明有降级路径，但不能单独证明完整条件矩阵。

## 实现建议

短期不建议伪造 `x-oai-attestation`。如果后续真实链路出现必须 attestation 的 403/401，再单独做动态取证或抓官方运行态请求，确认该头来源后实现。

当前项目更应该保持：

- `responses_websockets=2026-02-06` beta 头。
- `x-client-request-id`、`x-codex-installation-id`、`x-codex-window-id`、`x-codex-turn-state`、`x-codex-turn-metadata` 的透传。
- `previous_response_id` 强制 WebSocket。
- `WebSocketPreferred` 可降级 HTTP SSE，`WebSocketRequired` 不降级。
- `codex.rate_limits` 内部事件不下发给客户端，但用于刷新账号限额状态。
- Cloudflare cookie 捕获、重放、challenge cooldown、path-block 清 cookie/禁用账号。
