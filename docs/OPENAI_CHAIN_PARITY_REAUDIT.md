# OpenAI 链路 100% 一致性复审计账本

日期：2026-06-14

目标：逐项复核 Rust 版 `codex-proxy-rs` 与原版 Node.js `codex-proxy` 的 OpenAI/Codex 相关链路是否达到行为级 100% 一致。每完成一个链路点，就把证据、结论和缺口写回本文档，避免审计结论只存在于上下文。

原版仓库：`/home/zyy/桌面/Codes/codex-proxy`

Rust 仓库：`/home/zyy/Codes/codex-proxy-rs`

明确非目标：原版的 IP 代理、VPN、本地代理探测、`HttpsProxyAgent`、账号代理池，以及所有非 OpenAI upstream family（例如 Anthropic、Gemini、Ollama、OpenRouter、Sub2API）不属于本项目移植范围，不纳入 100% 一致性判断。

## 审计方法

1. 先读原版实现，再读 Rust 实现，最后查测试是否覆盖该行为。
2. 每个条目必须记录“原版证据”“Rust 证据”“结论”“缺口/后续动作”。
3. 不能因为已有旧文档写过就直接沿用结论；旧文档只能作为索引，最终结论以本轮读到的代码为准。
4. 结论分为：
   - `已对齐`：行为、边界条件和测试证据均匹配。
   - `部分对齐`：主路径一致，但边界、状态机或测试覆盖不足。
   - `未对齐`：原版存在明确行为，Rust 缺失或语义不同。
   - `非目标`：用户明确要求不移植的能力。

## 总体结论

本轮 10 个 OpenAI/Codex 链路点已全部复核并写入账本。结论：当前不能声明与原版 100% 一致；除“代码证据索引与审计边界”外，其余链路均为 `部分对齐`。

最高优先级未对齐项：
- 安全模拟仍缺 TLS 指纹证明；WS permessage-deflate offer、协商响应和服务端压缩 frame 解码已补齐测试与实现。
- `/v1/responses` 请求构造仍存在非 `/v1` alias route、`use_websocket:false` 本地扩展等差异；`/v1/responses/review` 强制 review subagent 和 `/v1/responses/compact` 已补齐。
- response 转换和错误恢复仍缺最终错误外壳对齐；tuple schema request conversion/reconvert、streaming premature close 合成、reasoning replay、implicit resume failure restore、`response.failed`/`error` 的 429/402/403 分类、账号 fallback、`previous_response_not_found`/unanswered function call strip-and-retry、5xx same-account retry、CF path-block 404、model unsupported fallback、401 token invalid fallback 已补齐。
- session affinity 的显式续链、implicit resume 主路径、implicit resume failure restore、reasoning replay cache、function call continuation 预检、variant_hash 基础算法、variant identity 和 OpenAI chat `user` conversation identity 已接入。
- rate-limit/usage/cookie 持久化主路径已接近，但 image_generation usage、dirty quota verification、失败请求持久计数和 cookie 过期清理仍有差异。

明确不纳入缺口：IP 代理、VPN、本地代理探测、`HttpsProxyAgent`、账号代理池。

明确不移植：非 OpenAI upstream family 的兼容路由、translator、cache usage hint 估算及其 `clientConversationId` 来源不属于本项目目标；后续只按 OpenAI `/v1/responses`、`/v1/responses/compact` 与 `/v1/chat/completions` 链路核对。

## 审计进度

| 序号 | 链路点 | 状态 | 结论 |
| --- | --- | --- | --- |
| 1 | 代码证据索引与审计边界 | 已完成 | 已建立 |
| 2 | 安全指纹与默认 headers | 已完成 | 部分对齐 |
| 3 | OAuth/device/refresh 链路 | 已完成 | 部分对齐 |
| 4 | `/v1/responses` 请求构造与 prompt cache identity | 已完成 | 部分对齐 |
| 5 | HTTP SSE 上游请求链路 | 已完成 | 部分对齐 |
| 6 | WebSocket 上游链路与连接池 | 已完成 | 部分对齐 |
| 7 | response/SSE/WS frame 转换链路 | 已完成 | 部分对齐 |
| 8 | fallback、retry、错误分类 | 已完成 | 部分对齐 |
| 9 | rate-limit、usage、quota、cookie 持久化 | 已完成 | 部分对齐 |
| 10 | session affinity、implicit resume、reasoning replay | 已完成 | 部分对齐 |

## 1. 代码证据索引与审计边界

原版证据：
- OpenAI/Codex 请求核心集中在 `src/proxy/codex-api.ts`、`src/proxy/responses-upstream.ts`、`src/proxy/ws-transport.ts`、`src/proxy/ws-pool.ts`、`src/routes/responses.ts`、`src/routes/shared/*`。
- 账号、OAuth、调度和亲和性集中在 `src/auth/*`。
- 指纹、安全 headers、安装标识和 Cookie 集中在 `src/fingerprint/manager.ts`、`src/proxy/installation-id.ts`、`src/proxy/cookie-jar.ts`。
- 原版还包含 IP 代理相关文件：`src/proxy/proxy-pool.ts`、`src/tls/proxy.ts`、`src/routes/proxies.ts` 等。

Rust 证据：
- OpenAI/Codex 请求核心集中在 `src/codex/gateway/transport/*`、`src/codex/serving/dispatch/*`、`src/codex/serving/responses.rs`、`src/codex/serving/chat.rs`。
- 账号、OAuth、调度和亲和性集中在 `src/codex/accounts/*`、`src/codex/gateway/oauth/*`、`src/codex/serving/dispatch/affinity.rs`。
- 指纹、安全 headers、安装标识和 Cookie 集中在 `src/codex/gateway/fingerprint/*`、`src/codex/gateway/transport/headers.rs`、`src/codex/gateway/installation_id.rs`、`src/codex/accounts/cookies/*`。
- Rust 项目当前没有引入 IP 代理/VPN 作为 OpenAI 上游能力，这符合用户明确要求。

结论：已建立本轮复审计索引。IP 代理/VPN 作为明确非目标，不参与一致性缺口计算。

缺口/后续动作：继续逐项核对安全指纹、请求链路、响应链路和状态机。不得在全部条目完成前声明 100% 一致。

## 2. 安全指纹与默认 headers

原版证据：
- `src/fingerprint/manager.ts:63-139` 从 config/fingerprint 构造 `Authorization`、`ChatGPT-Account-Id`、`originator`、`User-Agent`、`sec-ch-ua`、默认浏览器 headers，并按 `fingerprint.header_order` 重排。
- `config/fingerprint.yaml` 明确 header 顺序和默认 headers；`config/default.yaml` 提供 `originator`、`app_version`、`build_number`、`platform`、`arch`、`chromium_version`。
- `src/proxy/codex-api.ts:320-358` 的 WebSocket 请求追加 `OpenAI-Beta`、`x-openai-internal-codex-residency`、`x-client-request-id`、`x-codex-installation-id`、`session_id`、`x-codex-window-id`、turn/context headers、`x-openai-subagent`，并把 installation/window/turn/parent 写入 `client_metadata`。
- `src/proxy/codex-api.ts:375-414` 的 HTTP SSE 请求追加同一组 Responses 上游 headers，并把安全上下文同步到 body。
- `src/proxy/installation-id.ts:45-66` 的 installation ID 查找顺序是 `~/.codex/installation_id`、项目 data dir、生成并持久化。
- `src/proxy/codex-usage.ts:17-29` 和 `src/proxy/codex-models.ts:11-31` 的 usage/models 辅助请求只在调用方传入的基础 headers 上追加 `Accept: application/json`，非 impersonate transport 下把 `Accept-Encoding` 降到 `gzip, deflate`；不追加 Responses 专属的 `OpenAI-Beta` 和 `Content-Type`。
- `src/auth/health-check.ts:1-110` 的账号健康检查只走 OAuth refresh，不打 `chatgpt.com` Codex API，文件头说明这是为了避免触发风险检测。

Rust 证据：
- `src/codex/gateway/fingerprint/model.rs:17-95` 内置默认 Codex Desktop 指纹，包含 originator、版本、平台、UA 模板、默认 headers、header_order。
- `src/codex/gateway/transport/headers.rs:5-74` 构造认证、账号、originator、residency、request id、turn state、UA、sec-ch 和默认 headers。
- `src/codex/gateway/transport/http_client.rs:452-520` 在请求 headers 中注入 cookie、installation、session、turn metadata、beta features、timing metrics、version、window、parent、`Content-Type`、`OpenAI-Beta`、`x-openai-subagent`。
- `src/codex/gateway/transport/http_client.rs:523-600` 会把 `session_id` 写回 `prompt_cache_key`，并把 installation/window/turn/parent 写入 `client_metadata`。
- `src/codex/gateway/transport/websocket/mod.rs:419-440` WebSocket upgrade 复用同一份 HeaderMap，所以 WS 和 HTTP SSE 主链路共享安全 headers。
- `src/codex/gateway/installation_id.rs:21-79` 实现了与原版一致的 installation ID 查找和持久化顺序。
- `src/runtime/state.rs` 会把应用启动时加载的 fingerprint 传入 `/v1/responses`、`/v1/chat/completions`、显式配额查询和模型刷新使用的 upstream client。
- 本轮修正后，`src/codex/accounts/service/health.rs` 的账号健康检查已回到原版 OAuth refresh-only 语义，不再访问 Codex usage endpoint；`tests/admin/accounts/cookies_quota.rs` 覆盖 health check 不触碰 `/api/codex/usage`。
- 本轮修正后，`src/codex/accounts/service/mod.rs` 持有 AppState 注入的 fingerprint，`fetch_account_usage()` 使用该 fingerprint 创建 Codex client；`tests/admin/accounts/cookies_quota.rs` 覆盖显式配额查询的 `User-Agent` 与 `sec-ch-ua` 来自注入 fingerprint。
- 本轮修正后，`src/codex/models/service.rs` 持有 AppState 注入的 fingerprint，模型刷新使用该 fingerprint 创建 Codex client；`tests/admin/models_route.rs` 覆盖 `/codex/models` 请求的 `User-Agent` 与 `sec-ch-ua` 来自注入 fingerprint。
- 本轮修正后，`src/codex/serving/diagnostics.rs` 持有 AppState 注入的 fingerprint，`/debug/upstream` 探测 `/codex/models` 时使用同一份运行时 fingerprint；`tests/codex_serving/diagnostics_route.rs` 覆盖诊断 JSON 与真实上游探测请求头均使用注入 fingerprint。
- 本轮修正后，`src/codex/gateway/transport/http_client.rs` 已拆分 HTTP SSE、WebSocket、usage/models 辅助请求 header profile：WS 不携带 `Content-Type`/`Accept: text/event-stream`；usage/models 只携带基础身份 header、Cookie、`Accept: application/json` 和 `Accept-Encoding: gzip, deflate`。
- 本轮修正后，`src/codex/gateway/fingerprint/model.rs` 与 `src/codex/gateway/fingerprint/repository.rs` 的 `user_agent_template` 使用原版 `{version}` 占位符，而不是 Rust 临时的 `{app_version}`；`Fingerprint::user_agent()` 也按原版替换 `{version}`。`tests/codex_gateway/headers.rs::fingerprint_user_agent_should_reflect_updated_version_fields` 先以原版模板失败，再修复通过，覆盖数据库/auto-update fingerprint 发往上游时不会泄漏未替换占位符。
- 本轮修正后，`src/codex/gateway/fingerprint/model.rs` 的默认 `header_order` 回到原版 `config/fingerprint.yaml` 形状，不再包含 `session_id`、`x-codex-window-id`、turn metadata、beta features、timing、version、parent 等动态追加头；`tests/codex_gateway/headers.rs::fingerprint_default_header_order_should_match_original_config` 覆盖该配置。
- 本轮修正后，`src/codex/gateway/transport/http_client.rs` 的 HTTP SSE 与 compact header 构造按原版真实顺序执行：基础指纹头先按 `header_order` 排序，Cookie 先注入，随后按 `codex-api.ts` 赋值顺序追加 `Accept`/`OpenAI-Beta`/residency/request id/installation/session/window/turn/context 等动态头；`tests/codex_gateway/http_client.rs::codex_backend_client_should_send_http_sse_headers_in_fingerprint_order` 和 `codex_backend_client_should_send_compact_headers_in_fingerprint_order` 通过 raw TCP 请求验证 wire order。
- `tests/codex_gateway/websocket.rs:273-375` 覆盖 WS 安全 body 字段、`prompt_cache_key`、`client_metadata`、`x-client-request-id`、`x-openai-subagent`，并断言 WS handshake 不带 `Content-Type`/`Accept`。
- `tests/codex_gateway/http_client.rs` 覆盖 usage/models 辅助请求不携带 `Content-Type`、`OpenAI-Beta`、residency、request id、installation id 和 session id。
- `tests/codex_serving/responses_http_sse.rs:403-513` 覆盖 HTTP SSE 安全 headers、派生 identity、`client_metadata` 和 installation UUID。

结论：部分对齐。

已对齐：
- `/v1/responses` 主链路的字段级安全上下文已基本对齐：authorization、account id、originator、residency、request id/session id、installation id、window id、turn metadata、parent thread、subagent、body `client_metadata` 均已进入 HTTP SSE 和 WS 上游路径。
- installation ID 查找顺序和持久化行为与原版一致。
- `User-Agent` 模板占位符已回到原版 `{version}`，auto-update fingerprint 与默认 fingerprint 生成的上游 header 不再使用 Rust 自造的 `{app_version}` 语义。
- 应用主请求链路会使用启动时加载的最新 fingerprint，而不是永远使用硬编码默认值。
- WebSocket handshake headers 已按原版拆分，不再携带 HTTP SSE/body 语义的 `Content-Type` 和 `Accept: text/event-stream`。
- usage/models 辅助请求 headers 已按原版拆分，不再复用 Responses 主链路的 `Content-Type`、`OpenAI-Beta`、residency、request id、installation id、session id。
- HTTP SSE 与 compact 的真实 wire header 相对顺序已按原版构造顺序覆盖，包括默认 `header_order`、Cookie 注入位置和动态安全头追加顺序。
- 账号健康检查已按原版安全边界改为只走 OAuth refresh，不再访问 `chatgpt.com` Codex API。
- 显式配额查询与模型刷新已复用 AppState 加载的 fingerprint，不再各自创建默认 fingerprint。
- 本地诊断的 `/debug/upstream` 探测请求已复用 AppState 加载的 fingerprint，不再创建独立默认 fingerprint。

未完全对齐：
- Rust 当前没有测试证明真实传输层 TLS/HTTP 指纹与原版 native transport 行为完全一致。IP 代理/VPN 明确非目标，但 TLS/header 指纹仍属于安全模拟链路，不能在未验证时声明 100%。

缺口/后续动作：
- 若目标是安全模拟 100%，需要继续处理 TLS 指纹真实上游验证；TLS 是否要追到 native transport 级别需要单独决策。

## 3. OAuth/device/refresh 链路

原版证据：
- `src/auth/oauth-pkce.ts:101-128` 构造 PKCE 授权 URL，参数包含 `response_type=code`、Codex `client_id`、`redirect_uri`、`scope=openid profile email offline_access`、`code_challenge_method=S256`、`id_token_add_organizations=true`、`codex_cli_simplified_flow=true`、`originator=codex_cli_rs`。
- `src/auth/oauth-pkce.ts:244-276` 固定 OAuth callback 为 `http://localhost:1455/auth/callback`，注释说明 OpenAI 只白名单这个地址。
- `src/auth/oauth-pkce.ts:133-163` 授权码换 token 使用 `application/x-www-form-urlencoded`，字段为 `grant_type=authorization_code`、`client_id`、`code`、`redirect_uri`、`code_verifier`。
- `src/auth/oauth-pkce.ts:171-240` refresh 使用 form body：`grant_type=refresh_token`、`client_id`、`refresh_token`；保护一次性 RT，只有确认请求没到服务器的 pre-flight 错误才继续 fallback。
- `src/auth/oauth-pkce.ts:476-532` device flow 使用 `/oauth/device/code` 和 token endpoint，字段与 RFC device code flow 一致。
- `src/auth/refresh-scheduler.ts:21-32` permanent error 分为 banned 与 expired，且 `PERMANENT_THRESHOLD = 2`，连续两次才标记状态。
- `src/auth/refresh-scheduler.ts:174-192` 全局 refresh 并发由 `auth.refresh_concurrency` 限制。
- `src/auth/refresh-lock.ts:1-70` 每账号 refresh 有跨进程锁，锁 5 分钟过期。
- `src/auth/health-check.ts:1-110` 账号健康检查只调用 OAuth refresh，不访问 Codex API。

Rust 证据：
- 本轮修正后，`src/config/types.rs` 和 `config.yaml` 已加入 `oauth_client_id`、`oauth_auth_endpoint`、`oauth_token_endpoint`；`src/codex/gateway/oauth/client.rs` 通过 `OAuthConfig::from_auth_config()` 从 `AuthConfig` 派生 OAuth 参数，device code endpoint 保持原版固定值。
- 本轮修正后，`src/runtime/bootstrap.rs` 的 `OpenAiOAuthRefresher` 使用配置派生的 `OAuthConfig`，不再硬编码 token endpoint 或 client id。
- 本轮修正后，`src/admin/session/service.rs` 的 PKCE login 使用 AppState 注入的 `OAuthConfig`，`tests/admin/accounts/oauth.rs` 覆盖自定义 auth endpoint 与 client id 会进入授权 URL。
- `src/codex/gateway/oauth/client.rs:363-387` 构造 PKCE 授权 URL，参数集合与原版基本一致。
- 本轮修正后，`src/codex/gateway/oauth/client.rs` 固定 callback 为 `http://localhost:1455/auth/callback`，与原版白名单路径一致。
- `src/admin/api/router.rs` 只注册 `/auth/callback`，不注册 `/auth/openai/callback` 或 `/api/admin/auth/callback`。
- `tests/admin/accounts/oauth.rs` 覆盖授权 URL、code relay、直接 callback route 和非原版 path 404；`tests/codex_gateway/oauth_refresh.rs` 覆盖 authorization code exchange body 中的 redirect URI。
- `src/codex/gateway/oauth/client.rs:214-319` 授权码、device code、device poll、refresh token 请求均为 form body；`tests/codex_gateway/oauth_refresh.rs:10-124` 覆盖了请求字段。
- `src/codex/tasks/token_refresh.rs:36-50` 定义了与原版一致的重试次数、退避基数、恢复延迟、permanent threshold 和 banned/expired 错误集合。
- `src/codex/tasks/token_refresh.rs:52-65` 用 `Semaphore` 实现全局 refresh 并发限制。
- `src/codex/accounts/service/refresh.rs:45-68` 用 SQLite refresh lease 实现跨进程/跨实例防重，TTL 5 分钟。
- 本轮修正后，`src/codex/accounts/service/refresh.rs` 提供 `probe_account_refresh()`，用于执行 OAuth refresh 探测但失败不直接落库状态；admin/manual refresh 仍通过 `refresh_account()` 应用结果。
- 本轮修正后，`src/codex/tasks/token_refresh.rs` 后台调度器调用 `probe_account_refresh()`，只有 `AccountProbeOutcome::Alive` 视为成功，`Dead` 会进入重试与 permanent threshold 判断；第二次 permanent failure 后才写入 expired/banned。
- `src/codex/tasks/token_refresh.rs` 单元测试 `do_refresh_inner_should_mark_expired_only_after_second_permanent_failure` 覆盖连续两次 `InvalidGrant` 前数据库状态仍为 `active`，第二次失败后才写入 `expired`。
- `src/codex/gateway/oauth/client.rs:402-423` 把 `quota`、`banned`、`token_revoked` 等映射为结构化 `RefreshFailure`。
- 本轮修正后，`src/codex/accounts/service/health.rs` 健康检查通过 `refresh_account()` 调用 OAuth refresh token 链路，不再访问 Codex usage endpoint；`tests/admin/accounts/cookies_quota.rs` 断言 `/api/codex/usage` 调用次数为 0。

结论：部分对齐。

已对齐：
- OAuth 授权码交换、refresh token 交换、device code request、device poll 的 form body 字段与原版主路径一致。
- OAuth client id、authorize endpoint、token endpoint 已进入配置面，并由生产 bootstrap、后台 refresh client、admin PKCE login 共用同一份配置。
- PKCE redirect URI 和 callback route 已改为原版固定白名单路径 `http://localhost:1455/auth/callback`。
- PKCE verifier/challenge、state、5 分钟 session TTL、并发交换保护和 completed session 语义基本一致。
- 全局 refresh 并发限制已经存在。
- 每账号 refresh 防重已经存在，Rust 用 SQLite lease 替代文件锁，语义上更适合当前数据库架构。
- refresh 成功时保留未轮换的旧 refresh_token，这一点符合原版保护逻辑。
- 后台 refresh 调度器已不再把 `Dead` probe 误判为成功；scheduler 路径 permanent failure 的状态写入已延后到连续两次失败。
- 账号健康检查已回到原版 OAuth refresh-only 安全边界。

未完全对齐：
- admin/manual refresh 仍在第一次结构化 permanent failure 后立即更新状态；原版 health check 也会立即 mark expired，但后台 scheduler 已按原版 threshold=2。若要所有手动路径也完全同构，需要重新定义 admin 手动刷新语义。
- 原版 refresh 对一次性 RT 的网络错误非常谨慎，只有请求确认未到服务器时才 fallback/retry；Rust 当前 `reqwest` refresh client 不能区分 pre-flight 与 mid-flight 失败，且 runtime 调度器错误处理路径与原版不等价。

缺口/后续动作：
- 增加 transport error 的后台恢复调度测试，覆盖非 permanent 错误耗尽重试后进入 10 分钟恢复调度。

## 4. `/v1/responses` 请求构造与 prompt cache identity

原版证据：
- `src/routes/responses.ts:75-87` 对非对象 JSON body 返回 400，错误码为 `invalid_request`。
- `src/routes/responses.ts:117-202` 构造 `CodexResponsesRequest`，字段包括 `model`、`instructions`、sanitized `input`、`stream: true`、`store: false`、默认 `useWebSocket = true`、`previous_response_id`、`prompt_cache_key`、`include`、`reasoning`、`service_tier`、`tools`、`tool_choice`、`parallel_tool_calls`、`text.format`。
- `src/routes/responses.ts:130-142` 的 `x-openai-subagent` 优先级是 review route 强制 `review`，否则 header 优先，再读 `client_metadata`；`sanitizeClientMetadata()` 只保留字符串值，并移除非法 subagent。
- `src/proxy/reasoning-input-sanitizer.ts:1-75` 对 `input` 中的 `reasoning` 和 `compaction` 项做白名单清洗：reasoning 需要非空 `id` 和 `summary`，只保留合法 `status`、`encrypted_content`、`content`；compaction 需要非空 `encrypted_content`。
- `src/routes/shared/proxy-session-helpers.ts:41-60` 的 prompt cache identity 优先级是显式 `prompt_cache_key`、`clientConversationId`、稳定派生 key、随机 UUID。
- `src/routes/shared/stable-conversation-key.ts:1-43` 稳定派生 key 使用 `model + "\0" + instructions前2000字符 + "\0" + 首条 user 文本`，并会剥离开头的 `<system-reminder>...</system-reminder>`。
- `src/routes/shared/proxy-request-preparation.ts:17-28` 在进入上游前强制写入 `prompt_cache_key`；当存在 `reasoning` 且 `include` 为空时，才设置 `["reasoning.encrypted_content"]`。
- `src/routes/responses.ts:268-274` 还注册 `/v1/responses/review`、`/responses`、`/responses/review`、`/v1/responses/compact`。

Rust 证据：
- `src/codex/serving/http/responses.rs:13-30` 路由层直接读取 raw `Bytes` 并交给 service。
- 本轮修正后，`src/codex/serving/responses.rs` 先按 JSON object 解析 `/v1/responses` body；非法 JSON 或非 object body 直接返回 400 `invalid_request`，不再访问上游。
- `src/codex/serving/responses.rs` 构造 `CodexResponsesRequest` 时按原版宽松读取字段：类型不匹配的字段被忽略，`input` 会先清洗 reasoning/compaction 项，`client_metadata` 只保留字符串值。
- `src/codex/serving/responses.rs` 的 subagent 优先级是 header 优先，再读已清洗的 `client_metadata`，没有 review route 强制逻辑。
- `src/codex/serving/responses.rs:105-112` 的 reasoning effort 与 service tier 主路径和原版一致：显式 body 优先，其次模型后缀，最后配置默认值；`fast` 会在 dispatch 层规范成 `priority`。
- 本轮修正后，`ensure_reasoning_include()` 仅在存在 `reasoning` 且 `include` 为空时设置 `["reasoning.encrypted_content"]`。
- `src/codex/gateway/conversation_identity.rs:58-90` 的稳定 prompt cache key 派生规则与原版 `stable-conversation-key.ts` 对齐，包含 instructions 前 2000 字符、首条 user 文本、system-reminder 剥离、无锚点时随机 UUID。
- `src/codex/serving/dispatch/mod.rs:143-164` 对显式 `previous_response_id` 会继承已记录 conversation id 和 turn state；否则调用 `ensure_prompt_cache_key()`。
- 本轮修正后，`src/codex/serving/http/router.rs` 注册 `/v1/responses/review`，`src/codex/serving/http/responses.rs` 在该路由强制注入 `x-openai-subagent: review`，复用普通 Responses 发送链路。
- 本轮修正后，`src/codex/serving/http/router.rs` 注册 `/v1/responses/compact`，`src/codex/serving/responses.rs` 解析并清洗 compact body，`src/codex/gateway/transport/http_client.rs` 固定 `POST /codex/responses/compact`，使用 JSON response 链路。
- `src/codex/serving/http/router.rs` 仍不注册原版的 `/responses`、`/responses/review` 非 `/v1` alias。
- `tests/codex_serving/responses_http_sse.rs:403-562` 覆盖了字段转发、service tier 规范化、metadata/body 安全字段和默认 reasoning include。
- `tests/codex_serving/responses_websocket.rs:47-110` 覆盖默认 WebSocket 上游、派生 `prompt_cache_key` 和安全 metadata；`tests/codex_gateway/websocket.rs:78-85` 覆盖本地 `use_websocket` 不序列化到上游。

结论：部分对齐。

已对齐：
- `/v1/responses` 主路径字段覆盖基本完整：模型解析、instructions、input、reasoning、service tier、tools、tool choice、parallel tool calls、text format、prompt cache key、include、client metadata、previous response id、Codex context headers 均有对应实现。
- 稳定 prompt cache key 派生规则与原版一致，包含 system-reminder 剥离和随机 UUID 兜底。
- `use_websocket`/`force_http_sse` 是本地传输控制字段，不会序列化给上游，这一点有测试覆盖。
- 非法 JSON 或非 object body 已返回 400 `invalid_request`，并由 `tests/codex_serving/responses_http_sse.rs` 覆盖“不访问上游”。
- `input` reasoning/compaction 白名单清洗已对齐原版，并由 `v1_responses_should_sanitize_reasoning_and_compaction_input_before_upstream` 覆盖。
- `client_metadata` 已只保留字符串值，再注入合法 `x-openai-subagent`。
- `reasoning.encrypted_content` include 策略已改为原版语义：只在 `include` 为空时补齐。
- OpenAI chat 的 `clientConversationId` 来源已由 `/v1/chat/completions` 的 `user` 字段接入；`/v1/responses` 原生入口没有该字段，原版 shared handler 中非 OpenAI 分支的额外来源属于非目标。
- `/v1/responses/review` 已按原版强制 `x-openai-subagent=review` 进入上游 header 和 body `client_metadata`；`tests/codex_serving/responses_http_sse.rs::v1_responses_review_route_should_force_review_subagent_upstream` 覆盖客户端未显式传 subagent 时的上游请求。
- `/v1/responses/compact` 已按原版走非 streaming JSON compact 链路：只转发 `model`、sanitized `input`、`instructions`、非空 `tools`、`parallel_tool_calls`、仅含 `effort/summary` 的 `reasoning` 和 `text.format`，不转发 `stream`、`store`、`prompt_cache_key`。`tests/codex_serving/responses_http_sse.rs::v1_responses_compact_should_post_json_to_codex_compact_upstream` 先以 404 失败，再修复通过。

未完全对齐：
- 路由表面仍有差异。Rust 没有 `/responses`、`/responses/review` 非 `/v1` alias。非 `/v1` alias 属于旧入口兼容，按当前“只要 OpenAI 规范链路”的边界暂不移植。
- 客户端传输开关不一致。原版 `/v1/responses` 总是内部设置 `useWebSocket = true`，不从 body 读取 `use_websocket:false`；Rust 支持 `use_websocket:false` 强制 HTTP SSE。这是本项目明确保留的本地传输控制扩展，不作为上游 OpenAI 字段发送。

缺口/后续动作：
- `use_websocket:false` 已明确作为本项目本地扩展保留；后续审计只需确保它不进入上游 OpenAI 请求体。
- 非 `/v1` alias 若不需要公开，应正式标为非目标；非 OpenAI/Codex adapter 分支不进入本项目实现。

## 5. HTTP SSE 上游请求链路

原版证据：
- `src/proxy/codex-api.ts:365-418` 的 HTTP SSE 请求固定 `POST {baseUrl}/codex/responses`，headers 使用 `buildHeadersWithContentType()` 后追加 `Accept: text/event-stream`、`OpenAI-Beta: responses_websockets=2026-02-06`、`x-openai-internal-codex-residency: us`、`x-client-request-id`、`x-codex-installation-id`、`session_id`、`x-codex-window-id`、turn/context headers、`x-openai-subagent`。
- `src/proxy/codex-api.ts:389-416` 会从 body 中剥离 `previous_response_id`、`useWebSocket`、`turnState`、`turnMetadata`、`betaFeatures`、`version`、`includeTimingMetrics`、`codexWindowId`、`parentThreadId`，再写入上游 `prompt_cache_key` 和 `client_metadata`。
- `src/proxy/codex-api.ts:107-128` 使用 `entryId ?? accountId ?? "anonymous"` 对客户端 `prompt_cache_key` 与 `x-codex-window-id` 做账号作用域哈希，得到 `cp_...` 与 `cw_...`。
- `src/proxy/codex-api.ts:193-207` 从 CookieJar 注入 Cookie，并在 HTTP SSE 响应后 `captureCookies()` 保存 `Set-Cookie`。
- `src/proxy/codex-api.ts:420-449` 对非 2xx 读取最多 1 MiB 错误体并抛 `CodexApiError(status, body, headers)`。
- `src/proxy/codex-api.ts:457-508` 的 compact 请求固定 `POST {baseUrl}/codex/responses/compact`，headers 使用 `buildHeadersWithContentType()` 后追加 `OpenAI-Beta: responses_websockets=2026-02-06`、`x-openai-internal-codex-residency: us`、随机 `x-client-request-id`、`x-codex-installation-id`；不设置 `Accept: text/event-stream`，读取完整 JSON body。
- `src/routes/responses-compact.ts:44-104` 构造 `CodexCompactRequest`，只包含 `model`、sanitized `input`、`instructions`、`tools`、`parallel_tool_calls`、`reasoning.effort/summary`、`text.format`，不包含 `stream`、`store`、`prompt_cache_key`。
- `src/tls/native-transport.ts:75-117` HTTP stream post 接受 `AbortSignal`，并通过 native binding 的 `httpPostStream` 返回 `ReadableStream` 与 `setCookieHeaders`。
- `native/src/lib.rs:31-64` 原版 native addon 缓存 reqwest client，key 为 `(proxy_url, force_http11)`，并设置 `use_rustls_tls()`、`pool_max_idle_per_host(4)`、`tcp_keepalive(30s)`、可选 `http1_only()` 和可选 proxy。
- `native/Cargo.toml:9-24` 原版 native transport 使用 `reqwest 0.12.28`、`rustls = 0.23.36`，并启用 `rustls-tls-native-roots`、`stream`、`gzip`、`brotli`、`zstd`、`deflate`、`http2`、`socks`。
- `src/routes/shared/proxy-stagger.ts:1-25` 在同账号存在 previous slot 时，按 `request_interval_ms` 乘以 ±30% jitter 计算目标间隔，再扣除 elapsed 后等待。

Rust 证据：
- 本轮修正后，`src/codex/gateway/transport/http_client.rs` 的 HTTP SSE 非 2xx 错误体最多读取 1 MiB 后返回 `CodexClientError::Upstream`，成功 SSE body 仍完整读取。
- `src/codex/gateway/transport/http_client.rs:428-438` 实际发送使用 `reqwest::Client::post(url).headers(headers).json(request).send()`。
- `src/codex/gateway/transport/http_client.rs:452-520` headers 会注入认证、account id、originator、request id/session id、Cookie、installation id、session_id、turn metadata、beta features、timing metrics、version、window、parent、`Content-Type: application/json`、`OpenAI-Beta`。
- `src/codex/gateway/transport/http_client.rs:523-600` 会把 `session_id` 写入 `prompt_cache_key`，并把 installation/window/turn metadata/parent 写入 body `client_metadata`；metadata 只保留字符串值。
- `src/codex/serving/dispatch/mod.rs:555-585` 和 `590-660` 分别为非流式与流式 HTTP SSE 请求准备 Cookie、账号作用域 conversation/window identity、installation id、reqwest client 与 `CodexRequestContext`。
- `src/codex/serving/dispatch/mod.rs:338-476` 流式 HTTP SSE 将上游 bytes stream 直接透传给客户端，并每 15 秒补 `: ping` heartbeat；结束后由 `StreamAudit` 记录 usage、affinity 和 rate-limit。
- 本轮修正后，`src/codex/gateway/transport/types.rs` 增加 `CodexCompactRequest`，字段严格限制为原版 compact request；`src/codex/gateway/transport/http_client.rs` 增加 `create_compact_response()`，固定发送 `POST /codex/responses/compact`，使用 compact 专用 headers，不携带 `Accept: text/event-stream`。
- 本轮修正后，`src/codex/serving/dispatch/mod.rs` 增加 compact 上游发送入口，复用账号 Cookie、installation id、rate-limit header 解析和 5xx 同账户重试；不生成 Responses 会话专属的 `session_id`、`x-codex-window-id`、turn/context headers。
- 本轮修正后，`src/codex/serving/responses.rs` 的 `/v1/responses/compact` body 构造只保留原版 compact 字段，并复用 reasoning/compaction input 白名单清洗。
- 本轮修正后，`tests/codex_gateway/http_client.rs` 通过 raw TCP 请求验证 HTTP SSE 和 compact 请求的业务 header 相对顺序：基础指纹头、Cookie 注入位置、HTTP SSE `Accept`、`OpenAI-Beta`、residency、request/session id、installation、window、turn/context headers 与原版 `codex-api.ts` 赋值顺序一致。
- 本轮修正后，`src/codex/serving/dispatch/mod.rs` 的 `stagger_request_with_deps()` 会在同账号 previous slot 存在时按 `request_interval_ms` 的 ±30% jitter 计算等待时间；`tests/codex_serving/responses_http_sse.rs` 覆盖同账号并发请求发送上游前会等待，`jitter_request_interval_ms_with_factor_should_match_original_bounds` 覆盖 0.7/1.3 边界。
- 本轮修正后，`src/codex/gateway/transport/http_client.rs` 的 Rust reqwest client 按 `force_http11` 缓存复用，使用 `reqwest = 0.12.28`、`rustls = 0.23.36`，启用 rustls/native roots、`pool_max_idle_per_host(4)`、`tcp_keepalive(30s)`、gzip/brotli/zstd/deflate，并可选 `http1_only()`；同时显式 `.no_proxy()`。
- `tests/codex_gateway/http_client.rs:13-78` 覆盖 HTTP SSE POST、desktop headers、Cookie 注入、`Set-Cookie` 捕获、turn state 捕获和 usage 提取。
- `tests/codex_serving/responses_http_sse.rs:403-513` 覆盖 `/v1/responses` HTTP SSE 路径的 context headers、account-scoped prompt cache key、metadata 与 service tier。

结论：部分对齐。

已对齐：
- HTTP SSE 主请求的 URL、method、SSE Accept、Content-Type、OpenAI-Beta、residency、request/session id、installation id、window id、turn/context headers、subagent、Cookie 注入、Set-Cookie 捕获、rate-limit/turn-state 捕获均有对应实现。
- 上游 body 会使用账号作用域 `prompt_cache_key`，并将安全链路字段同步到 `client_metadata`。
- 非 2xx 错误体读取上限已对齐原版 1 MiB，并由 `codex_backend_client_should_cap_non_success_error_body_at_one_mib` 覆盖。
- reqwest client 已按 `force_http11` 缓存复用，并对齐原版 `pool_max_idle_per_host(4)` 与 `tcp_keepalive(30s)`；`build_reqwest_client_should_reuse_cached_connection_pool` 覆盖跨 builder 调用的连接池复用。
- `request_interval_ms` 已生效并按原版 ±30% jitter 计算同账号请求间隔。
- Rust 与原版 native transport 使用同一 reqwest/rustls 主版本组合，且都支持 `force_http11`。
- `/v1/responses/compact` 已对齐原版 JSON 上游链路：URL/method、核心 headers、Cookie/Set-Cookie、非 SSE Accept、body 字段白名单、reasoning/text 清洗和 JSON 响应透传均有实现；`v1_responses_compact_should_post_json_to_codex_compact_upstream` 覆盖该路径。
- HTTP SSE 与 compact 请求的真实 wire header 相对顺序已由 raw TCP 测试覆盖，不再只是字段级断言。
- IP 代理/VPN/proxy config/HttpsProxyAgent 是用户明确排除的非目标。Rust `.no_proxy()` 与原版 proxy 支持的差异不计入本项目 100% 一致性缺口，但需要保留边界说明。

未完全对齐：
- 原版 native transport 对 HTTP stream post 使用 JS `AbortSignal` 主动中断；Rust 依赖 response/body stream drop 和 reqwest future 生命周期，中断语义接近但没有对应测试证明客户端断开时上游请求一定及时终止。
- 原版 native client builder 没有显式 `.no_proxy()`，而是通过 proxy 参数控制；Rust 显式 `.no_proxy()` 符合非代理目标，但也意味着环境代理不会生效。此项按非目标处理，不算 OpenAI 链路缺口。

缺口/后续动作：
- 增加 HTTP SSE golden test：断言上游 body 不包含本地控制字段，header/body 中 account-scoped identity、metadata、subagent、turn/context 字段与原版一致。
- 若要宣称安全模拟 100%，还需要处理 WS handshake header order、header casing、TLS 指纹和客户端断开 abort 语义；当前 HTTP SSE/compact order 已有 wire-level 证据。

## 6. WebSocket 上游链路与连接池

原版证据：
- `src/routes/responses.ts:125-131` `/v1/responses` 默认设置 `codexRequest.useWebSocket = true`；`src/proxy/codex-api.ts:276-307` WebSocket 失败时，若带 `previous_response_id` 则不能降级 HTTP SSE，否则可降级 HTTP SSE。
- `src/proxy/codex-api.ts:313-360` WebSocket URL 为 `{baseUrl}/codex/responses` 的 `wss://`/`ws://` 版本，handshake headers 使用 `buildHeaders()` 加 `OpenAI-Beta`、residency、request/session id、installation/window/turn/context/subagent。
- `src/proxy/ws-transport.ts:155-171` 只向 `ws` constructor 传 `{ headers }`；原版依赖 `ws@8.19.0` 默认 opening handshake。抓包证据显示默认顺序包含 `Host`、`Connection`、`Upgrade`、`Sec-WebSocket-Version`、`Sec-WebSocket-Extensions: permessage-deflate; client_max_window_bits`、`Sec-WebSocket-Key`，随后按传入 headers 对象顺序输出业务 headers。
- `src/proxy/codex-api.ts:335-359` `response.create` frame 包含 `model`、`instructions`、`input`、`store:false`、`stream:true`、`previous_response_id`、`reasoning`、`tools`、`tool_choice ?? "auto"`、`parallel_tool_calls ?? true`、`text`、normalized `service_tier`、account-scoped `prompt_cache_key`、`include`、`client_metadata`。
- `src/proxy/ws-transport.ts:238-287` 优先使用 pool；busy/cap/dead/disabled/no_key 时 bypass 到 one-shot；复用连接 stale failure 时 one-shot 重试一次。
- `src/proxy/ws-transport.ts:310-316` one-shot WebSocket handshake 有 20 秒 open timeout。
- `src/proxy/ws-transport.ts:411-445` `codex.rate_limits` 内部事件只回调 rate-limit，不转发给客户端；首个可见错误 frame 会被分类成 `CodexApiError`；terminal event 后关闭 stream。
- `src/proxy/ws-pool.ts:535-539` 默认连接池配置为 `enabled=true`、`maxAgeMs=3_300_000`、`maxPerAccount=8`。
- `src/proxy/ws-pool.ts:156-244` pooled WS 每 25 秒 ping，默认 liveness timeout 为 ping interval 的 2.5 倍；busy 时不 ping。
- `src/proxy/ws-pool.ts:566-704` pool 以 `${entryId}:${conversationId}` 为 key，严格单请求 in-flight；busy 时 bypass；超过账号 cap 时 bypass；GC 清理 dead/expired idle；`evictByEntryId()` 会对账号状态变化做级联驱逐。
- `src/routes/shared/proxy-ws-context.ts:21-43` 只有在 `useWebSocket` 且存在稳定 `conversationId` 时才构造 pool context；显式 `previous_response_id` 续链即使 variantHash 变化，也沿用同一 upstream response chain。
- `src/config-schema.ts:172-181` 暴露 `ws_pool.enabled`、`ws_pool.max_age_ms`、`ws_pool.max_per_account` 配置。

Rust 证据：
- `src/codex/gateway/transport/websocket/mod.rs:143-154` 默认 `WebSocketPreferred`，带 `previous_response_id` 时 `WebSocketRequired`，`force_http_sse` 时走 HTTP SSE。
- `src/codex/gateway/transport/http_client.rs:143-179` 普通 create_response 先尝试 WebSocket；若是 preferred 且错误允许降级，则回落 HTTP SSE；`previous_response_id` required 不降级。
- `src/codex/gateway/transport/websocket/mod.rs:405-450` WebSocket URL 构造与原版一致，`https://` 转 `wss://`，`http://` 转 `ws://`，路径为 `/codex/responses`。
- `src/codex/gateway/transport/websocket/codec.rs:14-54` `response.create` frame 字段与原版主路径一致：`tool_choice` 默认 `auto`，`parallel_tool_calls` 默认 `true`，并带 `previous_response_id`、reasoning/tools/text/service_tier/prompt_cache_key/include/client_metadata。
- `src/codex/gateway/transport/http_client.rs:523-600` 在进入 WS 前把 account-scoped `session_id` 写入 `prompt_cache_key`，并写入 body `client_metadata`。
- 本轮修正后，WS handshake header profile 已独立于 HTTP SSE profile，不再携带 `Content-Type` 与 `Accept: text/event-stream`；`tests/codex_gateway/websocket.rs` 已覆盖该行为。
- 本轮修正后，`src/codex/gateway/transport/websocket/opening.rs` 不再使用 `tokio-tungstenite` 默认 `HeaderMap` opening 序列化，而是显式生成原版 `ws@8.19.0` opening request：`Sec-WebSocket-Extensions` 位于 `Sec-WebSocket-Key` 前，业务 headers 按原版对象插入顺序输出，并恢复 `Authorization`、`ChatGPT-Account-Id`、`User-Agent`、`Accept-Encoding`、`Accept-Language`、`OpenAI-Beta`、`Cookie` 等原版大小写。
- `tests/codex_gateway/websocket.rs::websocket_handshake_should_offer_original_permessage_deflate_extension` 通过 raw TCP 断言 WS opening request 的关键 header 顺序、大小写和 permessage-deflate offer 与原版抓包一致。
- 本轮修正后，`src/codex/gateway/transport/websocket/deflate.rs` 在服务端接受 `Sec-WebSocket-Extensions: permessage-deflate` 时，会在底层流上解码 server-to-client RSV1 text/binary frame，再交给 `tungstenite` 处理；未协商时保持原始流直通。`tests/codex_gateway/websocket.rs::websocket_should_decode_permessage_deflate_response_frame_when_server_accepts_extension` 先以 `NonZeroReservedBits` 失败，再修复通过，覆盖服务端接受扩展并返回压缩 `response.completed` frame。
- `src/codex/gateway/transport/websocket/mod.rs:230-328` pooled/reused WS stale failure 会重试一次 one-shot；内部 `codex.rate_limits` 会转为 rate-limit headers；terminal event 转为 SSE chunk。
- 本轮修正后，`src/codex/gateway/transport/websocket/mod.rs` 的 one-shot WebSocket handshake 使用 20 秒 open timeout；`connect_websocket_with_timeout_should_fail_when_handshake_stalls` 覆盖握手停滞时返回 timeout。
- `src/codex/gateway/transport/websocket/pool.rs:58-76` 默认 `max_age=55min`、`max_per_account=8`、`maintenance_interval=25s`、`ping_interval=25s`、`ping_timeout=5s`、`liveness_timeout=62.5s`。
- `src/codex/gateway/transport/websocket/pool.rs:129-170` pool 以 `(base_url, account_id, conversation_id)` 为 key；busy/checking bypass；超过账号 cap bypass；expired idle 关闭。
- `src/codex/gateway/transport/websocket/pool.rs:225-319` maintenance 会 ping idle connection，并按 max age/liveness timeout 清理。
- `src/codex/accounts/service/refresh.rs:170-195`、`src/codex/serving/dispatch/limits.rs:8-73`、`src/codex/serving/dispatch/fallback.rs:350-470`、`src/codex/accounts/service/lifecycle.rs:67-77` 在账号 refresh、quota cooldown、fallback 状态变化、账号删除等路径会驱逐账号 WS。
- `src/config/types.rs`、`config.yaml`、`src/runtime/state.rs` 已暴露并接入 `ws_pool.enabled/max_age_ms/max_per_account`，默认值与原版 `enabled=true/max_age_ms=3_300_000/max_per_account=8` 一致。
- `src/codex/gateway/transport/websocket/codec.rs` 已按原版区分 one-shot 与 pooled WS 错误分类 profile：one-shot 不提前分类 `websocket_connection_limit_reached`，pooled path 仍将其映射为 503 并标记连接 fatal。
- `tests/codex_serving/responses_websocket.rs:47-110` 覆盖默认 WebSocket 上游；`tests/codex_serving/responses_websocket.rs:300-360` 覆盖 recorded conversation 复用同一 WS。
- `tests/config.rs` 覆盖默认 `ws_pool` 配置和 `local.yaml` 覆盖；`v1_responses_websocket_should_not_reuse_connection_when_pool_is_disabled` 覆盖关闭 pool 后仍走 WebSocket 但不复用物理连接。
- `tests/codex_gateway/websocket.rs::websocket_one_shot_should_passthrough_connection_limit_failed_frame` 与 `websocket_pooled_connection_limit_frame_should_surface_as_503` 覆盖同一错误码在 one-shot/pool 两条路径的原版差异。
- `tests/codex_gateway/websocket.rs:44-76` 覆盖 transport 策略；`tests/codex_gateway/websocket.rs:376-760` 覆盖 handshake 429、首帧 rotatable error、HTTP SSE fallback、handshake headers 捕获、内部 `codex.rate_limits` 不转发、mid-stream close error。
- `tests/codex_gateway/websocket/pool.rs:1-559` 覆盖 busy key bypass、账号 cap bypass、keepalive pong/no-pong、过期 GC、shutdown。

结论：部分对齐。

已对齐：
- 默认上游传输策略已与原版方向一致：普通 `/v1/responses` 优先 WebSocket，`previous_response_id` 必须 WebSocket 且不能安全降级，非历史 WebSocket 传输错误可降级 HTTP SSE。
- WS URL、`response.create` frame 主字段、tool 默认值、service tier、prompt cache key、client metadata、internal rate-limit event、terminal event SSE 包装均已基本对齐。
- WS handshake headers 已去掉原版没有的 `Content-Type` 与 `Accept: text/event-stream`。
- WS opening request 的关键 wire 顺序、大小写和 `Sec-WebSocket-Extensions` offer 已按原版 `ws@8.19.0` 对齐，并由 raw TCP 测试覆盖。
- WS permessage-deflate 协商后的 server-to-client 压缩 text/binary frame 已支持解码，避免上游接受扩展后因 RSV1 frame 触发 `tungstenite` `NonZeroReservedBits`。
- one-shot WebSocket handshake 已增加原版同等 20 秒 open timeout；普通无历史请求超时后仍可按原版策略降级 HTTP SSE，`previous_response_id` required WS 不降级。
- 连接池核心状态机基本对齐：55 分钟 max age、每账号最多 8 条、严格单 in-flight、busy/cap bypass、stale reuse 一次 fresh retry、idle keepalive、liveness/GC、shutdown、账号状态变更驱逐。
- WebSocket pool 配置面已对齐原版：配置文件显式提供 `ws_pool.enabled/max_age_ms/max_per_account`，运行时按配置启停复用和调整 max age/account cap。
- `websocket_connection_limit_reached` 已按原版保留路径差异：pooled path 视为 503 fatal 并驱逐；one-shot/bypass path 不在 WS transport 层提前分类，而是透传 `response.failed` frame 给上层处理。
- Rust 对账号状态变化的 WS 驱逐覆盖面较完整，refresh、quota cooldown、fallback、删除/禁用等路径都会清理对应账号连接。

未完全对齐：
- pool key 表达不完全一致。原版 key 是 `${entryId}:${rawConversationId}`；Rust key 是 `(base_url, account_id, accountScopedConversationId)`。行为上通过测试证明同 conversation 会复用，但若后续要与原版日志/配置/诊断完全一致，命名与 key 语义仍不同。
- Rust pool maintenance 是集中 sweep ping idle connection；原版是每条 `PersistentWs` 自带 interval，busy 时跳过 ping。默认行为接近，但调度模型不是逐字等价。

缺口/后续动作：
- 若需要更强证明，可增加真实 OpenAI 上游抓包验证，确认当前 Codex 后端是否实际返回压缩 frame；本地压缩 frame fixture 已覆盖客户端能力。

## 7. response/SSE/WS frame 转换链路

原版证据：
- `src/proxy/codex-sse.ts:8-41` 的 `parseSSEBlock()` 除标准 `data:` 行外，还解析非标准上游错误流：漂亮打印 JSON 的续行即使没有重复 `data:` 前缀，也会拼入 data；`[DONE]` 会被忽略。
- `src/proxy/codex-sse.ts:44-95` 的 `parseSSEStream()` 对单个未解析 SSE event buffer 设置 64 MiB 上限；如果 response 不是 SSE 但有 body，会合成 `event: error`，code 为 `non_sse_response`。
- `src/routes/shared/response-processor.ts:50-229` streaming 入口每 15 秒写 SSE heartbeat；上游读取错误会写一个 Responses 格式的失败 SSE 后再关闭。
- `src/routes/responses-passthrough.ts:168-333` 的 `streamPassthrough()` 逐个解析 Codex event，记录 response id、usage、function call ids、reasoning replay items；如果上游在 `response.completed`/`response.failed`/`error` 之前关闭，会合成 `response.failed`，code 为 `stream_disconnected`。
- `src/routes/responses-passthrough.ts:343-488` 的 `collectPassthrough()` 会从 event 流重建最终 response，补齐 output/output_text，提取 usage、image usage、function call ids、reasoning replay items；遇到 `error` 或 `response.failed` 会抛 `Codex API error`，供外层错误分类和 retry/fallback 使用。
- `src/routes/responses-passthrough.ts:230-267`、`439-463` 对 tuple schema 做 streaming/collect 双路径 reconvert。
- `src/translation/codex-event-extractor.ts:46-79` 区分 `EmptyResponseError` 与 `UpstreamPrematureCloseError`，后者用于上游无 terminal event 关闭，原版注释明确这种情况不应跨账号重试。
- `src/translation/codex-api-error-from-event.ts:1-31` 将 `error`/`response.failed` SSE event 转换为 HTTP-equivalent `CodexApiError`，让 non-streaming catch 路径能复用 400/401/402/403/429/502 的恢复逻辑。
- 2026-06-15 复核 reasoning replay：`src/proxy/reasoning-replay-cache.ts` 以 55 分钟 TTL、512 entries、256 KiB 单条、4 MiB 总量缓存 `reasoning` 与 `function_call` replay items，key 为 `responseId + entryId + conversationId + variantHash`。
- 2026-06-15 复核 implicit resume：`src/routes/shared/proxy-session-context.ts` 在无显式 `previous_response_id`、存在 continuation input 且 conversation/variant 命中时查找最新 response id；`src/routes/shared/proxy-implicit-resume-request.ts` 会把 `previous_response_id`、`useWebSocket=true`、turnState 和 `reasoningReplayItems + continuationInput` 写回本次上游请求。

Rust 证据：
- 本轮修正后，`src/codex/gateway/transport/sse.rs` 的 parser 支持标准 `event/data/id/retry/comment`，同时按原版处理非 `data:` 前缀的 JSON 续行、忽略 `[DONE]`，并在非 SSE body 场景合成 `error/non_sse_response` 事件。
- `src/codex/gateway/transport/websocket/codec.rs:14-85` 将 WS `response.create` frame 构造成上游 JSON，并把每个可见 WS JSON frame 包装成 `event: <type>\ndata: <raw>` 的 SSE chunk；`codex.rate_limits` 是内部事件，不转发给客户端。
- `src/codex/gateway/transport/websocket/mod.rs:230-328` 在 WS stream 中遇到 terminal event 后结束；mid-stream close 会返回 `ClosedBeforeTerminal` 传输错误。
- `src/codex/serving/dispatch/mod.rs:338-476` HTTP SSE streaming 直接透传上游 bytes，并每 15 秒写 `: ping` heartbeat；上游 bytes stream error 会记录 transport error 并以 stream error 结束。
- `src/codex/serving/dispatch/mod.rs:830-923` WebSocket streaming 将 WS SSE chunk 透传给客户端，并每 15 秒写 heartbeat；WS stream error 会转换为 `std::io::Error` 结束响应流。
- `src/codex/serving/dispatch/stream.rs:48-104` non-streaming collect 会解析 SSE，拼接 output text、output item、completed response；识别 `error`/`response.failed` 为 `CollectedResponse::Failed`。
- 本轮修正后，`src/codex/serving/dispatch/stream.rs` 会从 `response.output_item.done` 和 `response.completed.response.output[]` 收集 function call ids 与 replay items；`reasoning`/`function_call` replay item 会进入后续 implicit resume 缓存。
- 本轮修正后，`src/codex/serving/dispatch/reasoning_replay.rs` 按原版约束缓存 replay items：TTL 55 分钟、512 entries、单项 256 KiB、总量 4 MiB，并对白名单 `reasoning`/`function_call` item 做清洗和去重。
- 本轮修正后，`src/codex/gateway/transport/sse.rs` 增加与原版 `parseSSEStream()` 一致的单个未解析 SSE event buffer 上限：64 MiB。该限制按事件块计数，遇到空行完成事件后重置；`tests/codex_gateway/usage_events.rs::parse_sse_events_should_reject_single_event_buffer_above_original_limit` 先以 oversized event 被接受失败，再修复通过。
- 本轮修正后，`src/codex/gateway/protocol/tuple_schema.rs` 按原版 `tuple-schema.ts`/`prepareSchema()` 实现 OpenAI JSON Schema tuple 处理：请求侧把 `prefixItems` 转为数字键 object schema，并注入 `additionalProperties:false`；响应侧按原始 schema 把 `{"0":...}` 还原为数组。
- 本轮修正后，`/v1/responses` native 链路会把原始 tuple schema 保存在本地 `CodexResponsesRequest::tuple_schema`，该字段 `serde(skip)` 不进入上游；non-streaming collect 和 HTTP SSE/WebSocket streaming 均在返回客户端前 reconvert output text，并按原版从 `output[].content` 同步 `output_text`。`tests/codex_serving/responses_http_sse.rs::v1_responses_should_convert_tuple_schema_before_upstream`、`v1_responses_should_reconvert_tuple_schema_output_for_client`、`v1_responses_stream_should_reconvert_tuple_schema_output_for_client` 覆盖该链路。
- 本轮修正后，`/v1/chat/completions` OpenAI Chat 链路也接入相同 tuple schema 处理：`response_format.json_schema.schema` 上游前会转换，non-streaming message content 和 streaming content delta 会还原给客户端；`tests/codex_serving/chat_completions.rs::chat_completions_should_convert_and_reconvert_tuple_schema` 与 `chat_completions_stream_should_reconvert_tuple_schema` 覆盖该链路。
- 本轮修正后，`src/codex/serving/dispatch/mod.rs` 在无显式 `previous_response_id` 的 full-history continuation 中，会按 conversation/variant 找 55 分钟内最新 response，校验 instructions hash 与 function call continuation，随后写入 `previous_response_id`、强制 WebSocket、截取 continuation input，并在 input 前注入 reasoning replay items；若该隐式续链遇到 previous-response 错误，会恢复原始 full-history request 后同账号重试。
- `tests/codex_serving/responses_websocket.rs::v1_responses_websocket_should_implicitly_resume_full_history_continuation_with_reasoning_replay` 覆盖第二轮上游请求自动携带 `previous_response_id`、复用 prompt cache key，并将 cached reasoning replay item 放在 continuation input 前。
- `tests/codex_serving/responses_websocket.rs::v1_responses_websocket_should_restore_full_history_when_implicit_resume_previous_response_is_missing` 覆盖隐式续链 `previous_response_not_found` 后，下一次上游请求恢复完整 3 条历史 input 且不带 `previous_response_id`。
- 本轮修正后，`src/codex/serving/responses.rs:145-365` non-streaming 对空响应最多跨账号重试 2 次；`CollectedResponse::Failed` 会转换为可分类的 `CodexClientError::Upstream`，429/402/403 会复用账号 retry/fallback 状态机，不再固定降级为 502。
- 本轮修正后，`src/codex/serving/responses.rs` 会复用第一次 SSE collect 结果生成最终响应，不再对同一个 upstream body 二次解析。
- 本轮修正后，`src/codex/serving/dispatch/fallback.rs` 增加 request-level recovery 分类，识别 `previous_response_not_found` 和 `No tool output found for function call`；`src/codex/serving/responses.rs`、`src/codex/serving/dispatch/mod.rs` 在账号 fallback 前先清除 stale `previous_response_id`/`turn_state` 并同账号重试一次。
- `tests/codex_serving/upstream_errors.rs:67-194` 覆盖 non-streaming `response.failed` 按 `rate_limit_exceeded` 返回 429、streaming 透传 `response.failed` 并记录失败审计。
- 本轮新增 `tests/fixtures/responses/http_sse/response_failed_quota.sse` 与 `tests/codex_serving/upstream_fallback.rs::v1_responses_should_classify_non_stream_sse_failure_and_retry_next_account`，覆盖 non-streaming mid-SSE `quota_exceeded` 会标记当前账号 `quota_exhausted` 并 fallback 到下一账号。
- 本轮新增 `tests/fixtures/responses/websocket/previous_response_not_found.json`、`tests/fixtures/responses/websocket/unanswered_function_call.json`，并用 `tests/codex_serving/responses_websocket.rs` 覆盖 non-streaming 与 streaming `previous_response_not_found`、non-streaming unanswered function call 的同账号 strip-and-retry。
- `tests/codex_gateway/usage_events.rs` 覆盖 multiline data、非标准 JSON 续行、`[DONE]` 忽略和非 SSE JSON body 转 `non_sse_response`。
- `tests/codex_gateway/websocket.rs:376-760` 覆盖 WS 内部 rate-limit event 不转发、首个 rotatable error、mid-stream close error 和 fallback。
- 本轮修正后，HTTP SSE streaming 在上游正常 EOF 但缺少 `response.completed`/`response.failed`/`error` 时，会给客户端追加原版格式的 `response.failed`，错误码为 `stream_disconnected`；`tests/codex_serving/upstream_errors.rs::v1_responses_stream_should_synthesize_response_failed_when_http_sse_closes_before_terminal` 覆盖该行为。
- 本轮修正后，WebSocket streaming 在首个可见 frame 已发送后遇到 `ClosedBeforeTerminal` 或其它未见 terminal 的 body stream 错误时，也会追加同样的 `response.failed/stream_disconnected`，不再把 body stream error 直接暴露给客户端；`tests/codex_serving/responses_websocket.rs::v1_responses_websocket_stream_should_synthesize_response_failed_when_closed_before_terminal` 覆盖该行为。
- 本轮修正后，HTTP SSE 与 WebSocket streaming 的 heartbeat 首次触发延后到 15 秒，不会因为 `tokio::time::interval()` 的立即 tick 抢在上游首个可见 frame 前面；`tests/codex_serving/responses_websocket.rs::v1_responses_websocket_should_stream_first_frame_before_terminal_event` 覆盖首个 WebSocket delta 可先于 terminal 返回给客户端。

结论：部分对齐。

已对齐：
- 标准 SSE 事件解析、WS JSON frame 到 SSE chunk 的基本转换、terminal event 判断、`codex.rate_limits` 内部事件过滤、heartbeat 15 秒节奏、usage 提取、function call ids 提取、空响应重试等主路径已经具备。
- SSE parser 已对齐原版的三个容错点：漂亮打印 JSON 续行、`[DONE]` 忽略、非 SSE body 合成 `error/non_sse_response`。
- streaming 路径对客户端保持 SSE 格式，HTTP SSE 与 WebSocket 两种上游传输都能以 SSE 返回给下游。
- `response.failed`/`error` 在 streaming 路径会被透传并写入审计；non-streaming 路径也能识别为上游失败，并已对 429/402/403 进入账号状态机和 fallback。
- request-level `previous_response_not_found` 与 unanswered function call 已按原版优先于账号 fallback 处理：忘记 stale affinity，清除 `previous_response_id`/`turn_state`，同账号重试一次。
- streaming premature close 已按原版对齐：HTTP SSE 正常 EOF、HTTP SSE body stream error、WebSocket body stream error 在缺少 terminal event 时都会合成 `event: response.failed`，错误码 `stream_disconnected`，并进入现有 stream 审计失败记录。
- 单个未解析 SSE event buffer 已按原版限制为 64 MiB，避免异常上游响应在解析阶段无界增长。
- OpenAI Responses 与 OpenAI Chat 的 tuple schema request conversion/reconvert 已对齐原版，不涉及非 OpenAI translator。

未完全对齐：
- mid-SSE/request failure 尚未完全覆盖原版恢复集合。Rust 已补齐 429/402/403 分类与 fallback、401 token invalid fallback，以及 `previous_response_not_found`/unanswered function call strip-and-retry。
- non-streaming upstream error 响应外壳仍不完全一致。原版 Responses pooled path 通过 `PASSTHROUGH_FORMAT` 返回 `codex_api_error`/`rate_limit_exceeded` 风格错误；Rust 仍通过 `codex_client_error_response()` 包装为 OpenAI error 风格的 `upstream_error`。

缺口/后续动作：
- 对齐 Responses pooled path 的最终错误响应外壳，或明确 Rust 统一 OpenAI error 格式为本项目边界。
- 增加 golden tests：标准 SSE、漂亮打印 JSON 续行、非 SSE body、premature close、`response.failed` auth recovery、reasoning replay、WS frame to SSE chunk。

## 8. fallback、retry、错误分类

原版证据：
- `src/proxy/error-classification.ts:37-148` 分类 429 retry-after、402 quota、403 Cloudflare challenge、403 ban、401 token invalid、400 `previous_response_not_found`、400 unanswered function call、空 body 404 Cloudflare path-block、model not supported。
- `src/routes/shared/proxy-retry-classifier.ts:34-68` 的恢复优先级是：implicit resume replay、strip stale `previous_response_id`、再交给通用错误处理。
- `src/routes/shared/proxy-retry-recovery.ts:27-96` 对 `previous_response_not_found` 和 `No tool output found for function call` 只 strip 一次 `previous_response_id`/`turnState`，忘记 stale affinity，并在同账号重试。
- `src/routes/shared/proxy-retry-recovery.ts:98-151` 有 Cascading Ban Defense：只有 preferred account 已 banned/disabled 且实际换账号时，才主动 strip `previous_response_id` 和 `turnState`。
- `src/routes/shared/proxy-error-handler.ts:63-185` 处理上游 `CodexApiError`：model unsupported 只重试一次其他账号；429 写 cachedQuota rate_limit 并 fallback；402 标记 quota_exhausted；CF challenge 进入 cooldown；非 CF 403 标记 banned；401 标记 expired/banned 并 fallback；空 body 404 清 cookie，连续 3 次 auto-disable；其他错误保留 body 返回。
- `src/routes/shared/proxy-error-retry-transition.ts:43-102` 和 `proxy-fallback-account-retry.ts:39-75` 统一处理 release、restore implicit resume、fallback account acquire、API rebuild、无 fallback 时的 account exhaustion response。
- `src/utils/retry.ts:1-25` 对同一上游 attempt 的 5xx `CodexApiError` 做最多 2 次指数退避重试。
- `src/routes/shared/non-streaming-handler.ts:144-204` 把 collect 中的 `CodexApiError` 重新抛给外层统一 catch，确保 mid-SSE `previous_response_not_found`/unanswered function call 与 HTTP-time 错误走同一恢复逻辑。
- `src/routes/shared/proxy-upstream-attempt.ts:71-96` 每次上游请求后记录 egress log，并应用 rate-limit headers。

Rust 证据：
- `src/codex/serving/dispatch/fallback.rs:22-190` 把 429、402、403 Cloudflare challenge、403 ban、空 body 404 Cloudflare path-block、401 token invalid/deactivated 分类成 `UpstreamAccountRetry`。
- `src/codex/serving/dispatch/fallback.rs:350-470` 对 429 设置 quota cooldown 并记录 request attempt；402 设置 `QuotaExhausted`；Cloudflare challenge 删除 cookies 并设置 persisted cooldown；空 body 404 清理当前账号 cookies、记录 1 小时滑窗内 path-block 次数，连续 3 次设置 `Disabled`；401 token invalid 设置 `Expired`，401 deactivated 设置 `Banned`；403 ban 设置 `Banned`；状态变化会 evict WS pool。
- 本轮修正后，`src/codex/serving/dispatch/account_refresh.rs` 已删除；serving request path 不再对上游 401 做同账号 OAuth refresh，refresh 仍保留在账号健康检查、后台调度和 admin/manual refresh 链路。
- `src/codex/serving/dispatch/mod.rs:338-476`、`689-923` 在 HTTP SSE/WS streaming 请求建立前，遇到可分类错误会尝试 acquire fallback account；显式 `previous_response_id` 的 WS history 请求遇到 429/402/403 会保持原账号，不切换 fallback。
- 本轮修正后，`src/codex/serving/responses.rs:145-365` non-streaming 请求前错误与 mid-SSE `error`/`response.failed` 都会先尝试 request-level recovery，再进入 `classify_upstream_account_retry()`；显式 `previous_response_id` 遇到可分类 429/402/403 时只记录并保持原账号。
- 本轮修正后，`src/codex/serving/dispatch/mod.rs` 的 HTTP SSE streaming 与 WebSocket streaming 请求建立失败也会先尝试 request-level recovery，`previous_response_not_found` 可同账号 strip-and-retry。
- 本轮修正后，`src/codex/serving/dispatch/mod.rs` 在 non-streaming、HTTP SSE streaming、WebSocket streaming 的上游 request attempt 外层加入 `retry_upstream_5xx()`：仅对 `CodexClientError::Upstream` 的 5xx 做同账号最多 2 次重试，退避为 1000ms、2000ms，与原版 `withRetry()` 默认策略一致。
- 本轮修正后，`src/codex/serving/dispatch/fallback.rs` 识别 4xx `model not supported`、`model_not_supported`、`model not available`、`model_not_available`，只允许一次备用账号 retry，不标记账号状态，也不驱逐 WebSocket pool。
- 本轮修正后，`src/codex/serving/dispatch/stream.rs` 会把 non-streaming collect 得到的 `response.failed` `model_not_supported`/`model_not_available` 合成为 400 上游错误，使它进入同一 model fallback 状态机。
- `src/codex/serving/http/errors.rs:7-40` 统一把 `CodexClientError::Upstream` 映射为原 status + `upstream_error`，其他 transport 映射为 502；无 fallback 的空 body 404 会按原版 path-block 语义返回 502 `Upstream blocked the request (Cloudflare path-block)`。
- `src/codex/accounts/cloudflare_challenge.rs:1-120` 实现了 CF path-block tracker，连续次数超过 1 小时会重置，阈值为 3。
- `tests/codex_serving/upstream_fallback.rs:40-1400` 覆盖 429 fallback、streaming 429 fallback、402 标记并 fallback、403 ban、Cloudflare challenge cooldown/cookie 清理、Cloudflare path-block 404 清 cookies 并 fallback、连续 3 次 path-block 后禁用账号、401 token invalid 标记 expired 后 fallback、401 deactivated 标记 banned、无 fallback 时返回 401。
- `tests/codex_serving/responses_websocket.rs` 覆盖 WebSocket 非历史请求先因 429 fallback 到备用账号，再因 401 将备用账号标记 expired 并返回 401，不做同账号 refresh。
- 本轮新增 `tests/fixtures/responses/http_sse/after_model_unsupported_retry.sse`、`tests/fixtures/responses/http_sse/response_failed_model_unsupported.sse`，并用 `tests/codex_serving/upstream_fallback.rs` 覆盖 HTTP-time model unsupported fallback、non-streaming mid-SSE model unsupported fallback、以及 model unsupported 最多只重试一次。
- 本轮新增 `tests/fixtures/responses/http_sse/after_5xx_retry.sse`、`tests/fixtures/responses/http_sse/stream_after_5xx_retry.sse`，并用 `tests/codex_serving/upstream_fallback.rs` 覆盖 non-streaming 与 streaming 两条路径在同账号连续 2 次 5xx 后第三次成功，且不会切换到备用账号。
- `tests/codex_serving/responses_websocket.rs:652-927` 覆盖 `previous_response_id` WS 429 不跨账号重试、非历史 WS fallback 和 fallback 账户 refresh。

结论：部分对齐。

已对齐：
- 429、402、403 ban、403 Cloudflare challenge 的主路径已经具备账号状态更新和 fallback account retry。
- non-streaming mid-SSE `response.failed`/`error` 的 429、402、403 已不再固定返回 502，会进入同一账号状态机和 fallback 逻辑；`quota_exceeded` fixture 已覆盖 quota exhausted 标记与备用账号接管。
- `previous_response_not_found` 与 unanswered function call 已优先于账号 fallback 处理，清除 stale history 后同账号重试一次；streaming 与 non-streaming 的 WS 首帧错误均有测试覆盖。
- 5xx same-account retry 已对齐原版：上游 request attempt 对 5xx 同账号最多重试 2 次，退避 1000ms、2000ms；non-streaming 和 streaming HTTP SSE 均有集成测试覆盖，WebSocket streaming 也复用同一请求建立 helper。
- CF path-block 404 已接入主链路：空 body 404 会清理当前账号 cookies、记录 1 小时滑窗内连续次数、尝试 fallback account，达到 3 次后把账号标记为 `disabled`；无 fallback 时返回 502 `upstream_error`。
- model unsupported fallback 已对齐原版：账号 plan 不支持当前 model 时最多换一个备用账号重试一次，不改变账号状态；HTTP-time 错误与 non-streaming mid-SSE `response.failed` 均有测试覆盖。
- 401 token invalid fallback 已对齐原版：request path 不再 refresh 同账号；普通 401 会标记当前账号 `expired` 并尝试备用账号，包含 `deactivated` 的 401 会标记 `banned`；HTTP SSE streaming/non-streaming 与 WebSocket fallback exhausted 均有测试覆盖。
- Rust 的 Cloudflare challenge 处理比原版更持久：会把 cooldown 写入数据库，并清理对应账号 cookies。
- WS history 请求不跨账号重试这一点从安全角度更保守，避免 `previous_response_id` 的 server-side history 静默丢失。

未完全对齐：
- recovery 优先级仍不是完整原版状态机。Rust 已实现 implicit resume restore/replay 与 strip-and-retry 优先于账号 fallback，但状态机仍分散在 responses/dispatch 中，没有抽成原版同等的 `RetryAction` 分层。
- non-streaming mid-SSE failure 仍不是完整原版 catch 状态机。429/402/403、401、model unsupported、400 strip-and-retry 与隐式续链 restore 已补齐；剩余主要是最终错误外壳和 fallback exhausted body。
- `previous_response_id` 限流/配额语义不一致。原版没有显式“history 请求保持原账户”的 retry hold，可能经 fallback 后再通过 `previous_response_not_found` strip-and-retry 恢复；Rust 遇到 429/402/403 会保持原账号并返回错误，保护历史但牺牲可恢复性。
- 无 fallback 时的响应体不一致。原版会把 account pool exhaustion summary 拼进错误；Rust 多数路径直接返回 `Codex upstream error: ...`，缺少账号耗尽上下文。
- CF challenge exhausted status 不一致。原版 Cloudflare challenge fallback exhausted 使用 502 语义；Rust 若无 fallback，最终会走原 403 upstream error。

缺口/后续动作：
- 引入原版级 `UpstreamRecoveryAction` 状态机：implicit resume replay、strip-and-retry、error handler、fallback transition 分层清晰，避免分散在 responses/dispatch 中。
- 继续补 HTTP SSE mid-SSE failure 的 `previous_response_not_found` 与 unanswered function call 覆盖；当前已有 WS 首帧错误覆盖 streaming/non-streaming。
- 增加 fallback exhausted response body 的 parity tests。

## 9. rate-limit、usage、quota、cookie 持久化

原版证据：
- `src/proxy/cookie-jar.ts:27-45` cookie 持久化格式 v2 记录 value 与 expires；自动捕获白名单只有 `cf_clearance`，关键 cookie 立即异步落盘，普通变更 1 秒 debounce。
- `src/proxy/cookie-jar.ts:78-102` 构造 Cookie header 时跳过过期 cookie；`cleanupExpired()` 每 5 分钟清理并持久化。
- `src/proxy/rate-limit-headers.ts:44-85` 从 `x-codex-primary-*`、`x-codex-secondary-*`、`x-codex-code-review-*`/`x-codex-review-*`/`x-code-review-*` 解析 rate-limit，并转为 normalized quota。
- `src/proxy/rate-limit-headers.ts:101-169` 解析 `codex.rate_limits` WS 内部 event，支持 review limit name 归类。
- `src/auth/quota-utils.ts:1-105` 从 `/codex/usage` body 归一化 plan、primary/secondary/code_review/additional_rate_limits/credits。
- `src/routes/shared/proxy-rate-limit.ts:28-49` passive rate-limit 更新 cachedQuota、同步 window_reset_at/limit_window_seconds；若 primary 已达 100 且 reset_at 在未来，会主动写入 rate-limit 侧效应。
- `src/auth/account-registry.ts:215-266` 429 会合成或更新 cachedQuota.rate_limit，且不会缩短已知更晚 reset_at；`countRequest` 时同步增加 request_count/window_request_count。
- `src/auth/account-registry.ts:414-480` release 时记录 request_count、token usage、cached tokens、image_generation tokens、empty response、window counters。
- `src/auth/account-registry.ts:520-584` window 到期会重置窗口计数并推进下一窗口；cachedQuota 离线 reset 后会设置 `quotaVerifyRequired=true`。
- `src/routes/shared/proxy-handler.ts:88-139` 在账号被 dirty quota 标记时，请求进入前最多做 5 次 `/usage` 校验，避免本地窗口推断把仍然限流的账号重新放回池。

Rust 证据：
- `src/codex/accounts/cookies/repository.rs:1-111` cookie 存在 SQLite `account_cookies`，value 加密，唯一键为 `(account_id, domain, name, path)`；自动捕获白名单只有 `cf_clearance`。
- `src/codex/accounts/cookies/repository.rs:70-102` 构造 Cookie header 时按 domain 过滤并跳过过期 cookie；`delete_account_cookies()` 支持按账号清理。
- `src/platform/storage/schema.sql:68-83` `account_usage` 已持久化 `window_request_count`、`window_input_tokens`、`window_output_tokens`、`window_cached_tokens`、`window_started_at`、`window_reset_at`、`limit_window_seconds`、`last_used_at`。
- `src/codex/accounts/repository/usage.rs:12-44` `record_usage()` 会累计 total/window request 和 token 计数，并设置 `last_used_at`。
- `src/codex/accounts/repository/usage.rs:63-99` `sync_rate_limit_window()` 会在 reset_at 漂移超过窗口 50% 或默认 3600 秒时重置窗口计数，否则只同步 reset_at/limit_window_seconds。
- `src/codex/accounts/repository/accounts.rs:329-562` list pool accounts 时会把持久化 usage/window 字段恢复到运行时 `Account`。
- `src/codex/gateway/transport/rate_limits.rs:58-144` rate-limit header 与 `codex.rate_limits` event 解析规则基本对应原版，并在 passive quota 缺 credits 时保留已有 credits。
- `src/codex/serving/dispatch/limits.rs:8-67` response headers 或 WS rate-limit updates 会更新 `quota_json`、同步运行时和数据库窗口，并在 primary limit reached 时写 quota cooldown、驱逐 WS。
- `src/codex/serving/dispatch/usage.rs:7-30` 成功 usage 会同步运行时窗口 token usage，并写入数据库累计 usage。
- `src/codex/accounts/pool.rs:403-441` acquire 时更新运行时 `request_count`、`window_request_count`、`last_used_at`，并在已知窗口长度但无窗口开始时间时推导窗口 reset。
- `src/codex/accounts/pool.rs:502-571` 运行时会在窗口到期时重置窗口计数、清除过期 quota cooldown，并推进下一窗口 reset。
- `tests/codex_accounts/repository.rs:230-326` 覆盖 window usage 持久化与恢复。
- `tests/codex_serving/responses_http_sse.rs:279-350` 覆盖 passive rate-limit headers 缓存 quota、窗口字段和 cooldown。
- `tests/codex_accounts/pool_scheduling.rs:150-466` 覆盖窗口过期重置、least_used 的 quota/window/request_count 排序和 runtime request count。

结论：部分对齐。

已对齐：
- 之前审计中提到的窗口统计字段已经不再缺失：Rust schema、repository、runtime pool 和测试都覆盖了 window request/token/reset/limit_window_seconds。
- Cookie 自动捕获白名单与原版一致，只自动保存 `cf_clearance`，不会保存 `__cf_bm`。
- Rust 使用 SQLite + AES 加密保存 cookie，比原版 JSON 文件更适合当前数据库架构；主请求链路能注入 Cookie 并捕获 `Set-Cookie`。
- primary/secondary/code_review rate-limit header 和 `codex.rate_limits` event 的解析与 quota 归一化主路径基本对齐，并能保留已有 credits。
- usage input/output/cached token 的成功路径持久化、窗口统计和重启恢复已对齐到可用水平。
- least_used 依赖的 request_count、window_reset_at、quota_limited 字段已经进入运行时调度和数据库恢复路径。

未完全对齐：
- image_generation usage 未对齐。原版记录 image input/output tokens、image request success/failure 和对应窗口计数；Rust `TokenUsage` 只有 input/output/cached/total，没有解析或持久化 `tool_usage.image_gen`。
- dirty quota verification 缺失。原版离线 reset cachedQuota 后设置 `quotaVerifyRequired`，下一次请求会先调用 `/usage` 验证；Rust 只在本地窗口到期后清除 cooldown/推进窗口，没有等价 dirty 标记和请求前最多 5 次 upstream usage 校验。
- 失败请求的持久 request_count 语义不一致。原版 release 时默认记录 request_count，错误 release 也计数；Rust runtime acquire 会计运行时 request_count，但数据库只在成功 usage、429 `record_request_attempt()`、empty response 等路径写入，部分 transport/5xx/403/402 fallback 失败尝试重启后不会体现在持久 request_count 中。
- Cookie 过期清理语义不一致。Rust 读取时会跳过过期 cookie，但没有原版每 5 分钟清理并持久删除的后台任务；长期运行时数据库中可能保留过期 cookie 行。
- Cookie domain/path 语义不同。原版 CookieJar 是 per-account 简单 map，不按 domain/path 过滤；Rust 按 domain/path 存储和匹配。对 `chatgpt.com` 主链路更严格，但不是原版逐字行为。
- quota window reset 细节不完全一致。原版 reset 后设置 `window_counters_reset_at` 并可能标记 quotaVerifyRequired；Rust 设置 `window_started_at`，没有 `window_counters_reset_at` 字段，也不触发 quotaVerifyRequired。
- passive header 文档提到 `x-codex-active-limit`，原版和 Rust 当前都没有实际解析该 header；如果 Codex Desktop 当前依赖 active-limit，这两边都需要重新确认。
- usage 提取策略有潜在差异。Rust `extract_sse_usage()` 会合并所有 SSE event 中出现的 usage；原版 Responses passthrough collect/stream 主要在 `response.completed` 中取 usage。若上游多个事件重复携带同一 usage，Rust 可能重复累计。

缺口/后续动作：
- 补 `TokenUsage` 的 image_generation 字段与数据库/运行时窗口字段，或明确 Rust 不支持 image_generation usage 统计。
- 增加 dirty quota verification 机制：窗口离线 reset 后标记需要校验，请求前调用 usage endpoint，失败时保留 dirty 标记并限制放大次数。
- 统一持久 request_count 计数点，决定是否像原版一样在 release/attempt 维度记录所有真实上游尝试；至少补 transport/5xx/403/402 exhausted 的测试。
- 为 `account_cookies` 增加过期清理任务或在读取时顺手删除过期行。
- 明确 cookie domain/path 精细化是 Rust 新架构选择还是需要回到原版 per-account map；如果保留，应补 domain/path 行为测试。
- 为 usage merge 增加测试，覆盖多个 event 带 usage 时是否重复累计。
- 若 OpenAI/Codex 当前返回 `x-codex-active-limit`，补解析和 quota_json 存储；否则将其记录为双方未使用字段。

## 10. session affinity、implicit resume、reasoning replay

原版证据：
- `src/auth/session-affinity.ts:1-181` 维护 response_id 到 account/conversation/turnState/instructionsHash/inputTokens/functionCallIds/variantHash 的映射；TTL 4 小时，每 10 分钟清理。
- `src/routes/shared/proxy-session-context.ts:45-121` 为每个请求构造 explicit/implicit session context：prompt cache identity、continuation input start、chain conversation id、variant hash、implicit previous response candidate、preferred account、turnState、function call 校验输入。
- `src/routes/shared/proxy-session-helpers.ts:8-16` implicit resume 最大年龄为 55 分钟，与 WS pool max age 对齐，避免旧 `previous_response_id` 落到新 LB 后端。
- `src/routes/shared/proxy-session-helpers.ts:34-56` prompt cache identity 优先级为显式 `prompt_cache_key`、`clientConversationId`、稳定派生 key、随机 UUID。
- `src/routes/shared/proxy-session-helpers.ts:58-70` variant identity 由 `codexWindowId` 与稳定 anchor 组成；随后 `computeVariantHash(instructions, tools, variantIdentity)` 区分并发分支。
- `src/routes/shared/proxy-session-helpers.ts:92-147` `evaluateImplicitResume()` 会校验 previous response 是否存在、continuation 是否非空、账号是否匹配、instructions hash 是否一致、function_call_output 是否都能对应上轮 function_call，且识别 self-contained replay。
- `src/routes/shared/proxy-implicit-resume-request.ts:30-78` implicit resume 激活时会写入 `previous_response_id`、强制 `useWebSocket=true`、把 input 截成 continuation，并在前面插入 reasoning replay items；失败恢复时还原原请求。
- `src/routes/shared/proxy-implicit-resume-lifecycle.ts:42-119` 管理 implicit resume 激活、跳过警告、WebSocket failure 后完整历史 replay、usage hint 和 restore。
- 本轮复核确认：原版 `UsageHint` 由 shared implicit resume lifecycle 生成，但 OpenAI chat translator `codex-to-openai.ts` 和 Responses passthrough `responses-passthrough.ts` 不消费它；实际消费点是非 OpenAI translator 的 cache token 估算。按当前边界，非 OpenAI translator 不移植，因此 usage hint 不计入 OpenAI 链路缺口。
- `src/proxy/reasoning-replay-cache.ts:1-247` reasoning replay cache TTL 55 分钟，限制 512 entries、单项 256 KiB、总 4 MiB；只缓存白名单 reasoning/function_call replay item，并按 entry/conversation/variant 匹配。
- `src/routes/shared/streaming-handler.ts:74-108` streaming 成功 completed 后记录 affinity 和 reasoning replay cache；invalid encrypted content 会驱逐 replay identity。
- `src/routes/shared/non-streaming-handler.ts:96-121` collect 成功后记录 affinity、function call ids 和 reasoning replay cache。

Rust 证据：
- `src/codex/serving/dispatch/affinity.rs:1-76` `AffinityEntry` 字段覆盖 account_id、conversation_id、turn_state、instructions_hash、input_tokens、function_call_ids、variant_hash、created_at。
- `src/codex/serving/dispatch/affinity.rs:78-154` `SessionAffinityRepository` 持久化 session affinity 到 SQLite，包含 expires_at 和 function_call_ids_json。
- `src/codex/serving/dispatch/affinity.rs:156-344` runtime `SessionAffinityMap` TTL 4 小时、清理间隔 10 分钟，支持 record/restore/lookup account/conversation/turnState/instructions/inputTokens/functionCallIds/latest/forget。
- `src/runtime/bootstrap.rs` 与 `tests/runtime/startup.rs:197-336` 覆盖启动后从 SQLite 恢复 session affinity，并用 `previous_response_id` 路由回记录账号。
- `src/codex/serving/dispatch/mod.rs:142-164` 显式 `previous_response_id` 会继承已记录 conversation id 到 `prompt_cache_key`，并补 turn_state；无显式 `previous_response_id` 时会先确保 prompt cache key，再尝试 implicit resume。
- `src/codex/serving/dispatch/mod.rs:167-186` 显式 `previous_response_id` 会查 affinity preferred account，并传给 account pool acquire。
- `src/codex/serving/dispatch/mod.rs:760-820` completed response 后记录 affinity，并持久化到 repository；记录 usage input_tokens、function_call_ids、variant_hash，并同步写入 reasoning replay cache。
- `src/codex/gateway/conversation_identity.rs:53-90` 稳定 prompt cache key 派生规则与原版 stable key 对齐。
- `src/codex/serving/dispatch/affinity.rs` 的 `compute_variant_hash()` 已改为原版基础形态：`instructions + "\0" + JSON.stringify(tools) + optional identity` 后取 SHA-256 前 12 位。
- `src/codex/serving/dispatch/implicit_resume.rs` 实现 continuation input start、function_call_output 校验、missing/unanswered 检查和 self-contained replay 跳过。
- `src/codex/serving/dispatch/reasoning_replay.rs` 实现 reasoning replay cache：TTL 55 分钟、512 entries、单项 256 KiB、总量 4 MiB，按 account/conversation/variant 匹配。
- `src/codex/serving/dispatch/stream.rs` 会从 completed SSE 中收集 replay items；streaming 和 non-streaming completed 都通过同一个 affinity 记录函数写入 cache。
- `src/codex/serving/dispatch/implicit_resume.rs` 提供 request snapshot/restore；non-streaming 与 streaming request recovery 在隐式续链 previous-response 错误后会先恢复 snapshot，再清除 `previous_response_id`/`turn_state` 并同账号重试。
- `tests/codex_serving/responses_websocket.rs:476-590` 覆盖显式 previous_response_id 路由到记录账号、继承 previous_response_id、持久化 function_call_ids/input_tokens/expires_at。
- 本轮新增 `tests/fixtures/responses/websocket/completed_with_reasoning_replay.json` 和 `tests/codex_serving/responses_websocket.rs::v1_responses_websocket_should_implicitly_resume_full_history_continuation_with_reasoning_replay`，覆盖 completed response 记录 replay item 后，下一轮 full-history continuation 自动变成 implicit resume request。
- 本轮新增 `tests/codex_serving/responses_websocket.rs::v1_responses_websocket_should_restore_full_history_when_implicit_resume_previous_response_is_missing`，覆盖隐式续链失败后的 full-history restore/replay。

结论：部分对齐。

已对齐：
- 显式 `previous_response_id` 的核心 affinity 已对齐：response_id 能映射回账号、conversation id、turn_state，后续请求可路由到同一账号并继续 WebSocket 链。
- Rust 增加了 SQLite 持久化与启动恢复，这是对当前无历史负担架构更优雅的增强；原版 session affinity 本身是内存单例。
- TTL 4 小时、清理 10 分钟、function_call_ids、instructions hash、input_tokens、variant_hash 字段都已经出现在 Rust 数据模型中。
- prompt cache stable key 派生规则本身与原版对齐。
- stale affinity recovery 已接入：上游 `previous_response_not_found` 或 unanswered function call 时会忘记 stale affinity，清除 `previous_response_id`/`turn_state`，同账号重试一次。
- implicit resume 主路径已接入：无显式 `previous_response_id`、存在 continuation input、conversation/variant 命中、instructions hash 一致且 function call continuation 合法时，Rust 会自动设置 `previous_response_id`、强制 WebSocket、截取 continuation input 并注入 turn_state。
- implicit resume failure restore 已接入：隐式续链遇到 previous-response 错误时，会恢复原始 full-history input，清除 `previous_response_id`/`turn_state` 并同账号重试。
- reasoning replay cache 主路径已接入：completed response 的 `reasoning`/`function_call` replay items 会按原版 TTL/容量/大小约束缓存，并在 implicit resume 时插入 continuation input 前部。
- function call continuation 预检已接入：missing/unanswered/self-contained replay 会阻止错误的 implicit resume。
- variant_hash 已不再序列化整个 request，基础算法已对齐为 instructions/tools/optional identity 的 12 位 hash。
- variant identity 已接入：Rust 会在 session preparation 阶段、implicit resume 改写 input 前按原版语义预先保存 identity；`codexWindowId` 会进入 `window:{id}`，显式 `prompt_cache_key` 且存在 stable anchor 时会进入 `anchor:{derivedConversationId}`，后续 implicit resume lookup、affinity record 和 reasoning replay 使用同一个 variant hash。
- 本轮新增 `tests/codex_serving/responses_websocket.rs::v1_responses_websocket_should_not_implicitly_resume_across_codex_windows`，覆盖同一显式 `prompt_cache_key` 下不同 `codexWindowId` 不会串用 `previous_response_id`；新增 affinity 单元测试覆盖 window identity 和显式 prompt cache anchor。
- invalid encrypted content 驱逐已接入：Rust 会识别 `error`/`response.failed` 中的 `invalid_encrypted_content` 或等价的 invalid/encrypted/content 文本，并按 `account_id + conversation_id + variant_hash` 驱逐 reasoning replay cache；non-streaming、HTTP SSE streaming audit、WebSocket streaming audit 都使用同一驱逐方法。
- 本轮新增 `tests/fixtures/responses/websocket/invalid_encrypted_content.json` 与 `tests/codex_serving/responses_websocket.rs::v1_responses_websocket_should_evict_reasoning_replay_after_invalid_encrypted_content`，覆盖 replay item 被上游判无效后，后续 implicit resume 不再注入同一个 encrypted reasoning item。

已对齐：
- OpenAI chat `clientConversationId` 已接入。原版 `/v1/chat/completions` 会把 OpenAI `user` 字段作为 shared handler 的 `clientConversationId`；Rust 现在将 `ChatCompletionRequest.user` 写入 Codex 请求的内部 `client_conversation_id` 和 `prompt_cache_key`，进入上游前仍按账号作用域转换为 `cp_...` session id。
- 本轮新增 `tests/codex_serving/chat_completions.rs::chat_completions_should_use_user_as_client_conversation_id`，覆盖 OpenAI chat `user` 进入上游 `prompt_cache_key`。

未完全对齐：
- 当前剩余风险主要是覆盖面，而不是已确认的 OpenAI 行为缺失：还需要更多端到端用例证明 missing/unanswered tool calls、self-contained replay、SQLite restore 后 implicit resume 等边界不会回归。

非目标：
- usage hint 不移植。它在原版只影响非 OpenAI cache token 展示/估算，不影响 OpenAI chat translator 或 Responses passthrough；Rust 只保留 OpenAI 链路时不需要该兼容层。

缺口/后续动作：
- 增加端到端测试：missing/unanswered tool calls 跳过、self-contained replay、WS not_found restore full-history、SQLite restore 后继续 implicit resume、variant identity 分支隔离。
