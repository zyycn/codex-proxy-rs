# Codex Proxy RS 接口

本文列出 v3 当前公开 HTTP 接口。路由事实以
`backend/crates/gateway-api/src` 中的 router 为准。

## 1. 鉴权与公共约定

### 客户端接口

所有 `/v1/*` 请求都使用管理端创建的 Client Key：

```http
Authorization: Bearer sk_...
```

Client Key 固定绑定一个 Provider：`openai` 或 `xai`。同一次请求不会跨 Provider fallback。

### 管理接口

除登录、会话状态和登出外，所有 `/api/admin/*` 请求都需要以下任一鉴权方式：

- 浏览器登录后得到的 `cpr_admin_session` Cookie；
- `x-api-key: <admin-api-key>`。

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

错误响应仍使用 `code`、`message`、`data`，其中 `data` 为 `null`。稳定业务码包括参数错误
`40001`、会话缺失 `40101`、登录凭据错误 `40102`、管理 API Key 错误 `40103`、资源不存在
`40401`、业务冲突 `40901`、内部错误 `50001`、上游失败 `50201` 和依赖不可用 `50301`。

### 管理写入一致性

Admin API 不暴露全局配置版本，mutation 请求也不要求客户端提供全局配置版本。会改变路由快照或安全配置的写入由后端在事务内推进内部配置 revision，并用于快照发布与审计。

## 2. 健康检查

| 方法 | 路由 | 鉴权 | 说明 |
| --- | --- | --- | --- |
| `GET` | `/healthz` | 无 | Core、Store 和后台任务健康时返回 `204`，否则返回 `503` |

## 3. Responses 与模型目录

Responses HTTP body、WebSocket message 和 frame 不设置网关私有长度上限；协议可接受性由上游决定。

| 方法 | 路由 | 说明 |
| --- | --- | --- |
| `POST` | `/v1/responses` | OpenAI Responses JSON；`stream=true` 返回 SSE，否则返回完整 JSON |
| `GET` | `/v1/responses` | 通过 HTTP Upgrade 建立 Responses WebSocket |
| `POST` | `/v1/responses/review` | 使用同一 Responses 合同发起 review 子代理请求 |
| `GET` | `/v1/models` | 返回当前 Client Key 所属 Provider 的可用公开模型；有两种响应形态，见下 |
| `GET` | `/v1/models/catalog` | 返回 Codex 客户端使用的模型目录 |
| `GET` | `/v1/models/{model_id}/info` | 返回 Codex 客户端使用的单模型信息 |
| `GET` | `/v1/models/{model_id}` | 返回 OpenAI 兼容的单模型详情 |

`GET /v1/models` 默认返回 OpenAI 兼容列表 `{"object": "list", "data": [...]}`；请求携带非空
`client_version` query 参数（Codex 客户端）时改为返回 Codex 专用目录合同 `{"models": [...]}`。

OpenAI 路径保留客户端 Responses wire 语义：请求 body 的未知字段和字段顺序保持不变（受控模型
映射除外），HTTP SSE 与 WebSocket 的上游业务事件字节原样转发，response ID 按 opaque 值处理而不
假设 UUID 或固定长度；OpenAI 上游错误 envelope 和允许下发的 opaque header 值也不由 canonical
观测结果重写。xAI 是 Grok wire 与 Responses wire 之间的协议转换层，转换只在 xAI Provider 内完成。
上游结构化错误的 message/code/type 会透传给客户端，其中内嵌的账号指纹 UUID 已脱敏。模型映射是
全局精确映射，未命中时模型名原样交给所属 Provider。

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
| `GET` | `/api/admin/accounts` | `page`、`pageSize`、`provider`、`search`、`status`、排序字段 | 分页查询账号与汇总 |
| `GET` | `/api/admin/accounts/detail` | `accountId` | 查询账号详情、额度和本地用量 |
| `GET` | `/api/admin/accounts/export` | `accountIds`、`confirm=export_sensitive_accounts` | 显式导出最多 200 个账号的敏感 Provider 文档 |
| `POST` | `/api/admin/accounts/import` | `{ provider, data }` | 导入或按上游身份更新账号 |
| `POST` | `/api/admin/accounts/refresh` | `{ accountId }` | 手工刷新 OAuth credential（AT/RT），不刷新额度 |
| `POST` | `/api/admin/accounts/rotate` | OpenAI rotation 字段 | 手工替换 OpenAI OAuth token |
| `POST` | `/api/admin/accounts/enable` | `{ provider, accountId }` | 启用账号 |
| `POST` | `/api/admin/accounts/disable` | `{ provider, accountId }` | 禁用账号 |
| `POST` | `/api/admin/accounts/delete` | `{ provider, accountIds }` | 批量删除 1–200 个账号 |
| `GET` | `/api/admin/accounts/quota` | `accountId` | 读取当前额度，不强制访问上游 |
| `POST` | `/api/admin/accounts/quota/refresh` | `{ accountId }` | 访问 Provider 并刷新额度，同时同步额度所属状态 |
| `GET` | `/api/admin/accounts/models` | `accountId` | 优先读取该 Provider + 套餐的模型 cache，缺失时有限实时拉取 |
| `POST` | `/api/admin/accounts/models/refresh` | `{ accountId }` | 强制拉取最新模型并覆盖 cache |
| `GET` | `/api/admin/accounts/connection-test` | `accountId`、`modelId` | 通过 SSE 返回实时连接测试事件，不作为业务 Responses 用量记录 |
| `POST` | `/api/admin/accounts/oauth/start` | `{ provider, name, accountId? }` | 创建 OpenAI 或 xAI OAuth flow；`accountId` 表示重新授权 |
| `POST` | `/api/admin/accounts/oauth/complete` | `{ provider, flowId, callbackUrl }` | 消费 OAuth callback 并写入账号 |

账号列表支持以下稳定值：

- `provider`: `all`、`openai`、`xai`；
- `status`: `active`、`expired`、`quota_exhausted`、`disabled`、`banned`；
- `sortBy`: `email`、`status`、`planType`、`usage`、`lastUsedAt`、`expiresAt`；
- `sortDirection`: `asc`、`desc`。

导入的 `data` 必须是 JSON object，内部 schema 由目标 Provider 独占解释：

- OpenAI 接受 OAuth 账号文档、账号 bundle 和 Agent Identity；
- xAI 从单账号 object 或 `accounts` 数组中提取 OAuth token；包装中的代理、并发、优先级等字段不参与认证；
- xAI 批量导入逐条独立校验：失败条目跳过并记录日志，不中断其余条目，仅当没有任何条目成功时整个导入才报错；
- xAI API Key 不是受支持的账号 credential；
- 导入仍会通过 Provider 的身份或刷新链路校验，不能只凭文件外形写入账号。

OpenAI 的 CPR 导出保持 OAuth 账号的既有字段；Agent Identity 账号则输出
`authMode: "agentIdentity"`、`agentRuntimeId`、`agentPrivateKey` 和可选的
`taskId`，不伪造 OAuth token 或过期时间。一个 `sourceFormat: "cpr"` 文档可以包含两种
认证类型，导入时按每个账号的认证模式分别解析。

```json
{
  "sourceFormat": "cpr",
  "accounts": [{
    "id": "acct_...",
    "accountId": "...",
    "userId": "...",
    "authMode": "agentIdentity",
    "agentRuntimeId": "...",
    "agentPrivateKey": "...",
    "taskId": "..."
  }]
}
```

OpenAI rotation 请求字段为：

```json
{
  "provider": "openai",
  "accountId": "acct_...",
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

- 导入会校验签名 access token 和稳定账号身份；`/wham/usage` 用于补全账号/套餐事实和额度，
  不是判断 token 是否有效的唯一依据。
- 账号文件导入和首次 OAuth 创建在 credential 提交后立即尝试一次额度观测。观测失败只记录告警，
  不回滚已提交的账号；重新授权只轮换目标账号 credential，不隐式等同于手工额度刷新。
- 重新授权在 start 时绑定目标账号，complete 时重新读取目标的当前 credential revision，并验证上游
  account/user 身份没有换绑。Free token 缺少 `chatgpt_account_id` 时，后端使用目标账号 header 调用
  `/wham/usage` 交叉确认，而不是把该账号推断成 K12。
- OAuth pending flow 先取得带过期时间的独占 claim，只有账号事务提交成功后才消费。失败会释放 claim，
  但上游 authorization code 本身通常只能交换一次；已完成过 token exchange 时应重新创建 OAuth flow。
- `GET /accounts/quota` 只读取最后一次落库快照；`POST /accounts/quota/refresh` 才访问上游。access token
  已过期时，额度刷新要求先走 credential 刷新或重新授权，不会拿过期 token 探测额度。
- 成功额度观测会 revision-fenced 写入 quota，并在 `active` 与 `quota_exhausted` 等额度所属状态间同步；
  不会清除 `expired`、`banned` 等 credential/封禁终态。额度接口的 401/403 也不足以判定 refresh token
  永久失效，credential 终态只由 OAuth refresh 或身份校验链路写入。
- 正常 Responses 请求会解析上游响应的 rate-limit headers，合并进同一 quota 快照并同步状态。Free、
  K12 等套餐共用该状态机；套餐只参与账号展示和按套餐隔离的模型目录 cache，不存在 K12 专属额度路径。
- 账号页没有定时静默轮询。手工额度刷新完成后页面会重新读取该账号；请求驱动或后台任务产生的状态
  变化，需要下一次显式查询账号列表后才会显示。

## 6. Client Key

| 方法 | 路由 | 主要 query/body | 说明 |
| --- | --- | --- | --- |
| `GET` | `/api/admin/client-keys` | `cursor`、`limit`、`search`、`sortBy`、`sortDirection` | 游标分页查询 |
| `POST` | `/api/admin/client-keys/create` | 创建字段 | 创建绑定 Provider 的 Client Key |
| `GET` | `/api/admin/client-keys/reveal` | `id` | 显式读取完整明文 Key |
| `POST` | `/api/admin/client-keys/update` | 更新字段 | 原子更新名称、Provider 和限额 |
| `POST` | `/api/admin/client-keys/enable` | `{ id }` | 启用 |
| `POST` | `/api/admin/client-keys/disable` | `{ id }` | 禁用 |
| `POST` | `/api/admin/client-keys/delete` | `{ id }` | 删除 |

创建字段为 `name`、可选 `label`、`providerKind`、`maxConcurrency`、`requestsPerMinute`。更新请求再增加 `id`。创建和 reveal 响应会返回完整明文 Key，调用方必须立即安全保存。

## 7. 运行设置

| 方法 | 路由 | 说明 |
| --- | --- | --- |
| `GET` | `/api/admin/settings` | 读取运行设置 |
| `POST` | `/api/admin/settings/update` | 原子替换全部运行设置 |
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
usageRetentionDays
opsEventRetentionDays
auditRetentionDays
```

`rotationStrategy` 可取 `smart`、`quota_reset_priority`、`round_robin`、`sticky`。

## 8. Dashboard、用量与错误

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

汇总与洞察中的请求数与 outcome 分布覆盖筛选范围内全部请求；token、缓存、延迟与成本聚合仅统计
已完整交付客户端的成功响应。

OpenAI 的 `serviceTier` 只接受上游响应生命周期事件确认的实际 `response.service_tier`；请求里的
期望档位只保留在 request summary，不能冒充响应事实。计费展示把 `priority`/`fast` 映射为 `Fast`，
`flex` 映射为 `Flex`，缺失或 `default` 映射为 `Default`；未知非空值原样展示。Fast 优先使用模型的
priority 价格，缺少专用价格时回退到标准价格的 `2.00x`；Flex 为 `0.50x`，Default 为 `1.00x`。

## 9. 版本、更新与重启

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
提供：跨大版本目标会以 `40901` 冲突拒绝，需按发布说明手动迁移。仓库发版流程见
[部署文档](../deploy/README.md) 与根目录 [README](../README.md)。
