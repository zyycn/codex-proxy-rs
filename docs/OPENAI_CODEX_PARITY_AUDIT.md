# OpenAI Codex 原版链路对齐审计

日期：2026-06-14

范围：对比原版 Node.js 仓库 `/home/zyy/桌面/Codes/codex-proxy`，审计 Rust 版的安全链路、请求链路、响应链路。这里的“代理”不包含 IP 代理、VPN、`HttpsProxyAgent`、本地代理探测等网络代理能力，这部分明确不移植。

## 结论

当前 Rust 版已补齐关键安全模拟字段、默认 WebSocket 上游请求、稳定 `prompt_cache_key`、`previous_response_id` 账号亲和性、WS 首帧错误分类、WS 实时边收边转发、账号 + conversation 维度的 WS 连接复用。IP 代理/VPN 能力明确不移植。

本轮已补齐请求发送细节和账号生命周期差异：自动 Cookie 捕获白名单、`Max-Age` 优先级、账号刷新/管理状态变化后的 WS pool 驱逐、`request_interval_ms` 发送前 stagger、`least_used` 的 reset 缺失排序。仍未达到 100% 对齐的项目是 Responses implicit resume/reasoning replay 状态机。

## 1. 安全链路和指纹

原版证据：
- `src/fingerprint/manager.ts` 构造 `Authorization`、`ChatGPT-Account-Id`、`originator`、`User-Agent`、`sec-ch-ua`、默认浏览器头和 header order。
- `src/proxy/codex-api.ts` 在 WS/HTTP 两条路径都追加 `OpenAI-Beta`、`x-openai-internal-codex-residency`、`x-client-request-id`、`x-codex-installation-id`、`session_id`、`x-codex-window-id`、turn metadata、parent thread、`x-openai-subagent`。
- `buildCodexClientMetadata()` 会把 installation/window/turn/parent 写入 body 的 `client_metadata`，不是只发 header。

Rust 状态：
- 到位：`src/codex/gateway/transport/headers.rs` 构造账号、residency、request id、UA、sec-ch、默认 header 和顺序。
- 到位：`src/codex/gateway/transport/http_client.rs` 将 installation/window/turn/parent/subagent 同步到 header 和 `client_metadata`。
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
- 到位：`src/codex/gateway/transport/websocket.rs` 增加进程内 `CodexWebSocketPool`，默认 55 分钟过期，默认每 25 秒维护空闲连接。
- 到位：池 key 由 base URL、本地账号和派生后的 conversation/prompt cache key 组成；同 key busy、账号连接数达到上限或 pool disabled/shutdown 时旁路 one-shot，避免排队和死锁。
- 到位：复用连接如果在首帧前发现已被对端关闭，会丢弃后用 fresh one-shot WS 重试一次；mid-stream 提前关闭会向客户端 stream 报错，不做重试。
- 到位：空闲连接支持 keepalive ping/pong 和 liveness timeout，周期 sweep 会关闭过期、无响应或被 shutdown/evict 的连接；`websocket_connection_limit_reached` 会按连接级 fatal 错误剔除。
- 到位：被动 rate-limit、上游 fallback 状态变更、refresh 成功/失败、管理端 disable/delete/batch lifecycle 都会驱逐该账号的 WS；`AccountService` 和 `CodexUpstreamService` 使用同一个应用级共享 pool。

## 5. Cookie 捕获语义

原版证据：
- `src/proxy/cookie-jar.ts` 的 `CAPTURABLE_COOKIE_NAMES` 只包含 `cf_clearance`。
- 自动捕获 `Set-Cookie` 会跳过 `__cf_bm`；管理端手动 `set()` 不受白名单限制。
- `Max-Age` 会优先于 `Expires`，`Max-Age<=0` 代表立即过期。

Rust 状态：
- 到位：`CookieRepository::capture_set_cookie()` 自动捕获只允许 `cf_clearance`。
- 到位：`parse_set_cookie()` 支持 `Max-Age`，并让它优先于 `Expires`。
- 到位：手动 `set_cookie_header()` 可以持久化任意 Cookie，这一点已保留。

## 6. 请求发送节流

原版证据：
- `routes/shared/proxy-handler.ts` 获取账号后调用 `staggerIfNeeded(acquired.prevSlotMs)`。
- fallback 到备用账号后同样会在发送上游请求前 stagger。

Rust 状态：
- 到位：`AccountPool::acquire_with()` 返回 `previous_slot_at`。
- 到位：`CodexUpstreamService::acquire_account*()` 保留完整 `AcquiredAccount`，普通请求、stream 请求和 fallback 账号请求在发送上游前按 `auth.request_interval_ms` sleep。

## 7. 账号调度排序

原版证据：
- `least_used` 的优先级是 quota-exhausted 降权、两侧都有 `window_reset_at` 时比较 reset、`request_count`、LRU。

Rust 状态：
- 到位：quota 降权、两侧都有 `window_reset_at` 时比较 reset、`request_count` 和 LRU 已实现。
- 到位：任一侧缺少 reset 时继续比较 `request_count`，不再把 `Some(window_reset_at)` 永远排在 `None` 前面。

## 8. Responses 续接状态机

原版证据：
- shared handler 包含 implicit resume、strip-and-retry、reasoning replay 的状态机。

Rust 状态：
- 到位：显式 `previous_response_id` 会强制 WebSocket 并优先回到原账号。
- 未对齐：implicit resume、strip-and-retry、reasoning replay 尚未完整迁移，应单独设计和测试。

## 9. 非目标：IP 代理/VPN

原版包含本地代理探测、`proxy_url`、`HttpsProxyAgent`、账号代理池等能力。Rust 版不移植这些 IP 代理/VPN 功能，也不新增 `proxy_url` 配置。安全链路只关注 Codex/OpenAI 请求自身的身份、指纹、Cookie、header、body metadata 和 WS 语义。

## 当前验证点

- `tests/codex_gateway/websocket.rs` 覆盖默认 WS、forced HTTP SSE、WS 安全字段、握手错误、首帧错误分类、HTTP SSE 降级。
- `tests/codex_serving/responses_websocket.rs` 覆盖默认 WS 上游、派生 `prompt_cache_key`、`client_metadata` 安全字段、`previous_response_id` 原账号亲和性、WS 首帧实时返回、同 conversation 连接复用、限流/刷新路径。
- `tests/codex_serving/responses_http_sse.rs` 覆盖显式 HTTP SSE 兼容路径和 body/header 安全字段。
