# OpenAI Codex 原版链路对齐审计

日期：2026-06-14

范围：对比原版 Node.js 仓库 `/home/zyy/桌面/Codes/codex-proxy`，审计 Rust 版的安全链路、请求链路、响应链路。这里的“代理”不包含 IP 代理、VPN、`HttpsProxyAgent`、本地代理探测等网络代理能力，这部分明确不移植。

## 结论

当前 Rust 版已补齐关键安全模拟字段、默认 WebSocket 上游请求、稳定 `prompt_cache_key`、`previous_response_id` 账号亲和性、WS 首帧错误分类、WS 实时边收边转发、账号 + conversation 维度的 WS 连接复用。IP 代理/VPN 能力明确不移植。

## 1. 安全链路和指纹

原版证据：
- `src/fingerprint/manager.ts` 构造 `Authorization`、`ChatGPT-Account-Id`、`originator`、`User-Agent`、`sec-ch-ua`、默认浏览器头和 header order。
- `src/proxy/codex-api.ts` 在 WS/HTTP 两条路径都追加 `OpenAI-Beta`、`x-openai-internal-codex-residency`、`x-client-request-id`、`x-codex-installation-id`、`session_id`、`x-codex-window-id`、turn metadata、parent thread、`x-openai-subagent`。
- `buildCodexClientMetadata()` 会把 installation/window/turn/parent 写入 body 的 `client_metadata`，不是只发 header。

Rust 状态：
- 到位：`src/codex/gateway/transport/headers.rs` 构造账号、residency、request id、UA、sec-ch、默认 header 和顺序。
- 到位：`src/codex/gateway/transport/client.rs` 将 installation/window/turn/parent/subagent 同步到 header 和 `client_metadata`。
- 到位：`src/codex/gateway/installation.rs` 兼容读取 `~/.codex/installation_id`，否则落库生成。
- 到位：`src/runtime/bootstrap.rs` 从数据库加载自动更新指纹，避免实际请求继续使用硬编码默认值。
- 边界：Rust 使用 `reqwest + rustls` 传输，未声称复制原版 native transport 的完整 Chrome TLS 指纹。IP 代理/VPN 相关能力不属于移植范围。

## 2. 请求链路

原版证据：
- `src/routes/responses.ts` 默认 `codexRequest.useWebSocket = true`。
- `stable-conversation-key.ts` 用 model、instructions、首条 user 文本派生稳定 conversation key。
- `proxy-session-context.ts` / `proxy-request-preparation.ts` 确保 `prompt_cache_key` 总是写入上游请求。
- `session-affinity.ts` 记录 `response_id -> entryId/conversationId/turnState`，后续 `previous_response_id` 优先回到原账号。

Rust 状态：
- 到位：普通 `/v1/responses` 默认走 WebSocket，上游失败且是传输/不支持类错误才降级 HTTP SSE。
- 到位：`src/codex/gateway/identity.rs` 已增加稳定 conversation key 派生，空请求使用 UUID 防止所有空请求共享同一缓存链。
- 到位：请求发上游前会确保 `prompt_cache_key`，再按账号作用域派生成 `cp_*`，并同步到 `session_id` / `x-client-request-id`。
- 到位：`src/codex/serving/dispatch/affinity.rs` 已接入 runtime；成功 response 会记录 affinity，显式 `previous_response_id` 会优先选择原账号并继承原 conversation identity。

## 3. 响应链路

原版证据：
- `ws-transport.ts` 打开 WS 后返回 `ReadableStream`，把每个 JSON frame 包装成 SSE 事件。
- 首帧 `error` / `response.failed` 如属于可轮换账号错误，会转成 `CodexApiError` 状态码，让统一重试/换账号逻辑接管。
- 未知错误码继续作为 SSE 透传，避免误轮换。

Rust 状态：
- 到位：WS frame 会转成 SSE 文本，客户端仍看到 `text/event-stream`。
- 到位：WS 握手状态码、`retry-after`、cookie、rate-limit header 会被捕获。
- 到位：WS 首帧 `usage_limit_reached`、`rate_limit_exceeded`、`quota_exhausted`、`token_expired`、`banned`、`previous_response_not_found` 等会映射成对应 HTTP 状态，不走 HTTP SSE fallback。
- 到位：WS streaming path 在首个非错误 frame 到达后立即返回客户端响应，后续 frame 边收边转成 SSE；usage、affinity、event log 在 body stream 收尾阶段记录。

## 4. WebSocket 连接池

原版证据：
- `ws-pool.ts` 说明 upstream LB 以物理 WS 连接做后端粘滞，连接复用能保持 prompt cache。
- 默认 `maxAgeMs = 3_300_000`，即 55 分钟。
- key 以账号和 conversation 维度复用，一条 WS 严格单 in-flight，busy/cap/dead 时旁路 one-shot。

Rust 状态：
- 到位：`src/codex/gateway/transport/websocket.rs` 增加进程内 `CodexWebSocketPool`，默认 55 分钟过期。
- 到位：池 key 由 base URL、本地账号和派生后的 conversation/prompt cache key 组成；活跃连接从池中移出，terminal frame 后再放回，确保单连接严格单 in-flight。
- 到位：复用连接如果在首帧前发现已被对端关闭，会丢弃后用 fresh WS 重试一次；账号状态被标记限流/封禁/额度耗尽时会驱逐该账号的空闲 WS。

## 5. 非目标：IP 代理/VPN

原版包含本地代理探测、`proxy_url`、`HttpsProxyAgent`、账号代理池等能力。Rust 版不移植这些 IP 代理/VPN 功能，也不新增 `proxy_url` 配置。安全链路只关注 Codex/OpenAI 请求自身的身份、指纹、Cookie、header、body metadata 和 WS 语义。

## 当前验证点

- `tests/codex_gateway/websocket.rs` 覆盖默认 WS、forced HTTP SSE、WS 安全字段、握手错误、首帧错误分类、HTTP SSE 降级。
- `tests/codex_serving/responses_websocket.rs` 覆盖默认 WS 上游、派生 `prompt_cache_key`、`client_metadata` 安全字段、`previous_response_id` 原账号亲和性、WS 首帧实时返回、同 conversation 连接复用、限流/刷新路径。
- `tests/codex_serving/responses_http_sse.rs` 覆盖显式 HTTP SSE 兼容路径和 body/header 安全字段。
