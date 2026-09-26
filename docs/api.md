# Codex Proxy RS 接口

本文列出 v3 源码中的公开 HTTP 接口，路由以
`backend/crates/gateway-api/src` 中的 router 为准。配置 Codex 请先看 [客户端配置](../deploy/README.md#客户端配置)；
运行实例是否包含这些功能，应结合其版本和 revision 确认。

| 查找内容 | 入口 |
| --- | --- |
| 鉴权与响应 | [公共约定](#1-鉴权与公共约定) · [浏览器会话与 Key 自助页](#4-浏览器认证) |
| 客户端协议 | [Responses、Images、模型与 Provider 操作](#3-openai-数据面与模型目录) · [额度查询](#api-key-额度查询) |
| 账号与授权范围 | [账号](#5-账号) · [代理](#独立代理管理--managed-proxies) · [分组](#6-账号分组) · [Client Key](#7-client-key) |
| 系统与运维 | [健康检查](#2-健康检查) · [运行设置](#8-运行设置) · [备份](#9-备份) · [统计与诊断](#10-dashboard用量与错误) · [版本与更新](#11-版本更新与重启) |
| 插件 | [安装包与来源](#安装包与来源) · [运行实例](#121-运行实例) · [管理扩展](#122-管理扩展页面与原始-api) |

## 1. 鉴权与公共约定

### OpenAI 数据面客户端接口

默认情况下，`/v1/*` 使用管理端创建的 Client Key：

```http
Authorization: Bearer sk_...
```

自动生成的 Key 保持 `sk_` 格式；迁入的自定义 Key 使用保存时的原值，不限制前缀或固定长度。
无论格式如何，只有已保存且启用的 Client Key 能通过鉴权。

启用[入口认证插件](../backend/crates/gateway-plugin/sdk/docs/capabilities.md#数据面入口认证)时，
`Authorization` 可按插件协议校验，并由宿主将外部身份映射到明确绑定的 Client Key。
账号范围、模型权限、并发和预算仍由该 Key 决定；插件认证不接管管理员或 Key 自助页登录。

Codex 原生生图配置还会携带 `X-OpenAI-Actor-Authorization: proxy-managed`。
它仅用于客户端识别服务端托管认证，不能代替 Client Key。网关和 OpenAI Provider 都会过滤该请求头，
上游账号身份只由服务端选中的账号提供；不要把真实账号 token 放进该标记。

Client Key 通过账号分组限定路由范围：未绑定分组时可使用全部账号，绑定一个或多个分组时只能使用
已启用分组成员的并集。分组可以混合 `openai` 与 `xai` 账号；同一请求只会在模型能力明确匹配且满足
重放安全边界时跨 Provider fallback。

运行设置可以分别配置 `minCodexDesktopVersion` 与 `minCodexCliVersion`。两者只接受 SemVer，`null`
表示不限制。API 在 Client Key 鉴权成功后识别官方 Desktop/CLI 请求头；适用门禁的客户端没有合法版本，或版本
低于对应门槛时，除只读 `/v1/usage` 外的 `/v1/*` HTTP 请求和新 WebSocket 握手在访问上游前返回 `426 Upgrade Required`。
未知客户端保持兼容，不应用版本门禁。

Desktop 应用版本优先取 `version` 头，未提供时取 User-Agent 中的 `(Codex Desktop; <版本>)`。
ChatGPT 远程控制使用 `(codex_chatgpt_<平台>_remote; <版本>)` 形式的 User-Agent 后缀，已知平台包括
`android` 和 `ios`。网关按该命名格式识别非空的平台名，后缀须完整，平台名和版本均不能含空白、括号或分号。
这类 Desktop 请求未提供应用版本时不应用版本门禁；Core 版本和
远程客户端版本不能替代 Desktop 应用版本，因此也无法保证其满足 Desktop 最低版本要求。
携带 `version` 头或 Desktop 应用版本后缀时仍按上述规则校验，非法版本不会因远程标记而放行。

低版本响应使用 OpenAI 风格错误格式：

```json
{
  "error": {
    "message": "Codex CLI 0.151.0 is below the minimum required version 0.152.0. Upgrade Codex CLI and retry.",
    "type": "invalid_request_error",
    "code": "client_version_too_old",
    "client": "codex_cli",
    "current_version": "0.151.0",
    "min_version": "0.152.0"
  }
}
```

因缺失或非法版本被门禁拒绝时，`code` 为 `client_version_unavailable`，`current_version` 为 `null`。

### 管理接口

所有 `/api/admin/*` 请求都需要以下任一鉴权方式：

- 管理员登录后得到的 `cpr_session` Cookie（服务端会话身份必须为 `admin`）；
- `x-api-key: <admin-api-key>`。

同源管理端的登录和退出根据浏览器 `Origin` 自动设置会话 Cookie：HTTP 来源省略 `Secure`，
HTTPS 来源以及缺失、`null` 或非法来源保留 `Secure`。`HttpOnly`、`SameSite=Lax` 始终保留。
不根据 `X-Forwarded-Proto` 等转发头降级；HTTPS 反代使用 HTTP 回源不影响 Cookie。
部署要求见 [公网访问](../deploy/README.md#公网访问)。

请求无需自带 `x-request-id`；缺失时服务端自动生成 UUID 并在响应头回传同一 request ID。
`api.request_id_header` 可改变注入与回传的 header 名，管理端鉴权不依赖该名字。
管理端 JSON 与动态响应统一带 `Cache-Control: no-store`；插件静态资源按下述资源接口的私有缓存策略返回。

配置了 CORS 白名单 origin 时，跨域请求以凭据模式放行，仅允许 `GET`/`POST` 方法和
`authorization`、`content-type`、`x-api-key` 与 request ID 四个请求头，不使用通配符。

普通成功响应使用以下信封：

```json
{
  "code": 200,
  "message": "OK",
  "data": {}
}
```

所有 `/api/admin/*` 错误（包括 JSON/Query rejection、未知路由和错误 HTTP method）统一返回
`application/json`：

```json
{
  "code": 40001,
  "message": "请求参数不合法",
  "data": null
}
```

管理端本地产生的 `message` 是可安全展示的中文文案，不包含内部异常或原始上游正文。稳定业务码如下：

| HTTP | `code` | 含义 |
| ---: | ---: | --- |
| 400 | `40000` | 请求体不是合法 JSON |
| 400 / 405 / 415 / 422 | `40001` | 通用请求、方法、Content-Type 或字段错误；HTTP 状态保留具体语义 |
| 400 | `40002` | 时间范围不合法 |
| 401 | `40101` / `40102` / `40103` | 会话缺失、过期或吊销 / 登录凭据错误 / 管理 API Key 错误 |
| 403 | `40301` | 有效会话的身份无权访问目标接口；不清除会话 |
| 404 | `40401` | 资源或管理接口不存在 |
| 409 | `40901` | 资源状态冲突 |
| 429 | `42901` | 登录尝试过多 |
| 429 | `42902` | 管理操作请求过于频繁；安全文案可说明具体受限操作 |
| 500 | `50001` | 服务内部错误 |
| 502 | `50201` | 上游服务请求失败 |
| 502 | `50202` | 不可逆上游操作的执行结果未知；刷新状态后再决定是否重试 |
| 503 | `50301` | 依赖服务暂不可用 |

未知 `/api/admin/*` 路径使用 `40401`，不会落入 SPA；已存在路径使用错误 method 时返回 `405`、
`40001`，并保留标准 `Allow` header。request ID 继续通过配置的响应 header 返回。

`50201`、`50202` 和 `50301` 的 `message` 提供可安全展示的具体原因；缺少安全消息时使用通用提示。
认证错误和未知内部错误使用固定文案，不公开 Provider 内部 message、原始响应或凭据。

手动刷新令牌时，按错误类型处理：

| `code` | 场景 | 调用方处理 |
| --- | --- | --- |
| `40901` | 刷新容量、账号租约占用或账号快照冲突 | 按提示等待，或重新查询账号后操作 |
| `40001` | 缺少或无效的凭据、已确认刷新令牌失效或账号停用 | 检查凭据或重新授权 |
| `50201` | 明确的上游失败，且未归类为凭据永久失效 | 根据安全消息处理上游拒绝、限流或服务异常 |
| `50202` | 无法确认令牌轮换结果，包括 xAI 返回无效的新凭据 | 先核对账号状态，不要自动重发可能已轮换的一次性令牌 |
| `50301` | 刷新服务或依赖暂不可用 | 检查出站连接与依赖服务后重试 |

OpenAI 上游 `401` 按 `50201` 返回，不代表管理员会话失效，也不要求管理端重新登录。
Codex PAT 验证服务不可用和身份响应无效分别返回 `50301`、`50201` 及对应的安全提示。

### 管理写入一致性

管理写入不要求客户端提供全局配置版本。会改变路由快照或安全配置的写入由后端在事务内推进
内部 `config_revision`，并用于快照发布与审计。账号更新和分组查询/写入的部分响应会返回
`configRevision` 作为已提交事实，但它不是客户端 mutation 的前置条件。
个别资源另有自己的并发检查，如代理的 `revision` 和插件实例的 `expectedRevision`；按对应接口提交，不与全局版本混用。

## 2. 健康检查

| 方法 | 路由 | 鉴权 | 说明 |
| --- | --- | --- | --- |
| `GET` | `/healthz` | 无 | Core、Store 和后台任务健康时返回 `204`，否则返回 `503` |

## 3. OpenAI 数据面与模型目录

除下述 Responses 入站解压保护外，Responses、Images 和 standalone Search HTTP body、
WebSocket message 和 frame 不设置网关私有长度上限；协议可接受性由上游决定。

| 方法 | 路由 | 说明 |
| --- | --- | --- |
| `POST` | `/v1/responses` | OpenAI Responses JSON；`stream=true` 返回 SSE，否则返回完整 JSON |
| `GET` | `/v1/responses` | 通过 HTTP Upgrade 建立 Responses WebSocket |
| `POST` | `/v1/alpha/search` | Codex standalone web search；JSON 请求与响应正文原样转发 |
| `POST` | `/v1/images/generations` | 通过 OpenAI Provider 发起图像生成；JSON 请求与响应正文原样转发 |
| `POST` | `/v1/images/edits` | 通过 OpenAI Provider 发起图像编辑；JSON 请求与响应正文原样转发 |
| `GET` | `/v1/models` | 返回当前 Client Key 账号范围内各 Provider 的可用公开模型并集；有两种响应形态，见下 |
| `GET` | `/v1/models/{model_id}` | 返回 OpenAI 兼容的单模型详情 |
| `POST` | `/v1/providers/{provider}/models/{model}/count_tokens` | 调用指定 Provider/model 声明的精确 Token 计数；请求和响应均为 JSON |
| `GET` / `POST` | `/v1/providers/{provider}/http/{endpoint}` | 调用指定 Provider 预先声明的固定 HTTP 操作；不是任意 URL 代理 |
| `GET` | `/v1/usage` | 查询当前 Client Key 的日与周额度，仅使用网关已结算的 USD 账本 |

### Provider 自有操作

这两类 `/v1/providers/*` 路由使用同一 Client Key 鉴权、账号组范围、Provider 账号资格、租约、
并发/频率准入、请求记录和发送事实。`provider`、`model` 与 `endpoint` 都是已发布目录中的稳定 ID；
宿主不会接受客户端提供的上游 URL，也不会把下游 Authorization、Cookie、API Key、Host、转发头或
`Connection` 声明的逐跳头交给插件。上游账号认证只来自宿主选中的账号。

`POST /v1/providers/{provider}/models/{model}/count_tokens` 接受最多 8 MiB 的合法 JSON，且只路由到
为该模型明确声明 `count_tokens` 的 Provider。Core 不解释或重写请求/响应 JSON，响应必须是插件按其
声明的本地精确 tokenizer 或上游精确计数合同返回的原始 JSON；没有准确实现时返回不支持，不从 usage
或字符数推算。响应固定使用 `application/json` 并重算 `Content-Length`。

`GET|POST /v1/providers/{provider}/http/{endpoint}` 只允许 Provider descriptor 中该 endpoint 声明的
方法；`HEAD`、其他方法、未声明 endpoint 和未声明的方法在出站前拒绝。GET 不能携带正文，POST 正文
最多 8 MiB；query 作为不透明值交给目标 Provider 再按其固定合同校验，不能改变上游 origin 或路径。
成功响应由最多 64 KiB 的有序原始片段组成，聚合后最多 8 MiB，字节不经 JSON 转码。宿主保留允许的
`Content-Type` 和安全业务响应头，移除认证、`Set-Cookie`、`Location`、逐跳、压缩及 framing 头，
并按最终正文重算 `Content-Length`；缺少媒体类型时使用 `application/octet-stream`。Provider 的结构化
失败仍沿用数据面错误映射和已确认的上游状态/可见正文边界，不把非 2xx 成功封套当作成功。

Codex 的 review 等子代理请求仍使用 `/v1/responses`，并通过 `x-openai-subagent` 请求头携带子代理类型；
网关不提供独立的子代理请求路径。

### Responses 请求与传输

`POST /v1/responses` 在鉴权后按 `Content-Encoding` 解压，再解析 JSON；支持单一 `gzip`、
`deflate`（zlib 封装）和 `zstd`，缺省、空值或 `identity` 直接使用原始正文。gzip 多成员与 zstd
多帧连续解码，整体展开结果受运行设置 `responsesMaxDecompressedBodyBytes` 约束（默认 64 MiB），
超限在继续展开前返回 `400 request_too_large`，错误信息包含当前请求的上限字节数。zstd
回溯窗口独立固定为最多 64 MiB，不能满足该限制的帧按解码失败处理。这个限制保护入站解压资源，不是
模型上下文或 Token 上限；未压缩正文不受此长度限制。
不支持的编码、逗号分隔的叠加编码和重复 `Content-Encoding` 头返回
`400 unsupported_content_encoding`；压缩正文损坏、截断或解压后不是合法 JSON 返回
`400 invalid_json`。本地错误不包含原始正文或解压库细节。WebSocket 文本帧不经过这条解压路径。

Responses 不透传下游的逐跳头、反代元数据（如 `cf-*`、`x-forwarded-*`、`forwarded`、`via`、
`cdn-loop`）以及 `Accept-Encoding` / `Content-Encoding`。链路元数据和编解码能力
由各段传输层独立管理；其余业务扩展头继续透传，不使用固定业务头白名单。
此规则同时适用于上游 HTTP 和 WebSocket，不影响上游响应的 `cf-ray` 等诊断信息。
API Key 与 OAuth 共用模拟客户端画像（`User-Agent`、`originator`、`version`）和业务头透传规则，
包括会话、线程、Lite 和其他业务扩展头。
上游认证只来自选中的账号；API Key 不携带 OAuth Cookie、ChatGPT 账号身份或下游的
`X-OpenAI-Actor-Authorization` 托管认证声明。

Responses 也不透传 `x-stainless-*`、`Origin`、`Referer`、`sec-ch-ua*` 和 `sec-fetch-*`
携带的下游 SDK/浏览器环境或页面来源。过滤规则适用于所有下游客户端，与 User-Agent 无关；
原始入站头仍供 CORS、鉴权和本地观测使用。
`session_id` 请求头仅作为入站会话别名，上游通过 `session-id` 表达；
两者同时存在时仍优先使用 `session-id`。正文中的 `client_metadata.session_id`、
`prompt_cache_key` 不受这条请求头规则影响。
`thread-id`、turn metadata 等 Codex 协议字段及未知业务扩展不受下游环境头过滤规则影响；
`traceparent`、`tracestate` 不因属于追踪字段而被删除。

Responses 上游编码会移除 Codex 不接受的顶层 `temperature`、`max_output_tokens` 和
`prompt_cache_retention`。缺少顶层 `store` 时补齐 `false`，与官方 Codex 客户端一致；
显式提供的值保持原样。顶层 `input` 为字符串时按公开 Responses API 的语义展开为一条
`user` 文本消息条目（`{"type": "message", "role": "user", "content": [{"type": "input_text", ...}]}`），
因为 Codex 后端只接受条目数组；数组及其他类型原样透传。`input` 数组中显式指定 `type: "message"`
且 `role: "system"` 的消息，其角色转换为 Codex 接受的 `developer`；消息内容、顺序、其他字段和顶层
`instructions` 保持不变。HTTP/SSE 与 WebSocket 共用这条正文兼容规则。
`prompt_cache_key`、`reasoning`、`include` 等 Codex 参数继续保留。上述参数过滤只作用于顶层，
不删除工具参数 schema、输入内容或 `client_metadata` 内的同名业务字段；其他未知字段继续透传。
`prompt_cache_key` 不用于补造会话或线程请求头；客户端未提供缓存键、会话或线程身份时保持缺省。

Grok 客户端经 Codex/OAuth 上游执行时，仅在带有 `x-grok-model-override`、`x-grok-turn-idx` 或
`x-grok-session-id` 标记的请求中，移除 `developer` 消息首个文本块开头的
`You are Grok released by xAI.`。其余内容、用户消息和顶层 `instructions` 保留；
普通 Codex 请求与 API Key 上游不应用这条兼容规则。

Codex/OAuth 上游的历史回填按字段形状兼容，不以 User-Agent 品牌区分：显式 `type: "reasoning"`
的 `input` 项移除顶层 `status`；该项具有非空字符串 `encrypted_content` 时，还会移除非空数组
`content`。其他字段及顺序保持不变，缺少非空加密载荷的明文历史由上游判定。
普通消息、工具项及未知类型不受此规则影响，API Key 上游不应用此规则。

请求头过滤不提供客户端匿名化；系统提示词、工具定义、工具结果、工作目录及其他业务 metadata
保持原有语义，可能包含客户端环境信息。

Responses WebSocket 仅接受文本 `response.create`，同一连接串行执行。当前响应期间收到的后续业务帧
留在有界接收队列中，待当前响应完成终结和写出后再逐条校验、准入与执行，不因请求提前到达而断开。
接收队列容量为 32 个事件，超载仍关闭连接；Ping/Pong、客户端关闭和服务关闭不等待队列中的请求执行。

OAuth 账号默认 `prefer_websocket`，客户端使用 HTTP/SSE 时仍可能选择上游 WebSocket；
可将账号上游传输方式设为 `http`，固定使用 HTTP/SSE。API Key 账号默认使用 HTTP/SSE，
也可配置 `prefer_websocket`。必须依赖 WS 的协议预热、非持久化新链和连接内续接不使用 HTTP-only 账号。
客户端配置的 `supports_websockets` 只控制第一段连接，不是服务端传输策略开关。
上游在响应终态前发送 Close 1000 仍属于失败，不能按“正常关闭”计为成功。

已建立模型执行的 Responses、Images 和 Search HTTP 响应按以下规则返回关联 ID：
`x-gateway-request-id` 为模型执行 ID；`x-request-id` 保留有效上游值，只有上游
`x-oai-request-id` 时复用其值，没有上游 ID 时使用模型执行 ID。`x-oai-request-id` 不是必需字段，
也不要求客户端识别它；OpenAI 与 xAI 路由使用相同规则。失败响应的关联 ID 不采用会话 opening ID，
错误正文读取失败时仍返回已知上游 ID；已采集的 turn state 等允许的会话头继续按原合同交付。
尚未建立执行的入口拒绝继续使用 middleware 的入口关联。

WebSocket 在尚未交付上游业务事件时合成的错误，除下述容量恢复合同外，保留已确认的失败状态，
以及 Provider 提取的结构化 message/type/code；没有结构化错误时使用稳定安全文案，不把原始
HTML 或截断正文当作 message。
合成错误自身的 `headers` 携带允许下发的响应头：优先保留实际失败的上游 request ID，无上游 ID 时
提供网关关联 ID，并用 `x-gateway-request-id` 独立标识网关请求。除下述客户端错误兼容与原生续写额度恢复外，
已经取得的原始上游错误帧不重写。

### 模型目录

`GET /v1/models` 默认返回 OpenAI 兼容列表 `{"object": "list", "data": [...]}`；请求携带非空
`client_version` query 参数（Codex 客户端）时改为返回 Codex 专用目录合同 `{"models": [...]}`。

OpenAI Provider 按客户端传入的 `client_version` 请求上游目录，完整保留每个模型 JSON 对象，包括
`base_instructions`、`model_messages`、`service_tiers`、工具与能力字段，以及未知嵌套字段、显式 `null`
和字段缺失的区别。
API Key 上游返回完整 Codex `models` 目录时沿用该合同；仅返回普通 `data` 模型列表时使用通用画像，
未提供的推理能力保持未知，不补充推理档位。
模型别名仅替换 `slug`，不替换上游展示名、提示词、能力或 `priority`；保持原生模型顺序，新增别名附在后面。
目录按当前路由快照的模型存在性及账号模型政策过滤，避免公布已知无法路由的模型；新模型需待后台目录对账后进入列表。
xAI 没有 Codex 原生目录，继续使用明确的通用画像适配。

目录账号只能来自本次 Client Key 冻结的账号范围。OpenAI 的 OAuth 目录按账号 ID 排序，使用首个成功读取的
合格账号，最多尝试三个账号；API Key 目录按账号模型权限过滤后聚合。同名模型优先采用 OAuth 原生对象，
其次采用 API Key 的完整 Codex 对象，再使用普通模型 ID；同类 API Key 目录按账号 ID 确定来源，不跨账号
或套餐混拼字段。因此单账号、无别名时可保持该账号的模型对象一致，多账号/多 Provider 聚合不代表
“与某个官方账号的整个目录完全一致”，也不会固定后续推理账号。
读取失败返回 `503 model_catalog_unavailable`，不以简化模板或空成功响应覆盖客户端缓存。

原生目录可能返回缓存结果，成功缓存有效期为 5 分钟。每次查询都检查账号资格和 Client Key 范围。
成功目录响应带 `Cache-Control: private, no-store`，不借用上游 ETag 标识经过选择/别名/聚合后的正文。

`service_tiers` 的 `name` 用于生成 `/fast` 等命令，`id` 是请求使用的 `service_tier` 值，二者不能互换。
上游未声明或明确返回空数组时不补档位，也不根据模型名或旧 `additional_speed_tiers` 字段推断 Fast。
该合同对齐
[官方 Codex 0.154.0 的模型元数据](https://github.com/openai/codex/blob/6b9826e3aa83b1a5947db50f4332cb9c65f1b340/codex-rs/protocol/src/openai_models.rs)
与其动态服务档位命令；客户端需重新加载模型目录才能使用更新后的档位信息。

Codex 专用目录中的 `context_window` 与 `max_context_window` 分别表示默认上下文窗口和客户端本地
覆盖的上限。OpenAI Provider 原样保留上游对应字段，缺失与 `null` 不互相转换；网关不通过部署配置
覆盖这些值。Codex 客户端配置 `model_context_window` 后，按该值与非空 `max_context_window` 的较小值
使用窗口；上限为空时保留客户端本地值。xAI 目录只声明一个窗口，其 Provider 继续以该值作为客户端覆盖上限。

### 透传与错误恢复

OpenAI 路径保留客户端 Responses wire 语义：请求 body 的未知字段和字段顺序保持不变（受控模型
映射除外），HTTP SSE 与 WebSocket 的上游业务事件除下述客户端错误兼容外按原始字节转发，
response ID 按 opaque 值处理而不假设 UUID 或固定长度；除客户端错误兼容与原生续写额度恢复外，
OpenAI 上游错误 envelope 和允许下发的 opaque header 值也不由 canonical 观测结果重写。
Images 请求不读取或重建 JSON，也不要求或映射模型字段；
它固定使用 OpenAI Provider，
只在原始字节之外完成账号选择、鉴权头替换和端点路由，成功与非容量失败响应正文保持原始字节。
`/v1/alpha/search` 使用相同的 OpenAI Provider 原生端点边界：body（包括 `model`）不解析、不映射，
`x-codex-turn-metadata` 在移除客户端账号身份并按当前 lease 重写 installation ID 后转发；上游账号
Authorization、Cookie、account ID、originator 和 User-Agent 均由代理安全重建。xAI 是 Grok wire 与
Responses wire 之间的协议转换层，转换只在 xAI Provider 内完成。
上游结构化错误的 message/code/type 按上述边界交付客户端，其中内嵌的账号指纹 UUID 已脱敏。模型映射是
全局精确映射，未命中时模型名原样交给候选 Provider；分组只限定账号集合，不参与模型改名。

OpenAI 明确返回 `server_is_overloaded`、`slow_down` 或模型容量不足错误时，代理在允许安全重放且
尚未交付输出的前提下，先做最多 3 次同账号指数退避，再通过现有调度换号。默认间隔从 500ms 开始，
上游 `Retry-After` 参与退避计算，单次等待不超过 8 秒；重试同时受请求总尝试次数和截止时间约束。
`server_is_overloaded`、`slow_down` 等可计分的结构化错误按已发送的失败尝试计入 Smart 账号
健康分。已确认容量拒绝的平滑权重为 0.4，其他可计分失败与成功样本保持 0.2。
失败率使用账号级平滑与时间衰减，影响后续普通选路，已有可用账号的
会话亲和仍优先。容量不足不触发 Provider 全局熔断，也不作为账号额度耗尽；启用账号自动冻结时，
达到容量失败阈值会另外写入临时冷却。
客户端错误兼容由 API 编码出口统一处理：最终交付的 `server_is_overloaded`、`slow_down` 错误码
投影为 `server_error`，HTTP 错误状态及 WS 包装错误的数字状态投影为 `503`，让客户端执行自己的
有界重试。Provider 已确认容量不足的初始失败，即使没有这两个错误码，也返回 `503`。
SSE/WS 的 `response.failed` 保留原消息、响应 ID 与其他业务字段；客户端无法消费的裸 `error`
继续按现有规则投影为 `response.failed`。`Retry-After` 等允许下发的响应头保留，
其他错误码不受影响。内部上游状态、错误码、原始事件及计量事实保持不变；已开始输出的请求由
客户端决定如何恢复，代理不因此重放已提交的请求。
明确额度耗尽触发账号隔离与安全换号，
包括 WebSocket 握手返回的 429；不会因其长 `Retry-After` 而转入同账号传输恢复等待。
OpenAI 选号阶段确认本次可选账号全部额度耗尽时，HTTP 返回 `429`，WebSocket 错误帧返回
`status: 429`，两者的 `error.type` 与 `error.code` 均为 `usage_limit_reached`，提示客户端停止
本轮自动重试，等待额度恢复或补充可用账号。空账号池、认证失效和临时容量不足不按额度耗尽处理；
仍有其他可用 Provider 或可安全恢复的续写时，网关先按现有路由规则尝试恢复。

带 `previous_response_id` 的 OpenAI 原生续写仍绑定原账号。若该账号明确拒绝请求且额度已耗尽，
并且请求可安全重放、尚无语义输出且未提交下游，网关隔离该账号，对客户端返回 HTTP `400`
（WebSocket 为 `status: 400`）及 `previous_response_not_found`，不附带额度窗口的 `Retry-After`。
支持该恢复协议的客户端应去掉 `previous_response_id`、携带完整历史重试，由正常调度选择可用账号；
官方 Codex 的 WebSocket 客户端支持这一流程。其他客户端需要自行处理，网关不会跨账号发送原增量输入。
普通限流、容量不足、发送结果不明以及已经交付输出的失败不触发此转换。

### API Key 额度查询

`GET /v1/usage` 使用同一数据面入口认证，默认传入 `Authorization: Bearer <Client Key>`；
启用入口认证插件时可查询其映射 Key 的额度。不接受会话 Cookie、管理 API Key 或查询参数。
只返回当前 Key 的日与周额度，不包含明文 Key、账号资料或其他 Key 的数据。查询本身不执行模型推理、扣费或占用推理并发/RPM，
不更新最近使用时间或开启预算窗口；额度耗尽后仍可查询。已配置的入口认证和 request 中间件仍按授权执行。

成功响应直接返回以下 JSON，不使用管理接口信封，所有响应带 `Cache-Control: no-store`：

```json
{
  "unit": "USD",
  "daily": { "total": "1", "used": "0.640001", "remaining": "0.359999", "resetsAt": "2026-09-21T16:00:00Z" },
  "weekly": { "total": "5", "used": "2.35", "remaining": "2.65", "resetsAt": "2026-09-27T16:00:00Z" }
}
```

金额使用十进制字符串，`total` 为当前周期限额，`used` 为该周期已结算金额，`remaining` 为限额减已用且最低为零。
不限额时 `total`、`remaining` 均为 `null`，仍返回已用金额。`resetsAt` 为 RFC3339 时间，尚未开启或已到期的窗口返回 `null`，
已到期窗口的 `used` 为 `"0"`。日窗口按北京时间零点划分，周窗口沿用首次使用起的七天周期，不固定为周一。
修改限额、管理员重置和费用结算均复用现有 Key 账本，不从请求日志重算余额。

缺失、非法、已禁用或已删除的 Key 返回 OpenAI 风格 `401` 错误；未知查询参数返回 `400 invalid_usage_query`，
读取账本失败返回 `503 usage_unavailable`，不会用零余额掩盖故障。

## 4. 浏览器认证

### 统一登录与会话

管理员和密钥登录共用 `/api/auth/*`。登录模式 `mode` 只用于选择凭据验证方式，不直接授予权限；
验证成功后，由后端写入身份和绑定 ID。一个浏览器只持有一份 `cpr_session` HttpOnly Cookie，
原始 Key 不进入 URL、Pinia 或浏览器存储。登录页的切换只改变本地表单，不改变 URL。
管理员进入管理端；Key 登录后进入 `/key-usage`，只读取当前会话绑定 Key 的数据。

| 方法 | 路由 | 请求 | 说明 |
| --- | --- | --- | --- |
| `POST` | `/api/auth/login` | `{ mode: "admin", username?, password }` 或 `{ mode: "key", apiKey }` | 验证凭据、创建会话；成功后撤销请求携带的旧会话 |
| `GET` | `/api/auth/status` | 无 | 从 Cookie 恢复服务端身份，返回 `{ authenticated, session }` |
| `POST` | `/api/auth/logout` | 无 | 删除当前会话并清除 Cookie；存储失败返回 503，不假装退出成功 |
| `POST` | `/api/auth/password` | `{ currentPassword, newPassword }` | 仅管理员会话可用；验证当前密码后修改密码，撤销全部管理员会话并清除当前 Cookie |

登录返回 `data: { role: "admin" | "key", expiresAt }`；status 已登录时的 `session` 使用同一结构，
未登录时为 `{ authenticated: false, session: null }`。`role` 由服务端已验证身份推导，不接受客户端声明。
不返回凭据或绑定 ID。

修改密码要求新密码至少 12 个字符、最多 1024 字节，不能包含控制字符、使用常见弱口令或与当前密码相同。
成功返回 `{ message }`，需要重新登录；当前密码错误或新密码不合法返回 400，并保留原会话。
并发修改中只有旧密码哈希仍匹配的请求可以提交，冲突返回 409；密码更新与安全审计在同一事务提交。
该入口共用登录尝试限流，超限返回 429。普通设置变更和管理员 API Key 变更不撤销密码登录会话，密钥身份会话也不受改密影响。

会话由服务端保存，Cookie 属性为 `Path=/; HttpOnly; SameSite=Lax`，`Max-Age` /
`Expires` 对齐固定有效期，`Secure` 沿用上述 Origin 规则。轮询不会续期。
管理员有效期由 `admin.session_ttl_minutes` 控制；密钥有效期由 `client.session_ttl_minutes` 控制，默认 1440 分钟。

每次恢复密钥会话时重新确认 Key 存在且启用；停用或删除后会话失效，重新启用不会恢复已撤销会话。
依赖不可用时返回 503，不返回已认证或假装未登录。预算耗尽不妨碍登录。

密钥会话访问管理接口返回 403，不清除仍然有效的会话。
浏览器会话不能替代 `/v1/*` 的 Bearer Key，数据面 Key 也不能替代浏览器会话。

所有 `/api/auth/*` 响应带 `Cache-Control: no-store`；未知路径和错误 method 返回 JSON，不落入 SPA。
两种登录共享来源桶和全局桶，分别为每 60 秒 10 次 / 200 次；来源取连接 IP，不信任任意转发头。
拒绝时使用 `42901` 和 `Retry-After`。

认证错误共用 `40101`（会话失效）、`40102`（凭据错误）和 `40301`（权限不足）。
前端只在明确的会话失效时统一退出，不按 URL 或每个接口上的身份标记分发。

### Key 用量与客户端配置

以下接口仅接受 Key 身份的 `cpr_session`，不接受 Bearer Key 或管理 API Key。管理员会话返回 `40301`；
缺失、失效或已停用的 Key 会话返回 `40101`。所有响应带 `Cache-Control: no-store`，未知路径和错误方法返回 JSON。

| 方法 | 路由 | 查询 | 说明 |
| --- | --- | --- | --- |
| `GET` | `/api/key-usage/overview` | `startTime`、`endTime`、`model?` | 用量汇总、趋势、当前额度和北京时间今日健康时间线 |
| `GET` | `/api/key-usage/records` | 同上，另含 `kind?`、`currentPage?`、`pageSize?` | 当前 Key 的成功请求或错误记录 |
| `GET` | `/api/key-usage/config` | 无 | 当前 Key 的客户端配置凭据 |
| `GET` | `/api/key-usage/version` | 无 | “关于”弹窗使用的当前版本号和提交号 |

用量查询的起止时间使用 RFC3339，开始必须早于结束，一次最多 31 天。模型按完整名称匹配；
不接受 Key ID、账号、Provider 等范围参数或其他未知字段。页码默认 1，每页默认 20，允许 1–100 条；
`kind` 为 `success`（默认）或 `error`。分页响应为 `{ items, currentPage, pageSize, total }`。

overview 返回 `asOf`、`startTime`、`endTime`、`key`、`summary`、`trend`、`healthTimeline`。
`key` 仅包含名称、掩码前缀、并发/RPM、日与周限额、已用 USD 及重置时间；零限额表示不限，
未启动窗口的重置时间为 null。额度使用现有结算账本，不受日志日期或模型筛选影响。
健康时间线沿用管理端的 96 个北京时间日内桶与可用性语义，不受历史范围和模型筛选影响。

汇总和趋势返回请求数、输入、输出、缓存读写、推理、总 Tokens 与 USD 成本；输入已包含缓存读写，
推理为输出的明细，不得把缓存或推理重复计入总消耗。趋势另含 `time` 与 `bucketSeconds`。
`costUsd` 为十进制字符串或 null；`costIncomplete` 表示部分请求计费不完整，不能把已知费用当作完整总费用。
空请求范围的成本为 `"0"`，缺失定价保持 null。

日志只返回时间、公开请求模型、推理强度、接口、上下游传输方式、当前请求的 IP / User-Agent、
Token 明细、费用明细、用时/首字与状态。Token 和费用复用现有展示合同；延迟仅包含当前请求的首事件、
首推理、首文本和总耗时，不含账号容量或调度诊断。
成功记录的 `status` 为 `success`，不伪造未保存的 HTTP 状态；错误记录为 `error`，只返回客户端状态码，
缺失的 Token/费用明细为 null。不返回账号资料、Key ID、上游模型或请求标识、原始错误正文或诊断内容。

version 返回 `{ version, gitSha }`，不接受查询参数，不包含部署模式、更新状态或内部诊断。

config 返回 `{ name, plaintextKey }`，仅读取服务端会话绑定的当前 Key，不接受任何查询参数。
使用统计页在打开“密钥配置”弹窗时读取，用于复制 Codex 配置文件或导入 CCSwitch；
明文不进入用量轮询响应或浏览器持久化存储，关闭弹窗后清除页面中的配置状态。

## 5. 账号

账号 API 使用统一路由，不存在 Provider Instance 或 Provider 专属账号路由。`provider` 使用 Provider ID，
固定为内置 `openai`、`xai`；操作可用性由平台实现与账号认证类型决定。

| 方法 | 路由 | 主要 query/body | 说明 |
| --- | --- | --- | --- |
| `GET` | `/api/admin/accounts` | `page`、`pageSize`、`provider`、`groupId`、`search`、`status`、排序字段 | 分页查询账号与汇总 |
| `GET` | `/api/admin/accounts/detail` | `accountId` | 查询账号详情、额度和本地用量 |
| `GET` | `/api/admin/accounts/export` | `accountIds`、`confirm=export_sensitive_accounts` | 显式导出最多 200 个账号的敏感 Provider 文档 |
| `POST` | `/api/admin/accounts/import` | `{ provider, data, settings?, outboundProxyId? }` | 导入或按上游身份更新账号，可同时应用调度、分组设置与默认代理 |
| `POST` | `/api/admin/accounts/import-tasks` | `{ submissionId, items: [{ provider, data, settings?, outboundProxyId? }] }` | 接受后台导入，返回 HTTP 202 和任务摘要 |
| `GET` | `/api/admin/accounts/import-tasks` | 无 | 当前管理员仍保留的任务，按创建时间倒序 |
| `GET` | `/api/admin/accounts/import-tasks/detail` | `taskId` | 任务摘要和逐条结果，不含原始凭据 |
| `POST` | `/api/admin/accounts/import-tasks/stop` | `{ taskId }` | 跳过未开始的条目，已开始的条目继续完成 |
| `POST` | `/api/admin/accounts/refresh` | `{ accountId }` | 手工刷新 OAuth credential（`idToken` / `accessToken` / `refreshToken`），不刷新额度 |
| `POST` | `/api/admin/accounts/recover` | `{ accountId }` | 停用账号只启用调度；已启用账号强制清除本地错误/额度/cooldown 事实，不访问上游 |
| `POST` | `/api/admin/accounts/update` | `{ accountId, enabled, concurrencyLimit, weight, groupIds, notes?, modelAccess?, outboundProxyId?, outboundProxyUrl?, connection? }` | 一次更新账号设置；`connection` 支持 OpenAI OAuth 传输方式及 API Key 连接配置，见下文 |
| `POST` | `/api/admin/accounts/batch-update` | `{ accountIds, enabled?, concurrencyLimit?, weight?, groupIds?, modelAccess?, outboundProxyId?, outboundProxyUrl? }` | 一次事务更新所选账号；仅修改提供的字段，至少提供一项修改 |
| `POST` | `/api/admin/accounts/delete` | `{ provider, accountIds }` | 批量删除 1–200 个账号 |
| `GET` | `/api/admin/accounts/quota` | `accountId` | 读取当前额度，不强制访问上游；Provider 未提供额度能力时返回空额度投影 |
| `GET` | `/api/admin/accounts/quota-forecast` | `accountId` | 按需读取周/月容量预测、源窗口剩余估算与采样依据，不刷新上游额度 |
| `POST` | `/api/admin/accounts/quota/refresh` | `{ accountId }` | 访问 Provider 并刷新额度，同时同步额度所属状态 |
| `GET` | `/api/admin/accounts/personal-info` | `accountId` | 按需汇聚 Provider 上游个人资料、累计活动与订阅信息，不更新额度或 credential |
| `GET` | `/api/admin/accounts/profile-avatar` | `accountId`、可选 `version` | 返回支持头像能力的 Provider 字节流，需要管理员会话；`version` 仅区分浏览器缓存 |
| `GET` | `/api/admin/accounts/reset-credits` | `accountId` | 查询支持该能力的 Provider 重置卡，不读取本地库存 |
| `POST` | `/api/admin/accounts/reset-credits` | `{ accountId, creditId?, redeemRequestId }` | 使用 UUIDv4 幂等键消费一张 Provider 重置卡 |
| `GET` | `/api/admin/accounts/models` | `accountId` | 优先读取该 Provider + 套餐的模型 cache，缺失时有限实时拉取 |
| `GET` | `/api/admin/accounts/models/catalog` | `accountId` | 读取指定账号的完整 Codex 原生模型目录，返回 `{ modelCount, observedAt, catalog }` |
| `POST` | `/api/admin/accounts/models/refresh` | `{ accountId }` | 强制拉取最新模型并覆盖 cache |
| `GET` | `/api/admin/accounts/connection-test` | `accountId`、`modelId` | 通过 SSE 返回实时连接测试事件，不作为业务 Responses 用量记录 |
| `POST` | `/api/admin/accounts/oauth/start` | `{ provider, name, accountId?, outboundProxyId?, outboundProxyUrl? }` | 为支持登录的 Provider 创建 flow；`accountId` 表示重新授权 |
| `POST` | `/api/admin/accounts/oauth/complete` | `{ provider, flowId, callbackUrl, settings? }` | 消费 OAuth callback；首次授权可附带账号设置，重新授权保留原设置 |

账号列表支持以下稳定值：

- `provider`: `openai` 或 `xai`，省略或空值表示不过滤；
- `groupId`: 分组 ID、`ungrouped`，或省略以不过滤；
- `status`: `normal`、`quota_exhausted`、`rate_limited`、`disabled`、`error`；
- `sortBy`: `email`、`status`、`planType`、`usage`、`lastUsedAt`、`expiresAt`；
- `sortDirection`: `asc`、`desc`。

账号与用量页面使用固定的 OpenAI/xAI 平台选项，省略 `provider` 表示不过滤。

模型目录导出保留上游原生模型对象和能力字段，不包含账号凭据；不支持 Codex 原生目录的账号不能导出。
管理端下载文件名为 `cpr-model-catalog-<套餐>-<账号名称>.json`，文件正文为 `catalog`，可用于
Codex 的 `model_catalog_json` 配置。账号设置保存不等待上游模型目录刷新完成。

账号限流详情在 `quota` 中返回：`rateLimitReason` 为 `upstream_rate_limit`（上游临时限流）、
`capacity_freeze`（容量错误触发自动冻结）或 `null`。`recoveryProbeRequired` 表示解除冻结是否需要成功探测；
此时 `rateLimitedUntil` 是最早探测时间，到期后仍保持 `rate_limited`，直到探测成功或手动恢复。
未要求探测时，该字段表示冷却结束时间。所有此类情况统一显示“限流中”，仅详情原因和恢复条件不同。

账号列表和详情返回 `notes`（无备注时为 `null`）。编辑时省略或 `null` 保留原备注；字符串最多 500 个 Unicode
字符，允许换行和制表符，保存时去除首尾空白，空字符串清空备注。备注独立于上游身份，导入时未显式提供备注、
重新授权、凭据刷新及批量调度更新均保留已有备注。

账号视图和 Dashboard 账号概览中的 `planType` 保留原始套餐值；`planTypeDisplay` 由后端先按 Provider 解析名称，
再统一为大驼峰格式，前端直接展示该字段，例如 `Free`、`SuperGrokPro`、`EduPlus`。
OpenAI 的 `self_serve_business_prolite` 等 Team 套餐显示为 `Business`；新套餐也使用相同格式。
OpenAI 主动额度刷新和正常响应携带的明确套餐会同步到账号，支持升级与降级；
空值或 `unknown` 不覆盖已有套餐，同族泛化值（如 `team`）保留已知的具体套餐子类型。
账号套餐为空或 `unknown` 时，后端优先用已保存的上游额度响应
中的明确套餐值补全 `planType` 和 `planTypeDisplay`；两处均无套餐信息时才显示“未知套餐”。

`outboundProxyId` 绑定已保存的代理，不要求出口测试成功；省略或 `null` 保留当前绑定，空字符串清除绑定。
`outboundProxyUrl` 兼容 HTTP、HTTPS、SOCKS5、SOCKS5H 代理 URL，可带用户名和密码；不能与 ID 同时设置。
编辑时省略或 `null` 表示保持原配置，空字符串表示清除代理并直连。列表和详情只返回
不含认证信息的 `outboundProxyEndpoint`（直连时为 `null`）；只有显式敏感导出包含完整 URL。
指定代理后，推理、OAuth 服务端交换/刷新及账号辅助请求使用同一出口；代理失败不会退回直连。
浏览器打开的第三方 OAuth 授权页仍使用浏览器自身网络。
账号出口与连接隔离见 [架构说明](architecture.md#账号出站代理)。

### 账号模型限制

账号列表和详情返回 `modelAccess: { mode, models }`，模型 ID 区分大小写并精确匹配：

| `mode` | `models` | 含义 |
| --- | --- | --- |
| `all` | `[]` | 不额外限制模型，默认值 |
| `allowlist` | 非空 ID 数组 | 仅允许指定模型 |
| `denylist` | 非空 ID 数组 | 排除指定模型 |

最多 256 项，每个 ID 最多 256 字节；重复 ID 去重，不接受空白、控制字符、`__` 前缀或 `*` 通配符。
允许保存当前上游目录尚未返回的 ID。限制按全局模型映射后的上游 ID 判断，不扩大账号本身的上游权限。
例如 Plus 设置 `allowlist` 并选中 luna 的实际 ID，Pro 设置 `denylist` 并选中同一 ID，即可严格分流；
Pro 使用 `all` 时也能参与 luna 调度。套餐名称不自动生成或修改规则。

单账号更新或批量更新省略 `modelAccess` 时保留原值，显式提交 `{ "mode": "all", "models": [] }` 清除限制。
批量接口的调度、分组、模型与代理字段均可省略；省略的字段保持各账号原值。
提供 `groupIds` 时替换完整分组集合，提供 `concurrencyLimit: null` 时恢复继承运行参数。

限制适用于 Responses HTTP、WebSocket 及其带压缩触发的请求选号，包括重试、亲和和换号；没有合规账号时沿用
无可用账号错误，不会回退到被禁止的账号。已开始请求使用冻结的政策，新请求使用已发布的新配置。
Images、独立 Search 及管理员连接测试不受该文本模型限制；连接测试成功只证明指定账号的上游能力。
普通 `/v1/models` 和单模型查询将已发现的 OpenAI 模型关联到来源账号，至少一个来源账号在当前 Key 范围内且政策允许时才展示；
同 Provider 中未发现该模型的 `all` 账号不会使它进入列表。原生目录保留已有来源选择和完整模型对象。

### 独立代理管理 / Managed Proxies

所有端点要求管理员身份。所有响应只返回去掉认证信息的 `endpoint`，不会返回完整 URL。

| 方法 / Method | 路径 / Path | 请求 / Request | 结果 / Result |
| --- | --- | --- | --- |
| `GET` | `/api/admin/proxies` | `page`、`pageSize`（1-200）、`search`（名称） | `{ items, page }` |
| `GET` | `/api/admin/proxies/accounts` | `proxyId`、`page`、`pageSize`（1-200）、`search`（账号名称或邮箱） | `{ items, page }` |
| `POST` | `/api/admin/proxies/accounts/remove` | `{ proxyId, accountId }` | `{ configRevision }` |
| `POST` | `/api/admin/proxies/create` | `{ name, proxyUrl, location?, autoLocation? }` | `201 { record, configRevision }` |
| `POST` | `/api/admin/proxies/update` | `{ id, revision, name, proxyUrl?, location?, autoLocation? }` | `{ record, configRevision }` |
| `POST` | `/api/admin/proxies/probe` | `{ proxyUrl, detectLocation? }` | 测试未保存的地址，返回连通性及可选位置结果，不创建代理 |
| `POST` | `/api/admin/proxies/test` | `{ id, revision, detectLocation? }` | 最新代理记录 / Proxy record with test result |
| `POST` | `/api/admin/proxies/delete` | `{ id, revision }` | `{ configRevision }` |

`record` 包含 `id`、`name`、`endpoint`、`hasAuthentication`、`revision`、`accountCount`、`location`、
`autoLocation`、`detectedLocation`、`lastTestAt`、
`lastTest: { success, latencyMs, exitIp, exitIpv4, exitIpv6, message, location }`、`createdAt`、`updatedAt`。
未测试时 `lastTestAt` / `lastTest` 为 `null`。连通性失败返回 HTTP 200 和 `lastTest.success=false`；
记录版本过期、重复 URL、删除已绑定的代理返回 409，并发测试满载返回 429。

代理列表只返回关联账号数量。关联账号按需查询，每项包含 `id`、`name`、`email`、`provider`、`enabled`、
`authenticationKind`、`planType`、`planTypeDisplay` 和 `groups: [{ id, name, color, enabled }]`，
不返回账号凭据。默认每页 20 条，按名称、ID 稳定排序；搜索不区分大小写，匹配名称或邮箱的字面子串。
不存在的代理返回 404，未绑定账号或没有匹配结果时返回空页。数量与当前页对应同一查询快照。

移除关联账号只清除指定账号的代理绑定与连接地址，使其改为直连，保留凭据、调度参数与分组。
若账号已不再绑定请求中的代理，则返回 409。

更新省略 `proxyUrl` 保留认证；连接配置改变时清除测试结果并更新所有绑定账号。
测试结果只在请求中的版本仍匹配时保存。

`detectLocation: true` 在测试连接时解析出口位置；结果 `location.status` 为 `notRequested`、
`detected`、`failed` 或 `conflict`。`detected` 携带 `location`，`failed` 携带安全错误说明；
位置查询失败不等同于代理连接失败。管理端的解析按钮把成功结果填入自定义位置表单，保存后生效；
失败时保留已有输入，不自动启用持续跟随。
API 的 `autoLocation` 默认为 `false`；开启时使用已检测位置，测试连接会刷新检测结果。
`detectedLocation` 保存 `{ location, exitIpv4, exitIpv6, detectedAt }`，手动位置独立保留。

`location` 为 `null` 或完整对象 `{ country, region, city, timezone }`。国家代码为两位大写 ASCII 字母；
地区、城市禁止控制字符，去除首尾空白后须为 1–128 个字符；时区必须是有效 IANA 名称，例如 `Asia/Tokyo`。
创建时省略或 `null` 表示继承全局；更新时省略表示保留，`null` 清除覆盖，完整对象替换覆盖。
只改位置不清空连通性测试结果，也不更改账号凭据版本。

关联账号的 OpenAI/Codex Responses 请求（HTTP/SSE、WebSocket）优先使用代理位置，否则使用全局
运行设置中已开启的 `requestLocation`；两者均未开启时保留客户端原有位置和时区。全局覆盖按请求冻结，
新请求使用保存后的设置，无需重启；代理覆盖在每次执行时读取，
换号或换出口按该次选定账号解析。位置只影响带来源标记的环境上下文日期/时区和 Web Search 的结构化位置，
不改变用户普通文本、epoch 时间戳、真实出口 IP、服务或管理端时区、数据驻留约束及 xAI 请求。

测试经代理并发访问 IPv4 专用端点 `https://api.ipify.org?format=json` 与 IPv6 专用端点 `https://api6.ipify.org?format=json`，
分别验证并记录双栈出口（IPv4 与 IPv6 地址），在任一地址族可用时即判定连接成功。超时 15 秒，每进程最多同时测试 4 条。
探测器复用 OpenAI 的证书信任配置：优先读取非空的 `CODEX_CA_CERTIFICATE`，
其次读取 `SSL_CERT_FILE`，并保留系统根证书；证书配置错误不会回退为不验证证书。
出口测试结果仅供诊断，不限制代理的选择和绑定；未测试或测试失败的代理仍可使用。
出口测试通过不表示 Provider 账号权限或额度可用；账号可用性使用账号连接测试。
导入请求可以携带顶层 `outboundProxyId`，在令牌交换前解析为默认出口；文件中显式的代理配置优先。
文件及 AT/RT 导入从凭据交换到落库期间保护所选代理；此时修改、删除或写入测试结果返回 409，
避免已轮换的凭据因代理状态变化而丢失。完成导入或请求取消后自动释放保护。
OAuth 等待回调期间不持有保护；提交仍拒绝已删除或连接配置改变的代理。

### 账号连接测试 SSE

`GET /api/admin/accounts/connection-test` 固定探测请求指定的账号，不参与普通账号轮换。成功流沿用
`test_start`、`request`、`content`、`test_complete` 事件；失败事件为：

```json
{
  "type": "error",
  "source": "upstream",
  "gatewayErrorCode": "rate_limited",
  "sendState": "sent",
  "error": "upstream unavailable",
  "providerErrorCode": "usage_exhausted",
  "providerErrorType": "invalid_request_error",
  "upstreamStatus": 429,
  "upstreamContentType": "application/json",
  "upstreamBody": "{\"error\":{...}}"
}
```

- `source` 为 `gateway`、`provider` 或 `upstream`：分别表示尚未进入 Provider、Provider 本地且未发送、
  已发送/可能已发送或已经捕获到上游事实。
- `gatewayErrorCode` 是 `GatewayErrorKind` 的稳定机器值，管理端据此生成中文摘要。
- `sendState` 为 `not_sent`、`sent`、`ambiguous`，非 Provider 错误为 `null`。
- `error`、`providerErrorCode`、`providerErrorType`、`upstreamStatus`、`upstreamContentType` 和
  `upstreamBody` 是实际捕获的原始诊断字段；缺失时为 `null`，不会由本地猜测或翻译。

### 后台导入任务

管理端通过后台任务导入账号，关闭页面不会取消执行。`submissionId` 为客户端生成的 UUID；同一管理员在任务记录
保留期间使用相同标识和相同输入重新提交，会返回已有任务，内容改变则返回 409。修改输入须使用新标识。

每个任务接受 1–200 个 `items`，请求体上限为 4 MiB。每个条目遵循下节的 Provider 文档与设置合同，独立处理
并返回结果。批量 AT / RT 按非空输入顺序拆成单账号条目，因此可逐条统计；
JSON 文件按 Provider 文档分项，不拆解内部代理引用或改变 Provider 对文档的原子性与部分成功语义。
一个文档可导入多个账号，条目成功数与入库账号数可能不同。

摘要字段为 `taskId`、`createdAt`、`finishedAt`（未结束为 null）、`stopRequested`、`total`、`counts`。
`counts` 包含 `pending`、`running`、`succeeded`、`failed`、`unknown`、`skipped` 和 `importedAccounts`。
详情追加 `items: [{ index, provider, status, accountIds, message }]`，`index` 从 1 开始；`status` 对应上述前六类状态。
列表返回 `{ items: [摘要] }`。结果未知时保留 `unknown`，先核对账号目录再决定是否重新导入；服务端不自动重试凭据交换。
停止请求可重复调用，已结束的任务保持原结果；未知、已过期或其他管理员的任务 ID 返回 404。

所有后台导入共享 3 个执行槽位；
最多接受 8 个未结束任务、保留 100 个任务，达到上限返回 429。成功、失败或跳过后立即释放对应输入，
终态结果保留 1 小时后自动清理。服务重启会丢失任务与未执行输入，已提交的账号不受影响；
任务记录仍在保留期内时，可通过列表接口查询当前管理员的任务及进度。

### 账号能力、导入与 OAuth

敏感账号导出要求所选全部账号属于支持导出的内置平台。不支持时整个请求在读取任何凭据前拒绝，不返回部分导出结果。

账号列表及额度详情中的 `capabilities` 则针对当前账号，包含布尔字段 `quota`、`quotaRefresh`、`profile`、
`subscription`、`avatar`、`resetCredits` 和 `consumeResetCredit`，分别表示额度读取、额度刷新、资料、订阅、
头像、重置卡查询及消费能力。服务端结合 Provider 实现与账号认证类型生成，客户端据此显示
操作；只读与写入能力分别判断，不按 Provider 名称推测。
能力是查询时的投影，不能代替实际操作的权限校验，也不保证上游可用。

导入的 `data` 必须是 JSON object，Admin API 请求上限为 64 MiB；Provider 可以收紧限制，
当前 xAI 导入上限为 16 MiB。内部 schema 由目标 Provider 独占解释：

- OpenAI 接受单账号 OAuth 或 API Key 文档、`accounts` 数组（最多 200 项）、CPR 账号 bundle 和含代理引用的 sub2api 导出；
- OpenAI OAuth token 字段接受 `accessToken`、`refreshToken`、`idToken`，以及官方
  `auth.json` 中的 `access_token`、`refresh_token`、`id_token`，可以嵌套在 `tokens` 等账号 object 内；
  每项至少包含 AT 或 RT。仅含 `OPENAI_API_KEY` 的客户端代理配置不是 OAuth 账号导入材料；
  RT-only 会在导入时换取 AT，AT-only 不具备自动续期能力；
- OpenAI 与 xAI 的账号条目接受 `outboundProxyUrl`；OpenAI 还会解析 sub2api 的 `proxy_key` 和顶层 `proxies`。
  代理在 token 刷新前绑定。缺失、重复、停用、带到期时间或配置回退策略的 sub2api 代理会拒绝导入；
- xAI 从单账号 object 或 `accounts` 数组中提取 OAuth token；并发、优先级等字段不参与认证；
- xAI 批量导入逐条独立校验：失败条目跳过并记录日志，不中断其余条目，仅当没有任何条目成功时整个导入才报错；
- xAI API Key 不是受支持的账号 credential；
- OAuth 导入由目标 Provider 完成必要的 token exchange 或身份投影。API Key 导入校验凭据格式与地址，不调用 OAuth 或 ChatGPT 身份接口；可用性由模型目录和连接测试验证。

API Key 账号使用以下独立凭据形态：

```json
{
  "provider": "openai",
  "data": {
    "authentication_kind": "api_key",
    "name": "团队上游",
    "base_url": "https://api.example.com/v1",
    "api_key": "<upstream-api-key>",
    "transport": "http"
  }
}
```

`base_url` 是 API 前缀，追加 `responses`、`models`、`images/generations`、`images/edits` 或 `alpha/search`，
不会自动补 `/v1`；支持根路径和自定义前缀。
地址仅允许 HTTPS，HTTP 只允许本机回环；拒绝 URL 中的认证信息、查询串和 fragment。
`transport` 可省略（默认 `http`）或设为 `prefer_websocket`。API Key 使用 Bearer 认证与普通 JSON，
不携带 OAuth Cookie 或 ChatGPT 身份。上游模型列表使用标准 `/models` 格式，按账号和凭据版本隔离；
目录用于模型发现，不作为能力白名单，未列出的模型仍交由上游判断。
API Key 每次导入创建独立账号；更新已有账号使用 `/api/admin/accounts/update` 的 `connection` 字段。
导出会显式包含密钥，沿用敏感导出的确认合同。

sub2api 的 `platform=openai`、`type=apikey` 使用 `credentials.base_url` / `credentials.api_key`；
导入时按其端点规则将服务根、版本前缀或完整 `/responses` 地址转换为 API 前缀，缺省地址为官方 `/v1`。
非空模型映射、请求头覆盖、其他协议和启用的 `extra.openai_*` 设置尚未适配，返回输入错误，需先移除并在本项目重新配置。

API Key 账号与 OAuth 共用现有 Responses、Images 和 standalone Search 请求与响应链路，
包括 Responses Lite 和原生压缩协议的透传，实际支持情况由上游决定；不提供独立 compact 路由。
模型目录、连接测试和本地用量统计可用；OAuth 刷新/重新授权、ChatGPT 额度、个人资料、订阅和重置卡不适用。
客户端的 `image_generation` 和 `X-OpenAI-Actor-Authorization` 声明不改变账号的实际能力。
账号列表和详情的 API Key `usage` 汇总该账号创建后仍保留的本地请求记录，`windowLabelDisplay` 为 `通用额度`；
日志清理会影响累计范围，不代表上游余额。OAuth 账号仍按实际周/月额度窗口统计。

批量导入 AT / RT 使用 `accounts` JSON 数组，最多 200 项，不接受纯文本 token 列表。例如：

```json
{
  "provider": "openai",
  "data": {
    "accounts": [
      { "accessToken": "eyJ..." },
      { "accessToken": "eyJ...", "refreshToken": "rt_...", "idToken": "eyJ..." }
    ]
  }
}
```

RT-only 使用同一形状，只提交 `refreshToken`。不得把真实 token 写入日志、issue、fixture 或文档。

账号导入与首次 OAuth complete 可附带 `settings: { enabled, concurrencyLimit, weight, groupIds, notes?, modelAccess? }`。
提供 `settings` 时前四项均必填，`concurrencyLimit: null` 继承运行参数，否则为 1–4294967295 的整数；
`weight` 为 1–100，`groupIds` 为完整分组集合。设置应用于本次导入的全部账号，包括匹配到的已有账号，
与凭据在同一事务内提交；分组不存在时整次回滚。省略 `settings` 时新账号使用默认设置并保持未分组，
已有账号保留原有分组、权重与并发设置。可选 `notes` 与编辑备注使用相同的校验和清空语义，省略或 `null` 保留已有备注；
管理端新建表单留空时省略 `notes`。重新授权不接受 `settings`，凭据刷新保留账号设置。

账号列表的每个 item 返回轻量 `groups: [{ id, name, enabled }]`。

OpenAI 的 CPR 导出保持 OAuth 账号的既有 token 与过期时间字段。OpenAI 与 xAI 的 CPR 账号条目均包含
`modelAccess`，重新导入时恢复；导入 `settings.modelAccess` 显式提供时覆盖条目中的政策。
旧文档不带此字段时，新账号默认 `all`，已有账号保留原值；凭据刷新、重新授权和目录刷新均保留政策。
sub2api 的 `credentials.model_mapping` 不转换为本项目的账号模型限制。

编辑 OpenAI API Key 账号时，在 `POST /api/admin/accounts/update` 的账号设置中附带 `connection`：

```json
{
  "accountId": "acct_...",
  "enabled": true,
  "concurrencyLimit": null,
  "weight": 1,
  "groupIds": [],
  "connection": {
    "baseUrl": "https://api.example.com/v1",
    "transport": "http",
    "apiKey": "..."
  }
}
```

`connection.transport` 支持 `http`、`prefer_websocket`。省略 `connection` 时只更新账号设置；
省略 `connection.apiKey` 保留当前密钥，空字符串无效。
OpenAI OAuth 账号只接受 `connection: { transport }`，不接受 `baseUrl`、`apiKey` 或 OAuth token。
连接设置不能修改账号 ID、Provider 或认证类型，也不接受通用凭据文档。
凭据与设置在同一事务中保存，任一校验或持久化失败均不落库。
`GET /api/admin/accounts/detail` 对 API Key 账号额外返回 `credentialConfiguration: { base_url, transport }`；
OAuth 账号返回 `credentialConfiguration: { transport }`。响应不回显密钥或 token；不适用的账号省略该字段。
更新会推进凭据 revision 并失效目录与连接；旧版本会话不可静默续接到新上游。

OAuth start 使用：

```json
{
  "provider": "openai",
  "name": "account name",
  "accountId": null
}
```

重新授权已有账号时，start 请求仍携带 `provider` 和展示用 `name`，只额外提供目标 `accountId`；
客户端不得提交 `credentialRevision`、旧 token 身份或其他并发控制字段。complete 请求也不重复提交
`accountId`，后端通过 `flowId` 中保存的目标绑定完成授权。

流程只允许发起它的管理员身份继续；尚未完成的流程过期后需重新发起。
账号提交成功后，`complete` 保留 24 小时的完成结果。同一管理员使用相同 `provider`、`flowId`
重试时返回原账号结果，不再执行授权或更新凭据；新提交的 `callbackUrl` 和 `settings` 不会再次应用。

### OpenAI 身份、额度与状态

- OAuth 文件导入接受 camelCase 与 snake_case 的三个 token 字段，内部统一保存为
  `accessToken`、`refreshToken`、`idToken`，不接受含义模糊的 `token`。
  仅有 refresh token 时先换取 access token。
- 普通 OAuth 的身份补全复用官方 `token_data.rs::parse_chatgpt_jwt_claims`：优先解析 `idToken`，缺失字段再由
  `accessToken` 补齐；`email` 优先 JWT 顶层值、其次 `https://api.openai.com/profile.email`，用户 ID
  优先 `chatgpt_user_id`、其次 `user_id`。该路径不调用 `whoami`，也不信任导入文档顶层的
  `userId/accountId`。
- `at-` 开头的 Codex Personal Access Token（PAT）可直接粘贴到现有 **AT 导入** 入口，或使用
  `{"accessToken":"at-..."}` JSON；也接受官方 `auth.json` 的 `personal_access_token` 字段
  （兼容 `personalAccessToken`）。导入时向 OpenAI auth 的
  `/api/accounts/v1/user-auth-credential/whoami` 验证令牌，以响应中的用户 ID、账号 ID 和套餐建立身份，
  `email` 可缺失；不从导入文件的身份字段或附带的 ID token 回退补齐。验证失败不导入，接口区分提示
  PAT 格式无效、被上游拒绝、验证服务不可用和身份响应无效，不回显令牌或原始上游响应。
  PAT 不保存附带的 refresh token、ID token 或推测的过期时间，不参加 OAuth RT 刷新；
  失效后需取得新 PAT 再导入。普通 JWT 导入行为不变。
- 首次 OAuth 保留回调 `state`、PKCE 与官方 token exchange，并持久化 `idToken`、`accessToken`、
  `refreshToken`。刷新响应中的三个 token 字段均按官方语义独立轮换：返回新值时替换，省略时分别保留
  现值。重新授权也保留这些回调保护，但只轮换目标账号的 token。回调地址只承载 `code`/`state`，
  不以 host/path 形式作为拒绝条件。
- OAuth 账号文件导入和 OAuth complete（包括重新授权）在 credential 提交后后台尝试一次额度观测，不等待
  观测完成才返回成功。观测失败只记录告警，不回滚已提交的账号；手工或后台 RT 刷新只更新 token，
  不隐式等同于手工额度刷新，也不更新既有账号资料或 OAuth principal。xAI 导入与 OAuth complete
  使用相同的提交后观察流程。
- OAuth pending flow 先取得带过期时间的独占 claim，只有账号事务提交成功后才消费。失败会释放 claim，
  但上游 authorization code 本身通常只能交换一次；已完成过 token exchange 时应重新创建 OAuth flow。
- `GET /accounts/quota` 只读取最后一次落库快照；`POST /accounts/quota/refresh` 才访问上游。access token
  已过期时，额度刷新要求先走 credential 刷新或重新授权，不会拿过期 token 探测额度。
- OpenAI 已耗尽账号每 30 分钟主动复核一次，也会在最早未恢复窗口的 `resetAt + 2 分钟` 到期后
  提前复核；周期复核不等待旧重置时间，因此也能发现官方提前重置。正常账号有非零用量或触顶窗口时，
  在该窗口的 `resetAt + 2 分钟` 后主动复核。后台每 30 秒检查触发条件；同一重置边界复核后仍未更新时
  回到 30 分钟重试，避免旧 reset 持续触发请求。各窗口独立确认恢复，时间到期本身不会直接解除账号
  耗尽或将展示用量归零。
- `POST /accounts/recover` 对停用账号只将 `enabled` 改为 `true`，保留已有额度快照、凭据、错误和 Redis
  cooldown；启用后仍按这些事实投影状态，不会把已有错误或耗尽改成正常。对已启用账号则执行强制恢复：
  清除 Redis cooldown 和已保存的额度/错误，恢复为可调度 credential。两条路径均不访问上游；强制恢复
  不验证上游账号是否已经恢复，下一次真实请求仍可重新写入失败事实。
- 成功额度观测会 revision-fenced 写入 quota；明确 `Allowed` 投影为 `normal`，明确耗尽投影为
  `quota_exhausted`。额度观测不会清除凭据过期、无效或封禁事实；这些事实统一投影为 `error`，并由
  `errorReason` 区分。额度接口的 401/403 也不足以判定 refresh token 永久失效，credential 终态只由
  OAuth refresh 的明确永久错误写入。
- 正常 Responses 请求会解析上游响应的 rate-limit headers，合并进同一 quota 快照并同步状态。Free、
  K12 等套餐共用该状态机；套餐只参与账号展示和按套餐隔离的模型目录 cache，不存在 K12 专属额度路径。
- 账号详情的 Token 统计、模型排行和列表 Token 汇总优先使用账号级周额度窗口，无可统计的周窗口时
  使用月额度窗口；`usage.windowLabelDisplay` 随选中的窗口返回“周额度窗口”或“月额度窗口”。查询边界
  严格为 `[resetAt - windowSeconds, resetAt)`，不是自然周/月或最近 7/30 天；额度刷新若返回了更早的
  重置时间，会按新边界重新聚合。没有边界完整、可归属到账号的周/月窗口时显示无数据，标签为
  “周/月额度窗口”，不回退到 5 小时、日窗口或历史累计。
  金额原值保持完整精度，USD 展示值
  小于 1 美元时最多保留四位小数，其余保留两位。

### 周/月额度预测

`GET /api/admin/accounts/quota-forecast?accountId=...` 使用管理员鉴权，每次查询返回独立的预测结果。
该接口不刷新上游额度；需要重新观测时，先调用 `POST /api/admin/accounts/quota/refresh`，再查询预测。

响应 `data` 包含 `accountId`、`generatedAt` 和 `forecasts`（`weekly`、`monthly`）：

- `targetDays`、`extrapolated`：对应周期存在真实账号级窗口时采用实际时长；缺少对应窗口时，使用
  可统计的周/月窗口按 7/30 天折算，明确标记 `extrapolated: true`。不把短期限流或模型专属桶当作账号容量。
- `estimatedTokens` / `estimatedUsd` 及对应 `*Display`：本周期已记录用量加预计剩余量，
  公式为 `(本周期已记录用量 + 样本用量 × (100 - usedPercent) / sampledPercent) × 目标窗口秒数 / 源窗口秒数`。
  真实周期由上游额度重置边界定义，不按自然周/月累计；只有折算结果才乘以目标与源窗口的时长比。
- `remainingTokens` / `remainingUsd` 及对应 `*Display`：**额度快照时源窗口**的剩余估算，
  公式为 `样本用量 × (100 - usedPercent) / sampledPercent`；不随目标周期折算，不代表当前可消费余额。
- `source`：源窗口名称 `label`、已用比例 `usedPercent` / `usedPercentDisplay`、
  额度观测时间 `observedAt` / `observedAtDisplay`、用于过期检查的 `resetAt`，以及本周期累计
  已记录的 `tokensDisplay` / `usdDisplay`。`source: null` 表示没有可选的源窗口。
  `observedAt` 表示额度观测时间，与本次查询的 `generatedAt` 不同。
- `unavailableReason`：不能估算时的说明，正常为 `null`。有效进度少于 5 个百分点不预测；
  `lowSample` 表示有效进度不足 10 个百分点，或增量采样少于 2 个完整段，仅是质量提示，不承诺精度。
  `incompleteCost` / `incompleteTokens` 仅提示费用 / Token 记录存在缺口，不阻断已有数据的预测。
  Token 使用已记录的正数用量，费用使用已知的有效 USD 金额；对应数据完全缺失时该项为 `null`，
  两项均不可估算时才返回无可用数据的说明。明确记录的零 USD 仍可估算为零，未知费用不能当成零。
  不按请求数量补齐未知消耗；漏记、失败或缺少计量可能使估算偏低，不代表完整账单或真实剩余额度。

内部采样优先采用近期分段，条件不足时才使用窗口累计估算。同一额度段内，以历史额度和截至相应
完成时间的累计用量建立基线，每累计至少 5 个百分点形成一段，使用最近 3 个完整段及未满一段的尾部。
上式中的 `sampledPercent` 是内部有效额度进度（百分点），不是时间进度或对外响应字段。
只对合并区间计算比值，不平均逐请求小分母比值；重复读数不增加段数，大于 1 个百分点的回落或累计
计数倒退会中断采样，不能静默跨越。重置时间改变或额度明显回落时，旧段不参与新的累计和预测；
当查询区间内包含重置前记录时，以首个新段观测的累计值为基线重新计数，积累至少 5 个百分点后恢复预测。
该基线之前无法精确归属的用量不补算。缓存命中属于输入，不重复相加；其他币种或缺少有效金额不能当成零 USD。

采样查询 `[max(resetAt - windowSeconds, accountAddedAt), observedAt)` 内开始的请求，
仅把 `completedAt <= observedAt` 的完整交付用量计入当前分子；历史分子按各点的完成时间累计，
同完成时间使用同一累计值。历史点抽样不缩减区间内的累计用量，接口不返回原始 Provider 文档。

OpenAI 复用已有限流协议解析器匹配额度桶、槽位、时长和明确的套餐。仅允许最多 2 秒的重置时间抖动，
超出或套餐改变则截断历史基线；不将宽时间容差当作上游窗口身份。历史文档缺少独立额度观测时间，
只能以请求完成时间近似配对，不证明上游扣额与本地完成同步。无法积累有效增量时，仅在账号于窗口开始前
已加入且没有已知采样断点的情况下使用累计估算；中途加入账号可以在新基线之后积累，无需等待下次重置。

预测没有独立持久化或调度状态，也不参与账单结算。站外消耗、历史日志清理、异步观测延迟、未成功交付的
上游消耗和模型组合变化仍可能造成误差；等价 USD 费用不是官方订阅价格或固定额度承诺，
30 天折算也不是自然月额度。记录覆盖率不等于预测准确率，不输出未经校准的置信区间。

### OpenAI 账号辅助请求

OAuth 账号的额度、个人资料、订阅与重置卡请求先使用 `openai.api.base_url`。自定义上游明确返回 HTTP 404 时，
仅回退一次到官方账号接口，沿用当前账号凭据与出站代理；其他状态码、解析错误或传输失败不触发回退。
部署者需允许账号请求访问官方端点；回退不绕过账号出站代理，也不保证官方可达。
回退后的响应或错误作为最终结果，不以原来的 404 覆盖。重置卡消费回退复用原始请求体和幂等键，
传输失败仍按消费结果不明确处理。API Key 账号不会因此获得 OAuth 账号能力，也不会改变推理请求的目标地址。

### 账号个人信息

`GET /api/admin/accounts/personal-info?accountId=...` 需要管理员会话，提供 OpenAI/Codex OAuth 账号的个人信息。后端并发读取资料统计与订阅，
一次返回；每次请求均重新查询，不刷新 credential，不读取本地 usage/billing 记录，也不缓存或估算统计结果。
查询沿用上述 OpenAI 404 路由回退。

响应 `data` 包含：

| 字段 | 类型 | 含义 |
| --- | --- | --- |
| `profile` | object 或 null | 官方个人资料、累计统计与活动洞察；查询失败为 null |
| `profileError` | string 或 null | 资料查询失败时的安全错误提示；成功为 null |
| `subscription` | object 或 null | 当前绑定账号的订阅周期；无可用周期或查询失败为 null |

两部分的查询结果独立：资料失败时仍返回可用订阅，订阅失败时仍返回资料。账号不存在、查询参数不合法或
无管理员会话时，仍返回标准错误；请求期间账号身份或 credential revision 改变时拒绝整份结果。

#### 官方资料与累计统计

`profile` 包含：

- `displayName`、`username`、`imageUrl`：官方账号资料；
- `summary`：累计文本 Token、单日峰值 Token、最长任务时长、当前连续天数和最长连续天数；
- `dailyUsage`：按日期返回的 Token 活动；
- `activityInsights`：快速模式占比、上游原样返回的推理强度及占比、Skill 探索/使用数、聊天总数，
  以及插件与 Skill 调用排行。

官方未返回的字段保持 `null`，不使用本地数据补齐；`hasStatsError: true` 表示账号资料可用，但官方统计
部分不可用。access token 已过期或官方返回 401 时，通过 `profileError` 提示先刷新 credential 或重新授权。

#### 订阅信息

OpenAI 使用当前凭据、绑定的上游账号 ID 和账号出站代理访问 `/backend-api/subscriptions?account_id=...`，
不枚举其他账号，不返回上游订阅 ID 或原始响应。

`subscription` 为 `null`（未获得可用订阅周期），或包含以下字段：

| 字段 | 类型 | 含义 |
| --- | --- | --- |
| `startsAt` | RFC 3339 字符串或 null | 上游本期开始时间 |
| `expiresAt` | RFC 3339 字符串 | 上游本期结束时间，不代表自动续费账号最终失效 |
| `willRenew` | boolean 或 null | 自动续费状态；未知不推断为 false |
| `billingPeriod` | string 或 null | 上游计费周期标识 |
| `billingCurrency` | string 或 null | 上游计费币种 |
| `observedAt` | RFC 3339 字符串 | 本次查询时间 |

订阅不写入额度快照或数据库，不参与账号状态或调度；OpenAI 查询含 404 回退共用最多 5 秒预算，响应最多 64 KiB。
上游失败或未提供有效周期时返回未知，不据此标记免费、过期或禁用；请求期间账号身份或 credential
revision 变化时丢弃结果。

### 主动额度重置卡

`GET /api/admin/accounts/reset-credits?accountId=...` 每次都查询对应 Provider；后端不把卡片列表写入
PostgreSQL 或 Redis。OpenAI OAuth 与声明该能力的插件可使用，账号视图分别以 `resetCredits` 和
`consumeResetCredit` 表示查询和消费能力。

查询响应：

```json
{
  "availableCount": 1,
  "credits": [{
    "id": "credit_...",
    "status": "available",
    "title": "...",
    "expiresAt": "2026-08-31T12:00:00Z",
    "resetType": "..."
  }]
}
```

消费请求的 `redeemRequestId` 必须是小写、带连字符的 canonical UUIDv4；`creditId` 可省略，由上游选择
可用卡。一次请求发出后若传输结果不明确，重试必须复用完全相同的 `redeemRequestId`、`creditId` 和
账号。服务在单副本进程内按账号串行消费，并在 credential 需要刷新时以同一命令重试一次；它不会对不明
结果自动创建新消费。

若服务无法确认不可逆消费是否完成，返回 HTTP `502` / 业务码 `50202`；客户端应先刷新卡片与额度状态，
并在确需重试时复用原 `redeemRequestId`。明确的上游 HTTP 拒绝仍使用 `50201`，不会误标为结果未知。

```json
{
  "accountId": "acct_...",
  "creditId": "credit_...",
  "redeemRequestId": "8fbf302d-11df-4bd5-82e4-08e4b3df7874"
}
```

消费响应只返回上游结果 `code` 和可选 `credit`。消费端确认成功后应重新 GET 卡片列表，并显式调用
`POST /api/admin/accounts/quota/refresh` 回读上游额度；未提供 `quotaRefresh` 能力时不发起该查询。
不得因为消费成功直接改写本地 `resetAt` 或解除冻结。xAI 不支持该能力。

插件的已确认拒绝与凭据需刷新是完整业务结果；消费调用超时、进程退出、协议损坏或结果无法通过校验时，
按结果未知处理。消费期间账号身份或凭据版本变化也返回结果未知，不将旧结果套用到新绑定账号。

## 6. 账号分组

分组是 Provider-neutral 的账号集合；一个组可包含任意 Provider 账号，一个账号也可属于多个组。
分组详情和列表返回 `disableFast`，创建时省略默认为 `false`，更新时省略或 `null` 保留现值。
Client Key 绑定的任一分组开启此限制（包括已禁用分组）时，该 Key 的 OpenAI Responses 请求关闭 Fast；
未绑定分组的 Key 不限制 Fast，不按最终所选账号的分组判断。

关闭 Fast 只将顶层 `service_tier` 的 `priority`（含 `fast` 别名）改为显式 `default`，继续处理请求；
不改变 `flex`、`ultrafast`、缺失值、默认档、嵌套字段或其他 Provider。
HTTP 和每个 WebSocket `response.create` 均使用请求开始时的分组策略，同一请求重试保持该策略；
HTTP 请求头及新建 WS 的握手提示按当时的最终出站档位构造；复用 WS 时不重发握手头，
每个 `response.create` 仍独立应用档位策略，请求档位统计与本地费用估算使用各帧的最终出站档位。

| 方法 | 路由 | 主要 query/body | 说明 |
| --- | --- | --- | --- |
| `GET` | `/api/admin/account-groups` | `page`、`pageSize`、`search`、`enabled` | 分页查询分组；返回账号可用性、并发槽位（Redis 不可用时 `usedSlots=null`）及成功请求 USD 用量 |
| `POST` | `/api/admin/account-groups/create` | `{ name, description, color, disableFast? }` | 创建空分组；`color` 严格为 `#RRGGBBAA`，返回时统一大写 |
| `POST` | `/api/admin/account-groups/update` | `{ id, name, description, color, disableFast? }` | 更新名称、描述、颜色和 Fast 限制 |
| `POST` | `/api/admin/account-groups/enable` | `{ id }` | 启用 |
| `POST` | `/api/admin/account-groups/disable` | `{ id }` | 禁用；已绑定 Key 保持受限，不回退到全部账号 |
| `POST` | `/api/admin/account-groups/delete` | `{ id }` | 删除未被 Client Key 引用的组 |

列表数据为 `{ items, page, configRevision }`，其中 item 返回 `memberCount`、按 Provider 聚合的
`providerCounts` 和 `clientKeyCount`。查询分组成员使用账号列表的 `groupId` 筛选，
不提供独立的分组成员路由；账号的 Provider 不代表整个分组的 Provider。
`capacity.totalSlots` 为 `number | null`：`null` 表示可用成员中存在继承无限并发的账号，`0` 表示没有可用槽位。
`capacity.usedSlots` 继续返回实际在途数；Redis 不可用时为 `null`。

分组费用按请求执行时实际服务账号的分组快照归属，不按 Client Key 绑定的分组分摊。
账号属于多个组时，各组均包含该请求费用；之后调整账号分组不重写历史归属。

## 7. Client Key

| 方法 | 路由 | 主要 query/body | 说明 |
| --- | --- | --- | --- |
| `GET` | `/api/admin/client-keys` | `cursor`、`limit`、`search`、`sortBy`、`sortDirection` | 游标分页查询 |
| `POST` | `/api/admin/client-keys/create` | 创建字段 | 创建带账号范围的 Client Key |
| `GET` | `/api/admin/client-keys/reveal` | `id` | 显式读取完整明文 Key |
| `POST` | `/api/admin/client-keys/update` | 更新字段 | 原子更新名称、分组范围和限额 |
| `POST` | `/api/admin/client-keys/reset-budget` | `{ id, period }` | 管理员清零日／周已用金额；`period` 为 `daily`、`weekly` 或 `all` |
| `POST` | `/api/admin/client-keys/enable` | `{ id }` | 启用 |
| `POST` | `/api/admin/client-keys/disable` | `{ id }` | 禁用 |
| `POST` | `/api/admin/client-keys/delete` | `{ id }` | 删除 |

创建字段为 `name`、可选 `label`、`groupIds`、`maxConcurrency`、`requestsPerMinute`、可选
`dailyLimitUsd`、`weeklyLimitUsd`、`customKey` 和 `providerRequestProfileOverrides`。更新请求携带 `id`，不接受 `customKey`。
`groupIds` 必须显式提交：空数组派生 `routingScope: "all"`，非空数组派生
`routingScope: "groups"`。响应同时返回分组引用 `groups`，以及从当前有效账号池派生、仅供展示的
`providerKinds`。创建和 reveal 响应会返回完整明文 Key，调用方
必须立即安全保存。

`providerRequestProfileOverrides` 按 Provider ID 保存身份选择，列表返回同名对象。创建时各项为完整配置对象，
未提供的 Provider 跟随通用设置；更新时省略某项保留原值，设为 `null` 清除该项覆盖。最多 256 项，每项最多 64 KiB。
独立配置整体覆盖通用设置，不逐字段继承；切换全局配置不会影响独立 Key。目录与选择规则见 [Provider 客户端身份](#provider-客户端身份)。

兼容字段 `openaiClientProfileOverride` 和 `xaiClientProfileOverride` 分别对应上述映射中的 `openai`、`xai`。
创建时省略或 `null` 表示无覆盖；更新时省略保留、`null` 清除。与通用字段同时提交时必须一致，否则拒绝整个请求；
响应中的兼容字段从同一映射派生，不是第二份配置。

密钥列表的 `search` 仅匹配名称和标签，不匹配密钥值或可见前缀；搜索不区分大小写，使用字面量前缀匹配。
创建和更新时去除名称首尾空白，并按忽略大小写、首尾空格的名称查重，重复返回 `409`。
更新排除当前记录，并发写入同样执行查重；失败不会留下审计或配置版本变更。
历史重名数据不自动改名，已有凭据继续有效；再次保存时需使用未被其他密钥占用的名称。

`customKey` 仅用于创建：省略、`null` 或空字符串时继续自动生成；非空时按原值保存，不追加前缀、
不截断、不修剪空白，也不要求固定长度。Key 须为 HTTP Bearer 可传输的非空可见 ASCII 字符，
支持标点，空格、控制字符和非 ASCII 文本会返回 `400`。重复 Key 返回 `409`，包括并发创建时。
应用不另设 Key 长度上限；创建请求和 `Authorization` 头仍受 Web 服务器及反向代理的通用大小限制。
更新接口不接受 `customKey`，防止修改策略时意外替换正在使用的凭据。
列表仅展示最多前 10 个字符，且至少隐藏一半字符；单字符 Key 的可见前缀为空。
完整值仍只通过创建和显式 reveal 返回，不进入普通 Debug 或审计。

自定义 Key 创建示例：

```json
{
  "name": "迁入的客户端",
  "customKey": "legacy-platform-key/example+=",
  "groupIds": [],
  "maxConcurrency": 2,
  "requestsPerMinute": 0
}
```

自定义 Key 的分组权限、日／周限额和并发规则按本平台配置执行，不导入其他平台的历史用量。

金额字段为非负十进制字符串，最多 10 位整数与 10 位小数，`"0"` 表示不限额。
创建时省略金额字段默认为零；更新时省略或 `null` 保留当前值，修改限额不会清空已用金额。
`maxConcurrency` 和 `requestsPerMinute` 是非负整数，零表示不限。

列表返回 `dailyLimitUsd`、`weeklyLimitUsd`、`dailyUsedUsd`、`weeklyUsedUsd`（均为字符串）、
`dailyResetsAt`、`weeklyResetsAt`（RFC3339 或 `null`）。
记账和限额比较保留完整精度。
日窗口按北京时间零点重置；周窗口从首次准入当天零点起持续七天，到期后在下一次使用时重新开启。
手动重置仅清零所选周期的已用金额，保留限额上限、原到期时间和历史费用，返回 `{ id }`。
未使用或已过期的窗口不会因手动重置而重新开启。重置前完成但延迟结算的费用不再计入所选周期；
重置后完成的请求继续计费，包括重置时仍在进行的请求。操作保留管理员审计，不改变账号上游额度。
费用按请求完成时间归属窗口。并发按同一 Key 的执行中请求累计，包含 SSE 与每个 WebSocket
`response.create`；空闲连接不占名额，内部重试不重复占用。
修改 Key 策略对既有 WebSocket 连接的下一次请求同样生效，已开始的请求保持原有快照。

运行设置可以分别启用 Key 与账号的有界排队。Key 并发满时按 Key 等待；账号先使用其它可调度候选，
适用账号均暂时满载后按账号等待。RPM、金额限额、失效账号和上游冷却不通过排队绕过。
队列满返回 `429` / `concurrency_queue_full`，排队超时返回 `429` / `concurrency_queue_timeout`；
WebSocket 使用对应错误事件。等待期间不发送上游请求，取消后释放等待位置，排队重查不重复计入 RPM。
SSE 在取得有效执行前不发送保活帧，因此此阶段保留 HTTP 错误状态；已开始交付的失败沿用流内错误合同。

任一已结算金额达到限额后拒绝新请求，已准入请求可完成并使金额超过阈值。
HTTP 返回 `429`，`error.code` 为 `key_daily_budget_exceeded` 或 `key_weekly_budget_exceeded`，
并附 `Retry-After`；WebSocket 每次 `response.create` 执行相同检查并返回协议错误事件。
只累计上游上报或按用量与模型价格计算出的 USD 费用；无法取得费用的尝试按零累计，
保留错误和用量诊断，不产生待核账记录或阻断。内部重试中已经取得的费用仍会累计。
预算存储不可用时返回 `503`、`key_budget_unavailable`。

自动结算按网关请求 ID 幂等执行。账本独立于使用统计日志，记录保留至删除 Key，
不受 `usageRetentionDays` 影响。

## 8. 运行设置

| 方法 | 路由 | 说明 |
| --- | --- | --- |
| `GET` | `/api/admin/settings` | 读取运行设置 |
| `POST` | `/api/admin/settings/update` | 原子保存运行设置；身份配置按显式提供的 Provider 项更新 |
| `GET` | `/api/admin/settings/client-downloads/codex-desktop/windows` | 提取 Codex Desktop Windows 离线安装直链；`refresh=true` 强制刷新进程内短缓存 |
| `GET` | `/api/admin/settings/client-profiles/{provider}` | 读取 `openai` 或 `xai` 的可选配置和 `globalConfiguration` |
| `POST` | `/api/admin/settings/client-profiles/{provider}/preview` | `{ configuration }`，完整配置对象或 `null`（解析当前通用设置）；只预览，不保存 |
| `GET` | `/api/admin/settings/admin-api-key` | 只返回管理 API Key 是否存在 |
| `POST` | `/api/admin/settings/admin-api-key/delete` | 删除管理 API Key |
| `POST` | `/api/admin/settings/admin-api-key/regenerate` | 重新生成并一次性返回完整管理 API Key |

设置更新字段包括：

```text
providerRequestProfiles
openaiClientProfile
xaiClientProfile
requestLocationEnabled
requestLocation
modelMappings
refreshMarginSeconds
refreshConcurrency
maxConcurrentPerAccount
maxWaitingPerKey
maxWaitingPerAccount
concurrencyWaitTimeoutSeconds
responsesMaxDecompressedBodyBytes
requestIntervalMs
rotationStrategy
minCodexDesktopVersion
minCodexCliVersion
usageRetentionDays
opsEventRetentionDays
auditRetentionDays
accountAutoFreezeEnabled
accountAutoFreezeThreshold
accountAutoFreezeWindowSeconds
accountAutoFreezeDurationSeconds
accountAutoFreezeProbeEnabled
accountAutoFreezeProbeModel
accountAutoFreezeAdaptiveConcurrency
accountWarmupEnabled
accountWarmupScheduleTime
accountWarmupModel
```

定时账号预热默认关闭。`accountWarmupScheduleTime` 使用北京时间（UTC+8）的 `HH:MM`，
多个时段以逗号分隔，默认 `08:00`；`accountWarmupModel` 默认 `null`，开启前必须显式选择模型。
任务面向可用的 OpenAI OAuth 账号，跳过周额度耗尽及五小时窗口距离重置仍超过 30 分钟的账号。
只有收到响应成功终态才记为预热成功；预热不计入客户端业务用量。

`requestLocationEnabled` 是必填布尔值，默认 `false`：关闭时不覆盖客户端原有位置和时区；开启时使用已保存的
`requestLocation`。关闭不会清空自定义值，代理自定义位置仍优先。
`requestLocation` 是必填的完整对象 `{ country, region, city, timezone }`，不接受 `null`；初始保存值为
`{ country: "US", region: "Ohio", city: "Piketon", timezone: "America/New_York" }`。
字段约束与[代理位置](#独立代理管理--managed-proxies)一致。全局自定义开启后，OpenAI Responses 使用全局位置，
关联代理配置了自定义位置时优先使用代理值。保存后通过现有配置发布机制对新请求生效，
已开始请求及其重试保持同一份全局值；普通文本、绝对时间戳和数据驻留要求不受影响。

`maxConcurrentPerAccount` 是默认账号并发上限，取值 0～4294967295；`0` 表示不限制。
账号的 `concurrencyLimit: null` 继承该默认值，单独设置的正数上限仍优先生效。
无限并发仍统计在途请求，并遵守最小请求间隔、账号可用性与 Client Key 限制。

`maxWaitingPerKey` 与 `maxWaitingPerAccount` 是全局统一的排队容量，取值 0～1,000，默认 0（关闭）；
每个 Key、每个账号各自独立计数，没有单对象覆盖字段。执行并发为 5、最大排队数为 5 时，
该对象最多容纳 5 个执行请求与 5 个等待请求。Key 并发为 0（不限）时跳过 Key 排队。
`concurrencyWaitTimeoutSeconds` 取值 1～120，默认 30，从首次入队开始计时，密钥与账号两层共享该等待时限；
切换账号或内部重试不重新计时，等待同时计入请求总超时。该时限不用于中断已开始的上游生成。
设置更新请求须包含这三个字段，新请求使用更新后的快照。

`responsesMaxDecompressedBodyBytes` 是压缩 Responses HTTP 请求的解压输出上限，单位字节，默认
67108864（64 MiB）。必须为正整数，且可表示为进程平台的 `isize`；管理端以整数 MiB 编辑。
保存并发布成功后，新请求使用新值；已鉴权请求沿用原快照，无需重启。调高上限会增加大请求的内存占用，
它不代表整个进程的内存预算。

`rotationStrategy` 可取 `smart`、`quota_reset_priority`、`round_robin`、`sticky`。
两个 `minCodex*Version` 字段为 `string | null`，只设置最低版本，不存在最大版本字段。

### 模型定价

定价接口独立于运行设置的整体替换，仅允许管理员访问。改价影响本地估算费用与 Client Key 金额限额，
不改变上游报告金额、订阅账号实际扣额或历史费用。请求开始时冻结价格，内部重试沿用同一份配置。

| 方法 | 路由 | 请求与返回 |
| --- | --- | --- |
| `GET` | `/api/admin/settings/pricing` | 返回 `{ defaults, synced, overrides, syncedAt }` |
| `POST` | `/api/admin/settings/pricing/update` | `{ provider, models, change }`，成功返回 `{ saved: true }` |
| `POST` | `/api/admin/settings/pricing/sync/preview` | 无 body；返回 `{ prices, skipped }`，不写入配置 |
| `POST` | `/api/admin/settings/pricing/sync` | `{ preview: { prices, skipped }, models: { openai: ["gpt-5.4"] } }`；成功返回 `{ saved: true }` |

价目使用 `Provider → 精确上游模型 ID → { multiplierBps, bands }` 的映射。优先级为人工覆盖、已同步价目、
内置价目；按档位合并，不从客户端模型别名或响应模型猜测价格。`syncedAt` 为 ISO 时间或 `null`。
每个档位包含四个非负十进制字符串：`input`、`output`、`cacheRead`、`cacheWrite`，单位 USD / 百万
Token，范围 0～1000000、最多四位小数。`"0"` 表示免费，缺少整个档位表示继承；不能只缺少部分单价。

`bands` 可用键为 `standard`、`fast`、`flex`、`long_standard`、`long_fast`、`long_flex`、`image`。
OpenAI 长上下文为输入超过 272000 Token，xAI 为输入达到 200000 Token；仍按 Provider 支持的
模型与服务档位判断是否能估算。xAI 不接受 `flex`、`long_flex`、`image`。图像端点的 `standard`
用于文本输入，`image` 用于图像 Token；不能用文本输出单价替代图像输出单价。用量无法完整拆分时不估算。

`multiplierBps` 为 0～1000000 的整数，10000 表示 1 倍、0 表示本地估算为零，最大 100 倍。
它作用于本地费用及对应明细，独立于服务档位；缺少计价依据的请求即使倍率为零仍然是费用未知。

`provider` 为内置 `openai` 或 `xai`，对应价目由 `GET` 返回的 `defaults` 提供。
`models` 为 1～500 个模型 ID，每个 ID 为
1～128 字节且不含空白、控制字符。`change` 为以下形式之一：

- `{ "action": "replace", "pricing": { "multiplierBps": 12500, "bands": { ... } } }`：替换选中模型的人工
  配置，未提供档位重新继承来源；内置和同步均未登记的模型必须包含 `standard`。
- `{ "action": "multiplier", "multiplierBps": 20000 }`：设置目标倍率，保留已有人工单价；重复提交不连续相乘。
- `{ "action": "reset" }`：移除人工单价与倍率，恢复同步价或内置价；仅有人工价格的模型恢复为未配置。
- `{ "action": "delete" }`：删除非内置模型的同步价目与人工配置；批量包含任何内置模型时整批返回 400，
  即使该内置模型已有人工覆盖也不能删除。删除不影响历史账单，之后可重新添加或选中同步导入。

一批更新原子提交并写审计，不覆盖未选中的模型。未知字段、错误类型和非法价格字符串等 JSON 合同错误
返回 422；Provider、模型 ID、批量数量、倍率上限和不支持的档位等业务校验错误返回 400。
models.dev 同步只导入可表示为当前文本 Token 计价的 OpenAI/xAI 模型；不完整价格、其他输出模态及
不匹配的上下文梯度在 `skipped` 中返回 `provider/model`。确认会重新抓取价目；若与预览不同返回 400，
需重新预览。来源不可用返回 502，已有价目保持不变。`preview` 必须原样提交，`models` 按 Provider
指定 1～10000 个待同步模型；空选择或未登记的模型返回 400。同步只更新选中模型的来源层，未选中模型
及所有人工单价与倍率保持不变。选中的已同步模型若不再出现在来源价目中，则移除其来源层，恢复内置价格；
没有内置价目的模型变为未配置。

### Provider 客户端身份

`providerRequestProfiles` 是按 Provider ID 索引的通用配置对象，最多 256 项、每项最多 64 KiB。
更新时省略某项保留原值，对象替换该项，`null` 删除显式选择并使用 Provider 默认值；不清除其他 Provider 的配置。
配置内容由对应 Provider 校验，不包含账号凭据。Key 可通过 `providerRequestProfileOverrides` 整体覆盖所属 Provider 的选择。

`openaiClientProfile` 与 `xaiClientProfile` 是同一映射的兼容字段：省略保留，不能显式提交 `null`；
与通用字段同时提供时必须一致，否则整次更新拒绝。读取响应从映射派生这两个字段，不维护平行状态。

直接读取 `openai` 或 `xai` 的选项与预览。OpenAI 选项返回六个 `presets`，xAI 返回 `defaults`；
响应均包含 `globalConfiguration`。

### OpenAI 上游客户端身份

`openaiClientProfile` 保存通用选择，首次默认 `MacOS · Desktop · 自动最新`。
兼容字段的更新语义见上节。初始化不读取 YAML 身份字段。
该配置作用于 Client Key 的 OpenAI 模型请求与原生模型目录，适用于 HTTP/SSE、WebSocket、Images 和 Search。
不改变 xAI、入站客户端版本门禁、账号认证或后台 Desktop 专属操作。

自定义配置使用 `{ "mode": "custom", "userAgent": "完整 UA" }`。
已识别的 `Codex Desktop`、`codex-tui`、`codex_exec`、`codex_cli_rs` 前缀由后端解析 `originator` 和 Core `version`，
显式提供的配套字段必须与识别结果一致。未知前缀须另填 `originator` 和 `codexVersion`，这两个字段与 UA 一起发送。
UA 须为 1 至 4096 字节的单行可见 ASCII 文本，首尾不能含空白；`originator` 最多 128 字节，
`codexVersion` 最多 64 字节并须符合 SemVer。自定义配置不要求 Desktop 构建号，也不自动更新。

没有 `mode` 字段的预设配置按以下合同解析，可选字段省略或 `null` 时使用所选预设参数：

| 字段 | 取值与语义 |
| --- | --- |
| `client` | 必填，`desktop` 或 `cli` |
| `platform` | 必填，`macos`、`linux` 或 `windows` |
| `versionMode` | 必填，`latest` 或 `fixed` |
| `cliEntry` | CLI 可选 `tui` 或 `exec`，省略或 `null` 保留 Core 默认身份；Desktop 不接受此字段 |
| `originator`、`osType`、`osVersion`、`arch`、`terminal` | 可选自定义参数，非空、最多 128 字节；只接受可见 ASCII，不能包含括号、分号、反斜杠及首尾空白 |
| `codexVersion` | `fixed` 必填的 Core SemVer；`latest` 必须省略或为 `null` |
| `desktopVersion`、`desktopBuild` | 仅 Desktop 的 `fixed` 模式必填，分别为数字点分版本和数字构建号；CLI 不接受这些字段 |

```json
{ "client": "cli", "platform": "linux", "versionMode": "latest", "cliEntry": "tui", "osType": "Alpine Linux", "osVersion": "3.24.1", "terminal": "xterm-256color" }
```

TUI 默认标识为 `codex-tui`，Exec 为 `codex_exec`，入口后缀使用同一次解析的 Core 版本。
`originator` 覆盖只更改产品名前缀和配套头，后缀仍表示所选入口。省略 `osType` 使用平台名称；
自定义运行环境在自动更新时保持不变。旧配置未指定 `cliEntry` 时继续使用 `codex_cli_rs` 默认值且不添加入口后缀。

六套预设均支持自动更新：macOS Desktop 支持 arm64，Windows/Linux Desktop 及三套 CLI 支持 arm64、x86_64。
预设接口的 `automaticAvailable`、`reason` 表示当前组合的可用性；自定义架构可能使自动解析不可用。
每 24 小时后台检查官方稳定发布，失败保留同组合上次有效版本；固定值不受后台更新影响。
Desktop 的应用版本、Core 和构建号来自同一平台、架构的官方制品：macOS ZIP、Windows MSIX、Linux DEB。
Windows/Linux 通过 ETag 检查更新，未变化时复用已核验版本；CLI 依据官方 npm 稳定标签和对应平台依赖。

预览返回 `configuration`、`source`（`global` / `override`）、`userAgent`、解析后的环境和版本字段，
以及 `versionSource`（`official` / `custom`）、`verifiedAt`、`checkedAt`、`error`。
自定义预览中的 `recognized` 表示是否识别出配套请求头。
`verifiedAt` 只表示版本资料核验，不能代表自定义运行环境或 TLS 已核验；固定版本返回 `null`。
完整自定义配置不携带官方制品核验时间。
客户端画像配置控制应用层请求字段，不切换操作系统的 TLS 实现。默认 HTTP 使用 native TLS，
WebSocket 使用 rustls；配置自定义 CA 时 HTTP 也使用 rustls。TLS 指纹需按实际部署平台与传输路径核验。
未完成本次启动检查时 `checkedAt` 为 `null`。非法或当前不可用的选择返回 `400`，保存失败不提交其他修改。

配置在请求开始时冻结，Provider 首次解析的版本用于该请求的全部重试与换号。
已建立 WebSocket 的精确续写沿用所属连接；新请求使用保存后的选择。

### xAI 上游客户端身份

`xaiClientProfile` 保存 Grok CLI 的通用身份选择；首次使用内置 `grok-shell / headless / linux / x86_64`
并采用自动更新版本。初始化不读取 YAML 身份字段，数据库已有选择时不覆盖。
更新设置省略该字段保留现值，不能提交 `null`。

| 字段 | 取值与语义 |
| --- | --- |
| `versionMode` | 必填，`latest` 或 `fixed` |
| `clientVersion` | `fixed` 必填 SemVer，最多 64 字节；`latest` 必须省略或为 `null` |
| `clientIdentifier`、`clientMode`、`targetOs`、`targetArch` | 必填，各为 1～64 字节可见 ASCII，不含空白或控制字符 |

```json
{"versionMode":"latest","clientVersion":null,"clientIdentifier":"grok-shell","clientMode":"headless","targetOs":"linux","targetArch":"x86_64"}
```

配置作用于 Client Key 的 xAI 模型请求和压缩请求，影响 `x-grok-client-version`、`x-grok-client-identifier`、
`x-grok-client-mode` 与 User-Agent；OAuth、后台目录和额度查询使用 Provider 内置官方画像。
User-Agent 使用 `grok-shell/<版本> (<系统>; <架构>)`，其中 `arm64` 按官方规则展示为 `aarch64`。
`clientIdentifier` 只控制对应请求头，不替换 User-Agent 中的 `grok-shell` 产品名。
每个请求开始时解析并冻结身份，重试和换号沿用该身份；保存后新请求生效，密钥独立配置优先于通用设置。

自动版本通过官方 npm 检查稳定版本，周期 24 小时；检查失败保留当前进程最近有效版本，重启以内置基线开始。
固定版本不受后台更新影响。预览返回配置、来源、最终身份字段、`userAgent`、`versionSource`、`verifiedAt`、
`checkedAt` 和 `error`；固定版本不附带官方核验时间。Dashboard 展示已保存的通用身份。

### 账号自动冻结

账号自动冻结（`accountAutoFreezeEnabled`）默认关闭。启用后，在统计窗口内按尝试累计明确的上游容量拒绝
（`server_is_overloaded`、`slow_down` 或结构化错误中的明确过载提示），普通 5xx、`invalid_prompt` 和未识别的上游错误不计入。
达到阈值后把该账号冻结为带恢复倒计时的 `rate_limited` 状态。`accountAutoFreezeThreshold`
取值 2～1,000（默认 12，按普通请求的 attempt 计数，含请求内同账号重试，不含诊断探测与本地连接保护错误）；`accountAutoFreezeWindowSeconds`
取值 60～3,600（默认 600，随每次失败滑动顺延）；`accountAutoFreezeDurationSeconds` 取值 300～604,800
（默认 7,200，即 2 小时，探测失败后按该时长顺延）。`accountAutoFreezeProbeEnabled` 开启时恢复 worker
在到期后执行真实探测调用，成功才解冻；探测过程中和服务重启后继续阻止普通请求。关闭自动冻结或探测后，
已有冻结等待当前冷却结束再恢复调度。`accountAutoFreezeProbeModel` 为 `string | null`，留空时自动
选择账号可用的第一个模型。`accountAutoFreezeAdaptiveConcurrency` 开启时冻结期间把账号并发上限下调到
观测在途峰值的 80%（下限 2，只降不升）。这会持久修改账号并发设置；跟随全局默认的账号也会设为独立上限，
解冻后不自动恢复，管理员可手动改回。

### Windows 客户端下载

Windows 离线包接口固定解析 Microsoft Store Product ID `9PLM9XGG6VKS` 的 Retail 包，不接受调用方提供
产品 ID、上游地址、ring 或文件名。后端只返回通过包名、架构、Microsoft CDN host/path、scheme 和失效
时间校验的 `x64` / `arm64` MSIX 直链，不代理安装包字节。Store 内容通道返回 HTTP/80 临时地址时保留
原始 scheme，不强制改写为该 host 不保证支持的 HTTPS。动态链接不足 10 分钟即失效时不会下发；某个架构
解析失败时只将该架构降级到 OpenAI 官方 HTTPS 稳定 MSIX，并通过 `warning` 说明。响应形状为：

```json
{
  "resolvedAt": "2026-09-01T06:30:00Z",
  "cached": false,
  "warning": null,
  "packages": [
    {
      "architecture": "x64",
      "source": "microsoft_store",
      "version": "26.825.6671.0",
      "fileName": "OpenAI.Codex_26.825.6671.0_x64__2p2nqsd0c76g0.msix",
      "sizeBytes": 744250000,
      "downloadUrl": "http://dl.delivery.mp.microsoft.com/filestreamingservice/files/...",
      "expiresAt": "2026-09-01T07:30:00Z"
    }
  ]
}
```

`source` 为 `microsoft_store` 或 `official_openai`。Store 的四段 package version 只用于下载展示，不参与
Desktop 三段 SemVer 门禁，也不会自动回写最低版本设置。门禁规则见
[鉴权与公共约定](#1-鉴权与公共约定)，解析器职责见 [架构文档](architecture.md#11-生命周期安全与恢复)。

## 9. 备份

全部备份端点位于 `/api/admin/settings/backups/*`，使用管理接口响应信封，字段为 camelCase，响应带 `Cache-Control: no-store`。

| 方法 | 路由 | 请求 | 说明 |
| --- | --- | --- | --- |
| `GET` | `/api/admin/settings/backups` | 无 | 读取存储配置（含明文 Secret）、验证状态与调度配置 |
| `POST` | `/api/admin/settings/backups/storage/update` | S3 配置 | 更新存储配置；`secretAccessKey` 为空字符串会校验失败 |
| `POST` | `/api/admin/settings/backups/storage/test` | 无 | 测试已保存的存储配置（Put/Head/Get/Delete 探针） |
| `POST` | `/api/admin/settings/backups/schedule/update` | 调度配置 | 更新 Cron、时区与保留策略 |
| `GET` | `/api/admin/settings/backups/records` | 查询参数 | 分页查询备份记录 |
| `POST` | `/api/admin/settings/backups/create` | `{ expiresInDays? }` | 创建手动备份，返回 `202 Accepted`；`expiresInDays` 为过期天数（0 或缺省表示不过期） |
| `POST` | `/api/admin/settings/backups/download-url` | `{ backupId }` | 创建 5 分钟有效预签名下载地址（仅 completed） |
| `POST` | `/api/admin/settings/backups/delete` | `{ backupId }` | 请求删除（进入 `deleting`，由 Worker 收敛硬删除） |

读取设置响应（Secret 以明文返回）：

```text
storageRevision, endpoint, region, bucket, accessKeyId, secretAccessKey, prefix,
forcePathStyle, verified, scheduleEnabled, cronExpression, scheduleTimezone,
retentionDays, retentionCount, nextRunAt, lastVerifiedAt, updatedAt
```

更新存储请求字段：

```text
endpoint, region, bucket, accessKeyId, secretAccessKey, prefix, forcePathStyle
```

`secretAccessKey` 为空字符串会校验失败；由于 GET 会回传已保存的明文 Secret，保存时始终整体提交当前值。已有备份记录时，endpoint/region/bucket/forcePathStyle 不允许变化（存储身份锁定，`409`）；只允许轮换凭据与修改 prefix。

保存相同配置保留验证状态、定时计划及配置版本。存储配置实际变化时，会同时使验证失效、暂停定时计划并清空下次运行时间；连接测试通过后需重新启用计划。

更新调度请求字段：

```text
scheduleEnabled, cronExpression, scheduleTimezone, retentionDays, retentionCount
```

`cronExpression` 为 5 段格式；`retentionDays`/`retentionCount` 为 0 表示禁用对应清理。启用计划前必须已保存完整存储配置且通过连接测试。

记录列表查询参数：

```text
page, pageSize, status, trigger
```

`status` 可取 `queued/dumping/uploading/completed/failed/deleting`；`trigger` 可取 `manual/scheduled`。记录响应字段：

```text
id, triggerKind, status, scheduledAt, objectKey, sizeBytes, sha256, attemptCount,
errorCode, errorMessage, startedAt, completedAt, expiresAt, createdAt, updatedAt
```

`expiresAt` 在创建时确定：手动备份来自 `expiresInDays`，计划备份来自当时的
`retentionDays`；到期后由 Worker 进入删除流程。

连接测试响应：

```text
{ ok, stage, code, message }
```

`stage` 为 `putObject/headObject/getObject/deleteObject`。探测成功后以 `storageRevision` CAS 写入 `lastVerifiedAt`；测试期间配置变化则丢弃结果。

备份错误映射（`AdminErrorCode` 既有体系）：

| HTTP | 场景 |
| --- | --- |
| `400` | 配置、Cron、时区或状态参数无效 |
| `404` | 备份记录不存在 |
| `409` | 已有活跃任务、状态冲突或存储身份锁定 |
| `502` | S3 兼容服务返回无效或失败响应 |
| `503` | PostgreSQL、`pg_dump` 或对象存储暂不可用 |

审计动作：`backup.s3_config_updated`、`backup.s3_connection_tested`、`backup.schedule_updated`、`backup.created`、`backup.download_url_created`、`backup.delete_requested`。审计详情与记录表均不保存 Secret、数据库连接串或预签名 URL query。

## 10. Dashboard、用量与错误

| 方法 | 路由 | 说明 |
| --- | --- | --- |
| `GET` | `/api/admin/dashboard/summary` | Dashboard 汇总；支持 `kind`、`startTime`、`endTime` |
| `GET` | `/api/admin/dashboard/trend` | Dashboard 趋势；`kind=usage|latency|errors` |
| `GET` | `/api/admin/usage/records` | 请求记录分页列表 |
| `GET` | `/api/admin/usage/records/detail` | 按 `id` 查询请求详情 |
| `GET` | `/api/admin/usage/records/summary` | 当前筛选条件的请求汇总 |
| `GET` | `/api/admin/usage/insights/overview` | 用量、成本与成功率洞察 |
| `GET` | `/api/admin/usage/insights/diagnostics` | 按维度聚合诊断 |
| `GET` | `/api/admin/operations/errors` | 运维错误分页列表 |

Dashboard 的 `capacityInfo.maxConcurrentPerAccount` 为默认账号并发上限，`0` 表示不限制。
`capacityInfo.totalSlots` 为 `number | null`；可用账号池含无限并发账号时为 `null`，此时 `availableSlots` 也为 `null`。
`usedSlots` 仍表示实际在途数，Redis 不可用时为 `null`；没有可用账号时 `totalSlots` 为 `0`。

用量查询可组合页码/游标、时间范围、Provider、Client Key、账号、模型、route、transport、状态码、
request/response/upstream ID、outcome 与搜索文本。诊断 `dimension` 可取 `model`、`account`、
`apiKey`、`provider`、`transport`、`failureClass`、`status`。

管理端请求列表及 Dashboard 最近请求中的 `accountNotes` 为账号当前备注，按内部账号 ID 关联。
备注不写入请求历史快照；无备注或账号已删除时返回 `null`，修改备注不改变历史请求的账号归属。

管理端请求列表与详情分别保留 `requestedModel`（客户端请求）、`upstreamModel`（网关发送）与
`upstreamResponseModel`（上游返回）。返回模型缺失时为 `null`，不使用请求或映射模型补齐。
OpenAI 优先采用服务端 `openai-model` / `x-openai-model` 报告（流内报告可覆盖初始响应头），
没有报告时采用正文明确声明的 `response.model`；xAI 采用原始正文声明。正文模型以终态优先，
缺少终态声明时保留首次声明。这些值仅表示上游报告，不作为模型真实性证明，也不参与路由、
聚合或本地计价模型选择。历史数据只回填此前已保存的 OpenAI 模型报告，其余保留未知。

请求记录列表的 `search` 使用字面量前缀匹配，支持请求 ID、Client Key ID / 名称、
账号 ID、账号邮箱与名称、请求 / 上游模型 ID、上游请求 ID。密钥名称不区分大小写，其他字段区分大小写。
密钥名称按当前密钥记录检索，改名后使用新名称，删除后仍可按 Client Key ID 查询历史记录。
账号邮箱与名称按请求记录的历史快照检索，
不随当前账号修改或删除而改变；`%`、`_` 和 `\` 均按普通字符处理，不作为搜索通配符。

请求记录按模型执行 ID 或上游 ID 查询；只有入口 ID 时需结合时间与入口日志定位。
请求记录和错误列表支持按密钥名称搜索，不支持密钥值或可见前缀搜索。

已进入模型执行会话、但尚未登记执行尝试的失败请求也进入错误及详情查询，
包含无可用账号、准备失败、启动/准备超时与取消。此时 attempt 数为零，未确认的 Provider、账号及
上游传输为空；可用模型执行 ID 在“全部平台”下查询，不从路由候选推断实际调用平台。
已登记的尝试即使尚未收到首个上游事件，也会保留真实 attempt。
鉴权、解析、路由和准入等入口拒绝不属于该范围；请求观测仍是可能延迟或丢弃的异步投影。

汇总与洞察中的请求数与 outcome 分布覆盖筛选范围内全部请求；token、缓存、延迟与成本聚合仅统计
已完整交付客户端的成功推理响应。OpenAI 的 `generate: false` 连接与上下文准备记录归类为
`requestKind: "prewarm"`，不进入用量列表、账号用量或额度预测的 Token / 费用覆盖统计，但仍可按
请求 ID 读取审计详情，响应中的额度观测仍可用于预测配对。此分类以实际 `generate` 字段为准，
不能仅凭客户端的同名 metadata 或输出 Token 为零排除普通推理；其他 Provider 不套用该规则。

详情接口按 `id` 可读取成功、失败或未完成请求，返回 `trace`（未采集记录为 `null`）和
`relatedRequests[]`（`requestId / relation / outcome / completedAt`）；`relation` 为 `recovered_by` 或
`recovers`。`trace` 是执行终态时的有界脱敏时间线，包含 request、attempt 和 exchange 关联、阶段、
事件摘要及淘汰计数；普通用量列表不携带此字段。
新采集的未知 JSON 键名与值只保留结构和摘要；事件摘要中的 `eventType` 为已知事件名称字符串、
未知名称的 `{ bytes, sha256 }` 摘要，或缺失时的 `null`。旧 trace 不做清理或回填，
其中的 `sanitized` 标记不能作为可直接公开的保证。

管理端下载的诊断包 `schemaVersion: 2` 用于人工反馈，不是备份或导入格式。它包含关联 ID、错误分类摘要、
请求与错误事件各自的状态、attempt、时间线阶段和计时；不自动导出 message/raw error、任意 metadata、
trace event data、请求响应正文和头部。`availability` 与 `omitted` 明示未采集、不完整或主动省略的内容，
`null` 不代表没有发生错误。版本、环境及原始错误片段仍需操作者另行补充并审阅脱敏。

错误记录中的“已自动恢复”表示系统关联到了后续成功请求，不会把原来的失败记录改为成功。
`upstreamSendState = ambiguous` 表示无法确认该次上游执行结果，不代表后续恢复请求失败；
恢复关联也不等于逐字节验证过两次请求正文。

Dashboard 的 `accountUsage[]` 由后端提供 `usageWindow`、`metricLabel`、`metricValue`。
`usageWindow` 复用账号额度窗口合同，缺失额度事实时为 `null`；窗口标签、百分比、触顶状态、重置时间
和本地用量由 Provider/Admin 投影。前端不得从套餐缺失推断免费套餐，也不得从显示时舍入的百分比推断
触顶。滚动窗口使用相应时间范围的本地用量，独立于 Dashboard 的今日统计范围。

OpenAI 与 xAI 的本地费用估算按实际发送给上游的请求模型（`upstreamModel`）查价，结合响应中的实际
用量计算；客户端请求 A、路由后发送 B 时按 B 计价，响应返回 C 不改变计价模型。实际发送模型缺少
定价时不估算，也不借用响应模型的价格。Provider 明确上报的已计费金额仍优先于本地估算。

OpenAI Responses 用量记录的 `serviceTier` 与本地费用估算统一采用 Provider 最终发给上游的请求
`service_tier`，不使用响应档位覆盖或回退。例如发送 `priority`、响应回显 `default` 时，仍显示
`Fast` 并按 Priority 价格估算。未发送档位时，`serviceTier` 保持缺失，展示与估算按标准档处理。
计费展示把 `priority`/`fast` 映射为 `Fast`，`flex` 映射为 `Flex`，缺失或 `default`/`standard`
映射为 `Standard`；其他非空值以首字母大写展示。各模型、档位及长上下文区间使用明确登记的价格；缺少对应
价格时不估算（包括 `auto` 和未知档位），不以固定倍数兜底。展示的倍率由所选档位与标准档位费用之比
计算，托管工具调用费不随 Token 档位倍增。
Provider metadata 分别保留 `requestedServiceTier` 与 `upstreamServiceTier` 供诊断；发送给客户端的
原始 `response.service_tier` 不变。用量中的 Fast 仅表示发送档位，不能证明上游实际加速，本地费用
估算也不能代替官方账单。档位与费用在请求记录生成时确定；查询不会回填历史档位或重算已存储费用。

`/api/admin/usage/records` 与 `/api/admin/ops/errors` 的记录返回 `clientApiKeyName`，为关联 Key 的当前名称；
Key 已删除或未关联时为 `null`，不影响记录返回，不包含密钥原文。

本地计价使用[模型定价](#模型定价)的生效规则。缺少内置、同步及人工价格时不生成估价。新请求的本地
费用明细、有效单价、服务档位与自定义倍率随终态记录持久化，后续改价和清除覆盖不重算历史明细。
旧记录没有费用快照时仍按内置规则核对总额后补充拆分，核对失败只显示原总额。
`billing.longContextBillingApplied` 表示已应用长上下文价格区间，与服务档位、自定义倍率独立；
旧费用快照未记录该事实或只有总额时返回 `false`，不按当前价格倒推历史标识。图像明细通过可选的
`billing.image` 返回 `inputAmountDisplay`、`cacheReadAmountDisplay`、`inputPriceDisplay` 和
`cacheReadPriceDisplay`；存在该字段时，普通输入与缓存字段仅表示文本输入，输出字段表示图像输出。

## 11. 版本、更新与重启

| 方法 | 路由 | 主要 query/body | 说明 |
| --- | --- | --- | --- |
| `GET` | `/api/admin/system/version` | 无 | 当前构建、部署模式和可用更新 |
| `GET` | `/api/admin/system/update/detail` | `refresh=true|false`、`channel?` | 按临时通道读取或刷新 Release 详情 |
| `GET` | `/api/admin/system/update/events` | 无 | SSE 更新事件流 |
| `POST` | `/api/admin/system/update` | `{ targetVersion, channel? }` | 受理后台在线更新，返回 `202` |
| `GET` | `/api/admin/system/update/status` | 无 | 查询当前更新或回滚状态 |
| `POST` | `/api/admin/system/rollback` | 无 | 回滚到保留的上一版本 |
| `POST` | `/api/admin/system/restart` | 无 | 请求进程重启 |

在线更新遵循[版本命名与升级规则](../deploy/README.md#版本命名与升级规则)。版本接口的
`updateChannel` 由当前版本推导，取值为 `stable`、`alpha`、`beta`、`rc`、`exp`，无法识别时为 `unknown`。
详情接口的可选 `channel` 仅作用于本次查询，不保存实例偏好；省略时按当前运行版本推导。
响应的 `policy` 包含本次 `channel` 与 `availableChannels`，普通实例可选 `stable`、`rc`、`beta`、`alpha`，
实验实例仅允许 `exp`。不可用通道返回 `40901`，未知枚举值返回参数错误。
执行时应同时发送页面确认的 `channel` 与 `targetVersion`，服务端冻结该通道并重新复核远端目标；
旧客户端省略通道时按运行版本推导。版本摘要接口始终检查运行通道，不受临时查询影响。
检查与执行使用同一规则，禁止的通道转换、跨实验线、跨大版本、降级或同版本重装均以 `40901` 拒绝。
`hasUpdate=true` 仅表示当前构建支持在线更新，且存在允许的更高版本；`latestVersion`、`releaseUrl` 和
`notes` 对应这个候选。没有可升级候选时，`hasUpdate=false`、`latestVersion` 为当前版本，
`releaseUrl` 和 `notes` 保留当前版本的已发布 Release 信息；找不到匹配当前版本的 Release 时为空。
当前构建不支持在线更新时不查询 Release，`hasUpdate=false`、`latestVersion` 为当前版本，
`releaseUrl` 和 `notes` 为空，不支持原因通过 `updateSupported=false`、`unsupportedReason` 返回。
强制检查失败时通过 `warning` 返回错误，`hasUpdate=false`，不以旧缓存或“没有更新”掩盖失败。
普通查询可复用 20 分钟内的结果，缓存按通道隔离，较慢的旧检查不能覆盖新检查结果。下载时仍会校验目标资产、校验和及归档。

更新 POST 在本地校验目标版本并持久化任务后返回 `202`，数据包含 `operationId`、`targetVersion`、
`deploymentMode` 和 `message`，只表示已受理。Release 查询、远端目标复核、下载、校验及文件替换在后台
执行，结果通过 `/update/status` 的 `operation` 查询：`status` 为 `idle`、`running`、`succeeded` 或 `failed`，
终态包含 `finishedAt`，失败原因在 `error` 中。SSE 的 `operationId` 用于关联进度；终态事件发出前状态已落盘。
连接中断不取消已受理任务；响应丢失时先查询状态，不自动重复提交。打开更新页面时也会恢复最近一次任务。
下载并解包后，Admin 会以目标发行清单声明的宿主插件合同检查每个启用实例的精确制品；回滚则对备份发行清单
执行同一检查。两条路径都会在文件交换前后复核同一全局插件配置版本。任一实例不兼容、制品缺失、请求取消或
配置并发变化都会失败；已交换的二进制、Web 资源和官方插件目录会成组恢复。预检不会自动停用实例、切换插件
版本或增加权限。

状态响应的 `currentVersion` 表示已安装文件的版本，运行中的版本仍以 `/version` 为准。
`needRestart=true` 表示已验证的安装文件尚未在当前进程生效，此时应调用重启接口，不能重复发起更新或切换通道。
最近一次 `operation` 仅表示操作历史，成功记录不等于待重启，也不改变远端 `hasUpdate`。
手动部署后按实际文件校准 `currentVersion`；无法核实的回滚备份不再返回 `previousVersion`。
运行期间的外部文件变动或不完整安装返回错误，不伪装成安装成功。
Host 关闭或任务取消会记录失败终态；状态查询会收敛无执行锁的遗留 `running`。
异常退出留下的锁仍遵循 30 分钟过期规则，未过期前不会抢占其他进程的操作。
实例升级和仓库发版见 [部署文档](../deploy/README.md#镜像升级与源码构建)。

## 12. 插件管理

除明确标注的公开入口外，以下接口需要管理身份。图标读取及 12.2 中标注的原始响应接口直接返回资源、插件响应
或模型响应，其余接口使用统一管理响应信封。制品按摘要接受其不可变访问域；首次安装某个插件时会尝试创建默认配置，
配置完整则启用，缺少必填项则保留为待配置。操作流程见 [插件使用](plugins.md)，开发合同见
[SDK](../backend/crates/gateway-plugin/sdk/README.md)。不提供官方商店或第三方市场订阅接口。

### 安装包与来源

| 方法 | 路径 | 行为 |
| --- | --- | --- |
| GET | `/api/admin/plugins/artifacts` | 列出制品的 `metadata`、固定 `source`、`installedAt` 与可为 `null` 的 `acceptedAt` |
| GET | `/api/admin/plugins/artifacts/{sha256}/icon?theme=light\|dark` | 管理身份；按不可变制品摘要读取已校验图标；无图标返回 404 |
| POST | `/api/admin/plugins/artifacts/upload` | 上传原始 tar.gz 包体，最多 32 MiB；查询参数 `sha256` 必须固定已解析的包；接受访问域并完成安装 |
| POST | `/api/admin/plugins/artifacts/upload/verify` | 同样接收原始包体，只读解析并校验，返回 `metadata` 与 `source`，不安装 |
| POST | `/api/admin/plugins/artifacts/install` | 从 URL 或固定 GitHub Release 下载、校验，接受访问域并完成安装 |
| POST | `/api/admin/plugins/artifacts/verify` | 按远程来源只读解析并校验完整包，返回 `metadata` 与固定 `source`，不持久化或启动插件 |
| POST | `/api/admin/plugins/artifacts/accept` | 严格请求 `{ "sha256": "<小写 SHA-256>" }`，接受已导入制品的访问域并完成安装 |
| POST | `/api/admin/plugins/artifacts/delete` | 请求 `{ "sha256": "<小写 SHA-256>" }`，删除未被引用的制品 |
| POST | `/api/admin/plugins/releases/query` | 查询指定 GitHub 仓库的最新稳定版或明确 tag |
| POST | `/api/admin/plugins/updates/check` | 按插件已保存的来源和策略查询 Release，只读，不下载或切换实例 |
| GET / POST | `/api/admin/plugins/source-credentials` | 列出公开授权范围，或创建独立下载凭据 |
| POST | `/api/admin/plugins/source-credentials/delete` | 请求 `{ "id": "<凭据 ID>" }`，删除未被来源引用的下载凭据 |
| GET / POST | `/api/admin/plugins/update-sources` | 查询或显式改变插件后续安装的允许来源及更新检查策略 |

删除制品时，同一事务会清理该制品使用过且已无任何制品引用的下载凭据；删除插件的最后一个版本时，
同时清理其来源规则、更新检查策略及出站代理引用。其他版本共享的凭据、代理本身和审计记录保留，
独立创建但未被该制品使用的下载凭据不会被顺带删除。完整卸载后重新安装不受旧来源规则限制。

#### 包信息

安装包使用的版本与字段规则见 [SDK 清单](../backend/crates/gateway-plugin/sdk/docs/manifest.md)。
插件 ID 由 `publisher + "." + name` 派生；`displayName` 和可选 `author` 仅用于展示。

制品 `metadata` 返回 `pluginId`、`name`、`displayName`、`publisher`、可为 `null` 的 `author`、`version`、
`description`、`license`、`sha256`、`platforms`、可为 `null` 的 `icon`、`contributes`、`requestedPermissions`、
`permissionDescriptions`、`configurationSchema`、`secretFields` 和 `stateNamespaces`。`permissionDescriptions` 的每项为
`{ permission, label, description }`，供安装确认直接展示。`contributes` 是以稳定 snake_case capability 为 key 的对象；每个值包含
`id`、`version`、`stages`、`inputFormats` 和 `outputFormats`。`id` 是实例绑定引用的完整贡献项 ID，客户端必须通过
当前制品的 `contributes` 映射确定其 capability，不能解析 ID 文本推测能力。

#### 图标读取

`metadata.icon` 为清单中的包内路径或 `{ "light": "...", "dark": "..." }` 对象；未声明时为 `null`。
配置示例、支持格式与文件限制见 [SDK 插件图标](../backend/crates/gateway-plugin/sdk/docs/manifest.md#插件图标)。

读取浅色主题图标使用 `GET /api/admin/plugins/artifacts/{sha256}/icon?theme=light`：

- `sha256` 为制品的 64 位小写十六进制摘要；`theme` 必填，值为 `light` 或 `dark`。单路径图标在两种主题下返回相同内容
- 成功返回图片原始字节，`Content-Type` 为清单声明的 MIME，不使用 JSON 信封、不转换图片格式。制品不存在或未提供图标时返回 `404`
- 响应包含 `X-Content-Type-Options: nosniff` 和 `Cache-Control: private, max-age=31536000, immutable`；缓存按制品摘要与主题区分
- SVG 使用 sandbox CSP 隔离，允许内联样式和 data 图片，禁止脚本执行与外部资源请求

客户端应将接口地址用于 `<img src="…">`，不内联 SVG，也不把清单路径拼成文件系统路径或公开 URL。
没有图标时由客户端显示通用图标。

#### 解析与确认安装

本地包先调用 `artifacts/upload/verify`，远程包先调用 `artifacts/verify`。校验只返回包信息与来源，
不保存、不接受访问域、不启动插件，也不代表实例配置或私有状态已经兼容。
修改文件、地址、Release、摘要、凭据或代理选择后必须重新校验；确认安装时固定校验返回的身份、版本与摘要。

远程解析请求包含 `location`、可选 `credentialIds` 和 `outboundProxyId`，不需要预先提供 ID 或版本。
更新已有插件时可传 `expectedPluginId` 锁定预期身份；首次安装从包内清单读取身份，并按该身份复核已保存的来源绑定。
URL 与 GitHub 的 `location.sha256` 在解析时均可省略，由服务端计算实际包摘要；提供摘要时必须通过匹配检查。

确认安装时，将解析返回的 `metadata.pluginId`、`metadata.version` 和 `metadata.sha256` 分别填入
`pluginId`、`version`、`location.sha256`，与原来的来源、凭据和代理选择一起提交 `artifacts/install`。
这三个字段在远程安装时必填，确保实际安装内容与确认的包相同。清单 ID、版本、宿主兼容范围与平台
必须通过校验；来源类型、地址或代理选择变化返回冲突，需要先显式修改更新来源。相同包及来源重复安装为幂等操作；
同一插件版本的平台产物不允许替换成不同内容。安装新版本保留已安装旧版本。

```json
{
  "pluginId": "acme.example",
  "version": "1.0.0",
  "credentialIds": [],
  "outboundProxyId": null,
  "location": {
    "kind": "url",
    "url": "https://downloads.example.org/plugin.tar.gz",
    "sha256": "<64 位小写十六进制摘要>"
  }
}
```

上传、远程安装和 `artifacts/accept` 成功响应均为平铺对象：

```json
{
  "artifact": {
    "metadata": {},
    "source": { "kind": "upload" },
    "installedAt": "2026-09-22T00:00:00Z",
    "acceptedAt": "2026-09-22T00:00:01Z"
  },
  "configRevision": 2,
  "defaultInstanceId": "<实例 UUID 或 null>",
  "configurationRequired": false
}
```

接受制品时，若该插件没有任何实例，服务端使用稳定的 creation ID 创建一份默认实例：普通配置取 schema 默认值，
敏感字段不复制默认值；除 `management`、`command_line` 和 `frontend_authentication` 外，为声明的贡献项及阶段建立
空范围默认 binding。仅缺少必填项时 `configurationRequired=true` 且实例保持停用；配置完整时实例直接启用。
非法 schema、类型错误或把敏感字段放入普通配置会拒绝安装。已有该插件实例时不额外创建或切换实例，
`defaultInstanceId=null`。相同摘要的接受和默认实例创建可重试，不产生副本。

宿主发行目录导入的制品只写入不可变包和来源，`acceptedAt=null`；在严格复核
`permissionDescriptions` 后调用 `artifacts/accept`。未接受制品不能用于创建、更新或启用实例。

GitHub 的 `location` 使用 `kind: "github"`、`repository: "owner/repo"`、`tag`、`asset`、
`allow_prerelease` 和 `sha256`。解析时优先使用显式摘要或 GitHub 的 SHA-256，缺失时读取同一 Release 的
`checksums.txt`；都未提供时计算下载内容的摘要供确认安装使用。显式摘要与 GitHub 摘要冲突时拒绝下载。
摘要用于内容一致性检查，不证明发布者身份，只应安装可信来源的插件。

#### GitHub Release 查询

查询请求为 `{ "query": { "repository": "owner/repo", "tag": null, "allowPrerelease": false }, "credentialIds": [], "outboundProxyId": null }`。
`tag: null` 查询最新稳定版；预发行版须指定 tag 并允许预发行。响应包含固定 tag、产物列表、`queriedAt`
和 `expiresAt`。成功缓存 1 小时，失败缓存 30 秒，同一查询合并并发；限流返回错误，不转换成空列表。

#### 下载认证与代理

下载凭据创建字段为 `name`、`origin`、`pathPrefix`、`purposes`、`authentication`。
`purposes` 可选 `metadata`、`artifact`；`authentication.kind` 可选 `github`、`bearer`、`basic`、`header`，
分别携带 `token`、`token`、`username/password`、`name/value`。列表与创建响应只返回 ID 和授权范围。
每次重定向都按 origin、路径段边界和用途重新匹配凭据，重叠授权拒绝执行。
来源 URL 使用 HTTPS，不包含用户信息、查询参数或片段；回环地址允许 HTTP，供本机来源使用。

查询、校验和远程安装的 `outboundProxyId` 指向已保存的出站代理，与下载凭据独立，公开来源也可选择代理。
省略或 `null` 表示直连，不继承进程环境代理；代理失败不会回退直连。查询与下载期间代理配置变化时返回 `409`，需重新操作。
查询缓存与限流按凭据及代理身份隔离。制品历史 `source.outbound_proxy` 只保存下载时的 `{ id, revision }`，不返回代理地址或认证信息。
仍被更新来源或制品引用的代理不能删除；修改后续来源不会清除已安装制品的历史引用。

#### 更新来源与检查

修改更新来源请求为 `{ "pluginId": "acme.example", "source": { "kind": "github", "repository": "owner/repo" }, "policy": { "kind": "stable" }, "outboundProxyId": null }`；
其他可选来源为 `upload` 或带 `url` 的 `url`。`policy` 省略时为 `manual`；GitHub 还支持 `stable`（最新稳定版）和
`{ "kind": "pinned", "tag": "v2.0.0-rc.1", "allow_prerelease": true }`。其他来源只支持 `manual`。
来源、策略和代理选择一同持久化与审计；代理字段省略或 `null` 明确保存为直连。`upload` 和 `builtin` 不接受代理。
变更不改变已安装包的历史来源，不自动下载、接受制品或切换实例版本。

检查更新请求为 `{ "pluginId": "acme.example", "credentialIds": [] }`，返回 `binding` 和 `release`。
服务端使用已保存的来源、策略和代理，不接受临时替换仓库或代理；查询期间来源变更返回 `409`，不可访问或限流仍返回错误。
`release` 仅是来源元数据，不表示插件包已通过身份、版本、平台、兼容范围或摘要校验；选择产物后仍须显式调用安装接口，
安装成功也不会自动修改现有实例的固定制品。手动策略和不支持 Release 查询的来源返回明确错误，不伪装为没有更新。

### 12.1 运行实例

| 方法 | 路径 | 行为 |
| --- | --- | --- |
| GET | `/api/admin/plugins/instances` | 列出实例配置、功能绑定、配置完整性及发布状态，不返回 secret 值 |
| POST | `/api/admin/plugins/instances` | 创建实例，返回 `id` 和 `configRevision` |
| POST | `/api/admin/plugins/instances/update` | 请求 `{ "id": "<实例 ID>", "instance": { ...配置字段 } }`，整体更新实例 |
| GET | `/api/admin/plugins/instances/version-plan?id=<实例 ID>&artifactSha256=<目标摘要>` | 只读生成版本设置草稿，不启动插件、不返回密钥值 |
| POST | `/api/admin/plugins/instances/switch-version` | 请求 `{ "id": "<实例 ID>", "target": { "artifactSha256": "<已安装目标摘要>", "expectedRevision": 1 } }`，准备后切换版本 |
| GET | `/api/admin/plugins/instances/rollback-plan?id=<实例 ID>` | 只读列出当前版本、实例 revision 与按语义版本降序排列的已安装、已接受且有配置快照的旧版候选，不启动插件 |
| POST | `/api/admin/plugins/instances/rollback` | 请求 `{ "id": "<实例 ID>", "target": { "artifactSha256": "<已安装且已接受旧版摘要>", "expectedRevision": 1 } }`，校验后回滚 |
| POST | `/api/admin/plugins/instances/disable` | 请求 `{ "id": "<实例 ID>" }`，停用实例并发布新集合 |
| POST | `/api/admin/plugins/instances/delete` | 请求 `{ "id": "<实例 ID>" }`，删除已停用实例 |

#### 创建与更新

创建与更新的配置字段为 `name`、`artifactSha256`、`enabled`、`configuration` 和必填的 `bindings`，
`secrets` 可选。输入严格拒绝未知字段；`trustedProcess` 和 `grants` 不是输入字段。`artifactSha256` 必须指向
已接受的制品，其访问域由制品声明精确派生，实例不能增加或删减。`configuration` 是匹配插件 schema 的 JSON 对象；敏感字段必须
放入 `secrets`，空对象清除全部值。省略时同版本编辑保留当前值；跨版本编辑优先保留目标版本快照的密钥，没有快照则保留当前值。

创建可传 `creationId`（标准小写 UUID），同一草稿重试复用该 ID，已保存且内容不同则返回 409，不创建副本或覆盖旧配置。
相同内容的重试仍会重新准备并发布，不保证配置 revision 不变。更新可传 `expectedRevision`，与实例列表的 `revision`
不一致时返回 409，管理员应重新加载后确认。创建不能传 `expectedRevision`，更新不能传 `creationId`。
删除实例会一并删除其 secret、私有状态与版本配置快照，停用则保留这些数据。

保存启用配置时，可传 `replaceInstances: [{ "id": "<旧配置 ID>", "expectedRevision": 1 }]`，
明确确认同时停用的同插件配置（最多 256 项，不得包含当前配置或重复 ID）。同一插件最多启用一个实例，省略且已有其他启用配置时返回 409。
不能将已有实例切换成另一个插件。
目标配置准备通过后，停用旧配置与保存目标在同一事务提交，旧配置及其密钥、私有状态保留。
待停用配置必须仍启用且 revision 与确认时一致，否则返回 409，重新读取并确认后再提交。

列表项包含 `id`、`name`、`artifactSha256`、`enabled`、`configurationRequired`、`configuration`、
`secretFields`、`bindings`、`revision`、`running`、`publishedRevision` 和 `runtime`。`configurationRequired`
由当前制品 schema 与已保存的普通/敏感配置实时派生，不写入数据库；它只表示仍缺少必填值。停用实例也不能保存
类型错误、非法 schema 或把敏感字段混入普通配置。

#### 版本切换与私有状态

私有状态通过 [SDK 回调](../backend/crates/gateway-plugin/sdk/docs/capabilities.md#私有状态)访问，不提供任意读写的管理 HTTP 接口。
升级若需要迁移，实例可能暂时停用；迁移失败且发生并发配置变更时保持停用，不覆盖管理员的新配置。
版本切换只接受同一插件已安装且已接受的制品，`expectedRevision` 必须等于实例列表中的 `revision`，过期请求返回 409。
`version-plan` 返回 `instanceRevision`、`artifactSha256`、`configuration`、`secretFields`、`bindings` 和 `restored`，
最后一项表示使用了目标版本的配置快照。没有快照时只补充缺失的 schema 默认值，保留当前显式值；功能范围按能力与阶段映射到目标声明，
新能力使用默认绑定，已关闭的既有能力继续关闭，客户端认证仍须显式身份映射。草稿不代表已通过校验。

`switch-version` 重新生成草稿并执行准备与提交，名称和启停状态保持不变，权限从目标制品派生。不兼容设置返回 400，当前设置不变；
可读取草稿并通过 `instances/update` 携带原 `expectedRevision` 提交修正。不会推断字段改名或丢弃未知参数与密钥。

每个实例、制品摘要保存一份最近启用时提交的配置快照，包含普通配置、secret 与 binding，停用草稿不覆盖快照。
`rollback` 只接受有快照的较早语义版本，恢复该版本设置并重新检查平台、配置与私有状态，保留当前名称与启停状态。
快照与实例修改共享事务，不从审计日志重建，也不恢复私有业务数据；删除实例或制品时删除对应快照。

#### 访问域与受管资源

清单的 `requestedPermissions` 接受 `network`、`models`、`accounts`、`data`、`requests`、`public_endpoints`，确认结果以制品摘要为边界；实例输入没有单独授权字段。

| 标识 | 含义 |
| --- | --- |
| `network` | 访问网络 |
| `models` | 查询模型和 Client Key 基本信息并调用模型，可能产生消耗 |
| `accounts` | 读取和修改账号，包括访问原始凭据 |
| `data` | 仅在管理或命令入口只读全部账号的基础信息和已有额度观测，不含凭据或预测 |
| `requests` | 查看和处理请求、响应、路由及账号选择 |
| `public_endpoints` | 提供无需登录即可访问的资源与回调入口 |

访问域不是 operation、Provider、账号、origin 或命名空间白名单。Runtime 仍按当前贡献项、调用阶段、父调用和宿主业务
端口限制可用方法，不能把一个域借给无关调用。账号资源以每次请求的 `accountId` 查找权威 Provider，不接受插件提供的
Provider 事实；Client Key 列表只投影 `id`、`name`、`enabled`，不返回前缀、明文、账号绑定或额度策略。模型调用须显式
选择 Key ID，并继续经过该 Key 的当前准入、范围、预算、用量与计费合同。私有状态只能访问清单声明的命名空间。

受管 HTTP 还会执行宿主网络策略。目标默认只允许常规公网地址；每次请求重新校验全部解析地址，连接使用通过校验的
固定地址并保留原始 Host 与 TLS 域名校验。托管回调由宿主解析目标域名，HTTP/HTTPS/SOCKS 代理接收固定 IP；
无法由宿主安全解析的目标会被拒绝。代理沿用所选账号配置，不自动回退直连；重定向不会自动跟随，后续地址必须重新校验。

#### 能力绑定与范围

`bindings` 通过 `contribution` 指定清单已声明的贡献项，并包含该贡献项声明支持的 `stage`、顺序和失败策略；
尚未声明、尚未注册或阶段不匹配的贡献项不能启用。
首次安装生成的默认实例会为适用贡献项的每个声明阶段创建 binding，`order=0`，观察阶段使用 `observe`，
其他阶段使用 `reject`，所有范围数组为空。`management`、`command_line` 按清单声明注册，不接受 binding；
`frontend_authentication` 必须由管理员显式配置身份映射，也不自动创建。

请求终态观察的绑定格式如下；同一实例同时订阅 `request_lifecycle` 和 `usage` 时使用相同 `order`，
宿主合并为一次调用，仅附带命中订阅的事实。

```json
{
  "contribution": "acme.usage-observer.usage",
  "stage": "observation",
  "order": 10,
  "failurePolicy": "observe",
  "clientKeyIds": [],
  "accountGroupIds": [],
  "providerIds": [],
  "models": []
}
```

范围数组缺省或为空表示不限制该维度；同一数组中的条件取并集，不同维度取交集，全部为空表示全局。
`clientKeyIds` 和 `accountGroupIds` 引用已有资源 ID，保存启用实例时校验存在性；引用被删除后仍可停用实例，
保留原绑定供管理员修订，重新启用前必须修正引用。账号组范围使用请求开始时
Client Key 已冻结的绑定组（含绑定但禁用的组），不按最终选中账号重新解释；全账号 Key 没有绑定组。
Provider 范围匹配最终实际尝试的 Provider；未进入 Provider 的拒绝不会命中有限 Provider 范围。
模型范围匹配客户端请求的公开模型 ID。配置发布不改变在途请求的匹配范围，同一实例不会因命中多个组而重复调用。

`frontend_authentication` binding 只能使用 `authentication` 阶段、`reject` 或 `delegate` 失败策略，并通过
`identityBindings: [{ "principal": "...", "clientKeyId": "..." }]` 明确映射外部身份；其他范围数组必须为空。
普通 binding 不能携带 `identityBindings`。已停用、删除或不再授权的 Key 不会因映射恢复访问。

实例不为管理页面或命令行预绑定模型执行 Key。管理页面经 12.2 的模型桥在每次调用时显式提交 `clientKeyId`；
CLI 插件也须在具体调用中选择当前 Key。管理员身份本身不提升为推理身份，Key 明文不会发送给插件。

观察失败只记录诊断，不改变响应、路由、标准 Usage 或账单。当前仅派发一次终态，过载直接丢弃，
不承诺进程崩溃后的持久投递；这不是请求前拦截、流逐帧观察或账单重算接口。
尚未通过重新鉴权、未建立冻结请求上下文的失败不触发当前终态观察。

#### 发布状态

启用或修改时先准备候选进程及目录，再提交和发布；失败不替换正在运行的集合。
切换后在途请求保留原版本，其引用排空后回收旧进程。

| 字段 | 含义 |
| --- | --- |
| `enabled` | 期望启用状态 |
| `publishedRevision` | 当前已发布的全局配置版本 |
| `running` | 实例已启用且发布版本与持久化配置一致；不代表进程存活或外部服务健康 |

### 12.2 管理扩展页面与原始 API

`GET /api/admin/plugins/extensions` 需要管理身份，返回统一信封，`data` 为当前已发布的扩展数组。
每项包含 `target: { instanceId, artifactSha256, revision }`、`name`、`configurationSchema`、
`pages`、`routes`、`resources` 和 `callbacks`。schema 不是实例配置，不包含 secret 值。
页面目录的 `pages[].title` 与 `pages[].description` 分别用于宿主页面标题、副标题；未声明副标题时为 `null`。
目录来自冻结的插件注册结果，并与当前持久化实例复验；停用、换包或修改实例后旧目录项立即不再可用。

以下用 `{target}` 表示 `{instanceId}/{artifactSha256}/{revision}`，`{path}` 表示包内已声明的相对路径：

| 方法 | 路径 | 身份与响应 |
| --- | --- | --- |
| 已注册方法 | `/api/admin/plugins/extensions/{target}/api/{path}` | 管理身份；插件原始响应，不套管理信封 |
| GET | `/api/admin/plugins/extensions/{target}/resources/{path}` | 管理身份；已校验的静态字节 |
| POST | `/api/admin/plugins/extensions/{target}/models/responses?clientKeyId=<ID>` | 管理身份；原生 Responses JSON 或 SSE，不套管理信封 |
| POST | `/api/admin/plugins/extensions/{target}/callback-tickets` | 管理身份；统一信封中的一次性回调票据 |
| GET | `/plugins/resources/{target}/{path}` | 无须登录，但资源须声明公开且制品已接受 `public_endpoints` 访问域 |
| GET | `/plugins/callbacks/{target}/{path}` | 无须登录，但必须携带有效且未消费的 `state` 票据 |

实例目标失效返回 409，调用方应刷新扩展目录，不重放原操作。尚无可用发布视图时返回 503。
所有入口都要求当前实例仍启用、制品已接受且版本匹配；公开资源与回调还要求制品声明 `public_endpoints`。
公开入口不是读取任意包文件的路径。
单项静态资源最多 1 MiB，总量最多 8 MiB；资源必须同时出现在制品清单和注册结果中。

管理 API 只转交方法、相对路径、查询字符串、Content-Type、宿主请求关联与原始正文，不转交管理 Cookie
或任意认证头。仅接受注册的方法、路径和内容类型；查询最多 8 KiB，正文最多 1 MiB 且受运行帧预算约束，
GET/HEAD 不接受正文。响应状态码为 200–599，内容类型须在注册列表内，204/304 不允许正文；插件不能
自选 Set-Cookie、Location 或其他响应头。失败可能发生在副作用之后，不提供自动重试。

动态 API 与登录回调设置 `Cache-Control: no-store`。静态资源成功响应设置 `private, no-cache` 与内容 ETag，
浏览器可保存正文，但每次复用前必须重新校验管理员身份（公开资源除外）、实例版本和资源授权；
`If-None-Match` 匹配时返回无正文的 `304`。停用、版本或授权变更后，旧目标仍被拒绝，不返回缓存命中。
两类响应均设置 nosniff、no-referrer 与限制性 CSP。管理端页面使用隔离 iframe，
仅允许脚本，不具有父页面同源权限；联网、弹窗、表单、嵌套页面和顶层导航均不开放。
页面访问已声明管理路由须经过固定目标的宿主桥，不能指定其他实例或版本。插件页面作者合同见
[SDK 管理 API 与页面](../backend/crates/gateway-plugin/sdk/docs/capabilities.md#管理-api-与页面)。

模型桥只接受严格的 `clientKeyId` 查询参数和 Responses JSON 正文；传输正文与解压后的正文均最多 8 MiB。
服务端先复核固定目标与 `models` 访问域，再按 ID 查询当前 Client Key，并使用普通 Responses HTTP/SSE 执行链；
因此仍执行当前 Key 的准入、账号范围、预算、用量、计费与请求插件链，包括发起页面所属插件自己的适用 hook。
这不是嵌套的宿主模型回调，管理身份本身也不提供推理身份，Key 明文不会离开宿主。

等待首帧、非流式生成和流式正文交付期间约每秒复核一次固定目标与 `models` 访问域。实例停用、制品或 revision
变化以及访问资格失效会取消底层执行并停止响应。成功或协议错误沿用 Responses 内容类型，并返回可检索的
`x-request-id` 与 `x-gateway-request-id`；响应移除 Cookie 和认证头并设置 nosniff、no-referrer 与限制性 CSP。
内置页面桥最多并发 4 个模型请求，每次拉取最多 64 KiB，总期限 10 分钟；页面在已收到响应后 30 秒不继续拉取会取消，
等待首个响应本身不受该空闲计时误杀。

票据请求为 `{ "path": "oauth", "ttlSeconds": 60 }`，有效期只允许 1–600 秒；响应数据为
`{ "state": "<一次性不透明票据>", "expiresAtMs": 0 }`。state 绑定签发管理身份、实例、制品、版本与路径，
不得记录或截图。公开回调只接受无正文的 GET，查询参数必须恰好包含一个 `state`；过期、错路径、跨实例、
并发重复或已消费票据均拒绝。宿主在执行前原子消费票据，因此超时或插件失败后不能重放。
公开调用使用无宿主回调权限的独立阶段，不能读取或保存账号、读写状态、发送受管 HTTP 或日志。
票据用于受控接收第三方返回结果，不代替敏感管理操作的身份校验。
