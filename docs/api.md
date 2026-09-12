# Codex Proxy RS 接口

本文列出 v3 源码中的公开 HTTP 接口，路由以
`backend/crates/gateway-api/src` 中的 router 为准。配置 Codex 请先看 [客户端配置](../deploy/README.md#客户端配置)；
运行实例是否包含这些功能，应结合其版本和 revision 确认。

## 1. 鉴权与公共约定

### 客户端接口

所有 `/v1/*` 请求都使用管理端创建的 Client Key：

```http
Authorization: Bearer sk_...
```

自动生成的 Key 保持 `sk_` 格式；迁入的自定义 Key 使用保存时的原值，不限制前缀或固定长度。
无论格式如何，只有已保存且启用的 Client Key 能通过鉴权。

Codex 原生生图配置还会携带 `X-OpenAI-Actor-Authorization: proxy-managed`。
它仅用于客户端识别服务端托管认证，不能代替 Client Key。网关和 OpenAI Provider 都会过滤该请求头，
上游账号身份只由服务端选中的账号提供；不要把真实账号 token 放进该标记。

Client Key 通过账号分组限定路由范围：未绑定分组时可使用全部账号，绑定一个或多个分组时只能使用
已启用分组成员的并集。分组可以混合 `openai` 与 `xai` 账号；同一请求只会在模型能力明确匹配且满足
重放安全边界时跨 Provider fallback。

运行设置可以分别配置 `minCodexDesktopVersion` 与 `minCodexCliVersion`。两者只接受 SemVer，`null`
表示不限制。API 在 Client Key 鉴权成功后识别官方 Desktop/CLI 请求头；已识别客户端没有合法版本，或版本
低于对应门槛时，所有 `/v1/*` HTTP 请求和新 WebSocket 握手在访问上游前返回 `426 Upgrade Required`。
未知客户端保持兼容，不应用版本门禁。

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

已识别但缺失或携带非法版本时，`code` 为 `client_version_unavailable`，`current_version` 为 `null`。

### 管理接口

除登录、会话状态和登出外，所有 `/api/admin/*` 请求都需要以下任一鉴权方式：

- 浏览器登录后得到的 `cpr_admin_session` Cookie；
- `x-api-key: <admin-api-key>`。

同源管理端的登录和退出根据浏览器 `Origin` 自动设置会话 Cookie：HTTP 来源省略 `Secure`，
HTTPS 来源以及缺失、`null` 或非法来源保留 `Secure`。`HttpOnly`、`SameSite=Lax` 始终保留。
无需新增配置，不根据 `X-Forwarded-Proto` 等转发头降级；HTTPS 反代使用 HTTP 回源不影响 Cookie。
部署要求见 [公网访问](../deploy/README.md#公网访问)。

请求无需自带 `x-request-id`；缺失时服务端自动生成 UUID 并在响应头回传同一 request ID。
`api.request_id_header` 可改变注入与回传的 header 名，管理端鉴权不依赖该名字。
管理端响应统一带 `Cache-Control: no-store`。

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

管理端本地产生的 `message` 是可安全展示的中文文案；Store、Serde、Provider 内部 `Display` 和原始上游
body 不进入这个通用信封。稳定业务码如下：

| HTTP | `code` | 含义 |
| ---: | ---: | --- |
| 400 | `40000` | 请求体不是合法 JSON |
| 400 / 405 / 415 / 422 | `40001` | 通用请求、方法、Content-Type 或字段错误；HTTP 状态保留具体语义 |
| 400 | `40002` | 时间范围不合法 |
| 401 | `40101` / `40102` / `40103` | 缺少管理员会话 / 登录凭据错误 / 管理 API Key 错误 |
| 404 | `40401` | 资源或管理接口不存在 |
| 409 | `40901` | 资源状态冲突 |
| 429 | `42901` | 登录尝试过多 |
| 500 | `50001` | 服务内部错误 |
| 502 | `50201` | 上游服务请求失败 |
| 502 | `50202` | 不可逆上游操作的执行结果未知；刷新状态后再决定是否重试 |
| 503 | `50301` | 依赖服务暂不可用 |

未知 `/api/admin/*` 路径使用 `40401`，不会落入 SPA；已存在路径使用错误 method 时返回 `405`、
`40001`，并保留标准 `Allow` header。request ID 继续通过配置的响应 header 返回。

Provider 管理适配使用静态 `public_message` 提供可操作的具体原因，Admin 用例完成安全消息选择后，
API 对 `50201`、`50202` 和 `50301` 也保留该消息，不再用固定错误覆盖；缺少安全消息时仍回退到通用提示。
认证错误和未知内部错误继续使用固定文案，不公开 Provider 内部 message、原始响应或凭据。

手动刷新令牌时，容量／账号租约占用和账号快照冲突仍为 `40901`，但分别提示等待或刷新账号列表。
OpenAI 已收到的刷新失败响应不再统一归为资源冲突：原先落入宽泛 Transport 分类的明确拒绝使用
`50201`，按已解析的错误码区分令牌过期、已使用、已撤销和 `invalid_grant`；刷新接口返回
`token_expired` 时提示“刷新令牌不可用，请重新授权”，不据此断言具体失效原因。无法确认刷新结果时使用
`50202`，提示先核对账号状态、不要立即重复刷新。缺少刷新令牌及原有明确凭据无效分支仍为 `40001`。
上游 `401` 不代表管理员会话失效，也不会触发管理端重新登录。上述变化只修正管理错误的分类和展示，
不改变后台自动刷新、401 恢复退避或账号终态策略；客户端不得仅因状态码从 `409` 改为 `502` 自动重发刷新。
xAI 手动刷新返回无效的新凭据时，从 `40001` 改为 `50202`，因为上游可能已经轮换了旧 RT；
未能完成刷新、但没有明确凭据永久失效证据的 `Rejected` 从 `40001` 改为 `50201`，不再一概提示凭据无效。
Codex PAT 验证服务不可用和身份响应无效分别通过 `50301`、`50201` 保留原有具体提示。

### 管理写入一致性

管理写入不要求客户端提供全局配置版本。会改变路由快照或安全配置的写入由后端在事务内推进
内部 `config_revision`，并用于快照发布与审计。账号更新和分组查询/写入的部分响应会返回
`configRevision` 作为已提交事实，但它不是客户端 mutation 的前置条件。

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

Codex 的 review 等子代理请求仍使用 `/v1/responses`，并通过 `x-openai-subagent` 请求头携带子代理类型；
网关不提供独立的子代理请求路径。

`POST /v1/responses` 在鉴权后按 `Content-Encoding` 解压，再解析 JSON；支持单一 `gzip`、
`deflate`（zlib 封装）和 `zstd`，缺省、空值或 `identity` 直接使用原始正文。gzip 多成员与 zstd
多帧连续解码，整体展开结果最多 64 MiB，超限在继续展开前返回 `400 request_too_large`；zstd
回溯窗口同样最多 64 MiB，不能满足该限制的帧按解码失败处理。这个限制保护入站解压资源，不是
模型上下文或 Token 上限，也不新增未压缩正文的长度限制。
不支持的编码、逗号分隔的叠加编码和重复 `Content-Encoding` 头返回
`400 unsupported_content_encoding`；压缩正文损坏、截断或解压后不是合法 JSON 返回
`400 invalid_json`。本地错误不包含原始正文或解压库细节。WebSocket 文本帧不经过这条解压路径。

Responses 不透传下游的逐跳头、反代元数据（如 `cf-*`、`x-forwarded-*`、`forwarded`、`via`、
`cdn-loop`）以及 `Accept-Encoding` / `Content-Encoding`。链路元数据和编解码能力
由各段传输层独立管理；其余业务扩展头继续透传，不使用固定业务头白名单。
此规则同时适用于上游 HTTP 和 WebSocket，不影响上游响应的 `cf-ray` 等诊断信息。

Responses WebSocket 仅接受文本 `response.create`，同一连接串行执行。当前响应期间收到的后续业务帧
留在有界接收队列中，待当前响应完成终结和写出后再逐条校验、准入与执行，不因请求提前到达而断开。
这对齐 Codex 客户端 `stream_request` 持锁至本轮结束的串行行为，不表示支持额外控制消息类型。
接收队列容量为 32 个事件，超载仍关闭连接；Ping/Pong、客户端关闭和服务关闭不等待队列中的请求执行。

客户端使用 HTTP/SSE 时，OpenAI Provider 仍可能选择上游 WebSocket。
客户端配置的 `supports_websockets` 只控制第一段连接，不是服务端传输策略开关。
上游在响应终态前发送 Close 1000 仍属于失败，不能按“正常关闭”计为成功。

已建立模型执行的 Responses、Images 和 Search HTTP 响应使用现有 ID：
`x-gateway-request-id` 为模型执行 ID；`x-request-id` 保留有效上游值，只有上游
`x-oai-request-id` 时复用其值，没有上游 ID 时使用模型执行 ID。`x-oai-request-id` 不是必需字段，
也不要求客户端识别它；OpenAI 与 xAI 路由使用相同规则。失败响应的关联 ID 不采用会话 opening ID，
错误正文读取失败时仍返回已知上游 ID；已采集的 turn state 等允许的会话头继续按原合同交付。
尚未建立执行的入口拒绝继续使用 middleware 的入口关联。

WebSocket 在尚未交付上游业务事件时合成的错误保留已确认的失败状态，以及 Provider 提取的结构化
message/type/code；没有结构化错误时使用稳定安全文案，不把原始 HTML 或截断正文当作 message。
合成错误自身的 `headers` 携带允许下发的响应头：优先保留实际失败的上游 request ID，无上游 ID 时
提供网关关联 ID，并用 `x-gateway-request-id` 独立标识网关请求。已经取得的原始上游错误帧不重写。
客户端可能对特定状态另行统一展示；这不构成网关改写真实状态码的理由。

`GET /v1/models` 默认返回 OpenAI 兼容列表 `{"object": "list", "data": [...]}`；请求携带非空
`client_version` query 参数（Codex 客户端）时改为返回 Codex 专用目录合同 `{"models": [...]}`。

OpenAI Provider 按客户端传入的 `client_version` 请求上游目录，完整保留每个模型 JSON 对象，包括
`base_instructions`、`model_messages`、`service_tiers`、工具与能力字段，以及未知嵌套字段、显式 `null`
和字段缺失的区别。Core 不解释这些协议字段，API 不再根据通用模型画像重建 OpenAI 目录。
模型别名仅替换 `slug`，不替换上游展示名、提示词、能力或 `priority`；保持原生模型顺序，新增别名附在后面。
目录仍按当前路由快照的模型存在性过滤，避免公布已知无法路由的模型；新模型需待后台目录对账后进入列表。
xAI 没有 Codex 原生目录，继续使用明确的通用画像适配。

目录账号只能来自本次 Client Key 冻结的账号范围。OpenAI 在其中按账号 ID 排序，使用首个成功读取的
合格账号，最多尝试三个账号；同名模型不跨账号或套餐混拼字段。因此单账号、无别名时可保持该账号的
模型对象一致，多账号/多 Provider 聚合不代表“与某个官方账号的整个目录完全一致”，也不会固定后续推理账号。
读取失败返回 `503 model_catalog_unavailable`，不以简化模板或空成功响应覆盖客户端缓存。

原生目录使用 Provider 内的有界缓存：最多 32 份，成功 TTL 为 5 分钟，失败短缓存为 5 秒，单次上游
读取超时为 15 秒。缓存按账号、凭据 revision、套餐、上游账号身份、客户端版本和请求画像隔离；并发
读取合并，新的目录 ETag、后台目录内容变化或显式失效会清理缓存。每次读取仍重新检查账号资格和 Key 范围，
不会因缓存命中跨越权限。完整对象不写 PostgreSQL 或 Redis；现有套餐 Redis cache 仍只保存模型 ID。
成功目录响应带 `Cache-Control: private, no-store`，不借用上游 ETag 标识经过选择/别名/聚合后的正文。
Codex 自己仍会写 `models_cache.json`；Responses 的 `x-models-etag` 保持原协议，这可能让客户端再次
读取目录，但通常命中上述服务端缓存，不表示每轮都重新请求上游。

`service_tiers` 的 `name` 用于生成 `/fast` 等命令，`id` 是请求使用的 `service_tier` 值，二者不能互换。
上游未声明或明确返回空数组时不补档位，也不根据模型名或旧 `additional_speed_tiers` 字段推断 Fast。
该合同对齐
[官方 Codex 0.154.0 的模型元数据](https://github.com/openai/codex/blob/6b9826e3aa83b1a5947db50f4332cb9c65f1b340/codex-rs/protocol/src/openai_models.rs)
与其动态服务档位命令；升级网关后，已有客户端缓存需要在重新加载模型目录后才能反映修复。

Codex 专用目录中的 `context_window` 与 `max_context_window` 分别表示默认上下文窗口和客户端本地
覆盖的上限。OpenAI Provider 原样保留上游对应字段，缺失与 `null` 不互相转换；网关不通过部署配置
覆盖这些值。Codex 客户端配置 `model_context_window` 后，按该值与非空 `max_context_window` 的较小值
使用窗口；上限为空时保留客户端本地值。xAI 目录只声明一个窗口，其 Provider 继续以该值作为客户端覆盖上限。

OpenAI 路径保留客户端 Responses wire 语义：请求 body 的未知字段和字段顺序保持不变（受控模型
映射除外），HTTP SSE 与 WebSocket 的上游业务事件字节原样转发，response ID 按 opaque 值处理而不
假设 UUID 或固定长度；OpenAI 上游错误 envelope 和允许下发的 opaque header 值也不由 canonical
观测结果重写。Images 请求不读取或重建 JSON，也不要求或映射模型字段；它固定使用 OpenAI Provider，
只在原始字节之外完成账号选择、鉴权头替换和端点路由，成功与失败响应正文同样保持原始字节。
`/v1/alpha/search` 使用相同的 OpenAI Provider 原生端点边界：body（包括 `model`）不解析、不映射，
`x-codex-turn-metadata` 在移除客户端账号身份并按当前 lease 重写 installation ID 后转发；上游账号
Authorization、Cookie、account ID、originator 和 User-Agent 均由代理安全重建。xAI 是 Grok wire 与
Responses wire 之间的协议转换层，转换只在 xAI Provider 内完成。
上游结构化错误的 message/code/type 会透传给客户端，其中内嵌的账号指纹 UUID 已脱敏。模型映射是
全局精确映射，未命中时模型名原样交给候选 Provider；分组只限定账号集合，不参与模型改名。

## 4. 管理员认证

| 方法 | 路由 | 请求 | 说明 |
| --- | --- | --- | --- |
| `POST` | `/api/admin/auth/login` | `{ username?, password }` | 创建管理员会话并设置 Cookie |
| `GET` | `/api/admin/auth/status` | 无 | 返回当前 Cookie 是否已认证 |
| `POST` | `/api/admin/auth/logout` | 无 | 删除当前会话并清除 Cookie |

## 5. 账号

账号 API 使用统一路由，不存在 Provider Instance 或 Provider 专属账号路由。需要 Provider 的请求只接受
`provider: "openai" | "xai"`。

| 方法 | 路由 | 主要 query/body | 说明 |
| --- | --- | --- | --- |
| `GET` | `/api/admin/accounts` | `page`、`pageSize`、`provider`、`groupId`、`search`、`status`、排序字段 | 分页查询账号与汇总 |
| `GET` | `/api/admin/accounts/detail` | `accountId` | 查询账号详情、额度和本地用量 |
| `GET` | `/api/admin/accounts/export` | `accountIds`、`confirm=export_sensitive_accounts` | 显式导出最多 200 个账号的敏感 Provider 文档 |
| `POST` | `/api/admin/accounts/import` | `{ provider, data, settings?, outboundProxyId? }` | 导入或按上游身份更新账号，可同时应用调度、分组设置与默认代理 |
| `POST` | `/api/admin/accounts/refresh` | `{ accountId }` | 手工刷新 OAuth credential（`idToken` / `accessToken` / `refreshToken`），不刷新额度 |
| `POST` | `/api/admin/accounts/recover` | `{ accountId }` | 管理员显式清除该账号的本地错误/额度/cooldown 事实并重新启用，不访问上游 |
| `POST` | `/api/admin/accounts/rotate` | OpenAI rotation 字段 | 手工替换 OpenAI OAuth token |
| `POST` | `/api/admin/accounts/update` | `{ accountId, enabled, concurrencyLimit, weight, groupIds, outboundProxyId?, outboundProxyUrl? }` | 一次更新账号调度状态、并发上限（`null` 表示继承运行参数）、权重（1–100）、所属分组与出站代理 |
| `POST` | `/api/admin/accounts/batch-update` | `{ accountIds, enabled, concurrencyLimit, weight, groupIds, outboundProxyId?, outboundProxyUrl? }` | 一次事务统一更新所选账号的调度字段、完整分组集合与可选代理 |
| `POST` | `/api/admin/accounts/delete` | `{ provider, accountIds }` | 批量删除 1–200 个账号 |
| `GET` | `/api/admin/accounts/quota` | `accountId` | 读取当前额度，不强制访问上游 |
| `GET` | `/api/admin/accounts/quota-forecast` | `accountId` | 按需读取周/月容量预测、源窗口剩余估算与采样依据，不刷新上游额度 |
| `POST` | `/api/admin/accounts/quota/refresh` | `{ accountId }` | 访问 Provider 并刷新额度，同时同步额度所属状态 |
| `GET` | `/api/admin/accounts/profile-statistics` | `accountId` | 实时查询 OpenAI/Codex 官方个人资料中的累计活动与使用洞察 |
| `GET` | `/api/admin/accounts/reset-credits` | `accountId` | 查询 OpenAI 上游主动额度重置卡，不读取本地库存 |
| `POST` | `/api/admin/accounts/reset-credits` | `{ accountId, creditId?, redeemRequestId }` | 使用 UUIDv4 幂等键消费一张 OpenAI 上游重置卡 |
| `GET` | `/api/admin/accounts/models` | `accountId` | 优先读取该 Provider + 套餐的模型 cache，缺失时有限实时拉取 |
| `POST` | `/api/admin/accounts/models/refresh` | `{ accountId }` | 强制拉取最新模型并覆盖 cache |
| `GET` | `/api/admin/accounts/connection-test` | `accountId`、`modelId` | 通过 SSE 返回实时连接测试事件，不作为业务 Responses 用量记录 |
| `POST` | `/api/admin/accounts/oauth/start` | `{ provider, name, accountId?, outboundProxyId?, outboundProxyUrl? }` | 创建 OpenAI 或 xAI OAuth flow；`accountId` 表示重新授权 |
| `POST` | `/api/admin/accounts/oauth/complete` | `{ provider, flowId, callbackUrl, settings? }` | 消费 OAuth callback；首次授权可附带账号设置，重新授权保留原设置 |

账号列表支持以下稳定值：

- `provider`: `all`、`openai`、`xai`；
- `groupId`: 分组 ID、`ungrouped`，或省略以不过滤；
- `status`: `normal`、`quota_exhausted`、`rate_limited`、`disabled`、`error`；
- `sortBy`: `email`、`status`、`planType`、`usage`、`lastUsedAt`、`expiresAt`；
- `sortDirection`: `asc`、`desc`。

账号视图和 Dashboard 账号概览中的 `planType` 保留原始套餐值；`planTypeDisplay` 由后端先按 Provider 解析名称，
再统一为大驼峰格式，前端直接展示该字段，例如 `Free`、`SuperGrokPro`、`EduPlus`。
OpenAI 的 `self_serve_business_prolite` 等 Team 套餐显示为 `Business`；新套餐也使用相同格式。
账号套餐为空或 `unknown` 时，后端优先用已保存的上游额度响应
中的明确套餐值补全 `planType` 和 `planTypeDisplay`；两处均无套餐信息时才显示“未知套餐”。

`outboundProxyId` 绑定已保存且最近测试成功的代理；省略或 `null` 保留当前绑定，空字符串清除绑定。
`outboundProxyUrl` 兼容 HTTP、HTTPS、SOCKS5、SOCKS5H 代理 URL，可带用户名和密码；不能与 ID 同时设置。
编辑时省略或 `null` 表示保持原配置，空字符串表示清除代理并直连。列表和详情只返回
不含认证信息的 `outboundProxyEndpoint`（直连时为 `null`）；只有显式敏感导出包含完整 URL。
指定代理后，推理、OAuth 服务端交换/刷新及账号辅助请求使用同一出口；代理失败不会退回直连。
浏览器打开的第三方 OAuth 授权页仍使用浏览器自身网络。
账号出口与连接隔离见 [架构说明](architecture.md#账号出站代理)。

### 独立代理管理 / Managed Proxies

所有端点要求管理员身份。所有响应只返回去掉认证信息的 `endpoint`，不会返回完整 URL。
All endpoints require admin authentication and redact proxy credentials from responses.

| 方法 / Method | 路径 / Path | 请求 / Request | 结果 / Result |
| --- | --- | --- | --- |
| `GET` | `/api/admin/proxies` | `page`、`pageSize`（1-200）、`search`（名称） | `{ items, page }` |
| `GET` | `/api/admin/proxies/accounts` | `proxyId`、`page`、`pageSize`（1-200）、`search`（账号名称或邮箱） | `{ items, page }` |
| `POST` | `/api/admin/proxies/accounts/remove` | `{ proxyId, accountId }` | `{ configRevision }` |
| `POST` | `/api/admin/proxies/create` | `{ name, proxyUrl }` | `201 { record, configRevision }` |
| `POST` | `/api/admin/proxies/update` | `{ id, revision, name, proxyUrl? }` | `{ record, configRevision }` |
| `POST` | `/api/admin/proxies/test` | `{ id, revision }` | 最新代理记录 / Proxy record with test result |
| `POST` | `/api/admin/proxies/delete` | `{ id, revision }` | `{ configRevision }` |

`record` 包含 `id`、`name`、`endpoint`、`hasAuthentication`、`revision`、`accountCount`、
`lastTestAt`、`lastTest: { success, latencyMs, exitIp, message }`、`createdAt`、`updatedAt`。
未测试时 `lastTestAt` / `lastTest` 为 `null`。连通性失败返回 HTTP 200 和 `lastTest.success=false`；
记录版本过期、重复 URL、删除已绑定的代理返回 409，并发测试满载返回 429。

代理列表只返回关联账号数量。关联账号按需查询，每项包含 `id`、`name`、`email`、`provider`、`enabled`、
`authenticationKind`、`planType`、`planTypeDisplay` 和 `groups: [{ id, name, color, enabled }]`，
不返回账号凭据。默认每页 20 条，按名称、ID 稳定排序；搜索不区分大小写，匹配名称或邮箱的字面子串。
不存在的代理返回 404，未绑定账号或没有匹配结果时返回空页。数量与当前页来自同一个数据库只读快照。

移除关联账号只清除指定账号的代理绑定与连接地址，使其改为直连，保留凭据、调度参数与分组。
若账号已不再绑定请求中的代理，则返回 409；成功后在同一事务中更新配置版本与审计，并发布运行时快照。

更新省略 `proxyUrl` 保留认证；连接配置改变时清除测试结果并更新所有绑定账号。
Omit `proxyUrl` to preserve credentials. Connection changes invalidate the previous test and update all bound accounts.
Tests persist only when the requested revision still matches. Connectivity failures use HTTP 200 with
`lastTest.success=false`; stale revisions, duplicate URLs and deleting an in-use proxy return 409.
The test concurrency limit returns 429.

测试固定经代理访问 `https://api.ipify.org?format=json`，超时 15 秒，每进程最多同时测试 4 条。
探测器复用 OpenAI 的证书信任配置：优先读取非空的 `CODEX_CA_CERTIFICATE`，
其次读取 `SSL_CERT_FILE`，并保留系统根证书；证书配置错误不会回退为不验证证书。
出口测试通过不表示 Provider 账号权限或额度可用；账号可用性使用账号连接测试。
导入请求可以携带顶层 `outboundProxyId`，在令牌交换前解析为默认出口；文件中显式的代理配置优先。
文件及 AT/RT 导入从凭据交换到落库期间保护所选代理；此时修改、删除或写入测试结果返回 409，
避免已轮换的凭据因代理状态变化而丢失。完成导入或请求取消后自动释放保护。
OAuth 等待回调期间不持有保护；提交仍拒绝已删除、连接配置改变或测试失败的代理。

Tests reach `https://api.ipify.org?format=json` through the configured proxy, with a 15-second timeout
and four concurrent tests per process. Provider access still requires the account connection test.
Imports accept a top-level `outboundProxyId` as the default exit before token exchange; explicit per-account
settings in the document take precedence. Credential imports reserve their selected proxy until commit;
concurrent proxy mutations return 409. OAuth commits still reject a deleted, changed or failed proxy.

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

导入的 `data` 必须是 JSON object，Admin API 请求上限为 64 MiB；Provider 可以收紧限制，
当前 xAI 导入上限为 16 MiB。内部 schema 由目标 Provider 独占解释：

- OpenAI 接受单账号 OAuth 文档、`accounts` 数组（最多 200 项）、CPR 账号 bundle 和含代理引用的 sub2api 导出；
- OpenAI OAuth token 字段接受 `accessToken`、`refreshToken`、`idToken`，以及官方
  `auth.json` 中的 `access_token`、`refresh_token`、`id_token`，可以嵌套在 `tokens` 等账号 object 内；
  每项至少包含 AT 或 RT。仅含 `OPENAI_API_KEY` 的客户端代理配置不是 OAuth 账号导入材料；
  RT-only 会在导入时换取 AT，AT-only 不具备自动续期能力；
- OpenAI 与 xAI 的账号条目接受 `outboundProxyUrl`；OpenAI 还会解析 sub2api 的 `proxy_key` 和顶层 `proxies`。
  代理在 token 刷新前绑定。缺失、重复、停用、带到期时间或配置回退策略的 sub2api 代理会拒绝导入；
- xAI 从单账号 object 或 `accounts` 数组中提取 OAuth token；并发、优先级等字段不参与认证；
- xAI 批量导入逐条独立校验：失败条目跳过并记录日志，不中断其余条目，仅当没有任何条目成功时整个导入才报错；
- xAI API Key 不是受支持的账号 credential；
- 导入不会只凭文件外形写入账号；目标 Provider 使用认证材料完成必要的 token exchange 或已认证账号资料补全。

管理端的 OpenAI `AT` / `RT` 标签是同一导入 API 的输入便利层：每行一个 token，最多 200 行，提交前
转换为对应的 `accounts` JSON。Admin API 本身不接收纯文本 token 列表。例如：

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

账号导入与首次 OAuth complete 可附带 `settings: { enabled, concurrencyLimit, weight, groupIds }`。
提供 `settings` 时四项均必填，`concurrencyLimit: null` 继承运行参数，否则为 1–4294967295 的整数；
`weight` 为 1–100，`groupIds` 为完整分组集合。设置应用于本次导入的全部账号，包括匹配到的已有账号，
与凭据在同一事务内提交；分组不存在时整次回滚。省略 `settings` 时新账号使用默认设置并保持未分组，
已有账号保留原有分组、权重与并发设置。重新授权不接受 `settings`，普通 credential refresh/rotation 也保留账号设置。

管理端先配置账号设置，再选择 OAuth、AT/RT 或账号文件完成导入。返回设置保留输入；更改出站配置会使
旧 OAuth 链接失效。文件中显式的出站配置优先于表单代理，未指定时使用表单代理。
账号列表的每个 item 返回轻量 `groups: [{ id, name, enabled }]`。

OpenAI 的 CPR 导出保持 OAuth 账号的既有 token 与过期时间字段。

OpenAI rotation 请求字段为：

```json
{
  "provider": "openai",
  "accountId": "acct_...",
  "idToken": "...",
  "accessToken": "...",
  "refreshToken": "..."
}
```

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
- 账号文件导入和 OAuth complete（包括重新授权）在 credential 提交后后台尝试一次额度观测，不等待
  观测完成才返回成功。观测失败只记录告警，不回滚已提交的账号；手工或后台 RT 刷新只更新 token，
  不隐式等同于手工额度刷新，也不更新既有账号资料或 OAuth principal。xAI 导入与 OAuth complete
  使用相同的提交后观察流程。
- OAuth pending flow 先取得带过期时间的独占 claim，只有账号事务提交成功后才消费。失败会释放 claim，
  但上游 authorization code 本身通常只能交换一次；已完成过 token exchange 时应重新创建 OAuth flow。
- `GET /accounts/quota` 只读取最后一次落库快照；`POST /accounts/quota/refresh` 才访问上游。access token
  已过期时，额度刷新要求先走 credential 刷新或重新授权，不会拿过期 token 探测额度。
- OpenAI 已耗尽账号每 30 分钟主动复核一次，也会在最早未恢复窗口的 `resetAt + 2 分钟` 到期后
  提前复核。后台每 30 秒检查触发条件；同一重置边界复核后仍未恢复时回到 30 分钟重试，
  避免旧 reset 持续触发请求。各窗口独立确认恢复，时间到期本身不会直接解除账号耗尽。
- `POST /accounts/recover` 是管理员对本地事实的强制恢复：它清除 Redis cooldown 和已保存的额度/错误，
  把账号重新启用并恢复为可调度 credential；它不验证上游账号是否已经恢复，下一次真实请求仍可重新写入
  失败事实。
- 成功额度观测会 revision-fenced 写入 quota；明确 `Allowed` 投影为 `normal`，明确耗尽投影为
  `quota_exhausted`。额度观测不会清除凭据过期、无效或封禁事实；这些事实统一投影为 `error`，并由
  `errorReason` 区分。额度接口的 401/403 也不足以判定 refresh token 永久失效，credential 终态只由
  OAuth refresh 的明确永久错误写入。
- 正常 Responses 请求会解析上游响应的 rate-limit headers，合并进同一 quota 快照并同步状态。Free、
  K12 等套餐共用该状态机；套餐只参与账号展示和按套餐隔离的模型目录 cache，不存在 K12 专属额度路径。
- 账号展开区的 Token 结构、模型排行和列表 Token 汇总优先使用账号级周额度窗口，无可统计的周窗口时
  使用月额度窗口；`usage.windowLabelDisplay` 随选中的窗口返回“周额度窗口”或“月额度窗口”。查询边界
  严格为 `[resetAt - windowSeconds, resetAt)`，不是自然周/月或最近 7/30 天；额度刷新若返回了更早的
  重置时间，会按新边界重新聚合。没有边界完整、可归属到账号的周/月窗口时显示无数据，标签为
  “周/月额度窗口”，不回退到 5 小时、日窗口或历史累计。各额度条与 Dashboard 的百分比选择不受影响。
  金额原值保持完整精度，USD 展示值
  小于 1 美元时最多保留四位小数，其余保留两位。
- 账号页没有定时静默轮询。手工额度刷新只替换响应中的账号行并同步状态汇总，不触发整页 loading；若
  新状态不符合当前筛选，该行从当前页移除。请求驱动或后台任务产生的状态变化，需要下一次显式查询账号
  列表后才会显示。

### 周/月额度预测

`GET /api/admin/accounts/quota-forecast?accountId=...` 使用现有管理员鉴权，返回独立的预测结果，不向
账号列表或详情附加预测字段。管理端从“模型使用排行”后的图标打开弹窗时查询；切换周/月仅切换本次结果，
不轮询。关闭弹窗取消未完成的查询，重新打开重新采样。“刷新额度”仍调用现有
`POST /api/admin/accounts/quota/refresh`，成功后替换账号行并重新查询预测。

响应 `data` 包含 `accountId`、`generatedAt` 和 `forecasts`（`weekly`、`monthly`）：

- `targetDays`、`extrapolated`：对应周期存在真实账号级窗口时采用实际时长；缺少对应窗口时，使用
  可统计的周/月窗口按 7/30 天折算，明确标记 `extrapolated: true`。不把短期限流或模型专属桶当作账号容量。
- `estimatedTokens` / `estimatedUsd` 及对应 `*Display`：完整目标周期的近似容量，
  公式为 `样本用量 × 100 / sampledPercent × 目标窗口秒数 / 源窗口秒数`。
- `remainingTokens` / `remainingUsd` 及对应 `*Display`：**额度快照时源窗口**的剩余估算，
  公式为 `样本用量 × (100 - usedPercent) / sampledPercent`；不随目标周期折算，不代表当前可消费余额。
- `source`：只返回容量卡所需的源窗口名称 `label`、已用比例 `usedPercent` / `usedPercentDisplay`、
  额度观测时间 `observedAt` / `observedAtDisplay`、用于过期检查的 `resetAt`，以及选中采样区间
  已记录的 `tokensDisplay` / `usdDisplay`。`source: null` 表示没有可选的源窗口。
  不再返回请求数、Token 构成、费用覆盖计数、采样方法、基线与进度段等右侧明细，
  Admin 结果模型也不再复制这些诊断字段；计算与完整性检查所需的内部采样事实不变。
- `unavailableReason`：不能估算时的说明，正常为 `null`。有效进度少于 5 个百分点不预测；
  `lowSample` 表示有效进度不足 10 个百分点，或增量采样少于 2 个完整段，仅是质量提示，不承诺精度。
  `incompleteCost` 仅抑制费用预测；`incompleteTokens` 仅抑制 Token 预测。未知值不按确定的零消耗外推。
  弹窗仅显示容量卡，将低样本展示为“初步估算”，以简短说明提示估算限制；底部的更新时间取
  `source.observedAtDisplay`，不能用查询生成时间冒充额度观测时间。只有实际阻止 Token 或费用
  预测的数据缺失展示一条合并提示，不改变预测门槛。

内部采样优先采用近期分段，条件不足时才使用窗口累计估算。同一额度段内，以历史额度和截至相应
完成时间的累计用量建立基线，每累计至少 5 个百分点形成一段，使用最近 3 个完整段及未满一段的尾部。
上式中的 `sampledPercent` 是内部有效额度进度（百分点），不是时间进度或对外响应字段。
只对合并区间计算比值，不平均逐请求小分母比值；重复读数不增加段数，大于 1 个百分点的回落或累计
计数倒退会中断采样，不能静默跨越。缓存命中属于输入，不重复相加；其他币种或缺少有效金额不能当成零 USD。

采样查询 `[max(resetAt - windowSeconds, accountAddedAt), observedAt)` 内开始的请求，
仅把 `completedAt <= observedAt` 的完整交付用量计入当前分子；历史分子按各点的完成时间累计，
同完成时间使用同一累计值。每个源窗口从同一数据库语句取得累计数值和最多 128 个按时间分桶的历史文档，
不会因历史点抽样而丢弃其间的成功 Token，也不重新聚合模型排行；接口不返回原始 Provider 文档。

OpenAI 复用已有限流协议解析器匹配额度桶、槽位、时长和明确的套餐。仅允许最多 2 秒的重置时间抖动，
超出或套餐改变则截断历史基线；不将宽时间容差当作上游窗口身份。历史文档缺少独立额度观测时间，
只能以请求完成时间近似配对，不证明上游扣额与本地完成同步。无法积累有效增量时，仅在账号于窗口开始前
已加入且没有已知采样断点的情况下使用累计估算；中途加入账号可以在新基线之后积累，无需等待下次重置。

预测没有独立持久化或调度状态，也不参与账单结算。站外消耗、历史日志清理、异步观测延迟、未成功交付的
上游消耗和模型组合变化仍可能造成误差；等价 USD 费用不是官方订阅价格或固定额度承诺，
30 天折算也不是自然月额度。记录覆盖率不等于预测准确率，不输出未经校准的置信区间。

### OpenAI 官方个人资料统计

`GET /api/admin/accounts/profile-statistics?accountId=...` 仅支持 OpenAI/Codex OAuth 账号。每次查询直接
访问官方个人资料端点，不读取本地 usage/billing 记录，也不缓存或估算统计结果。响应 `data` 包含：

- `displayName`、`username`、`imageUrl`：官方账号资料；
- `summary`：累计文本 Token、单日峰值 Token、最长任务时长、当前连续天数和最长连续天数；
- `dailyUsage`：按日期返回的 Token 活动；
- `activityInsights`：快速模式占比、上游原样返回的推理强度及占比、Skill 探索/使用数、聊天总数，
  以及插件与 Skill 调用排行。

官方未返回的字段保持 `null`，不使用本地数据补齐；`hasStatsError: true` 表示账号资料可用，但官方统计
部分不可用。access token 已过期或官方返回 401 时，接口要求先刷新 credential 或重新授权。原账号级
`GET /api/admin/accounts/usage-statistics` usage/billing 报表接口及其查询链路已移除。

### OpenAI 主动额度重置卡

`GET /api/admin/accounts/reset-credits?accountId=...` 每次都查询 OpenAI 上游；后端不把卡片列表写入
PostgreSQL 或 Redis。管理端只在用户打开弹窗或点击刷新时调用，并在当前浏览器会话内缓存最近一次成功
结果，用于账号行上的 `xN` 提示。

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
`POST /api/admin/accounts/quota/refresh` 回读官方额度；不得直接改写本地 `resetAt`。xAI 不支持该能力。

## 6. 账号分组

分组是 Provider-neutral 的账号集合；一个组可包含任意 Provider 账号，一个账号也可属于多个组。

| 方法 | 路由 | 主要 query/body | 说明 |
| --- | --- | --- | --- |
| `GET` | `/api/admin/account-groups` | `page`、`pageSize`、`search`、`enabled` | 分页查询分组；返回账号可用性、并发槽位（Redis 不可用时 `usedSlots=null`）及成功请求 USD 用量 |
| `POST` | `/api/admin/account-groups/create` | `{ name, description, color }` | 创建空分组；`color` 严格为 `#RRGGBBAA`，返回时统一大写 |
| `POST` | `/api/admin/account-groups/update` | `{ id, name, description, color }` | 更新名称、描述和颜色 |
| `POST` | `/api/admin/account-groups/enable` | `{ id }` | 启用 |
| `POST` | `/api/admin/account-groups/disable` | `{ id }` | 禁用；已绑定 Key 保持受限，不回退到全部账号 |
| `POST` | `/api/admin/account-groups/delete` | `{ id }` | 删除未被 Client Key 引用的组 |

列表数据为 `{ items, page, configRevision }`，其中 item 返回 `memberCount`、按 Provider 聚合的
`providerCounts` 和 `clientKeyCount`。查询分组成员使用账号列表的 `groupId` 筛选，
不提供独立的分组成员路由；账号的 Provider 不代表整个分组的 Provider。

## 7. Client Key

| 方法 | 路由 | 主要 query/body | 说明 |
| --- | --- | --- | --- |
| `GET` | `/api/admin/client-keys` | `cursor`、`limit`、`search`、`sortBy`、`sortDirection` | 游标分页查询 |
| `POST` | `/api/admin/client-keys/create` | 创建字段 | 创建带账号范围的 Client Key |
| `GET` | `/api/admin/client-keys/reveal` | `id` | 显式读取完整明文 Key |
| `POST` | `/api/admin/client-keys/update` | 更新字段 | 原子更新名称、分组范围和限额 |
| `POST` | `/api/admin/client-keys/enable` | `{ id }` | 启用 |
| `POST` | `/api/admin/client-keys/disable` | `{ id }` | 禁用 |
| `POST` | `/api/admin/client-keys/delete` | `{ id }` | 删除 |

创建字段为 `name`、可选 `label`、`groupIds`、`maxConcurrency`、`requestsPerMinute`、可选
`dailyLimitUsd`、`weeklyLimitUsd` 和 `customKey`，更新请求增加
`id`。`groupIds` 必须显式提交：空数组派生 `routingScope: "all"`，非空数组派生
`routingScope: "groups"`。响应同时返回分组引用 `groups`，以及从当前有效账号池派生、仅供展示的
`providerKinds`；Client Key 不再保存 `providerKind`。创建和 reveal 响应会返回完整明文 Key，调用方
必须立即安全保存。

密钥列表的 `search` 仅匹配名称和标签，不匹配密钥值或可见前缀；搜索不区分大小写，使用字面量前缀匹配。
创建和更新时去除名称首尾空白，并按忽略大小写、首尾空格的名称查重，重复返回 `409`。
更新排除当前记录；并发写入复用控制面事务锁，失败不会留下审计或配置版本变更。
历史重名数据不自动改名，已有凭据继续有效；再次保存时需使用未被其他密钥占用的名称。

`customKey` 仅用于创建：省略、`null` 或空字符串时继续自动生成；非空时按原值保存，不追加前缀、
不截断、不修剪空白，也不要求固定长度。Key 须为 HTTP Bearer 可传输的非空可见 ASCII 字符，
支持标点，空格、控制字符和非 ASCII 文本会返回 `400`。重复 Key 返回 `409`，包括并发创建时。
应用不另设 Key 长度上限；创建请求和 `Authorization` 头仍受 Web 服务器及反向代理的通用大小限制。
更新接口不接受 `customKey`，防止修改策略时意外替换正在使用的凭据。
列表仅展示最多前 10 个字符，且至少隐藏一半字符；单字符 Key 的可见前缀为空。
完整值仍只通过创建和显式 reveal 返回，不进入普通 Debug 或审计。

迁移示例（其他创建字段同上）：

```json
{
  "name": "迁入的客户端",
  "customKey": "legacy-platform-key/example+=",
  "groupIds": [],
  "maxConcurrency": 2,
  "requestsPerMinute": 0
}
```

客户端沿用原 Key，将 Base URL 指向本平台即可。分组权限、日／周限额和并发规则按本平台配置执行，
不会导入旧平台的历史用量。部署升级会新增 `0006_custom_client_keys.sql`，保留全部已有 Key，
并以 SHA-256 唯一索引支持长 Key；鉴权仍校验完整原值。

金额字段为非负十进制字符串，最多 10 位整数与 10 位小数，`"0"` 表示不限额。
创建时省略金额字段默认为零；更新时省略或 `null` 保留当前值，修改限额不会清空已用金额。
`maxConcurrency` 和 `requestsPerMinute` 是非负整数，零表示不限。

列表增加 `dailyLimitUsd`、`weeklyLimitUsd`、`dailyUsedUsd`、`weeklyUsedUsd`（均为字符串）、
`dailyResetsAt`、`weeklyResetsAt`（RFC3339 或 `null`）。
管理端日／周金额显示两位小数，悬停可查看原始值；记账和限额比较保留完整精度。
日窗口按北京时间零点重置；周窗口从首次准入当天零点起持续七天，到期后在下一次使用时重新开启。
费用按请求完成时间归属窗口。并发按同一 Key 的执行中请求累计，包含 SSE 与每个 WebSocket
`response.create`；空闲连接不占名额，内部重试不重复占用。
修改 Key 策略对既有 WebSocket 连接的下一次请求同样生效，已开始的请求保持原有快照。

任一已结算金额达到限额后拒绝新请求，已准入请求可完成并使金额超过阈值。
HTTP 返回 `429`，`error.code` 为 `key_daily_budget_exceeded` 或 `key_weekly_budget_exceeded`，
并附 `Retry-After`；WebSocket 每次 `response.create` 执行相同检查并返回协议错误事件。
只累计上游上报或按用量与模型价格计算出的 USD 费用；无法取得费用的尝试按零累计，
保留错误和用量诊断，不产生待核账记录或阻断。内部重试中已经取得的费用仍会累计。
预算存储不可用时返回 `503`、`key_budget_unavailable`。

自动结算按网关请求 ID 幂等执行。账本独立于使用统计日志，记录保留至删除 Key，
不受 `usageRetentionDays` 影响。

English: Daily and weekly budgets use automatically recorded costs. Missing usage or interrupted requests
do not block a Key, and no manual reconciliation is required. New requests receive `429` once recorded
costs reach the daily or weekly limit; requests already admitted can finish above that threshold.

## 8. 运行设置

| 方法 | 路由 | 说明 |
| --- | --- | --- |
| `GET` | `/api/admin/settings` | 读取运行设置 |
| `POST` | `/api/admin/settings/update` | 原子替换全部运行设置 |
| `GET` | `/api/admin/settings/client-downloads/codex-desktop/windows` | 提取 Codex Desktop Windows 离线安装直链；`refresh=true` 强制刷新进程内短缓存 |
| `GET` | `/api/admin/settings/admin-api-key` | 只返回管理 API Key 是否存在 |
| `POST` | `/api/admin/settings/admin-api-key/delete` | 删除管理 API Key |
| `POST` | `/api/admin/settings/admin-api-key/regenerate` | 重新生成并一次性返回完整管理 API Key |

设置更新字段包括：

```text
modelMappings
refreshMarginSeconds
refreshConcurrency
maxConcurrentPerAccount
requestIntervalMs
rotationStrategy
minCodexDesktopVersion
minCodexCliVersion
usageRetentionDays
opsEventRetentionDays
auditRetentionDays
```

`rotationStrategy` 可取 `smart`、`quota_reset_priority`、`round_robin`、`sticky`。
两个 `minCodex*Version` 字段为 `string | null`，只设置最低版本，不存在最大版本字段。

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

全部备份端点位于 `/api/admin/settings/backups/*`，内部由独立 BackupService 承担，不并入设置用例。响应继续使用 `AdminEnvelope`，wire 字段 camelCase，`Cache-Control: no-store`。

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

读取设置响应（Secret 以明文返回，由前端掩码显示）：

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

用量查询可组合页码/游标、时间范围、Provider、Client Key、账号、模型、route、transport、状态码、
request/response/upstream ID、outcome 与搜索文本。诊断 `dimension` 可取 `model`、`account`、
`apiKey`、`provider`、`transport`、`failureClass`、`status`。

请求记录列表的 `search` 使用字面量前缀匹配，支持请求 ID、Client Key ID / 名称、
账号 ID、账号邮箱与名称、请求 / 上游模型 ID、上游请求 ID。密钥名称不区分大小写，其他字段区分大小写。
密钥名称按当前密钥记录检索，改名后使用新名称，删除后仍可按 Client Key ID 查询历史记录。
账号邮箱与名称按请求记录的历史快照检索，
不随当前账号修改或删除而改变；`%`、`_` 和 `\` 均按普通字符处理，不作为搜索通配符。

请求 ID 和上游 ID 继续通过既有字段查询；不增加入口 ID 字段，也不扫描 trace 建立查询映射。
旧响应中只有入口 ID 时仍需结合时间与入口日志定位，不能回填不存在的关联。
请求记录和错误列表支持按密钥名称搜索，不支持密钥值或可见前缀搜索。
错误列表的主动刷新、搜索和平台/时间条件变化会取得新的结束时间；翻页沿用该次查询快照。

已进入模型执行会话、但在首次合法 ProviderStream 建立前失败的请求也进入现有错误及详情查询，
包含无可用账号、准备失败、启动/准备超时与取消。此时 attempt 数为零，未确认的 Provider、账号及
上游传输为空；可用模型执行 ID 在“全部平台”下查询，不从路由候选推断实际调用平台。
已有合法 stream 后的失败仍保留真实 attempt，即使尚未收到首事件。
鉴权、解析、路由和准入等入口拒绝不属于该范围；请求观测仍是可能延迟或丢弃的异步投影。

汇总与洞察中的请求数与 outcome 分布覆盖筛选范围内全部请求；token、缓存、延迟与成本聚合仅统计
已完整交付客户端的成功推理响应。OpenAI 的 `generate: false` 连接与上下文准备记录归类为
`requestKind: "prewarm"`，不进入用量列表、账号用量或额度预测的 Token / 费用覆盖统计，但仍可按
请求 ID 读取审计详情，响应中的额度观测仍可用于预测配对。此分类以实际 `generate` 字段为准，
不能仅凭客户端的同名 metadata 或输出 Token 为零排除普通推理；其他 Provider 不套用该规则。

详情接口按 `id` 可读取成功、失败或未完成请求。新增 `trace`（历史未采集记录为 `null`）和
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

OpenAI 的 `serviceTier` 只接受上游响应生命周期事件确认的实际 `response.service_tier`；请求里的
期望档位只保留在 request summary，不能冒充响应事实。计费展示把 `priority`/`fast` 映射为 `Fast`，
`flex` 映射为 `Flex`，缺失或 `default` 映射为 `Default`；未知非空值原样展示。Fast 优先使用模型的
priority 价格，缺少专用价格时回退到标准价格的 `2.00x`；Flex 为 `0.50x`，Default 为 `1.00x`。

## 11. 版本、更新与重启

| 方法 | 路由 | 主要 query/body | 说明 |
| --- | --- | --- | --- |
| `GET` | `/api/admin/system/version` | 无 | 当前构建、部署模式和可用更新 |
| `GET` | `/api/admin/system/update/detail` | `refresh=true|false` | 读取或强制刷新 Release 详情 |
| `GET` | `/api/admin/system/update/events` | 无 | SSE 更新事件流 |
| `POST` | `/api/admin/system/update` | 可选 `{ targetVersion }` | 开始在线更新 |
| `GET` | `/api/admin/system/update/status` | 无 | 查询当前更新或回滚状态 |
| `POST` | `/api/admin/system/rollback` | 无 | 回滚到保留的上一版本 |
| `POST` | `/api/admin/system/restart` | 无 | 请求进程重启 |

在线更新仅在当前部署模式、Release 资产和进程重启能力都满足要求时可用，且只在同一 major 版本内
提供：跨大版本目标会以 `40901` 冲突拒绝，需按发布说明重新部署。
实例升级和仓库发版见 [部署文档](../deploy/README.md#镜像升级与源码构建)。
