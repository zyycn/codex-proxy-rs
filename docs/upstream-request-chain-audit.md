# 上游请求链路审计

日期：2026-06-20

状态：已完成审计，并按确认方向推进实现代码。

## 目标

本轮只审计 OpenAI/Codex 上游请求链路：

- `/backend-api/codex/responses` HTTP/SSE
- `/backend-api/codex/responses` WebSocket
- `/backend-api/codex/responses/compact`
- `/backend-api/codex/models`
- `/backend-api/wham/usage` 这类账号/额度辅助入口

不把项目扩展成通用 proxy、VPN 或多 provider 网关；Anthropic、Gemini、OpenRouter、Ollama 等旧项目内容不进入本轮设计。

## 结论

1. WebSocket 连接应使用 OpenAI 原版锁定的 `openai-oss-forks/tokio-tungstenite` 完整握手，不再手写实际 opening request。
2. WebSocket 帧压缩不应长期维护自写 `PerMessageDeflateStream`，已切到 fork 内建 `permessage-deflate` 协商和解压/压缩状态。
3. `opening_request_text()` 继续作为审计快照保留；真实发送字节由 fork 根据 request headers 和 `WebSocketConfig` 生成。
4. HTTP/SSE header 不应按 WebSocket 抓包删到最小集合。原版和旧 TS 都会带 Codex compatibility headers，例如 `x-codex-turn-state`、`x-codex-window-id`、`x-codex-parent-thread-id`、`x-openai-subagent`、`x-codex-installation-id`。
5. 默认指纹先按 TS `config/default.yaml` 与 `config/fingerprint.yaml` 对齐；真实 Linux 抓包和 macOS 解包信息只作为后续验证证据，不直接覆盖 TS 基线。
6. TLS 层当前 Rust 和原版 Rust 都走 rustls/native roots/custom CA 思路，不能从 HTTP 抓包直接证明 TLS 指纹完全一致。先对齐原版 Rust 依赖和请求头，再用真实链路继续验证。
7. Chat 请求的模型后缀、默认 reasoning、默认 service tier、`fast -> priority` 和展示模型名先以 TS 当前实现为准，不从抓包或经验补规则。
8. 账号导入时的 `chatgpt-account-id` 补齐先按 TS：导入/持久化从 JWT claim 提取，发送链路只消费已解析出的 account id；不能在请求头层随意发空值或测试占位值。

## 基线规则

本轮不再“猜着写”。所有请求链路变更必须能对应到以下至少一个来源：

- TS 当前实现：`/home/zyy/桌面/Codes/codex-proxy`
- OpenAI 原版 Rust：`/tmp/openai-codex-src/codex-rs`
- 新的真实链路证据，并且已和 TS / 原版对比过

`docs/openai-res.txt` 的真实抓包用于验收和发现差异，但不能单独反推出全局白名单。比如该抓包的 WS opening 没有 `x-codex-installation-id`，而 TS 当前 HTTP/WS 统一 header map 会发送它；本轮先保持 TS 行为，再用新抓包验证。

## 证据索引

### 真实抓包

`docs/openai-res.txt` 中 WebSocket opening header 的顺序和集合是：

```text
Host
Connection
Upgrade
Sec-WebSocket-Version
Sec-WebSocket-Key
chatgpt-account-id
authorization
user-agent
originator
openai-beta
x-client-request-id
session-id
thread-id
x-codex-window-id
x-codex-turn-metadata
sec-websocket-extensions
```

其中 `sec-websocket-extensions` 为：

```text
permessage-deflate; client_max_window_bits
```

抓包中 `/backend-api/wham/usage` 只有较少 HTTP header：`user-agent`、`authorization`、`chatgpt-account-id`、`accept`、`cookie`。这说明不同上游入口的 header 集合不同，不能把 WS opening 的最小集合直接套到所有 HTTP 请求。

### 当前 Rust 实现

调整前 workspace 依赖：

- `tokio-tungstenite = 0.29.0`，未启用 fork 的 `deflate` 能力。
- `reqwest = 0.12.28`，使用 rustls native roots，启用 gzip/brotli/zstd/deflate/http2。
- `rustls = 0.23.36`，`tokio-rustls = 0.26.4`。

当前 WebSocket opening 审计快照仍按真实抓包顺序生成：

- `crates/adapters/src/codex/websocket/connect.rs`
- `CodexWebSocketConnection::responses`
- `opening_request_bytes`

当前 WS business headers：

- `chatgpt-account-id`
- `authorization`
- `user-agent`
- `originator`
- `openai-beta`
- `x-client-request-id`
- `session-id`
- `thread-id`
- `x-codex-window-id`
- `x-codex-turn-metadata`
- `x-openai-subagent`，仅在 request metadata 提供合法 subagent 时发送

调整前仍有自写 deflate 层：

- `crates/adapters/src/codex/websocket/deflate.rs`
- `PerMessageDeflateStream`
- `flate2::Decompress`
- 支持 server context takeover 后，真实 WS stream 已跑通

这说明“旧修复可用”，但维护面仍然偏大：自写代码需要自己处理 frame parsing、RSV1、continuation、context takeover、错误传播和后续 tungstenite 行为变化。当前实现已删除这层自写协议代码，并改用 fork 的 `connect_async_tls_with_config` 完成握手、扩展协商和 response 校验。

### OpenAI 原版 Rust

原版路径：

- `/tmp/openai-codex-src/codex-rs/Cargo.toml`
- `/tmp/openai-codex-src/codex-rs/codex-api/src/endpoint/responses_websocket.rs`
- `/tmp/openai-codex-src/codex-rs/core/src/client.rs`
- `/tmp/openai-codex-src/codex-rs/core/src/responses_metadata.rs`

原版锁定 fork：

```toml
tokio-tungstenite = { version = "0.28.0", features = ["proxy", "rustls-tls-native-roots"] }
tungstenite = { version = "0.27.0", features = ["deflate", "proxy"] }

[patch.crates-io]
tokio-tungstenite = { git = "https://github.com/openai-oss-forks/tokio-tungstenite", rev = "132f5b39c862e3a970f731d709608b3e6276d5f6" }
tungstenite = { git = "https://github.com/openai-oss-forks/tungstenite-rs", rev = "9200079d3b54a1ff51072e24d81fd354f085156f" }
```

原版 WebSocket 连接：

- 使用 `connect_async_tls_with_config`
- `WebSocketConfig.extensions.permessage_deflate = Some(DeflateConfig::default())`
- `false` 表示不禁用 Nagle
- 自定义 CA 存在时给 WebSocket 显式 rustls connector，避免 WS 和 HTTP 的 CA 策略不一致

原版 Responses header 语义：

- `x-client-request-id` 使用 thread id
- `session-id` / `thread-id` 来自 session metadata
- `x-codex-window-id`、`x-codex-turn-metadata`、`x-codex-parent-thread-id` 属于 compatibility headers
- `x-codex-turn-metadata` 的 canonical 来源是 `client_metadata["x-codex-turn-metadata"]`，HTTP/ws 直接 header 是兼容投影
- `x-openai-subagent` 支持 review、compact、memory、collab 等内部语义

### 旧 TS 项目

旧项目路径：

- `/home/zyy/桌面/Codes/codex-proxy/src/proxy/codex-api.ts`
- `/home/zyy/桌面/Codes/codex-proxy/src/proxy/ws-transport.ts`
- `/home/zyy/桌面/Codes/codex-proxy/src/fingerprint/manager.ts`
- `/home/zyy/桌面/Codes/codex-proxy/config/fingerprint.yaml`
- `/home/zyy/桌面/Codes/codex-proxy/src/proxy/cookie-jar.ts`

有价值的差异线索：

- `src/fingerprint/manager.ts:91-113` 的 `buildHeaders` 明确只做这些事：写 `Authorization`，用传入 `accountId` 或 JWT claim 补 `ChatGPT-Account-Id`，写 `originator`，合并 `buildRawDefaultHeaders`，最后按 `fingerprint.header_order` 排序。
- `buildRawDefaultHeaders` 只展开 `User-Agent`、`sec-ch-ua`，再合并 `fingerprint.yaml` 的 `default_headers`。
- `src/proxy/codex-api.ts:createResponseViaHttp` 和 `createResponseViaWebSocket` 在统一 base headers 上追加 `OpenAI-Beta`、`x-openai-internal-codex-residency`、`x-client-request-id`、`x-codex-installation-id`、session/window/context headers。
- `ws-transport.ts` 使用 Node `ws` 完整握手，业务 headers 通过构造参数传入；没有手写 raw opening，也没有手写 `Sec-WebSocket-Extensions`。
- `buildWsConstructorOpts` 只传 `{ headers }` 和可选 agent；`ws@8.19.0` 客户端默认 `perMessageDeflate: true`，因此扩展 offer 由库生成。
- `ws@8.19.0` 的 `WebSocketServer` 默认 `perMessageDeflate: false`。旧 TS 测试 helper 若未显式打开 server deflate，不代表真实客户端不会发送扩展 offer。
- WebSocket 升级响应头通过 `upgrade` 事件采集后转回 SSE response headers。
- WebSocket 打开后发送 `response.create` JSON 文本帧，并用 25s ping 保活。
- `buildConversationIdentity` 用账号作用域派生 `cp_...` conversation id 和 `cw_...` window id。
- HTTP 与 WS 都会补 `OpenAI-Beta: responses_websockets=2026-02-06`。
- HTTP/SSE 会带 `Accept: text/event-stream`，compact 不带。
- `x-codex-installation-id` 同时出现在 header 和 `client_metadata`。
- `x-codex-turn-metadata`、`x-codex-beta-features`、`x-responsesapi-include-timing-metrics`、`Version`、`x-codex-parent-thread-id` 都是条件性透传。
- cookie 自动捕获只允许 `cf_clearance`，不自动持久化 `__cf_bm`。
- `CodexApi.captureCookies()` 的调用点在 transport response 返回后立即执行：HTTP SSE 和 compact 都在状态码判断前 capture；WebSocket 升级响应 header 由 `ws-transport.ts` 采集后随 response 返回；warmup 路径也会 capture，但当前账号导入链路禁用 warmup，普通 `getUsage()` 不做 capture。
- `fingerprint.yaml` 是 HTTP header 指纹基线，不是 TLS/JA3 指纹；其中定义 UA 模板、认证域白名单、header 顺序和默认浏览器态 header。
- `auth_domains` / `auth_domain_exclusions` 在当前 TS HEAD 只进入 schema，未找到请求发送链路使用点。
- `src/auth/account-registry.ts:addAccount` 和 `src/auth/account-persistence.ts:normalizeAccountEntries` 都从 JWT 中回填 `accountId`、`userId`、`planType`、`email`。因此 Rust 导入阶段刷新并解析 claims 后写入 `account_id`，是和 TS 一致的方向；请求发送阶段不需要引入 `proxyApiKey`、`proxy_api_key` 等 sub2api 私有字段。

不应迁移的内容：

- 多 provider 适配逻辑。
- 通用 proxy/VPN 语义。
- 旧迁移兼容测试。
- 与 OpenAI/Codex 无关的历史抽象。

## 三方对比

### WebSocket opening

| 项目 | 生成方式 | 风险 |
| --- | --- | --- |
| 真实抓包 | Codex Desktop 发送的最小 opening header | 当前最可信的行为目标 |
| 当前 Rust | fork `tokio-tungstenite` 完整握手 | 与原版一致；header 顺序由库生成 |
| OpenAI 原版 Rust | fork `tokio-tungstenite` 自动握手 | deflate 协商和状态可靠 |
| 旧 TS | Node `ws` transport | 同样由库生成 opening 并处理 permessage-deflate |

判断：TS 版和 OpenAI 原版都不是手写 raw opening。为了拿到 fork 内部 negotiated extension state，Rust 当前也应使用 fork 完整握手；审计快照只用于排查差异。

### WebSocket permessage-deflate

| 方案 | 优点 | 风险 |
| --- | --- | --- |
| 当前自写 `PerMessageDeflateStream` | 已真实跑通；不改依赖 | 协议维护面大，容易漏 frame/extension 边界 |
| OpenAI fork `tungstenite` | 与原版同源；支持协商和上下文 | 引入 git patch；版本从当前 0.29 回到原版 0.27/0.28 组合 |
| 旧 TS `ws@8.19.0` | 客户端默认开启 `perMessageDeflate`，无需项目手写扩展头 | 测试 server 默认关闭 deflate，不能用测试 helper 默认值反推真实客户端 opening |

判断：已切 fork，删除自写 deflate。运行时风险小于继续维护自写协议层。

### HTTP/SSE header

当前 Rust HTTP/SSE 已覆盖主要 Codex 头：

- `authorization`
- `chatgpt-account-id`
- `originator`
- `user-agent`
- `content-type`
- `accept: text/event-stream`
- `openai-beta`
- `x-openai-internal-codex-residency`
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
- `x-openai-subagent`
- `cookie`

注意：

- `session_id` 是 TS 当前 WebSocket/HTTP SSE 都会发送的兼容字段；`v2.0.78-beta.15c49d6` 和当前 `dev/v2.0.80` 都没有把它转换为 `session-id` / `thread-id`。
- WS opening 真实抓包和 OpenAI 原版 Rust 用的是 `session-id` / `thread-id`，不是 `session_id`。这是后续真实链路验证项，本轮不在 Rust 侧猜测改写。
- 当前 WS opening 按 TS `buildHeaders` + `applyCodexContextHeaders` 语义从统一 header map 投影：过滤 HTTP/SSE 专用的 `content-type`、`accept`，保留 `session_id`、fingerprint 默认头、cookie、`x-openai-internal-codex-residency`、`x-codex-installation-id` 和各类 Codex context 条件头。
- `docs/openai-res.txt` 的一次真实 WS opening 抓包是最小集合，但 TS 当前实现会发送更多兼容/条件头；本轮基线先与 TS 保持一致，不用手写白名单猜测。
- `x-codex-turn-metadata`、`x-codex-beta-features`、`x-responsesapi-include-timing-metrics`、`x-codex-window-id`、`x-codex-parent-thread-id` 的 canonical 来源允许是 `client_metadata`；Rust 已按 TS `firstRequestString` 语义补齐“直接字段优先，metadata 兜底”。这会影响 account-scoped window id、HTTP header 投影和 WS business header 投影。
- `x-openai-subagent` 在 TS WebSocket 和 HTTP 路径都会从 `client_metadata` 规范化后写入 header；OpenAI 原版 Responses stream 也会写该 header。Rust 已补齐 WS header 投影。

### Chat 模型选项

TS 参照：

- `src/translation/openai-to-codex.ts` 在 Chat 翻译阶段解析模型名后缀，并按“显式请求字段 > 模型后缀 > 配置默认值”的顺序设置 `reasoning.effort` 和 `service_tier`。
- `src/routes/chat.ts` 在翻译后判断 `codexRequest.reasoning?.effort`，因此后缀或配置默认 reasoning 也会影响 Chat stream 输出格式。
- `src/routes/chat.ts` 使用 `buildDisplayModelName(parseModelName(req.model))` 作为 OpenAI Chat 响应中的展示模型名。
- `src/routes/shared/proxy-request-preparation.ts` 在带 reasoning 且 include 为空时补 `["reasoning.encrypted_content"]`。
- `src/proxy/codex-api.ts` 的 `normalizeServiceTierForUpstream` 在真正发 HTTP/WS 前把 `fast` 映射为 `priority`。

Rust 对齐结果：

- `/v1/chat/completions` handler 在 `translate_chat_to_codex` 后立即调用 `apply_response_model_options`，让 suffix/config/default 对 `include_reasoning` 和 Chat stream 可见。
- `ChatDispatchService::complete` 也调用同一个函数兜底，避免非 handler 调用绕过模型选项。
- Chat stream 响应模型名改为 `ModelCatalog::build_display_model_name(parsed_model)`，与 TS display model 规则一致。
- `apply_response_model_options` 继续复用 Responses 路径的 `fast -> priority` 和 reasoning include 规则，避免 Chat/Responses 分叉。

### WebSocket payload

TS `src/proxy/codex-api.ts:createResponseViaWebSocket` 的 `wsRequest` 构造顺序是：

```text
type
model
instructions
input
store
stream
previous_response_id?
reasoning?
tools?
tool_choice
parallel_tool_calls
text?
service_tier?
prompt_cache_key?
include?
client_metadata
```

对齐判断：

- `instructions` 使用 TS 语义 `request.instructions ?? ""`，空字符串也必须发送；真实上游会在缺失该字段时返回 400。
- `tool_choice` 和 `parallel_tool_calls` 即使客户端未传也按 TS 补默认值：`auto`、`true`。
- `reasoning`、`tools`、`include` 不再发送旧 Rust 快照里的 `null` / 空数组；TS 只在有值或非空数组时赋值。
- `generate` 不属于 TS 当前 `WsCreateRequest`，WebSocket payload 不按 Rust 旧测试快照额外发送。
- `docs/openai-res.txt` 中原版抓包的 payload 字段集合可作为后续原版对照证据，但本轮先以 TS 当前实现为 Rust 基线。

## 指纹审计

TS 当前 `config/fingerprint.yaml`：

```yaml
user_agent_template: "Codex Desktop/{version} ({platform}; {arch})"
auth_domains: ["chatgpt.com", "*.chatgpt.com", "openai.com", "*.openai.com"]
auth_domain_exclusions: ["ab.chatgpt.com"]
default_headers:
  Accept-Encoding: "gzip, deflate, br, zstd"
  Accept-Language: "en-US,en;q=0.9"
  sec-ch-ua-mobile: "?0"
  sec-ch-ua-platform: '"macOS"'
  sec-fetch-site: "same-origin"
  sec-fetch-mode: "cors"
  sec-fetch-dest: "empty"
```

对应 TS 发送逻辑：

- `buildHeaders` / `buildHeadersWithContentType` 负责展开 UA、补 `sec-ch-ua`、合并默认 headers 并按 `header_order` 排序。
- `sec-ch-ua` 来自 `client.chromium_version`，不是写在 `fingerprint.yaml` 里。
- `auth_domains` / `auth_domain_exclusions` 当前只在 config schema/loading 里出现，没有发现实际发送链路使用点，不能作为 Rust 侧必须新增的运行时逻辑。

对 Rust 的影响判断：

- 这个 YAML 可以作为“默认 HTTP header 指纹基线”的证据，不应被理解成 TLS/JA3/ALPN 指纹配置。
- Rust 侧当前不需要引入同名 YAML 运行时配置；先保持 `Fingerprint::default_codex_desktop()` 这类强类型默认值，减少外部配置漂移面。
- `header_order` 在 TS 配置里是原始大小写，Rust 内部统一用小写 key 排序；这不影响实际 HTTP header 语义，但真实抓包排查时要按 wire 输出核对。
- `auth_domains` / `auth_domain_exclusions` 如果后续要启用，应该只用于 cookie/auth header 作用域校验，不能自动扩大成通用 proxy 域名白名单。

TS 默认指纹基线：

```text
originator: Codex Desktop
app_version: 26.519.81530
build_number: 3178
platform: darwin
arch: arm64
chromium_version: 146
user-agent: Codex Desktop/{version} ({platform}; {arch})
sec-ch-ua-platform: "macOS"
```

真实 Linux 抓包中的 UA：

```text
Codex Desktop/0.140.0 (Debian 13.0.0; x86_64) gnome-terminal (Codex Desktop; 26.609.71450)
```

macOS Codex Desktop 解包与探测来源：

- `/home/zyy/桌面/Codes/codex-desktop-linux/codex-app/resources/app.asar`
- `/tmp/codex-desktop-fingerprint/fingerprint-report.md`

确认到的 macOS arm64 UA：

```text
Codex Desktop/26.609.71450 (Macintosh; Intel Mac OS X; arm64)
```

风险：

- TS 默认指纹、真实 Linux 抓包和 macOS 解包信息不一致，不能在没有 TS 证据时自行混合。
- HTTP 默认 header 里仍有 macOS `sec-ch-ua-platform`，而真实 WS opening 不带 `sec-ch-*`。
- 如果上游把 UA、account id、cookie、TLS、IP、timing 关联起来，指纹漂移会增加风控风险。

当前对齐原则：

1. 先以 TS `config/default.yaml` + `config/fingerprint.yaml` 为 Rust 默认基线：版本 `26.519.81530`、构建 `3178`、平台 `darwin`、架构 `arm64`、Chromium `146`。
2. 后续仍应支持从真实导入/配置中持久化当前使用的 Codex Desktop 指纹。
3. WS opening 继续不带 `sec-ch-*`。
4. HTTP/SSE 是否保留 `sec-ch-*` 需要用真实抓包继续确认；在没有 HTTP `/codex/responses` 完整抓包前，不做大删减。

Rust 当前状态：

- `crates/core/src/gateway/fingerprint.rs` 已硬编码同一组默认 headers 和 header 顺序。
- 默认 UA 已对齐 TS 展开结果：`Codex Desktop/26.519.81530 (darwin; arm64)`。
- auto-update 没有 extracted Chromium 时保留已有 Chromium；首次无历史值时使用 TS 默认 `146`，不再回退到旧 `142`。
- `crates/adapters/src/codex/client.rs` 的 HTTP/SSE 路径会发送 `sec-ch-ua` 和 `fingerprint.default_headers`；WS opening 路径按 TS 当前实现从统一 Codex header map 投影，过滤 HTTP/SSE 专用 `content-type`、`accept`，保留 `session_id`，不按抓包最小集合手写白名单。

## TLS 与网络安全面

当前 Rust HTTP：

- `reqwest`
- rustls native roots
- 自定义 CA 支持
- `.no_proxy()`
- 可配置 `force_http11`
- 默认保留 HTTP/2

当前 Rust WS：

- fork `connect_async_tls_with_config`
- rustls native roots/custom CA
- 不禁用 Nagle
- opening response 和 permessage-deflate 协商由 fork 校验

OpenAI 原版 WS：

- fork `tokio-tungstenite`
- rustls native roots
- 自定义 CA connector
- 不禁用 Nagle
- 内建 permessage-deflate

审计判断：

- 目前与原版 Rust 的 WS/TLS 策略方向一致。
- `.no_proxy()` 符合本项目“不做 proxy/VPN”的定位。
- 仅凭 `docs/openai-res.txt` 不能判断 TLS ClientHello/JA3/ALPN 是否完全一致。
- 如果后续真实链路仍被风控，需要单独抓 TLS 指纹和 ALPN，而不是继续猜 header。

## Cookie 审计

当前 Rust 与旧 TS 的关键策略一致：

- 自动捕获只持久化 `cf_clearance`。
- 不自动持久化 `__cf_bm`。
- 请求时按 domain/path/expiry 过滤 cookie。
- Cloudflare challenge 或 path-block 时会清理账号 cookie 并进入冷却/禁用路径。
- HTTP/SSE、compact、stream 和 WebSocket 升级成功响应都会把上游 `Set-Cookie` 交给 runtime capture。
- HTTP/SSE 和 compact 的非 2xx 响应会通过 `CodexClientError::Upstream.set_cookie_headers` 带回 cookie，再由 runtime capture；这对齐 TS “收到 transport response 先 capture，再按状态码分类”的时机。
- WebSocket 错误帧本身没有 HTTP response headers，按空 cookie 列表处理；WebSocket opening 失败如果带 `Set-Cookie`，会从握手响应头传回。

这个方向合理。`__cf_bm` 与 IP、UA、TLS、时间强绑定，重放可能比不带 cookie 更差。

## 使用 OpenAI fork 的风险

收益：

- 与 OpenAI 原版 Codex Rust 同源。
- 解决真实 WS `permessage-deflate` 的长期维护问题。
- fork 已有 extension negotiation、context takeover、压缩/解压测试。
- 避免手写 opening 后无法访问 fork 私有 negotiated extensions 状态的问题。

风险：

- 引入 git patch，依赖供应链需要固定 rev。
- 当前项目 `tokio-tungstenite` 是 0.29，原版组合是 `tokio-tungstenite 0.28` + `tungstenite 0.27`，存在 API 细节回退风险。
- `[patch.crates-io]` 会影响整个 workspace 内所有 `tungstenite` / `tokio-tungstenite` 使用点。
- fork 不是 crates.io 正式发布版本，后续升级需要跟 OpenAI 原版同步审计。

风险控制：

- 固定到原版相同 rev。
- 只改 WS transport 相关代码，不顺手改账号、导入、日志。
- 保留 WS opening 审计快照测试。
- 新增最小的 deflate context takeover 行为测试，替代自写 deflate helper 测试。
- 跑真实 `gpt-5.5` HTTP/SSE + WS stream 复测。

## 本轮实现项

1. Cargo 依赖切到 OpenAI 原版 fork。
2. `connect.rs` 改用 fork `connect_async_tls_with_config` 完整握手。
3. 通过 `WebSocketConfig.extensions.permessage_deflate = Some(DeflateConfig::default())` 让 fork 生成 offer 并校验上游 agreed extensions。
4. 删除 `crates/adapters/src/codex/websocket/deflate.rs` 和相关 type alias。
5. 保留 WS opening 审计快照测试。
6. 适配器层 deflate 行为由 fork 内建握手覆盖，不再维护自写压缩帧。
7. 默认 Codex Desktop 指纹先对齐 TS 配置：`26.519.81530 / 3178 / darwin / arm64 / Chromium 146`。
8. OpenAI Responses 翻译层补齐 TS `firstRequestString` 语义：直接字段为空时从 string-only `client_metadata` 读取 Codex context 字段。
9. WebSocket header 投影回到从统一 Codex header map 派生，过滤 HTTP/SSE 专用头，保留 TS `createResponseViaWebSocket` 会发送的 `session_id`、`x-codex-turn-state`、`x-codex-installation-id`、`x-openai-internal-codex-residency`、`x-openai-subagent` 等条件头。
10. `/v1/chat/completions` 的 `stream:true` 复用 Responses stream 调度链路，并按 TS `streamCodexToOpenAI` 语义输出 OpenAI `chat.completion.chunk` SSE。
11. WebSocket `response.create` payload 按 TS 字段顺序和可选字段规则序列化：保留空 `instructions`，省略空 `tools` / `include` 和 `reasoning:null`。
12. `/v1/chat/completions` 的模型后缀、reasoning、service tier 和 display model 已按 TS Chat 路径对齐；定向测试固定了 `gpt-5.5-high-fast` 上游发送为 `model=gpt-5.5`、`service_tier=priority`、`reasoning.effort=high`，并验证 stream reasoning delta 会转为 OpenAI `reasoning_content`。
13. Chat 非流和 compact 成功路径已接入结构化事件日志，复用现有 `record_response_event`，按 route 区分 `v1.chat` / `v1.response` 并记录 accountId、usage、rate-limit headers；chat 非流失败耗尽和 compact 终态失败路径补充 `level=error`、`failureClass`、`exhaustedCount`、`upstreamError`。
14. Responses stream 启动阶段终态失败已接入结构化事件日志：账号耗尽、dirty quota 校验达到最大次数、模型不支持重试耗尽、首帧前空响应 / 缺 completion / SSE 解析失败都会记录 `stream=true`、`transport`、`failureClass`，与 TS `recordStreamCloseEvent` 的排查目标保持一致，但落到本项目现有 `event_logs`。
15. cookie capture 时机按 TS 补齐：业务响应成功、stream 启动成功、compact 成功、HTTP/compact upstream error、WebSocket opening error 都会消费上游 `Set-Cookie`，底层仍只允许自动持久化 `cf_clearance`。

## 验证顺序

1. `cargo fmt`。
2. `cargo check -p codex-proxy-runtime -p codex-proxy-server`。
3. `cargo test -p codex-proxy-adapters --test codex`。
4. `cargo test -p codex-proxy-server --test openai_chat_upstream openai_chat_routes::`。
5. `cargo test -p codex-proxy-core --test protocol protocol_openai_chat:: -- --nocapture`。
6. 重启服务，用自然请求文本和 `gpt-5.5` 跑真实 HTTP/SSE、WS 和 chat stream。

已完成的真实验证：

- `/v1/chat/completions` + `stream:true`：请求 `9805212d-401f-4a10-a8f1-3d5bc422299e` 返回 HTTP 200、`text/event-stream`、OpenAI `chat.completion.chunk`、final usage 和 `data: [DONE]`，无 error SSE。
- `/v1/chat/completions` + `stream:true` 日志复测：请求 `dc22ea52-9f74-428f-8fe5-3b1a3def0114` 返回 HTTP 200、`text/event-stream`、OpenAI `chat.completion.chunk`、final usage 和 `data: [DONE]`，管理端事件为 `kind=v1.chat`、`route=/v1/chat/completions`、`metadata.apiKind=chat`、`metadata.completed=true`。

真实请求验证时避免 `test`、`probe`、`real_chain` 这类显眼测试语义，请求 ID 使用 UUID 形态，请求文本使用正常用户表达。

## 后续仍需审计

- HTTP `/codex/responses` 的真实抓包 header，需要和当前 Rust 的 HTTP/SSE 逐项比对。
- 原版 attestation header 何时生成；当前项目是否需要支持，不能盲目伪造。
- `x-openai-internal-codex-residency` 在当前真实账号链路中的必要性。
- `version` header 的真实来源和是否仍被上游使用。
- TLS ClientHello/ALPN 是否需要与原版二进制进一步对齐。
- 账号导入时是否能补齐真实 `account_id`、指纹、installation id、cookie 状态。

## 当前建议

- WS opening/deflate 使用 OpenAI fork 完整握手。
- HTTP/SSE 保留 Codex compatibility headers，不按 WS 抓包全删。
- 后续真实链路继续以 `gpt-5.5`、自然请求文本、真实账号状态为准。
