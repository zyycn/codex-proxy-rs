# Codex Proxy RS 接口

本文描述当前源码的公开 HTTP 合同，路由以 `backend/crates/gateway-api/src` 中的 router 为准

配置客户端见[部署文档](../deploy/README.md#客户端配置)，内部职责见[系统架构](architecture.md)，部署实例的功能以实际运行版本和 revision 为准

| 查找内容 | 入口 |
| --- | --- |
| 鉴权与响应 | [公共约定](#1-鉴权与公共约定) · [浏览器会话与 Key 自助页](#4-浏览器认证) |
| 客户端协议 | [Responses、Chat、Images、模型与 Provider 操作](#3-openai-数据面与模型目录) · [额度查询](#api-key-额度查询) |
| 账号与授权范围 | [账号](#5-账号) · [代理](#独立代理管理--managed-proxies) · [分组](#6-账号分组) · [Client Key](#7-client-key) |
| 系统与运维 | [健康检查](#2-健康检查) · [运行设置](#8-运行设置) · [备份](#9-备份) · [统计与诊断](#10-dashboard用量与错误) · [版本与更新](#11-版本更新与重启) |
| 插件 | [安装包与来源](#安装包与来源) · [运行实例](#121-运行实例) · [管理扩展](#122-管理扩展页面与原始-api) |

## 1. 鉴权与公共约定

下文描述宿主路由的默认行为。已启用的 `http` 中间件在路由匹配、认证和正文解析前统一进入，
可改写请求、覆盖本次请求设置、直接返回响应或接管 WebSocket 升级；管理、模型、健康检查和静态资源使用同一入口。
挂载与覆盖合同见 [SDK 洋葱中间件](../backend/crates/gateway-plugin/sdk/docs/capabilities.md#洋葱中间件)

### OpenAI 数据面客户端接口

默认情况下，`/v1/*` 使用管理端创建的 Client Key：

```http
Authorization: Bearer sk_...
```

自动生成的 Key 保持 `sk_` 格式；迁入的自定义 Key 使用保存时的原值，不限制前缀或固定长度。
无论格式如何，只有已保存且启用的 Client Key 能通过鉴权

启用[入口认证插件](../backend/crates/gateway-plugin/sdk/docs/capabilities.md#数据面入口认证)时，
`Authorization` 可按插件协议校验，并由宿主将外部身份映射到明确绑定的 Client Key。
账号范围、模型权限、并发和预算仍由该 Key 决定；插件认证不接管管理员或 Key 自助页登录

Codex 原生生图配置还会携带 `X-OpenAI-Actor-Authorization: proxy-managed`。
它仅用于客户端识别服务端托管认证，不能代替 Client Key。网关和 OpenAI Provider 都会过滤该请求头，
上游账号身份只由服务端选中的账号提供；不要把真实账号 token 放进该标记

Client Key 通过账号分组限定路由范围：未绑定分组时可使用全部账号，绑定一个或多个分组时只能使用
已启用分组成员的并集。分组可以混合 `openai` 与 `xai` 账号；同一请求只会在模型能力明确匹配且满足
重放安全边界时跨 Provider fallback

运行设置可以分别配置 `minCodexDesktopVersion` 与 `minCodexCliVersion`。两者只接受 SemVer，`null`
表示不限制，比较版本时忽略 `+` 后的构建元数据。API 在 Client Key 鉴权成功后识别官方客户端请求头；
适用门禁的客户端没有合法版本，或版本低于对应门槛时，除只读 `/v1/usage` 外的 `/v1/*` HTTP 请求和新 WebSocket
握手在访问上游前返回 `400 Bad Request`，按不可重试的请求错误处理。
未知客户端保持兼容，不应用版本门禁

`minCodexCliVersion` 检查官方客户端 User-Agent 产品段中的 Core 版本，支持
`codex_cli_rs`、`codex-cli`、`codex-tui`、`codex_exec`、`codex_vscode`、`codex_sdk_ts`、
`codex_python_sdk`、`codex-app-server`、`codex-app-server-sdk` 与 `codex_mcp_server`
名称按完整标识匹配且不区分大小写，括号后缀中的应用或 SDK 版本不能替代 Core 版本

Desktop 应用版本优先取 `version` 头，未提供时取 User-Agent 中的 `(Codex Desktop; <版本>)`。
ChatGPT 远程控制使用 `(codex_chatgpt_<平台>_remote; <版本>)` 形式的 User-Agent 后缀，已知平台包括
`android` 和 `ios`。网关按该命名格式识别非空的平台名，后缀须完整，平台名和版本均不能含空白、括号或分号。
这类 Desktop 请求未提供应用版本时不应用版本门禁；Core 版本和
远程客户端版本不能替代 Desktop 应用版本，因此也无法保证其满足 Desktop 最低版本要求。
携带 `version` 头或 Desktop 应用版本后缀时仍按上述规则校验，非法版本不会因远程标记而放行

低版本响应使用 OpenAI 风格错误格式：

```json
{
  "error": {
    "message": "Codex CLI 0.151.0 is outdated. Upgrade to 0.152.0 or later.",
    "type": "invalid_request_error",
    "code": "client_version_too_old",
    "client": "codex_cli",
    "current_version": "0.151.0",
    "min_version": "0.152.0"
  }
}
```

因缺失或非法版本被门禁拒绝时，`code` 为 `client_version_unavailable`，`current_version` 为 `null`，
提示检查客户端版本及反向代理是否保留 `User-Agent` 和 `version` 头

### 管理接口

所有 `/api/admin/*` 请求都需要以下任一鉴权方式：

- 管理员登录后得到的 `cpr_session` Cookie（服务端会话身份必须为 `admin`）
- `x-api-key: <admin-api-key>`

同源管理端的登录和退出根据浏览器 `Origin` 自动设置会话 Cookie：HTTP 来源省略 `Secure`，
HTTPS 来源以及缺失、`null` 或非法来源保留 `Secure`。`HttpOnly`、`SameSite=Lax` 始终保留。
不根据 `X-Forwarded-Proto` 等转发头降级；HTTPS 反代使用 HTTP 回源不影响 Cookie。
部署要求见 [公网访问](../deploy/README.md#公网访问)

请求无需自带 `x-request-id`；缺失时服务端自动生成 UUID 并在响应头回传同一 request ID。
`api.request_id_header` 可改变注入与回传的 header 名，管理端鉴权不依赖该名字。
管理端 JSON 与动态响应统一带 `Cache-Control: no-store`；插件静态资源按下述资源接口的私有缓存策略返回

管理端长度已知且至少 1 KiB 的 `application/json` 响应按 `Accept-Encoding` 协商 gzip，
压缩时返回 `Content-Encoding: gzip`，可协商的响应带 `Vary: accept-encoding`。
小响应、SSE、其他流式正文及已有内容编码的响应不再压缩；模型执行接口不经过管理端压缩层

配置了 CORS 白名单 origin 时，跨域请求以凭据模式放行，仅允许 `GET`/`POST` 方法和
`authorization`、`content-type`、`x-api-key` 与 request ID 四个请求头，不使用通配符

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
`40001`，并保留标准 `Allow` header。request ID 继续通过配置的响应 header 返回

`50201`、`50202` 和 `50301` 的 `message` 提供可安全展示的具体原因；缺少安全消息时使用通用提示。
认证错误和未知内部错误使用固定文案，不公开 Provider 内部 message、原始响应或凭据

手动刷新令牌时，按错误类型处理：

| `code` | 场景 | 调用方处理 |
| --- | --- | --- |
| `40901` | 刷新容量、账号租约占用或账号快照冲突 | 按提示等待，或重新查询账号后操作 |
| `40001` | 缺少或无效的凭据、已确认刷新令牌失效或账号停用 | 检查凭据或重新授权 |
| `50201` | 明确的上游失败，且未归类为凭据永久失效 | 根据安全消息处理上游拒绝、限流或服务异常 |
| `50202` | 无法确认令牌轮换结果，包括 xAI 返回无效的新凭据 | 先核对账号状态，不要自动重发可能已轮换的一次性令牌 |
| `50301` | 刷新服务或依赖暂不可用 | 检查出站连接与依赖服务后重试 |

OpenAI 上游 `401` 按 `50201` 返回，不代表管理员会话失效，也不要求管理端重新登录。
Codex PAT 验证服务不可用和身份响应无效分别返回 `50301`、`50201` 及对应的安全提示

### 页面时间合同

管理端与 Key 用量接口保留原始 RFC3339 时间点，并提供 `createdAtDisplay`、`updatedAtDisplay`、
`expiresAtDisplay` 等展示字段。展示文本由后端按部署 `host.timezone` 生成，完整时间通常为
`YYYY-MM-DD HH:mm:ss`，短时间、相对时间和图表标签按对应视图返回；缺失时间的展示字段为 `null` 或占位文本。
调用者直接显示文本，使用原始时间点进行排序、比较和输入，不把展示文本转回请求时间

账号、模型用量和 Key 列表的最近使用时间由前端根据原始 RFC3339 时间点，通过 VueUse `useTimeAgoIntl` 按界面语言显示短相对时间，并随时间更新；悬停的完整日期仍使用后端 `lastUsedAtFullDisplay`，不使用浏览器本地时区重算

Dashboard 的 `lastUsedRelative` 和账号额度的 `refreshedAtRelative` 由后端确定时间语义，
分别以查询锚点和服务端当前时间计算，随接口刷新更新。前端只翻译分类与单位，不使用浏览器时钟或时区重算

- 缺失时间返回 `null`，前端显示占位文本
- `kind=just_now` 表示不足 60 秒
- `kind=elapsed` 包含整数 `value` 和 `unit=minute|hour|day`，分钟不足 60、小时不足 24，之后始终用天；数值向下取整
- `kind=absolute` 的 `value` 是后端按部署 `host.timezone` 生成的完整日期文本，用于按整秒判断的未来时间点

原始 RFC3339 `lastUsedAt`、`quota.refreshedAt` 与兼容展示字段保持可用，不用于前端重新决定上述展示方式

账号冷却返回 RFC3339 `rateLimitedUntil` 和 `rateLimitRecoveryDisplay`，后者包含剩余时长、
到期时间或等待恢复探测的提示，随账号数据刷新；前端不解析展示文本或自行计算恢复提示

`period=today|7d|30d` 表达自然日范围，`asOf` 是 Unix 毫秒查询锚点，省略时取服务端当前时刻。
近 7 天包含锚点所在日期及前 6 个自然日，近 30 天同理包含前 29 日；结束点为锚点。
同一组汇总、趋势与列表共享 `asOf`，分页保持锚点，刷新再更新。显式 `startTime` / `endTime`
使用带偏移的 RFC3339，不能与 `period` 或 `asOf` 混用；对应接口仍执行范围上限校验。
自然日刚开始时允许空的今日快照，显式起止范围仍要求开始早于结束

图表的 `label`、日期提示和空桶由后端提供；日粒度按本地日界，小时及 15 分钟桶以 UTC 时间点定位。
账号请求柱覆盖截至锚点的最近 24 个 UTC 小时桶，包含当前未完整小时，独立于今日汇总的自然日范围。
健康时间线覆盖锚点所在自然日，每 15 分钟一个桶，夏令时日期可以为 92 或 100 个桶。
图表时间标签不显示时区偏移，重复本地时刻可以使用相同展示文本；唯一键、排序和去重使用原始时间点。
只有日历日期的上游统计保留日期语义，不当作 UTC 午夜换算

### 管理写入一致性

会改变路由快照或安全配置的写入由后端在事务内推进 `config_revision`，用于快照发布与审计。
账号、分组等响应中的 `configRevision` 表示已提交事实，不是这些接口的写入前置条件。
[运行设置整体更新](#8-运行设置)必须提交读取时的 `configRevision`，过期返回 `409`；
代理的 `revision`、插件实例的 `expectedRevision` 属于各自资源，按对应接口提交，不与全局版本混用

## 2. 健康检查

| 方法 | 路由 | 鉴权 | 说明 |
| --- | --- | --- | --- |
| `GET` | `/healthz` | 无 | Core、Store 和后台任务健康时返回 `204`，否则返回 `503` |

## 3. OpenAI 数据面与模型目录

除下述入站解压保护外，Responses、Chat Completions、Images 和 standalone Search HTTP body、
WebSocket message 和 frame 不设置网关私有长度上限；请求字段与协议限制见各入口说明

| 方法 | 路由 | 说明 |
| --- | --- | --- |
| `POST` | `/v1/responses` | OpenAI Responses JSON；`stream=true` 返回 SSE，否则返回完整 JSON |
| `GET` | `/v1/responses` | 通过 HTTP Upgrade 建立 Responses WebSocket |
| `POST` | `/v1/chat/completions` | Chat Completions 转 Responses 执行；返回单 choice 的 Chat JSON 或 SSE |
| `POST` | `/v1/alpha/search` | Codex standalone web search；JSON 请求与响应正文原样转发 |
| `POST` | `/v1/images/generations` | 通过 OpenAI Provider 发起图像生成；JSON 请求与响应正文原样转发 |
| `POST` | `/v1/images/edits` | 通过 OpenAI Provider 发起图像编辑；JSON 原样转发，multipart 上传转为原生 Images JSON |
| `GET` | `/v1/models` | 返回当前 Client Key 账号范围内各 Provider 的可用公开模型并集；有两种响应形态，见下 |
| `GET` | `/v1/models/{model_id}` | 返回 OpenAI 兼容的单模型详情 |
| `POST` | `/v1/providers/{provider}/models/{model}/count_tokens` | 调用指定 Provider/model 声明的精确 Token 计数；请求和响应均为 JSON |
| `GET` / `POST` | `/v1/providers/{provider}/http/{endpoint}` | 调用指定 Provider 预先声明的固定 HTTP 操作；不是任意 URL 代理 |
| `GET` | `/v1/usage` | 查询当前 Client Key 的日与周额度，仅使用网关已结算的 USD 账本 |

### Chat Completions 请求

`POST /v1/chat/completions` 接受 Chat 消息历史，转换为 Responses 请求执行，再交付 Chat 格式结果。
每次请求须提供非空字符串 `model` 和非空数组 `messages`，多轮对话由客户端携带完整历史；
没有 Chat 查询、删除或服务端历史续接接口。`stream` 省略、`null` 或 `false` 时返回 JSON，`true` 时返回 SSE

官方 OpenAI SDK 可使用 Chat Completions 方法，将 `base_url` 指向网关的 `/v1`。
下面的 `CPR_CLIENT_KEY` 为管理端创建的 Client Key，`CPR_MODEL` 为当前 Key 可用的模型 ID，可通过 `/v1/models` 查询：

```python
import os
from openai import OpenAI

client = OpenAI(
    base_url="https://gateway.example.com/v1",
    api_key=os.environ["CPR_CLIENT_KEY"],
)
completion = client.chat.completions.create(
    model=os.environ["CPR_MODEL"],
    messages=[{"role": "user", "content": "你好"}],
)
print(completion.choices[0].message.content)
```

#### 请求字段

| 字段 | 支持与映射 |
| --- | --- |
| `messages[].role` | `system`、`developer`、`user`、`assistant`、`tool`，保持消息顺序；assistant 文本历史使用 Responses 的 `output_text` |
| `messages[].content` | 字符串或文本块数组；`user` 还可包含 `image_url`，支持 HTTP(S) URL 和非空 `data:image/...;base64,...`，保留 `detail`，不代取图片 |
| `messages[].refusal`、assistant 的 `refusal` 内容块 | 保留拒绝历史；assistant 的 `content` 仅在同时有工具调用或拒绝内容时可省略或为 `null` |
| `messages[].tool_calls`、`messages[].tool_call_id` | assistant 的 function 调用转为 `function_call`，tool 结果转为 `function_call_output`，保留调用 ID 与 arguments 字符串，不解析或执行函数 |
| `tools` | 仅支持现代 `type: "function"`；将 `function` 定义展开为 Responses 工具，保留 parameters schema；`strict` 省略或 `null` 时显式使用 `false` |
| `tool_choice`、`parallel_tool_calls` | 支持 `auto`、`none`、`required` 和指定 function；`required` 须提供工具，指定名称须存在于 `tools`；并行选项保留布尔值 |
| `response_format` | `text`、`json_object`、`json_schema` 映射到 `text.format`，JSON Schema 保留原值 |
| `reasoning_effort`、`verbosity` | 分别映射到 `reasoning.effort`、`text.verbosity`；接受非空字符串，不固定枚举档位 |
| `max_completion_tokens`、`max_tokens` | 正整数映射到 `max_output_tokens`，同时提供时前者优先 |
| `temperature`、`top_p` | 分别接受 0 到 2、0 到 1 的数字，包含端点；是否生效见下文 |
| `user`、`safety_identifier`、`service_tier`、`prompt_cache_key`、`prompt_cache_retention` | 两侧同语义的字符串保留原值，档位与缓存策略由上游判断 |
| `metadata`、`prompt_cache_options`、`moderation` | 两侧同结构的对象原样携带，不逐层重建内部字段 |
| 文本、图片内容块的 `prompt_cache_breakpoint` | 同结构对象原样携带；assistant 输出消息不支持此选项 |

可选映射参数为 `null` 时按未提供处理。字段类型、角色组合或工具选择无效时，在访问上游前返回 `400`，
错误中的 `param` 指向对应字段，不回显字段值。工具参数字符串和 schema 的业务有效性由客户端与上游按各自合同处理

以下参数只接受所列中性值；省略或 `null` 也接受，其他值返回 `400 unsupported_parameter`：

| 字段 | 接受值 |
| --- | --- |
| `n` | `1` |
| `store`、`logprobs` | `false` |
| `top_logprobs`、`frequency_penalty`、`presence_penalty` | `0` |
| `logit_bias` | `{}` |
| `stop`、旧式 `functions` | `[]` |
| `modalities` | `["text"]` |

`function_call`、`seed`、`prediction`、`audio`、`web_search_options`、`reasoning_content` 及消息级 `reasoning` 不支持非 `null` 值。
非空消息 `name`、旧式 `function` 角色、文件内容、音频内容、custom 或 hosted 工具及未知内容类型也会明确拒绝。
该入口不接收 Responses 请求形状；`input`、`previous_response_id`、`conversation`、`instructions`、`reasoning`、
`text`、`include`、`background` 和 `max_output_tokens` 不支持非 `null` 值，需要这些原生能力时使用 `/v1/responses`

未知客户端附加字段只在请求、消息及工具等协议包装对象中忽略，不据名称自动转交 Responses，
也不递归删除 schema、metadata 或工具参数内的业务字段。新枚举值和同结构对象内部的扩展交由上游判断；
新的工具类型、输出模态或续接语义需要明确映射，不承诺跨协议完全透传

#### 响应与共用规则

非流式响应包含一个 `choices` 项，支持文本、拒绝和 function 工具调用；流式响应交付对应的 Chat delta。
两种响应的 `id` 均由网关生成，`model` 保留客户端请求的模型 ID；流内各块使用同一身份。
正常完成的 `finish_reason` 为 `stop`，包含工具调用时为 `tool_calls`；上游明确因输出长度或内容过滤而未完成时，
分别为 `length` 或 `content_filter`

`stream_options` 仅在 `stream: true` 时可提供，
`include_usage: true` 在完整上游用量可用时于结束前追加 `choices: []` 的用量块，其余块的 `usage` 为 `null`。
未提供完整用量时不补造 Token 数；缺省不启用流长度混淆，`include_obfuscation: true` 不受支持，`false` 可接受

响应对象的额外普通属性、非必要事件不影响已有内容映射；无法表达的业务输出、缺少必要终态或终态冲突会失败，
不补造成功终态。SSE 已开始后通过 `data: {"error": ...}` 交付错误，再以 `[DONE]` 结束，
因此 `[DONE]` 仅表示流结束，不能单独作为成功依据。
OpenAI 是此入口的兼容目标，混合分组沿用账号范围内的选路与 fallback 规则，
不能据此推导所有 Provider 都具备相同的 Chat 能力

Responses 的 reasoning 项（包括摘要与 `encrypted_content`）不投影到 Chat 消息，
实际推理用量仍保留。需要推理密文回放或图片输出时使用原生 Responses 接口；Chat 的图片输入支持不代表图片输出可用

Chat 沿用 [Client Key 鉴权](#openai-数据面客户端接口)、[请求期限](#请求期限)、[请求解压](#请求解压)、
[请求头与身份](#请求头与身份)及[隐私策略](#隐私策略)。隐私策略的 `request_body` 面向转换后的 Responses 正文，
例如 `$.input`，不需要另外维护一套针对 `messages` 的规则

转换后的请求按选中 Provider 的正文规则处理。OpenAI 的[正文兼容](#正文兼容)会移除顶层 `temperature`、
`max_output_tokens` 和 `prompt_cache_retention`。因此接受相应 Chat 参数不表示采样、输出长度或缓存保留设置必然生效，
不会通过本地截断模拟长度控制；`top_p` 等其他参数继续由上游判断

### Images 请求

Images 使用原生非流式 JSON 端点，JSON 请求与响应正文原样转发。
`/v1/images/edits` 还接受 `multipart/form-data`，将上传转换为原生 Images JSON 请求

multipart 的 `image` 和 `image[]` 上传按出现顺序组成 `images` 数组，每项为
`{"image_url":"data:image/...;base64,..."}`；`mask` 上传转换为同结构的单个对象。
图片支持 PNG、JPEG、WebP，根据内容识别格式，原始文件字节直接编码，不缩放或重新编码

| 表单字段 | JSON 类型 |
| --- | --- |
| `n`、`output_compression`、`partial_images` | JSON 数字，不经浮点数中转 |
| `stream` | JSON 布尔值 `true` 或 `false`；转换不增加流式响应支持 |
| 其余文本字段，包括未知字段 | 字符串，保留字段名、空白与原值 |

未提供的参数保持缺省；除 `image` / `image[]` 聚合外，其余同名字段按出现顺序保留为重复 JSON 键，
解释与校验交给上游。
缺少图片、损坏的 multipart、无效的 UTF-8 或字段类型返回 `400`。
需要为扩展字段指定其他 JSON 类型，或传入 `file_id` 等图片引用时，使用 JSON 请求

### Provider 自有操作

这两类 `/v1/providers/*` 路由使用同一 Client Key 鉴权、账号组范围、Provider 账号资格、租约、
并发/频率准入、请求记录和发送事实。`provider`、`model` 与 `endpoint` 都是已发布目录中的稳定 ID；
宿主不会接受客户端提供的上游 URL，也不会把下游 Authorization、Cookie、API Key、Host、转发头或
`Connection` 声明的逐跳头交给目标 Provider。原生执行的上游账号认证来自宿主选中的账号

`POST /v1/providers/{provider}/models/{model}/count_tokens` 接受最多 8 MiB 的合法 JSON，且只路由到
为该模型明确声明 `count_tokens` 的 Provider。Core 不解释或重写请求/响应 JSON，响应必须是目标 Provider 按其
声明的本地精确 tokenizer 或上游精确计数合同返回的原始 JSON；没有准确实现时返回不支持，不从 usage
或字符数推算。响应固定使用 `application/json` 并重算 `Content-Length`

`GET|POST /v1/providers/{provider}/http/{endpoint}` 只允许 Provider descriptor 中该 endpoint 声明的
方法；`HEAD`、其他方法、未声明 endpoint 和未声明的方法在出站前拒绝。GET 不能携带正文，POST 正文
最多 8 MiB；query 作为不透明值交给目标 Provider 再按其固定合同校验，不能改变上游 origin 或路径。
成功响应由最多 64 KiB 的有序原始片段组成，聚合后最多 8 MiB，字节不经 JSON 转码。宿主保留允许的
`Content-Type` 和安全业务响应头，移除认证、`Set-Cookie`、`Location`、逐跳、压缩及 framing 头，
并按最终正文重算 `Content-Length`；缺少媒体类型时使用 `application/octet-stream`。Provider 的结构化
失败仍沿用数据面错误映射和已确认的上游状态/可见正文边界，不把非 2xx 成功封套当作成功

Codex 的 review 等子代理请求仍使用 `/v1/responses`，并通过 `x-openai-subagent` 请求头携带子代理类型；
网关不提供独立的子代理请求路径

### Responses 请求与传输

#### 请求期限

模型请求默认不限制总执行时长。排队受[并发等待设置](#8-运行设置)约束，上游传输仍有独立的连接、空闲与心跳超时；
持续有输出的请求不会仅因总时长达到固定值而结束。插件可以对单次模型执行设置显式总时限，
从该次请求开始计算，见 [SDK 请求设置](../backend/crates/gateway-plugin/sdk/docs/capabilities.md#模型请求与-attempt)

`api.request_timeout_seconds` 控制 HTTP 路由处理到返回响应的等待，默认 `null`；不限制已返回响应的流式正文寿命，
也不是 WebSocket 每轮模型执行的总时限。客户端断开或取消仍会结束所属执行，反向代理和客户端可另设超时

#### 请求解压

`POST /v1/responses` 与 `POST /v1/chat/completions` 在鉴权后按 `Content-Encoding` 解压，再解析 JSON；支持单一 `gzip`、
`deflate`（zlib 封装）和 `zstd`，缺省、空值或 `identity` 直接使用原始正文。gzip 多成员与 zstd
多帧连续解码，整体展开结果受运行设置 `responsesMaxDecompressedBodyBytes` 约束（默认 64 MiB），
超限在继续展开前返回 `400 request_too_large`，错误信息包含当前请求的上限字节数。zstd
回溯窗口独立固定为最多 64 MiB，不能满足该限制的帧按解码失败处理。这个限制保护入站解压资源，不是
模型上下文或 Token 上限；未压缩正文不经过此解压检查。插件受管出站仍受同一设置约束，
见[请求解压上限](#请求解压上限)。
不支持的编码、逗号分隔的叠加编码和重复 `Content-Encoding` 头返回
`400 unsupported_content_encoding`；压缩正文损坏、截断或解压后不是合法 JSON 返回
`400 invalid_json`。本地错误不包含原始正文或解压库细节。WebSocket 文本帧不经过这条解压路径

#### 请求头与身份

Responses 不透传下游的逐跳头、反代元数据（如 `cf-*`、`x-forwarded-*`、`forwarded`、`via`、
`cdn-loop`）以及 `Accept-Encoding` / `Content-Encoding`。链路元数据和编解码能力
由各段传输层独立管理；其余业务扩展头继续透传，不使用固定业务头白名单。
此规则同时适用于上游 HTTP 和 WebSocket，不影响上游响应的 `cf-ray` 等诊断信息。
API Key 与 OAuth 共用模拟客户端画像（`User-Agent`、`originator`、`version`）和业务头透传规则，
包括会话、线程、Lite 和其他业务扩展头。
上游认证只来自选中的账号；API Key 不携带 OAuth Cookie、ChatGPT 账号身份或下游的
`X-OpenAI-Actor-Authorization` 托管认证声明。
下游的 `x-openai-internal-codex-residency`、`x-openai-account-routing-override`、`x-openai-fedramp`
也不透传，地域、工作区路由与合规属性不能从原账号继承。
前两项仅按部署配置 `openai.residency`、`openai.account_routing_override` 的字符串值生成，
省略或 `null` 时不发送，配置说明见 [部署与运维](../deploy/README.md#启动后的运行设置)
上述过滤描述原生转发；已信任插件的显式 header 改写可以覆盖账号与画像生成的默认头，
不按认证或会话字段名称拦截，边界见 [SDK 请求中间件](../backend/crates/gateway-plugin/sdk/docs/capabilities.md#模型请求与-attempt)

Responses 也不透传 `x-stainless-*`、`Origin`、`Referer`、`sec-ch-ua*` 和 `sec-fetch-*`
携带的下游 SDK/浏览器环境或页面来源。过滤规则适用于所有下游客户端，与 User-Agent 无关；
原始入站头仍供 CORS、鉴权和本地观测使用。
`session_id` 请求头仅作为入站会话别名，上游通过 `session-id` 表达；
两者同时存在时仍优先使用 `session-id`。正文中的 `client_metadata.session_id`、
`prompt_cache_key` 不受这条请求头规则影响。
`thread-id`、turn metadata 等 Codex 协议字段及未知业务扩展不受下游环境头过滤规则影响；
出站移除下游的 `traceparent`、`tracestate` 及 WS metadata 中对应的
`ws_request_header_traceparent`、`ws_request_header_tracestate`

会话、线程、窗口及父子线程引用在出站时按本次安装身份稳定映射为 UUID；
默认使用账号安装身份，同账号保留引用关系，换账号使用不同标识。HTTP 请求头、正文 metadata、WS 副本及
已识别的 Desktop 自动 Git 上下文使用一致映射。
轮次及父子轮次引用保留客户端原值，包括输入项的 `internal_chat_message_metadata_passthrough.turn_id`、
请求 metadata 中的 `turn_id`、`parent_turn_id`、`root_turn_id`，以及 `x-client-turn-id`、
Images 的 `x-codex-image-turn-id` 和 Desktop 自动 Git 上下文的 `clientUserMessageId`。
输入项元数据保持不变。Search 的会话 `id` 按同一安装身份域映射，只替换该值的原始字节，其余正文保持不变。
`prompt_cache_key` 同样按安装身份映射，保留同一身份内的缓存共享关系；未提供时不补造。
受信任插件可读取原始请求，自行生成身份并在 `outbound` 改写完整请求与关联字段；宿主不重新派生或补回业务字段，
实际选号与计费归属保持不变，详见 [SDK 出站请求准备](../backend/crates/gateway-plugin/sdk/docs/capabilities.md#上游出站准备)。
续接 response ID、历史 item/call ID、业务正文和未知扩展不参与映射，
该处理减少显式追踪标识的跨账号复用，不保证会话内容或业务 metadata 无法关联

#### 正文兼容

OpenAI Provider 的 Responses 上游编码会移除 Codex 不接受的顶层 `temperature`、`max_output_tokens` 和
`prompt_cache_retention`。缺少顶层 `store` 时补齐 `false`，与官方 Codex 客户端一致；
显式提供的值保持原样。顶层 `input` 为字符串时按公开 Responses API 的语义展开为一条
`user` 文本消息条目（`{"type": "message", "role": "user", "content": [{"type": "input_text", ...}]}`），
因为 Codex 后端只接受条目数组；数组及其他类型原样透传。`input` 数组中显式指定 `type: "message"`
且 `role: "system"` 的消息，其角色转换为 Codex 接受的 `developer`；消息内容、顺序、其他字段和顶层
`instructions` 保持不变。HTTP/SSE 与 WebSocket 共用这条正文兼容规则。
`prompt_cache_key`、`reasoning`、`include` 等 Codex 参数继续保留。上述参数过滤只作用于顶层，
不删除工具参数 schema、输入内容或 `client_metadata` 内的同名业务字段；其他未知字段继续透传。
`prompt_cache_key` 不用于补造会话或线程请求头；客户端未提供缓存键、会话或线程身份时保持缺省

Codex/OAuth 上游的历史回填按字段形状兼容，不以 User-Agent 品牌区分：显式 `type: "reasoning"`
的 `input` 项移除顶层 `status`；该项具有非空字符串 `encrypted_content` 时，还会移除非空数组
`content`。其他字段及顺序保持不变，缺少非空加密载荷的明文历史由上游判定。
普通消息、工具项及未知类型不受此规则影响，API Key 上游不应用此规则

请求头过滤不提供客户端匿名化；系统提示词、工具定义、工具结果、工作目录及其他业务 metadata
保持原有语义，可能包含客户端环境信息。
环境正文中的工作区路径、shell 和文件权限信息不会因位置覆盖而隐藏。
turn metadata 的 `workspaces` 也会保留仓库绝对路径、Git 远端地址、提交及工作区变更状态；
安装身份和账号绑定字段的处理不提供这些信息的脱敏

`client_metadata.parent_response_id` 是 Guardian 的账号内响应引用，只有归属可信且仍为同一账号时保留；
切号或归属未知时移除。`x-codex-guardian`、`guardian_credits_requested` 和序列化
`x-codex-turn-metadata` 内普通扩展的同名 `parent_response_id` 保持原样

#### WebSocket 与上游传输

公开 HTTP 请求在路由匹配前经过已绑定的 `http` 中间件；插件可以改写握手路径和请求参数。
已升级的 Responses 连接在入站解析前和出站写入前经过 `websocket` 中间件，完整消息与主动发送接口见
[SDK 洋葱中间件](../backend/crates/gateway-plugin/sdk/docs/capabilities.md#洋葱中间件)。下述规则描述默认协议处理

Responses WebSocket 接受文本消息，`response.create` 在同一连接串行执行。当前响应期间收到的后续创建请求
留在有界接收队列中，待当前响应完成终结和写出后再逐条校验、准入与执行，不因请求提前到达而断开。
接收队列容量为 32 个事件，超载仍关闭连接；Ping/Pong、客户端关闭和服务关闭不等待队列中的请求执行

网关分别维护两段链路的心跳：上游 WS 被动回复 Pong，下游 WS 独立探活；Ping/Pong 不跨段转发，也不转换为 SSE 事件。
下游探活包含消息接收中的字节进度，完整消息收齐后才进入中间件与请求处理。
客户端断开时取消当前执行，并释放仍归该客户端所有的上游连接。`response.completed` 只结束本轮响应，保留客户端 WS 等待下一轮请求

| 入站 → 上游 | 正常回答结束后的连接处理 |
| --- | --- |
| SSE → SSE | 结束本次响应流，底层 HTTP 连接由 HTTP 连接池管理 |
| SSE → WS | 结束 SSE，上游 WS 归池，按无客户端绑定的空闲期限回收 |
| WS → SSE | 上游本次响应结束，下游 WS 保留 |
| WS → WS | 保留当前绑定的上游 WS，支持下一轮续接 |

活跃客户端 WS 的绑定不受普通空闲回收期限约束；上游关闭、总寿命到期或连接被回收仍可能使连接内续接失效，
客户端按既有 `previous_response_not_found` 合同完整重放。具体心跳和回收参数见[部署与运维](../deploy/README.md#公网访问)

其余文本消息发送到当前执行绑定的上游 WebSocket，未知类型、模式和扩展字段由上游校验。
`response.interrupt` 的 `discard_partial_items` 模式若指向当前逻辑请求已交付的响应 ID，
网关会保留中断意图；重试交付新的 `response.created` 后，以新 ID 转发中断，避免换号空档丢失控制消息。
跨账号缺少原连接上下文时，仍通过恢复错误要求客户端完整重放

控制帧不重新选号、不创建推理 attempt。响应结束后原连接仍可接收控制帧并返回上游事件；
下一轮执行使用新绑定的控制入口。没有绑定上游控制通道时（包括实际走 HTTP），有效的
`response.interrupt` / `discard_partial_items` 请求返回带原 `response_id` 的
`response.interrupt.failed`，当前模型响应继续交付真实事件与终态，客户端可收完后再续接。
该事件只表示网关无法转发中断，不表示上游已停止生成或丢弃部分输出。
未知控制消息、未知模式或无效响应 ID 字段在无控制通道时仍返回 `400`。
除上述重试期间保留中断的情况外，已绑定的原连接失效或发送失败仍返回 `400`，不能通过控制帧新建上游连接

中断后继续转发上游事件与终态；只有 `response.incomplete` 的 `incomplete_details.reason` 为
`interrupted` 时，才按官方中断语义保留原连接续接能力。部分输出是否被丢弃以上游终态为准。
控制帧发送成功不等于上游已确认中断；若上游仍返回 `response.completed`，按正常完成处理

OAuth 账号默认 `prefer_websocket`，客户端使用 HTTP/SSE 时仍可能选择上游 WebSocket；
可将账号上游传输方式设为 `http`，固定使用 HTTP/SSE。API Key 账号默认使用 HTTP/SSE，
也可配置 `prefer_websocket`。必须依赖 WS 的协议预热、非持久化新链和连接内续接不使用 HTTP-only 账号。
客户端配置的 `supports_websockets` 只控制第一段连接，不是服务端传输策略开关。
上游在响应终态前发送 Close 1000 仍属于失败，不能按“正常关闭”计为成功

内建 xAI 适配器不支持 `generate=false` 的非生成预热请求，在发送推理请求前返回
`400 unsupported_prewarm`，不会将其当作普通生成调用。`generate=true` 或省略该字段正常执行；
客户端可跳过预热继续正式请求，外部插件 adapter 按各自的协议能力处理

Codex OAuth backend 的候选上游为 WS 时，无 `previous_response_id` 的普通新链若规范化
`response.create` 达到 15 MiB，发送前选择 HTTP/SSE；HTTP 和 WS 入站均适用，下游交付协议不变。
该阈值为已观察到的上游消息大小边界预留余量，不是网关输入长度上限或 OpenAI 公布的统一限制；
API Key 账号、小请求、显式 warmup、未知外部 previous ID 和持久化续接不因该大小策略改用 HTTP

OAuth 的大 connection-local WS 续接，以及 HTTP `store=false` 成功后标记为 `ReplayRequired`
的 WS 增量，均在发送前返回 `status: 400 / previous_response_not_found`。客户端应清除旧 ID，
携带完整历史及工具调用／输出重试。官方 Codex 支持重连 WS 后完整重发，也可能按自身重试预算
切到 HTTP；其他客户端需要自行实现此合同。代理不缓存 transcript，不把增量输入当作独立新链，
连接内续接失去原连接时仍需客户端恢复；HTTP 链后续步骤可能增加一次恢复信号、重连及全量上传

已建立模型执行的 Responses、Images 和 Search HTTP 响应按以下规则返回关联 ID：
`x-gateway-request-id` 为模型执行 ID；`x-request-id` 保留有效上游值，只有上游
`x-oai-request-id` 时复用其值，没有上游 ID 时使用模型执行 ID。`x-oai-request-id` 不是必需字段，
也不要求客户端识别它；OpenAI 与 xAI 路由使用相同规则。失败响应的关联 ID 不采用会话 opening ID，
错误正文读取失败时仍返回已知上游 ID；已采集的 turn state 等允许的会话头继续按原合同交付。
尚未建立执行的入口拒绝继续使用 middleware 的入口关联

WebSocket 在尚未交付上游业务事件时合成的错误，除下述容量恢复合同外，保留已确认的失败状态，
以及 Provider 提取的结构化 message/type/code；没有结构化错误时使用稳定安全文案，不把原始
HTML 或截断正文当作 message。
合成错误自身的 `headers` 携带允许下发的响应头：优先保留实际失败的上游 request ID，无上游 ID 时
提供网关关联 ID，并用 `x-gateway-request-id` 独立标识网关请求。除下述客户端错误兼容与原生续写额度恢复外，
已经取得的原始上游错误帧不重写

### 模型目录

`GET /v1/models` 默认返回 OpenAI 兼容列表 `{"object": "list", "data": [...]}`；请求携带非空
`client_version` query 参数（Codex 客户端）时改为返回 Codex 专用目录合同 `{"models": [...]}`

OpenAI Provider 按客户端传入的 `client_version` 请求上游目录，完整保留每个模型 JSON 对象，包括
`base_instructions`、`model_messages`、`service_tiers`、工具与能力字段，以及未知嵌套字段、显式 `null`
和字段缺失的区别。
API Key 上游返回完整 Codex `models` 目录时沿用该合同；仅返回普通 `data` 模型列表时使用通用画像，
未提供的推理能力保持未知，不补充推理档位。
模型别名仅替换 `slug`，不替换上游展示名、提示词、能力或 `priority`；保持原生模型顺序，新增别名附在后面。
目录按当前路由快照的模型存在性及账号模型政策过滤，避免公布已知无法路由的模型；新模型需待后台目录对账后进入列表。
xAI 没有 Codex 原生目录，使用官方 CLI proxy 当前目录生成通用画像。推理菜单读取
`reasoning_efforts` 对象列表，默认值只取菜单中的显式标记；缺失时保持未知

已知模型只路由到明确提供该模型的 Provider，账号不可用时不转发给其他平台的未知模型通道。
未知模型只在当前范围与路由约束下存在唯一 Provider 透传候选时发送；无法确定归属时返回不可用错误，
可通过 Client Key 分组限定目标平台。一个 Provider 内的多个 API Key 账号仍按账号模型政策与调度规则选择，
不要求模型已被上游目录收录；ChatGPT OAuth 账号不接受 Grok 模型

目录账号只能来自本次 Client Key 冻结的账号范围。OpenAI 的 OAuth 目录按账号 ID 排序，使用首个成功读取的
合格账号，最多尝试三个账号；API Key 目录按账号模型权限过滤后聚合。同名模型优先采用 OAuth 原生对象，
其次采用 API Key 的完整 Codex 对象，再使用普通模型 ID；同类 API Key 目录按账号 ID 确定来源，不跨账号
或套餐混拼字段。因此单账号、无别名时可保持该账号的模型对象一致，多账号/多 Provider 聚合不代表
“与某个官方账号的整个目录完全一致”，也不会固定后续推理账号。
读取失败返回 `503 model_catalog_unavailable`，不以简化模板或空成功响应覆盖客户端缓存。
成功获取的 API Key 目录被账号模型政策全部过滤时返回空列表，区别于读取失败。
插件声明的适配模型按受管适配器账号范围与当前 Key 权限交集追加；同名原生完整对象优先保留，不与插件画像拼接字段

原生目录可能返回缓存结果，成功缓存有效期为 5 分钟。每次查询都检查账号资格和 Client Key 范围。
成功目录响应带 `Cache-Control: private, no-store`，不借用上游 ETag 标识经过选择/别名/聚合后的正文

`service_tiers` 的 `name` 用于生成 `/fast` 等命令，`id` 是请求使用的 `service_tier` 值，二者不能互换。
上游未声明或明确返回空数组时不补档位，也不根据模型名推断 Fast。
该合同对齐
[官方 Codex 0.154.0 的模型元数据](https://github.com/openai/codex/blob/6b9826e3aa83b1a5947db50f4332cb9c65f1b340/codex-rs/protocol/src/openai_models.rs)
与其动态服务档位命令；客户端需重新加载模型目录才能使用更新后的档位信息

Codex 专用目录中的 `context_window` 与 `max_context_window` 分别表示默认上下文窗口和客户端本地
覆盖的上限。OpenAI Provider 原样保留上游对应字段，缺失与 `null` 不互相转换；网关不通过部署配置
覆盖这些值。Codex 客户端配置 `model_context_window` 后，按该值与非空 `max_context_window` 的较小值
使用窗口；上限为空时保留客户端本地值。xAI 默认窗口取 `context_window`，缺失时取 `context_windows`
中的首个有效值；覆盖上限取默认窗口和可选窗口的最大值，只有标量时以该值作为上限

### 透传与错误恢复

#### OpenAI 透传

OpenAI 路径保留客户端 Responses wire 语义：除[正文兼容](#正文兼容)、[身份归一](#请求头与身份)、模型映射及已配置的请求策略外，
请求 body 的未知字段和字段顺序保持不变。HTTP SSE 与 WebSocket 的上游业务事件除下述客户端错误兼容及响应头隔离外按原始字节转发，
response ID 按 opaque 值处理而不假设 UUID 或固定长度；除客户端错误兼容与原生续写额度恢复外，
OpenAI 上游错误 envelope 和允许下发的 opaque header 值也不由 canonical 观测结果重写。
`reasoning.effort` 的数字和未知扩展值保留给上游判断，本地观测不限制参数形态。
`response.metadata` 的业务内容继续交付，仅其中的响应头沿用凭据、账号身份和逐跳头隔离规则。
未改写的 WebSocket 文本不依赖 JSON 旁路解析成功；插件改写后的响应仍复核必要关联，原始 SSE 心跳
和无法提取事实的未改写内容不构成协议失败。
Images 固定使用 OpenAI Provider，不重建正文或映射模型字段；Provider 从原始 JSON 旁路提取图片模型，
按[账号模型限制](#账号模型限制)选号，并复核插件改写后的模型。账号选择、鉴权头替换和端点路由
不改变请求正文，成功与非容量失败响应正文保持原始字节。

Images 排除已知为 Free 套餐的 OAuth 账号；套餐缺失或未知时不推断为 Free，API Key 账号不套用该套餐规则。
其余账号仍需满足模型限制、额度等调度条件，实际生图权限由上游决定

`/v1/alpha/search` 使用相同的 OpenAI Provider 原生端点边界：模型不映射，正文除会话 `id` 的账号归一及显式请求策略外原样转发，
Provider 旁路读取请求中的 `model`，按[账号模型限制](#账号模型限制)选号。
`x-codex-turn-metadata` 在移除客户端账号身份并按选中账号重写 installation ID 后转发；上游账号
Authorization、Cookie、account ID、originator 和 User-Agent 均由代理安全重建。
Images 与 Search 保留通过现有过滤规则的普通业务请求头及多值字节；Responses 原连接的 turn state
不随独立端点转发，turn metadata 仍由所属端点按当前账号处理。
模型映射是全局精确映射，未命中时模型名原样交给候选 Provider；分组只限定账号集合，不参与模型改名

#### xAI 适配

xAI Provider 将 Grok wire 转换为 Responses wire。上游结构化错误的 message/code/type 可透出，内嵌的账号指纹 UUID 脱敏。
模型名完整保留，不剥离前缀；别名使用显式模型映射。目录读取失败返回错误，不用硬编码型号补全能力

xAI 的 `reasoning.effort` 接受 `none / minimal / low / medium / high / xhigh / max`，去除两端
空白并转为小写后保留档位，不按模型名称降档或删除；模型是否支持该选择由上游判断。
未知值或错误类型返回字段错误，其他 reasoning 字段保留。`web_search.filters` 支持
`allowed_domains` 与 `excluded_domains`，两者不能同时为非空；客户端函数与托管搜索工具
同名时使用内部别名，并在返回工具调用时还原客户端名称。搜索工具顶层的 `allowed_domains` 不受支持，返回请求错误

#### 容量拒绝与重试

OpenAI HTTP 明确未发送的瞬时建连失败（含 TLS 握手 EOF）在单请求内共享最多 4 次 opening、30 秒预算。
先在原账号上退避恢复；允许换号且至少剩余两次 opening 时，本次恢复最多使用一半剩余时间，并保留一次 opening。
再次建连失败才按调度规则重选账号，选号不可用时使用原账号的剩余预算；固定账号或换号预算不足时仅在原账号恢复。
证书、TLS 协议及配置错误不进入该恢复，取消与请求截止时间优先终止等待

客户端推理请求提供有效逻辑线程、轮次及 `request_kind=turn` 时，网关按 Client Key 隔离短期失败状态。
“同账号重试后返回错误”策略终态失败后，5 分钟内同轮次再次命中该策略时直接返回错误，不重复执行其最多 5 次重试；
再次失败刷新有效期，成功结束后解除限制。其他重试策略不受影响，缺少有效身份、状态过期或状态存储不可用时沿用原策略。
这是一项轮次重试预算控制，不是客户端重试意图的精确检测；状态与交付顺序见[数据面请求生命周期](architecture.md#4-数据面请求生命周期)

`turn_rate_limit_exceeded` 保留原始错误和 `Retry-After`，由客户端按自身重试预算等待。
代理不因此写入账号冷却、换号或透明重试，其他会话仍可使用该账号

上游 WS `error` 转为 SSE `response.failed` 时，事件级的有效 `Retry-After` 同时放入
`response.error.headers`，供客户端读取；错误对象已有有效重试头时优先保留

Responses 的 HTTP/SSE 与 WebSocket 按顺序即时交付正常事件，不暂存前导等待后续输出。
未启用中间件链时，无输出的 `response.created`、`response.in_progress`
及 `codex.response.metadata`、`codex.rate_limits` 元数据交付后仍可按已有安全条件重试，
客户端可能收到下一次尝试的新 `response.created`。
其他事件也立即交付，但会关闭透明重试窗口，后续前导事件不再恢复重试资格。
中间件链不启用可替换前导例外；错误事件先按既有重试规则处理，无法重试时交付错误

等待沿用[请求期限](#请求期限)与传输超时配置。
WebSocket 独立 Ping 保活；Responses HTTP 流式请求完成准入后，执行仍在等待且连续 15 秒无输出时提交 `200`
和 SSE 注释心跳，此后的等待继续每 15 秒保活，包括请求中间件等待和上游尚未返回事件。
首次心跳前仍保留最终 HTTP 状态与响应头；心跳发出后发生的失败使用 `response.failed` 和 `[DONE]`，
后续取得的上游或插件响应头无法再补发，`Retry-After` 转入流内错误的 `response.error.headers`。
心跳不代表业务输出，不关闭已有的安全重试窗口。
SSE 注释和 WebSocket Ping/Pong 不重置官方 Codex 客户端的事件空闲超时，
其 `stream_idle_timeout_ms` 默认是 300 秒；长请求需持续收到真实事件，
或同时调整客户端与上游传输的空闲等待预算，单独缩短心跳间隔不能延长业务等待

WebSocket 在交付有效输出前断线或超时，可按 `stream_max_retries` 做同账号、有退避的尽力重试
（默认最多 5 次），耗尽后仅在请求允许时改用 HTTP。已发送或发送结果不明不单独阻止这项恢复；
次数限制不保证上游只执行一次，诊断保留原始发送状态。精确连接续接、显式终止拒绝、已观测的
外部副作用和下游提交仍限制恢复；取消与请求截止时间随时终止等待

OpenAI 仅在允许安全重放且尚未交付不可重放输出时执行下表的同账号重试；次数指首次尝试之外的额外尝试：

| 上游拒绝 | 同账号重试上限 | 没有有效 `Retry-After` 时的退避 | 耗尽动作 |
| --- | --- | --- | --- |
| `server_is_overloaded`、`slow_down` 或明确模型容量不足 | 3 次 | 500ms 起步的指数退避，单次最多 8 秒 | 按调度规则换号 |
| 普通 `503` 且 `error.code=server_error` | 5 次 | 500ms 起步的指数退避，基准最多 2 秒，每次追加 `[50ms, 250ms)` 等待 | 返回最后一次上游错误，不因此换号 |

有效 `Retry-After` 优先，接受秒数、HTTP 日期和零延迟，不受本地退避上限截断，也不追加随机等待。
流内失败事件使用相同解析规则，WS 转 HTTP 回退也遵守服务器建议；等待受请求截止时间和取消约束，
重试受[请求总尝试次数](#账号亲和与换号)约束。
已发送的容量失败计入[智能调度健康分](architecture.md#智能调度)，可用账号的会话亲和优先。
容量不足不触发 Provider 全局熔断，也不作为账号额度耗尽；账号冷却由独立的[错误保护规则](#账号错误保护)决定。
客户端错误兼容由 API 编码出口统一处理：最终交付的 `server_is_overloaded`、`slow_down` 错误码
投影为 `server_error`，HTTP 错误状态及 WS 包装错误的数字状态投影为 `503`，让客户端执行自己的
有界重试。Provider 已确认容量不足的初始失败，即使没有这两个错误码，也返回 `503`。
SSE/WS 的 `response.failed` 保留原消息、响应 ID 与其他业务字段；客户端无法消费的裸 `error`
继续按现有规则投影为 `response.failed`。`Retry-After` 等允许下发的响应头保留，
其他错误码不受影响。内部上游状态、错误码、原始事件及计量事实保持不变；已开始输出的请求由
客户端决定如何恢复，代理不因此重放已提交的请求。
`flex_unavailable` 是当前请求的终止拒绝，保留原始错误，不自动重试、切换传输或冷却账号

#### 额度耗尽与续写

明确额度耗尽触发账号隔离与安全换号，包括 WebSocket 握手返回的 429；不会因其长 `Retry-After` 而转入同账号传输恢复等待

OpenAI 选号阶段确认本次可选账号全部额度耗尽时，HTTP 返回 `429`，WebSocket 错误帧返回
`status: 429`，两者的 `error.type` 与 `error.code` 均为 `usage_limit_reached`，提示客户端停止
本轮自动重试，等待额度恢复或补充可用账号。空账号池、认证失效和临时容量不足不按额度耗尽处理；
仍有其他可用 Provider 或可安全恢复的续写时，网关先按现有路由规则尝试恢复

带 `previous_response_id` 的 OpenAI 原生续写仍绑定原账号。若该账号明确拒绝请求且额度已耗尽，
并且请求可安全重放、尚无语义输出且未提交下游，网关隔离该账号，对客户端返回 HTTP `400`
（WebSocket 为 `status: 400`）及 `previous_response_not_found`，不附带额度窗口的 `Retry-After`。
支持该恢复协议的客户端应去掉 `previous_response_id`、携带完整历史重试，由正常调度选择可用账号；
官方 Codex 的 WebSocket 客户端支持这一流程。其他客户端需要自行处理，网关不会跨账号发送原增量输入。
普通限流、容量不足、发送结果不明以及已经交付输出的失败不触发此转换

### API Key 额度查询

`GET /v1/usage` 使用同一数据面入口认证，默认传入 `Authorization: Bearer <Client Key>`；
启用入口认证插件时可查询其映射 Key 的额度。不接受会话 Cookie、管理 API Key 或查询参数。
只返回当前 Key 的日与周额度，不包含明文 Key、账号资料或其他 Key 的数据。查询本身不执行模型推理、扣费或占用推理并发/RPM，
不更新最近使用时间或开启预算窗口；额度耗尽后仍可查询。已配置的入口认证和 request 中间件仍按授权执行

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
已到期窗口的 `used` 为 `"0"`。日窗口按部署时区的自然日划分，周窗口沿用首次使用起的七个本地日历日周期，不固定为周一。
修改限额、管理员重置和费用结算均复用现有 Key 账本，不从请求日志重算余额

缺失、非法、已禁用或已删除的 Key 返回 OpenAI 风格 `401` 错误；未知查询参数返回 `400 invalid_usage_query`，
读取账本失败返回 `503 usage_unavailable`，不会用零余额掩盖故障

### Codex Live 语音通话

语音通话复用数据面 Client Key 鉴权，并按该 Key 的账号分组选路。通话引导是一次性请求，走与其他
`/v1/*` 数据面请求相同的调度、准入与请求记录；通话建立后的 sideband 与 hangup 使用引导时选中的
账号（call id 与账号、创建 Key 绑定），音频流不经过网关。

| 路由 | 说明 |
| --- | --- |
| `POST /v1/live` | WebRTC SDP 引导；`POST /v1/realtime` 与 `POST /v1/realtime/calls` 为同一处理的别名，成功响应的 `Location` 会改写为 `/v1/realtime/calls/{call_id}` |
| `GET /v1/live/{call_id}` | 通话 sideband WebSocket，双向透传事件帧 |
| `GET /v1/realtime/calls/{call_id}` | 同上的 realtime 路径形态 |
| `GET /v1/realtime?call_id=…` | 同上的查询参数形态 |
| `POST /v1/realtime/calls/{call_id}/hangup` | 用钉住账号转发挂断，成功后解除绑定 |

引导请求接受三种入口形态并统一为上游要求的 `{"sdp": …, "session"?: …}` JSON：JSON 保留原有字段、
`application/sdp` / `text/plain` 包装为 `sdp` 字段、`multipart/form-data` 取 `sdp` 与 `session` 两个部分。
`session.model` 或顶层 `model` 中的 `gpt-realtime` 系模型名归一为 `gpt-live-1-codex`，其余模型原样透传；
模型字段缺失时按默认语音模型参与检查。归一后的模型参与账号模型权限检查：账号范围整体禁止该模型时
引导请求按路由错误拒绝，被禁账号不会服务语音通话

引导、sideband 与 hangup 的业务扩展头默认透传；认证、账号身份、下游环境与传输头遵守
[请求头与身份](#请求头与身份)边界。`X-Session-Id`、`Voice-Session-Id` 是 Live 业务会话标识，原样透传。
`Session-Id`、`Thread-Id` 等已识别追踪标识按选中账号的安装身份稳定映射，同账号各阶段保持一致，
换账号使用不同标识；客户端 trace 被剥离。创建请求可由 attempt 插件改写本次构造身份，后续 sideband / hangup 仍使用钉住账号的身份。
Live 不继承 Responses 的路由提示与续接状态；turn metadata 中的原账号绑定信息清理后保留其他字段。
真实 call ID 保留用于钉住账号访问通话。HTTP 业务头保留原始多值字节，sideband 握手沿用 WebSocket 可编码的头值

引导与 hangup 的成功响应透传上游状态码、普通业务响应头和 `Content-Type`，剥离认证、账号身份、Cookie
及传输字段；正文按字节透传。sideband 中继不做协议解释：Text/Binary 帧双向透传，
Ping 由网关本地应答，关闭帧保留原关闭码，异常中断投影为正常关闭。同一 call id 只允许一条 sideband，重复加入返回 `409`；
非创建该通话的 Client Key 访问 sideband 或 hangup 返回 `403`；call id 未知或已过期返回 `404`。
hangup 不受 sideband 占用限制，通话中可随时挂断；挂断与 sideband 都走钉住账号的出口代理。

通话使用引导时解析的 Client Key 画像，sideband、重连和 hangup 复用同一画像；画像配置更新只影响新通话。
后续连接与挂断读取钉住账号当前有效的凭据和代理配置

call 与账号的绑定保存在网关进程内，未连接时一小时过期；sideband 占用期间暂停过期，断开后重新计时，
客户端可重连加入同一通话。挂断成功或上游报告通话不存在时立即解除绑定。
进程重启后未完成通话失去 sideband 与 hangup 能力

限制：

- 引导只选择未明确标记为 Free 的 OAuth 账号，套餐缺失或未知时不推断为 Free；API Key 类账号不在语音调度范围
- 引导按当前账号范围、模型政策、额度与冷却状态选取账号；sideband 固定使用创建通话的账号，每次连接重新检查当前授权范围、模型政策与账号启用状态，失权时返回 `403`，不会切换账号
- 已连接的 sideband 不随权限变更自动断开；仍有效的创建方 Client Key 可调用 hangup 结束通话
- 标准实时 WebSocket（`GET /v1/realtime?model=…`）、realtime client secrets、legacy sessions、
  transcription/translation 会话与 SIP 控制返回 `501 realtime_capability_not_supported`。

## 4. 浏览器认证

### 统一登录与会话

管理员和密钥登录共用 `/api/auth/*`。登录模式 `mode` 只用于选择凭据验证方式，不直接授予权限；
验证成功后，由后端写入身份和绑定 ID。浏览器通过 `cpr_session` HttpOnly Cookie 访问业务接口，
管理员另持有仅发送到 `/api/auth` 的 `cpr_refresh` HttpOnly Cookie。原始 Key 不进入 URL 或浏览器持久化存储。
管理员进入管理端；Key 登录后进入 `/key-usage`，只读取当前会话绑定 Key 的数据

| 方法 | 路由 | 请求 | 说明 |
| --- | --- | --- | --- |
| `POST` | `/api/auth/login` | `{ mode: "admin", username?, password }` 或 `{ mode: "key", apiKey }` | 验证凭据、创建会话；成功后撤销请求携带的旧会话 |
| `GET` | `/api/auth/status` | 无 | 只读校验 Cookie 对应身份，返回 `{ authenticated, session }` |
| `POST` | `/api/auth/refresh` | 无 | 使用管理员续期 Cookie 恢复访问凭据；返回同 status 的结构并同步 Cookie，Key 会话保持原期限 |
| `POST` | `/api/auth/logout` | 无 | 删除当前会话并清除 Cookie；存储失败返回 503，不假装退出成功 |
| `POST` | `/api/auth/password` | `{ currentPassword, newPassword }` | 仅管理员会话可用；验证当前密码后修改密码，撤销全部管理员会话并清除当前 Cookie |

登录返回 `data: { role: "admin" | "key", expiresAt }`；status 和 refresh 已登录时的 `session` 使用同一结构，
未登录时为 `{ authenticated: false, session: null }`。`role` 由服务端已验证身份推导，不接受客户端声明。
不返回凭据或绑定 ID

修改密码要求新密码至少 12 个字符、最多 1024 字节，不能包含控制字符、使用常见弱口令或与当前密码相同。
成功返回 `{ message }`，需要重新登录；当前密码错误或新密码不合法返回 400，并保留原会话。
并发修改中只有旧密码哈希仍匹配的请求可以提交，冲突返回 409；密码更新与安全审计在同一事务提交。
该入口共用登录尝试限流，超限返回 429。普通设置变更和管理员 API Key 变更不撤销密码登录会话，密钥身份会话也不受改密影响

会话由服务端保存，两种 Cookie 均为 `HttpOnly; SameSite=Lax`，`Max-Age` / `Expires` 对齐各自有效期，
`Secure` 沿用上述 Origin 规则。访问 Cookie 使用 `Path=/`，续期 Cookie 使用 `Path=/api/auth`，不能直接访问业务接口。
普通业务请求和状态轮询不会续期。管理员访问凭据到期后，仍可通过续期 Cookie 恢复登录；续期成功会轮换两种凭据，
并重新计算不活动期限，但不能超过登录时确定的最长有效期。访问凭据仍有效时，refresh 返回同一组凭据和期限。
轮换后 30 秒内重试上一代续期凭据返回同一组新凭据；超出窗口再次使用已识别的上一代凭据会撤销该会话。
续期已过期或会话已撤销时不会重新创建；未认证返回 `{ authenticated: false, session: null }`，不写 Cookie；依赖故障返回 503。
退出可以只携带续期 Cookie，并清除两种 Cookie。Key 登录清除续期 Cookie，密钥身份会话保持固定期限。
期限配置见[部署配置](../deploy/README.md#手动安装)

每次恢复密钥会话时重新确认 Key 存在且启用；停用或删除后会话失效，重新启用不会恢复已撤销会话。
依赖不可用时返回 503，不返回已认证或假装未登录。预算耗尽不妨碍登录

密钥会话访问管理接口返回 403，不清除仍然有效的会话。
浏览器会话不能替代 `/v1/*` 的 Bearer Key，数据面 Key 也不能替代浏览器会话

所有 `/api/auth/*` 响应带 `Cache-Control: no-store`；未知路径和错误 method 返回 JSON，不落入 SPA。
两种登录共享来源桶和全局桶，分别为每 60 秒 10 次 / 200 次；来源取连接 IP，不信任任意转发头。
拒绝时使用 `42901` 和 `Retry-After`

认证错误共用 `40101`（会话失效）、`40102`（凭据错误）和 `40301`（权限不足）。
`40102` 和 `40301` 不触发会话恢复，网络或依赖故障不清除身份；浏览器的统一恢复与请求重放职责见[登录与查询身份](architecture.md#登录与查询身份)

### Key 用量与客户端配置

以下接口仅接受 Key 身份的 `cpr_session`，不接受 Bearer Key 或管理 API Key。管理员会话返回 `40301`；
缺失、失效或已停用的 Key 会话返回 `40101`。所有响应带 `Cache-Control: no-store`，未知路径和错误方法返回 JSON

| 方法 | 路由 | 查询 | 说明 |
| --- | --- | --- | --- |
| `GET` | `/api/key-usage/overview` | `period?`、`asOf?` 或 `startTime?`、`endTime?`，另含 `model?` | 用量汇总、趋势、当前额度和锚点所在日期的健康时间线 |
| `GET` | `/api/key-usage/records` | 同上，另含 `kind?`、`currentPage?`、`pageSize?` | 当前 Key 的请求记录 |
| `GET` | `/api/key-usage/config` | 无 | 当前 Key 的客户端配置凭据 |
| `GET` | `/api/key-usage/version` | 无 | “关于”弹窗使用的当前版本号和提交号 |

用量查询支持[页面时间合同](#页面时间合同)，默认 `period=7d`，一次最多 31 天。模型按字面量包含匹配，区分大小写，通配符按普通字符处理
不接受 Key ID、账号、Provider 等范围参数或其他未知字段。页码默认 1，每页默认 20，允许 1–100 条；
`kind` 为 `success`（默认）、`error` 或 `all`，沿用使用统计的请求结果筛选：错误包括失败和未完成请求，
全部还包括取消与进行中的请求；一次请求只占一条记录。分页响应为 `{ items, currentPage, pageSize, total }`

overview 返回 `asOf`、`asOfDisplay`、`startTime`、`endTime`、`key`、`summary`、`trend`、`healthTimeline`。
`key` 仅包含名称、掩码前缀、并发/RPM、日与周限额、已用 USD 及重置时间；零限额表示不限，
未启动窗口的重置时间为 null。额度使用现有结算账本，不受日志日期或模型筛选影响。
健康时间线沿用管理端的自然日分桶与可用性语义，以 `asOf` 为锚点，不受所选周期长度和模型筛选影响

汇总和趋势返回请求数、输入、输出、缓存读写、推理、总 Tokens 与 USD 成本；输入已包含缓存读写，
推理为输出的明细，不得把缓存或推理重复计入总消耗。趋势另含 `time` 与 `bucketSeconds`。
`costUsd` 为十进制字符串或 null；`costIncomplete` 表示部分请求计费不完整，不能把已知费用当作完整总费用。
空请求范围的成本为 `"0"`，缺失定价保持 null

日志只返回时间、公开请求模型、推理强度、接口、上下游传输方式、当前请求的 IP / User-Agent、
Token 明细、费用明细、用时/首字与状态。Token 和费用复用现有展示合同；延迟仅包含当前请求的首事件、
首推理、首文本、总耗时与上游性能指标，口径见[记录范围与统计口径](#记录范围与统计口径)，不含账号容量或调度诊断。
`status` 为 `success`、`error`、`incomplete`、`cancelled`、`running` 或 `unknown`，`statusCode` 只返回已记录的客户端状态码。
各结果均保留已采集的 Token/费用明细，缺失时为 null。不返回账号资料、Key ID、上游模型或请求标识、原始错误正文或诊断内容

version 返回 `{ version, gitSha }`，不接受查询参数，不包含部署模式、更新状态或内部诊断

config 返回 `{ name, plaintextKey }`，仅读取服务端会话绑定的当前 Key，不接受任何查询参数。
这是显式读取敏感凭据的入口，明文不进入用量响应；客户端应仅在需要配置凭据时调用，并避免持久化到浏览器

## 5. 账号

账号 API 使用统一路由，不存在 Provider Instance 或 Provider 专属账号路由。`provider` 使用 Provider ID，
固定为内置 `openai`、`xai`；操作可用性由平台实现与账号认证类型决定

| 方法 | 路由 | 主要 query/body | 说明 |
| --- | --- | --- | --- |
| `GET` | `/api/admin/accounts` | `page`、`pageSize`、`provider`、`groupId`、`search`、`status`、排序字段 | 分页查询账号与汇总 |
| `GET` | `/api/admin/accounts/concurrency` | `accountId`、`page=1`、`pageSize=20` | 查询账号当前并发占用；每页最多 100 条 |
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
| `GET` | `/api/admin/accounts/reset-credits/automation` | `accountId` | 读取自动使用开关与本地计划，不查询上游 |
| `POST` | `/api/admin/accounts/reset-credits/automation` | `{ accountId, enabled }` | 设置账号自动使用开关，默认关闭 |
| `GET` | `/api/admin/accounts/models` | `accountId` | 优先读取该 Provider + 套餐的模型 cache，缺失时有限实时拉取 |
| `GET` | `/api/admin/accounts/models/catalog` | `accountId` | 读取指定账号的完整 Codex 原生模型目录，返回 `{ modelCount, observedAt, catalog }` |
| `POST` | `/api/admin/accounts/models/refresh` | `{ accountId }` | 强制拉取最新模型并覆盖 cache |
| `GET` | `/api/admin/accounts/connection-test` | `accountId`、`modelId` | 通过 SSE 返回实时连接测试事件，不作为业务 Responses 用量记录 |
| `POST` | `/api/admin/accounts/oauth/start` | `{ provider, name, accountId?, outboundProxyId?, outboundProxyUrl? }` | 为支持登录的 Provider 创建 flow；`accountId` 表示重新授权 |
| `POST` | `/api/admin/accounts/oauth/complete` | `{ provider, flowId, callbackUrl, settings? }` | 消费 OAuth callback；首次授权可附带账号设置，重新授权保留原设置 |

### 查询与账号字段

账号列表支持以下稳定值：

- `provider`: `openai` 或 `xai`，省略或空值表示不过滤
- `groupId`: 分组 ID、`ungrouped`，或省略以不过滤
- `status`: `normal`、`quota_exhausted`、`rate_limited`、`disabled`、`error`
- `sortBy`: `email`、`status`、`planType`、`usage`、`lastUsedAt`、`expiresAt`
- `sortDirection`: `asc`、`desc`

账号列表的 `search` 仅按字面量包含匹配名称和邮箱，不区分大小写；`%`、`_` 和 `\` 按普通字符处理

账号视图的 `capacity` 返回查询时的普通并发容量：`usedSlots` 不含独立审批池和排队请求，
读取运行态失败时为 `null`；`totalSlots` 是应用账号独立配置或全局默认值后的上限，`null` 表示不限。
该上限不代表上游实际允许的并发数

并发明细返回 `accountId`、`entries`、`total`、`page`、`pageSize` 和 `observedAt`，只统计普通并发池。每条包含 `leaseId`、`requestId`、`clientKeyId`、`clientKeyName`、`model`（入口请求模型）、`clientIp`、`startedAt`、`startedAtDisplay` 和 `occupiedMs`。占用时长从取得账号租约开始，不包含排队；缺少入口事实时相应字段为 `null`，租约仍计入总数

查询需要管理员会话，账号不存在返回 404；`page` 从 1 开始，`pageSize` 为 1–100，偏移不能超过 `u32` 范围。结果是查询时的实时快照，按租约有效期排列，续期和请求完成可能改变跨页位置；接口禁止缓存，不提供历史记录

模型目录导出保留上游原生模型对象和能力字段，不包含账号凭据；不支持 Codex 原生目录的账号不能导出。
响应中的 `catalog` 可直接保存为 JSON 文件，用于 Codex 的 `model_catalog_json` 配置。
账号设置保存不等待上游模型目录刷新完成

账号限流详情在 `quota` 中返回：`rateLimitReason` 为 `upstream_rate_limit`（上游临时限流）、
`error_protection`（自定义错误规则触发保护）或 `null`。`recoveryProbeRequired` 表示解除冻结是否需要成功探测；
此时 `rateLimitedUntil` 是最早探测时间，到期后仍保持 `rate_limited`，直到探测成功或手动恢复。
未要求探测时，该字段表示冷却结束时间。`protectionRules` 返回触发时的规则快照 `[{ id, name }]`，
没有保护原因时为空数组，修改或删除当前规则不改写此快照。错误保护与上游限流独立恢复，解除保护不保证其他限制已经解除

`quota.credits` 返回上游点数余额，未提供点数信息时为 `null`。对象包含 `hasCredits`、`unlimited` 和
`balance`，余额为保留上游精度的十进制字符串，未提供余额时为 `null`，明确的零余额为 `"0"`。
OpenAI 点数随现有额度刷新和正常请求的额度信息同步，不与主动重置卡库存或百分比限额合并，
不根据 `balance` 单独推导可用性。主动查询明确 `hasCredits=true` 或 `unlimited=true`，且无阻止
点数支出的上限或工作区限制时，即使套餐窗口耗尽也可使用点数，并解除已有额度耗尽状态。
真实额度拒绝后的后台回读同样支持此恢复路径，但不会自动重发本次失败请求；点数不能清除
凭据错误、停用或冷却状态。上游点数信息也可能延迟，后续真实拒绝仍会记录额度耗尽

账号列表和详情返回 `notes`（无备注时为 `null`）。编辑时省略或 `null` 保留原备注；字符串最多 500 个 Unicode
字符，允许换行和制表符，保存时去除首尾空白，空字符串清空备注。备注独立于上游身份，导入时未显式提供备注、
重新授权、凭据刷新及批量调度更新均保留已有备注

账号视图和 Dashboard 账号概览中的 `planType` 保留原始套餐值；`planTypeDisplay` 使用 Provider 提供的最终名称，
前端直接展示。OpenAI 的 `prolite`、`pro`、`promax` 分别显示为 `ProLite`、`Pro`、`ProMax`；
其余已知套餐使用官方名称，未知 OpenAI 套餐保留原值。
展示名称保留空格和括号，未提供专属名称规则的 Provider 使用通用大驼峰格式。
OpenAI 主动额度刷新和正常响应携带的明确套餐会同步到账号，支持升级与降级；
空值或 `unknown` 不覆盖已有套餐，同族泛化值（如 `team`）保留已知的具体套餐子类型。
账号套餐为空或 `unknown` 时，后端优先用已保存的上游额度响应
中的明确套餐值补全 `planType` 和 `planTypeDisplay`；两处均无套餐信息时才显示“未知套餐”

`outboundProxyId` 绑定已保存的代理，不要求出口测试成功；省略或 `null` 保留当前绑定，空字符串清除绑定。
`outboundProxyUrl` 兼容 HTTP、HTTPS、SOCKS5、SOCKS5H 代理 URL，可带用户名和密码；不能与 ID 同时设置。
编辑时省略或 `null` 表示保持原配置，空字符串表示清除代理并直连。列表和详情返回绑定的
`outboundProxyId`（未绑定时为 `null`，禁用代理仍保留 ID）和不含认证信息的 `outboundProxyEndpoint`
（未配置时为 `null`）；只有显式敏感导出包含完整 URL。
指定代理后，推理、OAuth 服务端交换/刷新及账号辅助请求使用同一出口；代理失败不会退回直连。
浏览器打开的第三方 OAuth 授权页仍使用浏览器自身网络。
账号出口与连接隔离见 [架构说明](architecture.md#账号出站代理)

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
Pro 使用 `all` 时也能参与 luna 调度。套餐名称不自动生成或修改规则

单账号更新或批量更新省略 `modelAccess` 时保留原值，显式提交 `{ "mode": "all", "models": [] }` 清除限制。
批量接口的调度、分组、模型与代理字段均可省略；省略的字段保持各账号原值。
提供 `groupIds` 时替换完整分组集合，提供 `concurrencyLimit: null` 时恢复继承运行参数

限制适用于 Responses HTTP、WebSocket 及其带压缩触发的请求，以及 Images 生图、编辑与独立 Search 选号，包括重试、亲和和换号；没有合规账号时沿用
无可用账号错误，不会回退到被禁止的账号。已开始请求使用冻结的政策，新请求使用已发布的新配置。
Images 和 Search 按原始请求正文的模型 ID 检查，不依赖模型目录或全局模型映射；可手动添加目录未列出的模型 ID。
模型缺失、重复或无法识别时，仅使用模型政策为 `all` 的账号，不推测上游默认模型。
Search 的选号限制不约束插件在选号后主动改写正文。
管理员连接测试不受该模型限制；连接测试成功只证明指定账号的上游能力。
普通 `/v1/models` 和单模型查询将已发现的 OpenAI 模型关联到来源账号，至少一个来源账号在当前 Key 范围内且政策允许时才展示；
同 Provider 中未发现该模型的 `all` 账号不会使它进入列表。原生目录保留已有来源选择和完整模型对象

### 独立代理管理 / Managed Proxies

所有端点要求管理员身份。所有响应只返回去掉认证信息的 `endpoint`，不会返回完整 URL

| 方法 | 路径 | 请求 | 结果 |
| --- | --- | --- | --- |
| `GET` | `/api/admin/proxies` | `page`、`pageSize`（1-200）、`search`（名称） | `{ items, page }` |
| `GET` | `/api/admin/proxies/accounts` | `proxyId`、`page`、`pageSize`（1-200）、`search`（账号名称或邮箱） | `{ items, page }` |
| `POST` | `/api/admin/proxies/accounts/remove` | `{ proxyId, accountId }` | `{ configRevision }` |
| `POST` | `/api/admin/proxies/create` | `[{ name, proxyUrl, location?, autoLocation?, maxWebsocketConnections? }]` | 逐项结果数组，成功项的 `data` 为 `{ record, configRevision }` |
| `POST` | `/api/admin/proxies/update` | `{ id, revision, name, enabled?, proxyUrl?, location?, autoLocation?, maxWebsocketConnections? }` | `{ record, configRevision }` |
| `POST` | `/api/admin/proxies/probe` | `{ proxyUrl, detectLocation? }` | 测试未保存的地址，返回连通性及可选位置结果，不创建代理 |
| `POST` | `/api/admin/proxies/test` | `[{ id, revision, detectLocation? }]` | 逐项结果数组，成功项的 `data` 为最新代理记录 |
| `POST` | `/api/admin/proxies/delete` | `{ id, revision }` | `{ configRevision }` |

添加与测试统一接收 1–100 项数组，单条操作传一个元素，响应 `data` 按输入顺序返回等长数组。
每项为 `{ status: "succeeded", data }`、`{ status: "failed", httpStatus, code, message, data: null }`
或 `{ status: "skipped" }`。整个请求的 JSON、数组形状、数量或重复测试 ID 校验失败时返回 4xx，不执行任何项；
通过请求校验后返回 HTTP 200，各项独立提交，单项字段格式、名称或位置校验失败、重复 URL、版本冲突等只影响当前项。
遇到服务异常或结果不确定时停止派发后续项，已开始的测试等待结果，其余标记为 `skipped`。
调用方不能自动重试整批；请求中断时先刷新核对已保存项。测试每批最多并发两条，与单条探测共用四条并发上限；
调用方应按超时预算分组发送，请求中断不会回滚已提交项

`record` 包含 `id`、`name`、`enabled`、`endpoint`、`hasAuthentication`、`revision`、`accountCount`、`location`、
`autoLocation`、`detectedLocation`、`maxWebsocketConnections`、`lastTestAt`、
`lastTest: { success, latencyMs, exitIp, exitIpv4, exitIpv6, message, location }`、`createdAt`、`updatedAt`。
未测试时 `lastTestAt` / `lastTest` 为 `null`。连通性失败返回 HTTP 200 和 `lastTest.success=false`；
记录版本过期、重复 URL 或代理正在用于账号导入时对应 409，并发测试满载对应 429；
添加和测试在各项的 `httpStatus` 返回这些状态，其他单项接口使用 HTTP 状态码

代理列表只返回关联账号数量。关联账号按需查询，每项包含 `id`、`name`、`email`、`provider`、`enabled`、
`authenticationKind`、`planType`、`planTypeDisplay` 和 `groups: [{ id, name, color, enabled }]`，
不返回账号凭据。默认每页 20 条，按名称、ID 稳定排序；搜索不区分大小写，匹配名称或邮箱的字面子串。
不存在的代理返回 404，未绑定账号或没有匹配结果时返回空页。数量与当前页对应同一查询快照

移除关联账号只清除指定账号的代理绑定与连接地址，使其改为直连，保留凭据、调度参数与分组。
若账号已不再绑定请求中的代理，则返回 409
删除代理会自动解除全部关联账号的代理绑定与连接地址，账号保留其余设置并改为直连。
代理仍被插件来源或插件包引用时返回 409，账号绑定保持不变

`maxWebsocketConnections` 为此代理的 OpenAI Responses 上游 WS 连接上限，范围 0–4294967295，0 表示不限制。
创建时省略默认为 0，更新时省略或 `null` 保留原值。关联账号共享预算，修改后新请求使用新值，已存在的连接仍计数。
容量回收、等待及多实例边界见[部署说明](../deploy/README.md#公网访问)

更新省略 `proxyUrl` 保留认证；连接配置改变时清除测试结果并更新所有绑定账号。
测试结果只在请求中的版本仍匹配时保存

代理默认启用，更新 `enabled: false` 后保留配置、测试结果和账号绑定，关联账号的新请求改为直连，
不应用该代理的位置覆盖；重新启用后恢复原出口及位置规则。省略 `enabled` 保留当前状态。
禁用代理仍可执行连接测试，但不能以 ID 选择用于账号绑定、新的账号授权、导入或插件下载

`detectLocation: true` 在测试连接时解析出口位置；结果 `location.status` 为 `notRequested`、
`detected`、`failed` 或 `conflict`。`detected` 携带 `location`，`failed` 携带安全错误说明；
位置查询失败不等同于代理连接失败；检测结果不会自动开启持续跟随。
API 的 `autoLocation` 默认为 `false`；开启时使用已检测位置，测试连接会刷新检测结果。
`detectedLocation` 保存 `{ location, exitIpv4, exitIpv6, detectedAt }`，手动位置独立保留

`location` 为 `null` 或完整对象 `{ country, region, city, timezone }`。国家代码为两位大写 ASCII 字母；
地区、城市禁止控制字符，去除首尾空白后须为 1–128 个字符；时区必须是有效 IANA 名称，例如 `Asia/Tokyo`。
创建时省略或 `null` 表示继承全局；更新时省略表示保留，`null` 清除覆盖，完整对象替换覆盖。
只改位置不清空连通性测试结果，也不更改账号凭据版本

关联账号的 OpenAI/Codex Responses 请求（HTTP/SSE、WebSocket）优先使用代理位置，否则使用全局
运行设置中已开启的 `requestLocation`；两者均未开启时保留客户端原有位置和时区。全局覆盖按请求冻结，
新请求使用保存后的设置，无需重启；代理覆盖在每次执行时读取，
换号或换出口按该次选定账号解析。位置只影响客户端时间上下文的时区和 Web Search 的结构化位置，
不改变 epoch 时间戳、真实出口 IP、服务或管理端时区、数据驻留约束及 xAI 请求

时间上下文支持以下两种消息，文本块均为 `type: "input_text"`：

| 消息角色 | 完整 XML 根标签 | 内容分类 |
| --- | --- | --- |
| `user` | `environment_context` | `environments.environment_context` |
| `developer` | `codex_apps_client_time_context` | `additional_content.codex_apps_client_time_context` |

文本去除首尾空白后须由对应标签完整包围且为合法 XML，只替换根节点直属的 `timezone`。
`current_date` 保留客户端原值，不按请求时间刷新历史日期，也不根据仅有的日期推算目标时区日期。
有 `internal_chat_message_metadata_passthrough.content_item_kinds` 数组时，文本块的对应分类须匹配上表；
没有分类时，按消息角色和完整 XML 识别。
显式标为 `user.text` 或其他分类的内容、普通聊天中引用的示例、工具结果及无法解析的上下文保持原样。
客户端实际本机时区不受影响，工具读取本机时区后的输出仍可包含真实值；排查见
[时区与客户端环境信息](../deploy/README.md#时区与客户端环境信息)

测试经代理并发访问 IPv4 专用端点 `https://api.ipify.org?format=json` 与 IPv6 专用端点 `https://api6.ipify.org?format=json`，
分别验证并记录双栈出口（IPv4 与 IPv6 地址），在任一地址族可用时即判定连接成功。超时 15 秒，每进程最多同时测试 4 条。
解析位置时还会向 `https://ipwho.is/<出口 IP>` 查询地理位置，出口 IP 会发送给该第三方服务；
自定义位置和时区不改变这些探测请求
探测器复用 OpenAI 的证书信任配置：优先读取非空的 `CODEX_CA_CERTIFICATE`，
其次读取 `SSL_CERT_FILE`，并保留系统根证书；证书配置错误不会回退为不验证证书。
出口测试结果仅供诊断，不限制代理的选择和绑定；未测试或测试失败的代理仍可使用。
出口测试通过不表示 Provider 账号权限或额度可用；账号可用性使用账号连接测试。
导入请求可以携带顶层 `outboundProxyId`，在令牌交换前解析为默认出口；文件中显式的代理配置优先。
文件及 AT/RT 导入从凭据交换到落库期间保护所选代理；此时修改、删除或写入测试结果返回 409，
避免已轮换的凭据因代理状态变化而丢失。完成导入或请求取消后自动释放保护。
OAuth 等待回调期间不持有保护；提交仍拒绝已删除或连接配置改变的代理

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

所有连接测试事件都包含服务端发出时的 `occurredAt`、完整 `occurredAtDisplay` 和短 `timeDisplay`。
浏览器本地取消或断网没有服务端发生时间，应保持事件顺序和状态，不伪造时间戳

- `source` 为 `gateway`、`provider` 或 `upstream`：分别表示尚未进入 Provider、Provider 本地且未发送、
  已发送/可能已发送或已经捕获到上游事实
- `gatewayErrorCode` 是 `GatewayErrorKind` 的稳定机器值，管理端据此生成中文摘要
- `sendState` 为 `not_sent`、`sent`、`ambiguous`，非 Provider 错误为 `null`
- `error`、`providerErrorCode`、`providerErrorType`、`upstreamStatus`、`upstreamContentType` 和
  `upstreamBody` 是实际捕获的原始诊断字段；缺失时为 `null`，不会由本地猜测或翻译

### 后台导入任务

管理端通过后台任务导入账号，关闭页面不会取消执行。`submissionId` 为客户端生成的 UUID；同一管理员在任务记录
保留期间使用相同标识和相同输入重新提交，会返回已有任务，内容改变则返回 409。修改输入须使用新标识

每个任务接受 1–200 个 `items`，请求体上限为 4 MiB。每个条目遵循下节的 Provider 文档与设置合同，独立处理
并返回结果。批量 AT / RT 按非空输入顺序拆成单账号条目，因此可逐条统计；
管理端上传的 OpenAI JSON 文件按 `accounts` 数组拆成单账号条目，每项保留该账号引用的代理，
成功与失败分别记录；xAI、插件文档及直接调用导入 API 的多账号文档仍由 Provider 按自身语义处理。
管理端按条目数和请求体大小分批创建任务，同一输入重试时复用各批的提交标识，已创建任务不会重复执行。
一个未拆分的文档可导入多个账号，条目成功数与入库账号数可能不同

摘要字段为 `taskId`、`createdAt`、`finishedAt`（未结束为 null）、`stopRequested`、`total`、`counts`。
`counts` 包含 `pending`、`running`、`succeeded`、`failed`、`unknown`、`skipped` 和 `importedAccounts`。
详情追加 `items: [{ index, provider, status, accountIds, message }]`，`index` 从 1 开始；`status` 对应上述前六类状态。
列表返回 `{ items: [摘要] }`。结果未知时保留 `unknown`，先核对账号目录再决定是否重新导入；服务端不自动重试凭据交换。
停止请求可重复调用，已结束的任务保持原结果；未知、已过期或其他管理员的任务 ID 返回 404

所有后台导入共享 3 个执行槽位；
最多接受 8 个未结束任务、保留 100 个任务，达到上限返回 429。成功、失败或跳过后立即释放对应输入，
终态结果保留 1 小时后自动清理。服务重启会丢失任务与未执行输入，已提交的账号不受影响；
任务记录仍在保留期内时，可通过列表接口查询当前管理员的任务及进度

### 账号能力、导入与 OAuth

敏感账号导出要求所选全部账号属于支持导出的内置平台。不支持时整个请求在读取任何凭据前拒绝，不返回部分导出结果

账号列表及额度详情中的 `capabilities` 则针对当前账号，包含布尔字段 `quota`、`quotaRefresh`、`profile`、
`subscription`、`avatar`、`resetCredits` 和 `consumeResetCredit`，分别表示额度读取、额度刷新、资料、订阅、
头像、重置卡查询及消费能力。服务端结合 Provider 实现与账号认证类型生成，客户端据此显示
操作；只读与写入能力分别判断，不按 Provider 名称推测。
能力是查询时的投影，不能代替实际操作的权限校验，也不保证上游可用

导入的 `data` 必须是 JSON object，Admin API 请求上限为 64 MiB；Provider 可以收紧限制，
当前 xAI 导入上限为 16 MiB。内部 schema 由目标 Provider 独占解释：

- OpenAI 接受单账号 OAuth 或 API Key 文档、`accounts` 数组（最多 200 项）、CPR 账号 bundle 和含代理引用的 sub2api 导出
- OpenAI OAuth token 字段接受 `accessToken`、`refreshToken`、`idToken`，以及官方
  `auth.json` 中的 `access_token`、`refresh_token`、`id_token`，可以嵌套在 `tokens` 等账号 object 内；
  每项至少包含 AT 或 RT。仅含 `OPENAI_API_KEY` 的客户端代理配置不是 OAuth 账号导入材料；
  RT-only 会在导入时换取 AT，已知过期的 AT-only 会拒绝导入，其他 AT-only 不具备自动续期能力
- OpenAI 与 xAI 的账号条目接受 `outboundProxyUrl`；OpenAI 还会解析 sub2api 的 `proxy_key` 和顶层 `proxies`。
  代理在 token 刷新前绑定。缺失、重复、停用、带到期时间或配置回退策略的 sub2api 代理会拒绝导入
- xAI 从单账号 object 或 `accounts` 数组中提取 OAuth token；并发、优先级等字段不参与认证
- xAI 批量导入逐条独立校验：失败条目跳过并记录日志，不中断其余条目，仅当没有任何条目成功时整个导入才报错
- xAI API Key 不是受支持的账号 credential
- OAuth 导入由目标 Provider 完成必要的 token exchange 或身份投影。API Key 导入校验凭据格式与地址，不调用 OAuth 或 ChatGPT 身份接口；可用性由模型目录和连接测试验证

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
地址支持 HTTP 和 HTTPS，包括内网 IP 与容器名，拒绝 URL 中的认证信息、查询串和 fragment。
HTTP 会明文传输 API Key 和请求内容，仅用于可信网络。
`transport` 可省略（默认 `http`）或设为 `prefer_websocket`。API Key 使用 Bearer 认证与普通 JSON，
不携带 OAuth Cookie 或 ChatGPT 身份。上游模型列表使用标准 `/models` 格式，按账号和凭据版本隔离；
目录用于模型发现，不作为能力白名单，未列出的模型仍交由上游判断。
API Key 每次导入创建独立账号；更新已有账号使用 `/api/admin/accounts/update` 的 `connection` 字段。
导出会显式包含密钥，沿用敏感导出的确认合同

sub2api 的 `platform=openai`、`type=apikey` 使用 `credentials.base_url` / `credentials.api_key`；
导入时按其端点规则将服务根、版本前缀或完整 `/responses` 地址转换为 API 前缀，缺省地址为官方 `/v1`。
非空模型映射、请求头覆盖、其他协议和启用的 `extra.openai_*` 设置尚未适配，返回输入错误，需先移除并在本项目重新配置

API Key 账号与 OAuth 共用现有 Responses、Images 和 standalone Search 请求与响应链路，
包括 Responses Lite 和原生压缩协议的透传，实际支持情况由上游决定；不提供独立 compact 路由。
模型目录、连接测试和本地用量统计可用；OAuth 刷新/重新授权、ChatGPT 额度、个人资料、订阅和重置卡不适用。
客户端的 `image_generation` 和 `X-OpenAI-Actor-Authorization` 声明不改变账号的实际能力。
账号列表和详情的 API Key `usage` 汇总该账号创建后仍保留的本地请求记录，`usage.windowKind` 为 `general`，管理端按当前语言显示通用额度；
日志清理会影响累计范围，不代表上游余额。OAuth 账号仍按实际周/月额度窗口统计

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

RT-only 使用同一形状，只提交 `refreshToken`。不得把真实 token 写入日志、issue、fixture 或文档

账号导入与首次 OAuth complete 可附带 `settings: { enabled, concurrencyLimit, weight, groupIds, notes?, modelAccess? }`。
提供 `settings` 时前四项均必填，`concurrencyLimit: null` 继承运行参数，否则为 1–4294967295 的整数；
`weight` 为 1–100，`groupIds` 为完整分组集合。设置应用于本次导入的全部账号，包括匹配到的已有账号，
与凭据在同一事务内提交；分组不存在时整次回滚。省略 `settings` 时新账号使用默认设置并保持未分组，
已有账号保留原有分组、权重与并发设置。可选 `notes` 与编辑备注使用相同的校验和清空语义，省略或 `null` 保留已有备注；
管理端新建表单留空时省略 `notes`。重新授权不接受 `settings`，凭据刷新保留账号设置

账号列表的每个 item 返回轻量 `groups: [{ id, name, enabled }]`

OpenAI 的 CPR 导出保持 OAuth 账号的既有 token 与过期时间字段。OpenAI 与 xAI 的 CPR 账号条目均包含
`modelAccess`，重新导入时恢复；导入 `settings.modelAccess` 显式提供时覆盖条目中的政策。
导入条目未提供 `modelAccess` 且 `settings` 未指定该字段时，新账号默认 `all`，已有账号保留原值；凭据刷新、重新授权和目录刷新均保留政策。
sub2api 的 `credentials.model_mapping` 不转换为本项目的账号模型限制

编辑 Codex API Key 账号时，在 `POST /api/admin/accounts/update` 的账号设置中附带 `connection`：

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
Codex OAuth 账号只接受 `connection: { transport }`，不接受 `baseUrl`、`apiKey` 或 OAuth token。
连接设置不能修改账号 ID、Provider 或认证类型，也不接受通用凭据文档。
凭据与设置在同一事务中保存，任一校验或持久化失败均不落库。
`GET /api/admin/accounts/detail` 对 API Key 账号额外返回 `credentialConfiguration: { base_url, transport }`；
OAuth 账号返回 `credentialConfiguration: { transport }`。响应不回显密钥或 token；不适用的账号省略该字段。
更新会推进凭据 revision 并失效目录与连接；旧版本会话不可静默续接到新上游

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
`accountId`，后端通过 `flowId` 中保存的目标绑定完成授权

流程只允许发起它的管理员身份继续；尚未完成的流程过期后需重新发起。
账号提交成功后，`complete` 保留 24 小时的完成结果。同一管理员使用相同 `provider`、`flowId`
重试时返回原账号结果，不再执行授权或更新凭据；新提交的 `callbackUrl` 和 `settings` 不会再次应用

### OpenAI 身份、额度与状态

- OAuth 文件导入接受 camelCase 与 snake_case 的三个 token 字段，内部统一保存为
  `accessToken`、`refreshToken`、`idToken`，不接受含义模糊的 `token`。
  仅有 refresh token 时先换取 access token
- 普通 OAuth 的身份补全复用官方 `token_data.rs::parse_chatgpt_jwt_claims`：优先解析 `idToken`，缺失字段再由
  `accessToken` 补齐；`email` 优先 JWT 顶层值、其次 `https://api.openai.com/profile.email`，用户 ID
  优先 `chatgpt_user_id`、其次 `user_id`。该路径不调用 `whoami`，也不信任导入文档顶层的
  `userId/accountId`
- `at-` 开头的 Codex Personal Access Token（PAT）可直接粘贴到现有 **AT 导入** 入口，或使用
  `{"accessToken":"at-..."}` JSON；也接受官方 `auth.json` 的 `personal_access_token` 字段
  （兼容 `personalAccessToken`）。导入时向 OpenAI auth 的
  `/api/accounts/v1/user-auth-credential/whoami` 验证令牌，以响应中的用户 ID、账号 ID 和套餐建立身份，
  `email` 可缺失；不从导入文件的身份字段或附带的 ID token 回退补齐。验证失败不导入，接口区分提示
  PAT 格式无效、被上游拒绝、验证服务不可用和身份响应无效，不回显令牌或原始上游响应。
  PAT 不保存附带的 refresh token、ID token 或推测的过期时间，不参加 OAuth RT 刷新；
  失效后需取得新 PAT 再导入
- 首次 OAuth 保留回调 `state`、PKCE 与官方 token exchange，并持久化 `idToken`、`accessToken`、
  `refreshToken`。刷新响应中的三个 token 字段均按官方语义独立轮换：返回新值时替换，省略时分别保留
  现值。重新授权也保留这些回调保护，但只轮换目标账号的 token。回调地址只承载 `code`/`state`，
  不以 host/path 形式作为拒绝条件
- OAuth 账号文件导入和 OAuth complete（包括重新授权）在 credential 提交后后台尝试一次额度观测，不等待
  观测完成才返回成功。观测失败只记录告警，不回滚已提交的账号；手工或后台 RT 刷新只更新 token，
  不隐式等同于手工额度刷新，也不更新既有账号资料或 OAuth principal。xAI 导入与 OAuth complete
  使用相同的提交后观察流程
- 同一 OAuth flow 不能并发完成；账号事务提交成功后保留可重试读取的完成结果。
  失败会释放流程占用，但上游 authorization code 通常只能交换一次；已完成 token exchange 而未提交账号时应重新创建 OAuth flow
- `GET /api/admin/accounts/quota` 只读取最后一次落库快照；`POST /api/admin/accounts/quota/refresh` 才访问上游。access token
  已过期时，额度刷新要求先走 credential 刷新或重新授权，不会拿过期 token 探测额度
- Codex 已耗尽账号首次可复核，在 `resetAt` 尚未到达或未知时按 30 分钟间隔兜底，也可发现
  官方提前重置。到达最早未恢复窗口的 `resetAt` 后，先等待 10 秒传播余量，再在下一轮后台检查
  复核；reset 后前 5 分钟最小查询间隔为 30 秒，之后仍耗尽则改为 5 分钟。正常账号有非零用量或触顶窗口时，仍在
  `resetAt + 2 分钟` 后复核，未更新则按 30 分钟间隔重试。后台每 30 秒检查条件，实际恢复时间还受
  排队、查询耗时和上游传播影响；查询失败也遵守间隔。无可用点数时，到期前仍按原窗口证据恢复；
  本次耗尽之后的已记录窗口 reset 全部到期后，新查询明确 `allowed=true` 可解除限制，即使用量仍为
  100%。未知 reset 或再次拒绝后残留的旧 reset 仍走原规则，不复用旧边界解封；时间到期本身不会
  解除限制，也不会将展示用量归零
- 账号 `enabled` 控制是否参与请求调度，不控制 OAuth 凭据续期。OpenAI 与 xAI 的停用账号仍按各自刷新
  策略维护 Token；刷新结果更新凭据状态，但不会启用调度。无 refresh token 或凭据已进入失效、无效、
  封禁状态的账号不参加后台自动续期
- `POST /api/admin/accounts/recover` 对停用账号只将 `enabled` 改为 `true`，保留已有额度快照、凭据、错误和 Redis
  cooldown；启用后仍按这些事实投影状态，不会把已有错误或耗尽改成正常。对已启用账号则执行强制恢复：
  清除 Redis cooldown 和已保存的额度/错误，恢复为可调度 credential。两条路径均不访问上游；强制恢复
  不验证上游账号是否已经恢复，下一次真实请求仍可重新写入失败事实
- 成功额度观测会 revision-fenced 写入 quota；明确 `Allowed` 投影为 `normal`，明确耗尽投影为
  `quota_exhausted`。额度观测不会清除凭据过期、无效或封禁事实；这些事实统一投影为 `error`，并由
  `errorReason` 区分。额度接口的 401/403 也不足以判定 refresh token 永久失效，credential 终态只由
  OAuth refresh 的明确永久错误写入
- xAI 额度百分比只读取 `creditUsagePercent`，缺失时保持未知，不由旧月额度与用量计算。
  预付余额保留上游账本符号，非零余额及剩余按量额度可独立证明可用；窗口已过期或缺少可用证据时，
  不凭旧快照恢复账号
- 正常 Responses 请求会解析上游响应的 rate-limit headers，合并进同一 quota 快照并同步状态。Free、
  K12 等套餐共用该状态机；套餐只参与账号展示和按套餐隔离的模型目录 cache，不存在 K12 专属额度路径
- 账号详情的 Token 统计、模型排行和列表 Token 汇总优先使用账号级周额度窗口，无可统计的周窗口时
  使用月额度窗口；`usage.windowKind` 随选中的窗口返回 `weekly` 或 `monthly`。查询边界
  严格为 `[resetAt - windowSeconds, resetAt)`，不是自然周/月或最近 7/30 天；额度刷新若返回了更早的
  重置时间，会按新边界重新聚合。没有边界完整、可归属到账号的周/月窗口时显示无数据，`usage.windowKind` 为 `quota`
  （周/月额度窗口），不回退到 5 小时、日窗口或历史累计。
  金额原值保持完整精度，USD 展示值
  小于 1 美元时最多保留四位小数，其余保留两位

### 周/月额度预测

`GET /api/admin/accounts/quota-forecast?accountId=...` 使用管理员鉴权，每次查询返回独立的预测结果。
该接口不刷新上游额度；需要重新观测时，先调用 `POST /api/admin/accounts/quota/refresh`，再查询预测

响应 `data` 包含 `accountId`、`generatedAt` 和 `forecasts`（`weekly`、`monthly`）：

- `targetDays`、`extrapolated`：对应周期存在真实账号级窗口时采用实际时长；缺少对应窗口时，使用
  可统计的周/月窗口按 7/30 天折算，明确标记 `extrapolated: true`。不把短期限流或模型专属桶当作账号容量
- `estimatedTokens` / `estimatedUsd` 及对应 `*Display`：本周期已记录用量加预计剩余量，
  公式为 `(本周期已记录用量 + 样本用量 × (100 - usedPercent) / sampledPercent) × 目标窗口秒数 / 源窗口秒数`。
  真实周期由上游额度重置边界定义，不按自然周/月累计；只有折算结果才乘以目标与源窗口的时长比
- `source`：源窗口名称 `label`、已用比例 `usedPercent` / `usedPercentDisplay`、
  额度观测时间 `observedAt` / `observedAtDisplay`、用于过期检查的 `resetAt`，以及本周期累计
  已记录的 `tokensDisplay` / `usdDisplay`。`source: null` 表示没有可选的源窗口。
  `observedAt` 表示额度观测时间，与本次查询的 `generatedAt` 不同
- `unavailableReason`：不能估算时的说明，正常为 `null`。有效进度少于 5 个百分点不预测；
  `lowSample` 表示有效进度不足 10 个百分点，或增量采样少于 2 个完整段，仅是质量提示，不承诺精度。
  `incompleteCost` / `incompleteTokens` 仅提示费用 / Token 记录存在缺口，不阻断已有数据的预测。
  Token 使用已记录的正数用量，费用使用已知的有效 USD 金额；对应数据完全缺失时该项为 `null`，
  两项均不可估算时才返回无可用数据的说明。明确记录的零 USD 仍可估算为零，未知费用不能当成零。
  不按请求数量补齐未知消耗；漏记、失败或缺少计量可能使估算偏低，不代表完整账单或真实剩余额度

`sampledPercent` 是公式中的有效额度进度（百分点），不是时间进度或响应字段

样本仅使用当前额度窗口内、账号加入后开始且截至观测时间已完整交付的请求，缓存命中不重复计入输入

重置或套餐变化后重新积累有效进度，中途加入的账号可在新基线后采样，无需等待下一次重置，职责与断点处理见[额度预测架构](architecture.md#额度预测)

预测不参与调度或账单结算，站外消耗、日志清理、异步延迟、未成功交付的上游消耗和模型组合变化均可能造成误差

等价 USD 费用不是订阅价格或额度承诺，30 天折算不是自然月额度，记录覆盖率也不代表预测准确率

### Codex 账号辅助请求

OAuth 账号的额度、个人资料、订阅与重置卡请求先使用 `openai.api.base_url`。自定义上游明确返回 HTTP 404 时，
仅回退一次到官方账号接口，沿用当前账号凭据与出站代理；其他状态码、解析错误或传输失败不触发回退。
部署者需允许账号请求访问官方端点；回退不绕过账号出站代理，也不保证官方可达。
回退后的响应或错误作为最终结果，不以原来的 404 覆盖。重置卡消费回退复用原始请求体和幂等键，
传输失败仍按消费结果不明确处理。API Key 账号不会因此获得 OAuth 账号能力，也不会改变推理请求的目标地址

### 账号个人信息

`GET /api/admin/accounts/personal-info?accountId=...` 需要管理员会话，提供 Codex OAuth 账号的个人信息。后端并发读取资料统计与订阅，
一次返回；每次请求均重新查询，不刷新 credential，不读取本地 usage/billing 记录，也不缓存或估算统计结果。
查询沿用上述 OpenAI 404 路由回退

响应 `data` 包含：

| 字段 | 类型 | 含义 |
| --- | --- | --- |
| `profile` | object 或 null | 官方个人资料、累计统计与活动洞察；查询失败为 null |
| `profileError` | string 或 null | 资料查询失败时的安全错误提示；成功为 null |
| `subscription` | object 或 null | 当前绑定账号的订阅周期；无可用周期或查询失败为 null |

两部分的查询结果独立：资料失败时仍返回可用订阅，订阅失败时仍返回资料。账号不存在、查询参数不合法或
无管理员会话时，仍返回标准错误；请求期间账号身份或 credential revision 改变时拒绝整份结果

#### 官方资料与累计统计

`profile` 包含：

- `displayName`、`username`、`imageUrl`：官方账号资料
- `summary`：累计文本 Token、单日峰值 Token、最长任务时长、当前连续天数和最长连续天数
- `dailyUsage`：按日期返回的 Token 活动
- `activityInsights`：快速模式占比、上游原样返回的推理强度及占比、Skill 探索/使用数、聊天总数，
  以及插件与 Skill 调用排行

官方未返回的字段保持 `null`，不使用本地数据补齐；`hasStatsError: true` 表示账号资料可用，但官方统计
部分不可用。access token 已过期或官方返回 401 时，通过 `profileError` 提示先刷新 credential 或重新授权

#### 订阅信息

OpenAI 使用当前凭据、绑定的上游账号 ID 和账号出站代理访问 `/backend-api/subscriptions?account_id=...`，
不枚举其他账号，不返回上游订阅 ID 或原始响应

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
revision 变化时丢弃结果

### 主动额度重置卡

`GET /api/admin/accounts/reset-credits?accountId=...` 每次都查询对应 Provider，不用本地计划代替库存。
当前由 OpenAI Provider 为 OAuth 账号提供，账号视图分别以 `resetCredits` 和
`consumeResetCredit` 表示查询和消费能力

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
结果自动创建新消费

若服务无法确认不可逆消费是否完成，返回 HTTP `502` / 业务码 `50202`；客户端应先刷新卡片与额度状态，
并在确需重试时复用原 `redeemRequestId`。明确的上游 HTTP 拒绝仍使用 `50201`，不会误标为结果未知

```json
{
  "accountId": "acct_...",
  "creditId": "credit_...",
  "redeemRequestId": "8fbf302d-11df-4bd5-82e4-08e4b3df7874"
}
```

消费响应只返回上游结果 `code` 和可选 `credit`。消费端确认成功后应重新 GET 卡片列表，并显式调用
`POST /api/admin/accounts/quota/refresh` 回读上游额度；未提供 `quotaRefresh` 能力时不发起该查询。
不得因为消费成功直接改写本地 `resetAt` 或解除冻结。xAI 不支持该能力

消费期间账号身份或凭据版本变化时返回结果未知，不将旧结果套用到新绑定账号

自动使用通过 `/api/admin/accounts/reset-credits/automation` 按账号开启，默认关闭；账号本身停用时暂停执行。
GET 和 POST 均返回 `enabled`、`leadMinutes`、`nextDiscoveryAtDisplay`、`nextConsumeAtDisplay`、`lastResult`，
时间展示沿用部署时区，无计划时为 `null`。运行设置 `resetCreditLeadMinutes` 默认 10，范围 1～1440 分钟

`resetCreditMaxWeeklyRemainingPercent` 是可选的全局剩余周额度上限，范围 0～100 的整数，默认 `null` 表示不限制。
设置后，每次新的自动消费前刷新账号周额度；剩余比例大于上限或无法确认周额度时，保留卡片并在到期前错峰重查，
等于上限时允许使用。仅采用账号级周窗口，不回退到短期、月度或模型专属额度；手动使用不受此上限影响

开启后在一分钟内错峰执行首次发现，此后按部署时区每天随机发现一次，普通发现失败也等待下一日。
已知且有明确到期时间的可用卡片在提前窗口内错峰执行，消费前重新查询；已在其他地方使用或已到期的卡片
会移出计划，查询失败不直接消费。上游返回 `already_redeemed` 后结束该次操作，不切换另一张卡重试。
无卡片 ID 或到期时间时不自动消费；每天发现一次可能错过两次发现之间发放并到期的卡片

`lastResult` 的 `pending` 表示消费结果待确认；重试沿用已持久化的幂等键，重启或关闭再开启也不换键。
`expired_unknown` 表示未确认操作的卡片已经到期，停止重试；`reset`、`already_redeemed` 表示对应终态，
`discovery_failed` 表示最近查询失败，`not_redeemed` 表示上游明确未完成消费。
`weekly_quota_above_threshold` 表示剩余周额度高于上限，`weekly_quota_unavailable` 表示暂时无法确认周额度；
`nothing_to_reset` 表示当前没有可重置窗口，这三种状态均保留卡片并稍后重查，其中 `nothing_to_reset` 的下一次消费使用新幂等键。
结果未知的未决操作继续使用原幂等键确认，不重新应用周额度上限；自动操作未决且对应卡片尚未到期时，手动消费返回 `409`

## 6. 账号分组

分组是 Provider-neutral 的账号集合；一个组可包含任意 Provider 账号，一个账号也可属于多个组。
分组详情和列表返回 `fastMode`，取值为 `default`（默认）、`enabled`（开启）、`disabled`（关闭）。
创建时省略默认为 `default`，更新时省略或 `null` 保留现值。
策略取自 Client Key 绑定的分组，包括已禁用分组；多分组冲突时关闭优先，其次开启，最后默认。
未绑定分组的 Key 跟随客户端，不按最终所选账号的分组判断

- 默认：保留客户端的 `service_tier`
- 开启：所选上游模型的目录声明 `priority` 时，将缺失、`null` 或显式 `default` 改为 `priority`；目录未声明则保留原值
- 关闭：将 `priority`（含 `fast` 别名）改为显式 `default`，继续处理请求

策略只作用于 OpenAI Responses 的顶层 `service_tier`，不改变 `flex`、`ultrafast` 等其他显式档位、嵌套字段或其他 Provider。
宿主策略在 request 中间件之后应用，形成 attempt 输入基线；attempt 中间件继续改写档位时，以改写后的实际发送值为准。
HTTP 和每个 WebSocket `response.create` 均使用请求开始时的分组策略，同一请求重试保持该策略；
HTTP 请求头及新建 WS 的握手提示按当时的最终出站档位构造；复用 WS 时不重发握手头，
每个 `response.create` 仍独立应用档位策略，请求档位统计与本地费用估算使用各帧的最终出站档位

分组详情和完整列表还返回 `allowImageGeneration`。创建时省略默认为 `true`，更新时省略或 `null` 保留现值。
Client Key 任一绑定分组（包括已禁用分组）为 `false` 时，禁止生图；未绑定分组的 Key 允许生图。
限制覆盖 Images 生成与编辑，以及 Responses 中的原生 `image_generation` 工具声明或显式工具选择，
HTTP 返回 `403` 和 `image_generation_disabled`，WebSocket 每个 `response.create` 独立校验并返回对应错误事件。
普通图片输入和 Codex 被动声明的 `image_gen` namespace 不触发拒绝；实际 Images 调用仍受限制。
请求采用开始时冻结的权限，重试期间保持不变；attempt 中间件添加生图工具也会在发送前被策略拒绝

| 方法 | 路由 | 主要 query/body | 说明 |
| --- | --- | --- | --- |
| `GET` | `/api/admin/account-groups` | `page`、`pageSize`、`search`、`enabled` | 分页查询分组；返回账号可用性、并发槽位（Redis 不可用时 `usedSlots=null`）及成功请求 USD 用量 |
| `GET` | `/api/admin/account-groups/options` | `page`、`pageSize`、`search`、`enabled` | 分页查询分组选择项；item 仅返回 `id`、`name`、`color`、`enabled` |
| `POST` | `/api/admin/account-groups/create` | `{ name, description, color, fastMode?, allowImageGeneration? }` | 创建空分组；`color` 严格为 `#RRGGBBAA`，返回时统一大写 |
| `POST` | `/api/admin/account-groups/update` | `{ id, name, description, color, fastMode?, allowImageGeneration? }` | 更新分组信息、Fast 模式和生图权限 |
| `POST` | `/api/admin/account-groups/enable` | `{ id }` | 启用 |
| `POST` | `/api/admin/account-groups/disable` | `{ id }` | 禁用；已绑定 Key 保持受限，不回退到全部账号 |
| `POST` | `/api/admin/account-groups/delete` | `{ id }` | 删除未被 Client Key 引用的组 |

写入返回 `{ id, record, configRevision }`。删除或已提交变更的展示数据暂不可用时，`record` 为 `null`；
客户端可重查列表获取展示数据

两种列表数据均为 `{ items, page, configRevision }`。完整列表 item 返回 `memberCount`、按 Provider 聚合的
`providerCounts` 和 `clientKeyCount`。查询分组成员使用账号列表的 `groupId` 筛选，
不提供独立的分组成员路由；账号的 Provider 不代表整个分组的 Provider。
`capacity.totalSlots` 为 `number | null`：`null` 表示可用成员中存在继承无限并发的账号，`0` 表示没有可用槽位。
分组容量只统计普通池，`capacity.usedSlots` 返回其实际在途数；Redis 不可用时为 `null`

分组费用按当前组成员实际完成请求的费用汇总，不按 Client Key 绑定的分组分摊。
账号属于多个组时，各组均包含该账号的请求费用；调整账号分组后，留存窗口内费用随当前成员关系重新汇总

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
必须立即安全保存

`providerRequestProfileOverrides` 按 Provider ID 保存身份选择，列表返回同名对象。创建时各项为完整配置对象，
未提供的 Provider 跟随通用设置；更新时省略某项保留原值，设为 `null` 清除该项覆盖。最多 256 项，每项最多 64 KiB。
独立配置整体覆盖通用设置，不逐字段继承；切换全局配置不会影响独立 Key。目录与选择规则见 [Provider 客户端身份](#provider-客户端身份)

兼容字段 `openaiClientProfileOverride` 和 `xaiClientProfileOverride` 分别对应上述映射中的 `openai`、`xai`。
创建时省略或 `null` 表示无覆盖；更新时省略保留、`null` 清除。与通用字段同时提交时必须一致，否则拒绝整个请求；
响应中的兼容字段从同一映射派生，不是第二份配置

密钥列表的 `search` 仅匹配名称和标签，不匹配密钥值或可见前缀；搜索不区分大小写，使用字面量包含匹配，`%`、`_` 和 `\` 按普通字符处理。
创建和更新时去除名称首尾空白，并按忽略大小写、首尾空格的名称查重，重复返回 `409`。
更新排除当前记录，并发写入同样执行查重；失败不会留下审计或配置版本变更。
历史重名数据不自动改名，已有凭据继续有效；再次保存时需使用未被其他密钥占用的名称

`customKey` 仅用于创建：省略、`null` 或空字符串时继续自动生成；非空时按原值保存，不追加前缀、
不截断、不修剪空白，也不要求固定长度。Key 须为 HTTP Bearer 可传输的非空可见 ASCII 字符，
支持标点，空格、控制字符和非 ASCII 文本会返回 `400`。重复 Key 返回 `409`，包括并发创建时。
应用不另设 Key 长度上限；创建请求和 `Authorization` 头仍受 Web 服务器及反向代理的通用大小限制。
更新接口不接受 `customKey`，防止修改策略时意外替换正在使用的凭据。
列表仅展示最多前 10 个字符，且至少隐藏一半字符；单字符 Key 的可见前缀为空。
完整值仍只通过创建和显式 reveal 返回，不进入普通 Debug 或审计

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

自定义 Key 的分组权限、日／周限额和并发规则按本平台配置执行，不导入其他平台的历史用量

金额字段为非负十进制字符串，最多 10 位整数与 10 位小数，`"0"` 表示不限额。
创建时省略金额字段默认为零；更新时省略或 `null` 保留当前值，修改限额不会清空已用金额。
`maxConcurrency` 和 `requestsPerMinute` 是非负整数，零表示不限

列表返回 `dailyLimitUsd`、`weeklyLimitUsd`、`dailyUsedUsd`、`weeklyUsedUsd`（均为字符串）、
`dailyResetsAt`、`weeklyResetsAt`（RFC3339 或 `null`）。
记账和限额比较保留完整精度。
日窗口按部署时区的下一自然日边界重置；周窗口从首次准入当天日界起持续七个本地日历日，到期后在下一次使用时重新开启。
切换部署时区不修改已打开窗口的起止点或金额，到期后续接窗口不与旧窗口重叠。
手动重置清零所选周期的已用金额并清除到期时间，保留限额上限和历史费用，返回 `{ id }`。
所选窗口的重置时间返回 `null`，下次使用时按新建 Key 的规则重新开启。重置前完成但延迟结算的费用不再计入所选周期，也不会开启窗口；
重置后完成的请求继续计费，包括重置时仍在进行的请求。操作保留管理员审计，不改变账号上游额度。
费用按请求完成时间归属窗口。并发按同一 Key 的执行中请求累计，包含 SSE 与每个 WebSocket
`response.create`；空闲连接不占名额，内部重试不重复占用。
修改 Key 策略对既有 WebSocket 连接的下一次请求同样生效，已开始的请求保持原有快照

运行设置可以分别启用 Key 与账号的有界排队。Key 并发满时按 Key 等待；OpenAI 优先亲和下会话请求优先主账号，
繁忙时先按调度策略选择可用账号，临时分流不改写会话绑定；没有可立即使用的账号时才按配置排队。严格亲和下已有绑定的后代线程只等待当前会话账号，
根线程迁移后跟随新账号。绑定缺失时，任何有可靠会话身份的请求都可按调度策略原子首绑，后续请求沿用该绑定

严格亲和下的后代线程始终启用账号排队，沿用 `concurrencyWaitTimeoutSeconds` 和请求截止时间；
`maxWaitingPerAccount` 为 0 时使用每队列 1,000 人上限，否则沿用配置的上限。
可识别来源的 Search、Images、Live 创建请求采用同样规则，身份与轮次关联见[会话绑定](architecture.md#6-路由账号范围与-continuation)。
插件显式选号须通过账号资格与租约检查，见[调度合同](../backend/crates/gateway-plugin/sdk/docs/capabilities.md#路由调度与观察)。普通请求关闭账号排队且没有可用容量时返回 `503` / `account_capacity_unavailable`，
与无候选账号的 `no_available_provider` 区分；排队不绕过账号范围、模型权限、RPM 或金额限额

OpenAI 会话因之前的上游 `cyber_policy` 拒绝而排除全部可选账号时，选号失败返回 `400` / `cyber_policy`，
提示停止反复重试该会话，不代表账号被封或服务整体不可用；固定账号请求只判断其绑定范围。
该本地失败禁止自动重放；会话排除期限为一小时，严格亲和后代线程仍按上述规则等待。
同次请求换号耗尽时保留已收到的原始上游错误；后续请求没有上游失败可交付时才使用上述本地提示。
已开始交付的请求沿用流内错误合同，不能再改变 HTTP 状态

xAI 由本地账号/模型冷却、无可选账号、并发容量不足及选号基础设施不可用引起的终态错误返回 `503`，
保留 `account_unavailable_error` 类型和各自的机器码；可用备用 Provider 仍按路由范围与换号预算尝试。
同次请求选号耗尽时保留最近一次上游失败，新的基础设施故障仍按自身原因报告

队列满返回 `429` / `concurrency_queue_full`，排队超时返回 `429` / `concurrency_queue_timeout`；
WebSocket 使用对应错误事件。等待期间不发送上游请求，取消后释放等待位置，排队重查不重复计入 RPM。
SSE 在 Key 准入完成并取得执行会话前不发送保活帧，此阶段保留 HTTP 错误状态；
取得会话后的账号等待受 SSE 保活覆盖，已开始交付的失败沿用流内错误合同

任一周期的已落库金额与当前进程已接管的待结算费用合计达到限额后拒绝新请求，已准入请求可完成并使金额超过阈值。
HTTP 返回 `429`，`error.code` 为 `key_daily_budget_exceeded` 或 `key_weekly_budget_exceeded`，
并附 `Retry-After`；WebSocket 每次 `response.create` 执行相同检查并返回协议错误事件。
只累计上游上报或按用量与模型价格计算出的 USD 费用；无法取得费用的尝试按零累计，
保留错误和用量诊断，不产生待核账记录或阻断。内部重试中已经取得的费用仍会累计。
预算检查超时、存储不可用或待结算队列无法接受新请求时返回 `503`、`key_budget_unavailable`

自动结算按网关请求 ID 幂等执行，响应交付不等待费用落库，额度查询中的已用金额在后台写入后可见。
账本独立于使用统计日志，记录保留至删除 Key，
不受 `usageRetentionDays` 影响

## 8. 运行设置

| 方法 | 路由 | 说明 |
| --- | --- | --- |
| `GET` | `/api/admin/settings` | 读取运行设置 |
| `POST` | `/api/admin/settings/update` | 原子保存运行设置；身份配置按显式提供的 Provider 项更新 |
| `POST` | `/api/admin/settings/location/detect` | 无请求体，检测网关服务器直连出口的位置，不保存设置 |
| `POST` | `/api/admin/settings/privacy/preview` | 使用管理员提供的样本预览隐私规则，不保存设置或样本 |
| `GET` | `/api/admin/settings/client-downloads/codex-desktop/windows` | 提取 Codex Desktop Windows 离线安装直链；`refresh=true` 强制刷新进程内短缓存 |
| `GET` | `/api/admin/settings/client-profiles/{provider}` | 读取 `openai` 或 `xai` 的可选配置和 `globalConfiguration` |
| `POST` | `/api/admin/settings/client-profiles/{provider}/preview` | `{ configuration }`，完整配置对象或 `null`（解析当前通用设置）；使用已加载版本与最近检查时间，不触发上游检查或保存 |
| `GET` | `/api/admin/settings/admin-api-key` | 只返回管理 API Key 是否存在 |
| `POST` | `/api/admin/settings/admin-api-key/delete` | 删除管理 API Key |
| `POST` | `/api/admin/settings/admin-api-key/regenerate` | 重新生成并一次性返回完整管理 API Key |

读取设置返回 `configRevision`。更新请求必须携带读取时的同一版本；版本比较、设置替换和审计在同一事务内完成。
并发修改导致版本过期时返回 `409`，不保存当前请求；读取最新设置并确认差异后再提交，不能静默重试覆盖。
成功响应返回新版本，后续请求使用发布后的快照

位置检测返回 `status: detected` 与 `location`，双栈出口时区不一致时返回 `conflict`，
无法获取出口或位置时返回 `failed` 与 `message`。检测忽略进程代理环境变量，复用代理探测的超时与并发限制；
检测结果须通过设置更新接口保存后才会生效

设置更新字段按用途分组如下：

| 用途 | 字段 |
| --- | --- |
| 并发控制版本 | `configRevision` |
| [客户端身份](#provider-客户端身份) | `providerRequestProfiles`、`openaiClientProfile`、`xaiClientProfile` |
| [请求位置](#请求位置) | `requestLocationEnabled`、`requestLocation` |
| [隐私策略](#隐私策略) | `codexPrivacyPolicy` |
| 模型映射 | `modelMappings` |
| 凭据刷新 | `refreshMarginSeconds`、`refreshConcurrency` |
| [并发与排队](#并发与排队) | `maxConcurrentPerAccount`、`maxWaitingPerKey`、`maxWaitingPerAccount`、`openaiGuardianReservedConcurrency`、`concurrencyWaitTimeoutSeconds` |
| [亲和与换号](#账号亲和与换号) | `openaiAccountAffinity`、`openaiSessionAffinityTtlHours`、`maxAccountRotations` |
| [调度策略](#智能调度) | `rotationStrategy`、`smartScheduling` |
| [请求解压](#请求解压上限) | `responsesMaxDecompressedBodyBytes` |
| [客户端门禁](#1-鉴权与公共约定) | `minCodexDesktopVersion`、`minCodexCliVersion` |
| [历史保留](#历史保留) | `usageRetentionDays`、`opsEventRetentionDays`、`auditRetentionDays` |
| [账号错误保护](#账号错误保护) | `accountErrorProtection` |
| [账号预热](#账号预热) | `accountWarmupEnabled`、`accountWarmupScheduleTime`、`accountWarmupModel` |
| [主动额度重置卡](#主动额度重置卡) | `resetCreditLeadMinutes`、`resetCreditMaxWeeklyRemainingPercent` |

### 历史保留

`usageRetentionDays` 控制请求明细及其用量统计的保留天数，默认 31，支持 1～31 的整数天数。
`opsEventRetentionDays` 与 `auditRetentionDays` 分别控制独立运维事件和管理审计，默认 30、90，均须为正整数

清理任务每轮读取已保存设置，按完成时间分批删除过期请求及关联事件，不删除运行中的请求；有界单轮结束后等待一分钟再检查。
缩短保留期后，已有过期数据逐批清理，清理结果不可恢复；Client Key 结算账本不受影响

统计在后台异步更新，清理期间可短暂包含已删除请求的统计贡献，投影完成后更新为剩余数据。
查询范围无需随保留期缩短，超出保留期的已清理数据不再计入结果

### 账号预热

定时账号预热默认关闭。`accountWarmupScheduleTime` 使用部署时区中的 `HH:MM`，
多个时段以逗号分隔，默认 `08:00`；`accountWarmupModel` 默认 `null`，开启前必须显式选择模型。
任务面向可用的 Codex OAuth 账号，跳过周额度耗尽及五小时窗口距离重置仍超过 30 分钟的账号。
不存在的本地时刻跳过，重复时刻只执行较早一次；执行进度跨重启保留，时钟回拨不补跑已领取时刻之前的时段。
只有收到响应成功终态才记为预热成功；预热不计入客户端业务用量

### 请求位置

`requestLocationEnabled` 是必填布尔值，默认 `false`：关闭时不覆盖客户端原有位置和时区；开启时使用已保存的
`requestLocation`。关闭不会清空自定义值，代理自定义位置仍优先。
`requestLocation` 是必填的完整对象 `{ country, region, city, timezone }`，不接受 `null`；初始保存值为
`{ country: "US", region: "Ohio", city: "Piketon", timezone: "America/New_York" }`。
字段约束与[代理位置](#独立代理管理--managed-proxies)一致。全局自定义开启后，OpenAI Responses 使用全局位置，
关联代理配置了自定义位置时优先使用代理值。保存后通过现有配置发布机制对新请求生效，
已开始请求及其重试保持同一份全局值；普通文本、绝对时间戳和数据驻留要求不受影响

### 隐私策略

`codexPrivacyPolicy` 为必填对象，默认 `{ enabled: false, onError: "skip_rule", rules: [] }`。
只处理经网关发送的 OpenAI Responses HTTP／WebSocket 和独立 JSON 端点请求，不控制客户端直接发送的遥测。
规则按列表顺序执行，关闭策略或单条规则会保留配置。没有字段保护名单，认证、会话、工具等字段也可配置；改写后对上游行为的影响由配置者承担

每条规则包含唯一 `id`、`name`、`enabled` 和非空 `steps` 数组。步骤按顺序执行，后续步骤读取前序步骤的结果，
共享规则开关和执行结果。每个步骤包含 `scope`、`selector`、`action`、`pattern`、`replacement`、
`value`、`replaceAll`、`caseInsensitive`、`multiLine`。请求与返回均使用 `steps` 结构

作用范围如下：

| `scope` | 选择对象 |
| --- | --- |
| `turn_metadata` | 请求头、正文及 `client_metadata` 中的 turn metadata 对象，各副本独立处理 |
| `desktop_git_context` | 自动附加的 Desktop Git 上下文中解码后的 JSON 对象 |
| `environment_text` | 已识别的环境上下文或 Desktop 时间上下文文本，`selector` 为 `$` |
| `request_body` | 最终出站 JSON 正文 |
| `request_header` | 最终业务请求头，`selector` 直接填写头名称，重复头逐个处理 |

JSON 路径支持 `$`、`.成员`、`["带特殊字符的键"]`、`[索引]`、`.*` 和 `[*]`，不支持递归和过滤表达式。
缺少字段视为未命中；类型不符属于执行失败。`regex_replace` 替换字符串，`set_value` 保持原字段类型设置 JSON 值，
`rename_key` 替换所选对象的直接键名，重名时失败；`remove_field` 删除所选字段或数组项，不能删除作用范围根节点。
删除动作的 `pattern: null` 表示无条件删除，字符串模式表示只删除字符串值匹配的字段或数组项。
请求头规则生成的值必须是 ASCII 文本，以保持 HTTP 与 WebSocket opening 的一致性；metadata 范围会自动转义 Unicode

例如，`turn_metadata` 范围的 `$.workspaces` 配合 `remove_field` 删除整个工作区映射；
`$.workspaces.*.associated_remote_urls.*` 配合 `remove_field` 和 `pattern: "private-org"` 只删除命中的远端；
`$.workspaces` 配合 `rename_key` 可改写工作区路径键名。环境文本替换必须保留 XML 包裹结构

管理端提供「统一工作区信息」预设，生成一条含多个步骤的规则，仅处理 turn metadata 的 `workspaces`：

- `/home/<用户名>`、`/Users/<用户名>` 及 Windows 用户目录中的用户名改为 `user`，保留其余目录结构和项目名
- Git 远端 URL 的账号或组织路径段改为 `alex`，保留协议、主机和仓库名，支持 HTTP(S)、SSH、Git 协议和 `git@host:owner/repo` 形式；多级命名空间及本地远端路径保持原值

提交、分支、工作区标识和会话关联信息保持原值。正文中的工作目录及其他执行路径也保持原值，
供客户端工具使用，仍可能包含真实用户名和项目名。安装身份由账号隔离逻辑处理，
UA、平台参数及自动 Git 上下文版本由 [OpenAI 上游客户端身份](#openai-上游客户端身份)管理。
保存后的规则按其 `steps` 执行，不随预设定义变化

一条规则内的所有步骤和承载副本共同提交或回滚。目标为 `null` 时仍遵循类型校验与失败策略，路径键重名时失败，不合并不同工作区

正则使用 Rust `regex` 语法，不支持前后顾或模式反向引用；替换内容支持 `${1}`、`${name}` 和字面量 `$$`。
`replaceAll` 控制首个或全部匹配，另支持忽略大小写和多行锚点。最多 32 条规则，所有规则合计最多 32 个处理步骤；路径最多 16 层、1024 字节，
模式与替换内容分别最多 4096 字节，固定值编码后最多 4096 字节。XML 上下文最多 10000 个节点，环境文本最多 64 层。
执行限制为正文 32 MiB、单目标文本及编码后的上下文 1 MiB、
累计处理文本 16 MiB、每次执行 10000 次访问与匹配步骤。
执行检查点响应请求取消，步数超限报告「规则执行步数超限」，不以固定墙钟时长判定规则失败

执行失败时，`skip_rule` 撤销本条规则所有步骤在所有副本上的修改并继续，意味着本条脱敏未完成；
`reject_request` 阻止发送并返回 `400` / `privacy_policy_rejected`，提示隐私规则处理失败，
不因该错误自动重试或换号。设置保存时校验所有规则，包含未启用的规则

预览请求为 `{ policy, body, headers, turnMetadata }`，其中 `headers` 为头名称到字符串数组的映射，
`turnMetadata` 为编码后的 JSON 字符串或 `null`。正文最多 256 KiB，请求头与独立 metadata 合计最多 256 KiB。
响应返回处理后的三个承载值，以及 `outcomes: [{ ruleId, matches, status, reason }]`；
状态为 `disabled`、`unmatched`、`applied` 或 `skipped`，错误不回显请求内容或表达式

### 并发与排队

`maxConcurrentPerAccount` 是默认账号并发上限，初始值为 5，取值 0～4294967295；`0` 表示不限制。
账号的 `concurrencyLimit: null` 继承该默认值，单独设置的正数上限仍优先生效。
无限并发仍统计在途请求，并遵守账号可用性与 Client Key 限制

`maxWaitingPerKey` 与 `maxWaitingPerAccount` 是全局统一的普通排队容量，取值 0～1,000，默认 0（关闭）；
每个 Key、每个账号各自独立计数，没有单对象覆盖字段。执行并发为 5、最大排队数为 5 时，
该对象最多容纳 5 个执行请求与 5 个等待请求。Key 并发为 0（不限）时跳过 Key 排队。
OpenAI 严格亲和下后代线程的会话账号等待不随普通账号排队关闭，见 [Client Key 等待规则](#7-client-key)。
`concurrencyWaitTimeoutSeconds` 取值 1～120，默认 30，从首次入队开始计时，密钥与账号两层共享该等待时限；
切换账号或内部重试不重新计时；若插件设置了请求总时限，等待也计入该时限。排队超时不用于中断已开始的上游生成。
设置更新请求须包含这三个字段，新请求使用更新后的快照

`openaiGuardianReservedConcurrency`（默认 0，取值 0～4,294,967,295）设置 Codex Guardian 自动审批的每账号独立并发，保存后对新请求生效。
Guardian 以 `subagent_kind` 或 `client_metadata.x-openai-subagent` 值 `guardian` 识别。取值 R 大于 0 时，
普通请求保留账号原上限 L，Guardian 单独计数并最多运行 R 个，两类请求互不占用名额，也不借用对方空位。
例如 L 为 10、R 为 3 时，可以同时运行 10 个普通请求和 3 个审批请求；普通并发不限时，审批仍受 R 限制。
设为 0 时关闭独立额度，Guardian 使用普通并发。开启账号排队后，审批请求单独按 FIFO 等待，
不受普通队列位置或单账号排队上限约束，仍受自身队列总等待容量与等待时限约束。
两类请求仍遵守账号可用性和 Client Key 限额。设置更新请求须包含该字段

### 账号亲和与换号

`openaiAccountAffinity` 控制 Codex 账号亲和，默认 `strict`（严格）：

- `relaxed`（宽松）：会话内请求直接按 `rotationStrategy` 选号，不优先主账号
- `preferred`（优先）：会话内所有可关联请求优先主账号，并发已满、额度耗尽、停用、限流冷却或不支持当前模型时临时分流，保留会话绑定，后续请求仍优先主账号
- `strict`（严格）：同一会话共用账号，后代请求等待当前账号并跟随根请求换号

插件显式选号及原生续写的状态归属约束在三种模式下仍然生效

`openaiSessionAffinityTtlHours` 是账号绑定及其会话关联的滑动保留时长，单位小时，默认 24，取值 1～720。
各类请求统一在发送前成功准入时按请求冻结值续期。
响应完成不回写或续期。调整时长不扫描已有 Redis 键，已有记录保留原到期时间，下一次成功准入时使用新值；
真正过期或丢失后按缺失绑定处理，存储读取错误不会被当作过期

`maxAccountRotations` 是单请求最大换号次数，默认 3，取值 0～31；0 表示不换号。
首次选号、同账号重试和选号时过滤不可用候选不计入换号次数，总路由尝试仍最多 32 次。
提高该值允许请求尝试更多账号，但不放宽安全重放或交付后的重试限制。
设置更新请求须包含账号亲和、亲和时长与最大换号次数，保存后对新请求生效，执行中请求及其重试沿用冻结值

### 请求解压上限

`responsesMaxDecompressedBodyBytes` 限制压缩 Responses HTTP 请求的解压输出，以及插件受管 HTTP
的实际出站正文（包括账号上游请求，无论是否压缩），单位字节，默认 67108864（64 MiB）。
必须为正整数，且可表示为进程平台的 `isize`；管理端以整数 MiB 编辑。
插件模型调用从同一设置派生内部请求编码预算，具体边界见 [SDK 模型请求](../backend/crates/gateway-plugin/sdk/docs/capabilities.md#key模型与模型调用)

保存并发布成功后，新请求使用新值；在途模型请求及其路由、调度、attempt 和重试回调沿用冻结预算。
显式 Key 的模型调用沿用当前父调用内绑定的快照，父调用结束后重新绑定时使用新值；无模型请求上下文的
受管 HTTP 调用读取当前发布值，无需重启插件。调小该值可能使插件上传或转发返回 413，且不发送出站请求。
调高上限会增加大请求的内存占用，不代表扩大模型 Token 上限、响应缓冲或整个进程的内存预算

### 智能调度

`rotationStrategy` 可取 `smart`、`quota_reset_priority`、`round_robin`、`sticky`

`smartScheduling` 是必填的完整对象，仅在内置 `smart` 策略下生效，切换其他策略时仍保存其值：

```json
{
  "loadWeight": 1.0,
  "quotaWeight": 0.8,
  "healthWeight": 1.0,
  "latencyWeight": 0.5,
  "resetWeight": 0.0,
  "queueWeight": 0.0
}
```

六项系数分别调整负载、剩余额度、健康、首个有效输出延迟、额度重置和排队压力的评分偏好，范围 `0～10`、最多一位小数，
至少一项大于 `0`。`0` 仅关闭对应评分维度，不放宽账号资格、额度或并发限制；系数不表示流量百分比。
缺字段、`null`、未知字段及无效数值均返回 `422`，不写入配置。读取设置额外返回只读
`smartSchedulingDefaults`，内容为上述默认对象，用于恢复默认与自定义状态比较，不可提交到更新接口

`resetWeight` 越大越偏向即将重置额度的账号，复用已有有效重置时间，未知或已过期时不加分。
`queueWeight` 只在没有可立即使用的账号、需要选择等待队列时参与评分，越大越偏向等待人数少的账号。
设为 `0` 时沿用最短队列规则；启用后结合其余五项评分选择队列。队列人数限于当前进程，只有开启账号排队才会生效，
不影响已入队请求的位置、队内 FIFO、容量上限或等待超时。两项默认均为 `0`

没有可用亲和时，在最高可用权重层内按配置评分。原生续写账号绑定仍是硬约束。
配置随运行设置原子保存和发布，新请求使用新值，已开始请求及其重试沿用原快照，无需重启

### 模型定价

定价接口独立于运行设置的整体替换，仅允许管理员访问。改价影响本地估算费用与 Client Key 金额限额，
不改变上游报告金额、订阅账号实际扣额或历史费用。请求开始时冻结价格，内部重试沿用同一份配置

| 方法 | 路由 | 请求与返回 |
| --- | --- | --- |
| `GET` | `/api/admin/settings/pricing` | 返回 `{ defaults, synced, overrides, syncedAt }` |
| `POST` | `/api/admin/settings/pricing/update` | `{ provider, models, change }`，成功返回 `{ saved: true }` |
| `POST` | `/api/admin/settings/pricing/sync/preview` | 无 body；返回 `{ prices, skipped }`，不写入配置 |
| `POST` | `/api/admin/settings/pricing/sync` | `{ preview: { prices, skipped }, models: { openai: ["gpt-5.4"] } }`；成功返回 `{ saved: true }` |

价目使用 `Provider → 精确上游模型 ID → { multiplierBps, bands }` 的映射。优先级为人工覆盖、已同步价目、
内置价目；按档位合并，不从客户端模型别名或响应模型猜测价格。`syncedAt` 为 ISO 时间或 `null`。
每个档位包含四个非负十进制字符串：`input`、`output`、`cacheRead`、`cacheWrite`，单位 USD / 百万
Token，范围 0～1000000、最多四位小数。`"0"` 表示免费，缺少整个档位表示继承；不能只缺少部分单价

`bands` 可用键为 `standard`、`fast`、`ultrafast`、`flex`、`long_standard`、`long_fast`、`long_ultrafast`、`long_flex`、`image`。
OpenAI 长上下文为输入超过 272000 Token，xAI 为输入达到 200000 Token；仍按 Provider 支持的
模型与服务档位判断是否能估算。OpenAI 内置 Ultrafast 价目覆盖 `gpt-6-astra` 与 `gpt-6.1-sol`。
xAI 不接受 `flex`、`long_flex`、`ultrafast`、`long_ultrafast`、`image`。图像端点的 `standard`
用于文本输入，`image` 用于图像 Token；不能用文本输出单价替代图像输出单价。用量无法完整拆分时不估算

`multiplierBps` 为 0～1000000 的整数，10000 表示 1 倍、0 表示本地估算为零，最大 100 倍。
它作用于本地费用及对应明细，独立于服务档位；缺少计价依据的请求即使倍率为零仍然是费用未知

`provider` 为内置 `openai` 或 `xai`，对应价目由 `GET` 返回的 `defaults` 提供。
`models` 为 1～500 个模型 ID，每个 ID 为
1～128 字节且不含空白、控制字符。`change` 为以下形式之一：

- `{ "action": "replace", "pricing": { "multiplierBps": 12500, "bands": { ... } } }`：替换选中模型的人工
  配置，未提供档位重新继承来源；内置和同步均未登记的模型必须包含 `standard`
- `{ "action": "multiplier", "multiplierBps": 20000 }`：设置目标倍率，保留已有人工单价；重复提交不连续相乘
- `{ "action": "reset" }`：移除人工单价与倍率，恢复同步价或内置价；仅有人工价格的模型恢复为未配置
- `{ "action": "delete" }`：删除非内置模型的同步价目与人工配置；批量包含任何内置模型时整批返回 400，
  即使该内置模型已有人工覆盖也不能删除。删除不影响历史账单，之后可重新添加或选中同步导入

一批更新原子提交并写审计，不覆盖未选中的模型。未知字段、错误类型和非法价格字符串等 JSON 合同错误
返回 422；Provider、模型 ID、批量数量、倍率上限和不支持的档位等业务校验错误返回 400。
models.dev 同步只导入可表示为当前文本 Token 计价的 OpenAI/xAI 模型；不完整价格、其他输出模态及
不匹配的上下文梯度在 `skipped` 中返回 `provider/model`。确认会重新抓取价目；若与预览不同返回 400，
需重新预览。来源不可用返回 502，已有价目保持不变。`preview` 必须原样提交，`models` 按 Provider
指定 1～10000 个待同步模型；空选择或未登记的模型返回 400。同步只更新选中模型的来源层，未选中模型
及所有人工单价与倍率保持不变。选中的已同步模型若不再出现在来源价目中，则移除其来源层，恢复内置价格；
没有内置价目的模型变为未配置

### Provider 客户端身份

`providerRequestProfiles` 是按 Provider ID 索引的通用配置对象，最多 256 项、每项最多 64 KiB。
更新时省略某项保留原值，对象替换该项，`null` 删除显式选择并使用 Provider 默认值；不清除其他 Provider 的配置。
配置内容由对应 Provider 校验，不包含账号凭据。Key 可通过 `providerRequestProfileOverrides` 整体覆盖所属 Provider 的选择

`openaiClientProfile` 与 `xaiClientProfile` 是同一映射的兼容字段：省略保留，不能显式提交 `null`；
与通用字段同时提供时必须一致，否则整次更新拒绝。读取响应从映射派生这两个字段，不维护平行状态

直接读取 `openai` 或 `xai` 的选项与预览。OpenAI 选项返回六个 `presets` 与 `maxVersionLag`，xAI 返回 `defaults`；
响应均包含 `globalConfiguration`

### OpenAI 上游客户端身份

`openaiClientProfile` 保存通用选择，首次默认 `MacOS · Desktop · 自动最新`。
兼容字段的更新语义见上节。初始化不读取 YAML 身份字段。
该配置作用于 Client Key 的 OpenAI 模型请求与原生模型目录，适用于 HTTP/SSE、WebSocket、Images 和 Search。
不改变 xAI、入站客户端版本门禁、账号认证或后台 Desktop 专属操作

自定义配置使用 `{ "mode": "custom", "userAgent": "完整 UA" }`。
已识别的 `Codex Desktop`、`codex-tui`、`codex_exec`、`codex_cli_rs` 前缀由后端解析 `originator` 和 Core `version`，
显式提供的配套字段必须与识别结果一致。未知前缀须另填 `originator` 和 `codexVersion`，这两个字段与 UA 一起发送。
UA 须为 1 至 4096 字节的单行可见 ASCII 文本，首尾不能含空白；`originator` 最多 128 字节，
`codexVersion` 最多 64 字节并须符合 SemVer。自定义配置不要求 Desktop 构建号，也不自动更新

没有 `mode` 字段的预设配置按以下合同解析，可选字段省略或 `null` 时使用所选预设参数：

| 字段 | 取值与语义 |
| --- | --- |
| `client` | 必填，`desktop` 或 `cli` |
| `platform` | 必填，`macos`、`linux` 或 `windows` |
| `versionMode` | 必填，`latest` 或 `fixed` |
| `versionLag` | 仅 `latest` 可选的滞后数量，1～10 的整数；`fixed` 必须省略或为 `null` |
| `cliEntry` | CLI 可选 `tui` 或 `exec`，省略或 `null` 保留 Core 默认身份；Desktop 不接受此字段 |
| `originator`、`osType`、`osVersion`、`arch`、`terminal` | 可选自定义参数，非空、最多 128 字节；只接受可见 ASCII，不能包含括号、分号、反斜杠及首尾空白 |
| `codexVersion` | `fixed` 必填的 Core SemVer；`latest` 必须省略或为 `null` |
| `desktopVersion`、`desktopBuild` | 仅 Desktop 的 `fixed` 模式必填，分别为数字点分版本和数字构建号；CLI 不接受这些字段 |

```json
{ "client": "cli", "platform": "linux", "versionMode": "latest", "cliEntry": "tui", "osType": "Alpine Linux", "osVersion": "3.24.1", "terminal": "xterm-256color" }
```

TUI 默认标识为 `codex-tui`，Exec 为 `codex_exec`，入口后缀使用同一次解析的 Core 版本。
`originator` 覆盖只更改产品名前缀和配套头，后缀仍表示所选入口。省略 `osType` 使用平台名称；
自定义运行环境在自动更新时保持不变。未指定 `cliEntry` 时使用 `codex_cli_rs` 默认值且不添加入口后缀

`terminal` 填写官方客户端的真实终端标记，如 `iTerm.app/3.5.0`、`vscode/1.99.0`、`Apple_Terminal`、
`Ghostty`、`WindowsTerminal`、`tmux-256color`、`xterm-256color`。省略时使用 `unknown`：
官方 Desktop 由 GUI 启动、无终端环境变量，其 UA 中的终端标记本就是 `unknown`，
因此 Desktop 预设保持默认即可；CLI 预设建议配置真实值，多 Key 可通过各自的画像覆盖差异化

`versionLag` 让 `latest` 模式跟随官方发布但滞后 N 个已观察版本采用，省略时采用最新版本。
档位基于网关观察到的发布序列解析：启动种子与历次 24 小时检查的核验版本按时间倒序构成历史，
取第 N 项；序列不足时回退最旧已核验版本，不会因此解析失败。历史仅保留在进程内，
重启后从启动种子与制品缓存重新积累

六套预设均支持自动更新：macOS Desktop 支持 arm64，Windows/Linux Desktop 及三套 CLI 支持 arm64、x86_64。
预设接口的 `automaticAvailable`、`reason` 表示当前组合的可用性；自定义架构可能使自动解析不可用。
每 24 小时后台检查官方稳定发布，失败保留同组合上次有效版本；固定值不受后台更新影响。
Desktop 的应用版本、Core 和构建号来自同一平台、架构的官方制品：macOS ZIP、Windows MSIX、Linux DEB。
Windows/Linux 通过 ETag 检查更新，未变化时复用已核验版本；CLI 依据官方 npm 稳定标签和对应平台依赖

预览返回 `configuration`、`source`（`global` / `override`）、`userAgent`、解析后的环境和版本字段，
以及 `versionSource`（`official` / `custom`）、`versionLag`、`verifiedAt`、`checkedAt`、`error`。
自定义预览中的 `recognized` 表示是否识别出配套请求头。
`verifiedAt` 只表示版本资料核验，不能代表自定义运行环境或 TLS 已核验；固定版本返回 `null`。
完整自定义配置不携带官方制品核验时间。
客户端画像配置控制应用层请求字段，不切换操作系统的 TLS 实现。默认 HTTP 使用 native TLS，
WebSocket 使用 rustls；配置自定义 CA 时 HTTP 也使用 rustls。TLS 指纹需按实际部署平台与传输路径核验。
未完成本次启动检查时 `checkedAt` 为 `null`。非法或当前不可用的选择返回 `400`，保存失败不提交其他修改

Responses 的自动 Desktop Git 上下文与请求头使用同一画像快照：已有 `appServerVersion` 同步为 Core 版本，
`appVersion` 同步为 Desktop 版本；CLI 或自定义 UA 未提供 Desktop 版本时使用 `null`。
只处理完整、可解析的自动上下文，保留 Git 状态、会话字段和普通对话，不补充缺少的版本字段。
该同步不依赖隐私策略开关；显式隐私规则在其后执行，仍可覆盖最终字段

配置在请求开始时冻结，Provider 首次解析的版本用于该请求的全部重试与换号。
已建立 WebSocket 的精确续写沿用所属连接；新请求使用保存后的选择

### xAI 上游客户端身份

`xaiClientProfile` 保存 Grok CLI 的通用身份选择；首次使用内置 `grok-shell / headless / linux / x86_64`
并采用自动更新版本。初始化不读取 YAML 身份字段，数据库已有选择时不覆盖。
更新设置省略该字段保留现值，不能提交 `null`

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
每个请求开始时解析并冻结身份，重试和换号沿用该身份；保存后新请求生效，密钥独立配置优先于通用设置

自动版本通过官方 npm 检查稳定版本，周期 24 小时；检查失败保留当前进程最近有效版本，重启以内置基线开始。
固定版本不受后台更新影响。预览返回配置、来源、最终身份字段、`userAgent`、`versionSource`、`verifiedAt`、
`checkedAt` 和 `error`；固定版本不附带官方核验时间。Dashboard 展示已保存的通用身份

### 账号错误保护

`accountErrorProtection` 是设置替换请求的必填对象，按管理员定义的上游错误规则累计失败，达到阈值后暂停该账号后续调度。
不预设错误类型，不改变原始错误交付、认证与额度状态、重试资格或续接规则，新安装默认关闭且规则为空

| 字段 | 默认值 | 约束与含义 |
| --- | --- | --- |
| `enabled` | `false` | 启用时至少有一条有效规则 |
| `rules` | `[]` | 最多 32 条，任意规则命中后累计同一账号计数 |
| `threshold` | `12` | 2～1,000，按普通请求的实际 attempt 计数 |
| `windowSeconds` | `600` | 60～3,600，每次匹配失败续期的计数有效期 |
| `durationSeconds` | `7200` | 300～604,800，冻结及探测失败后等待的秒数 |
| `probeEnabled` | `true` | 到期后要求探测成功才解除保护 |
| `probeModel` | `null` | 1～128 字符的探测模型名，留空选账号首个可用模型 |

规则形状为 `{ id, name, conditions }`，`id` 必须唯一，`id` 和 `name` 是 1～128 字符的无首尾空白、无控制字符文本。
每条规则有 1～8 个条件，条件全部满足才命中，至少含一个非 Provider 条件。每个条件为
`{ field, operator, values, ignoreCase }`，候选值之间为 OR，`ignoreCase` 默认 `false`

| `field` | `operator` | `values` 内容 |
| --- | --- | --- |
| `provider` | `equals` | Provider 标识 |
| `status` | `equals` | 100～599 的三位数字字符串，如 `"400"` |
| `code`、`type` | `equals` | 原始结构化错误码或错误类型 |
| `message` | `equals`、`contains` | 原始结构化错误信息 |

每个条件有 1～16 个候选值，每个 message 候选最多 1,024 字符，其余最多 256 字符。
规则标识、名称、候选文本与探测模型的 UTF-8 字节总量不超过 64 KiB，超限整次保存失败。
`null` 候选显式匹配字段缺失，与空字符串不同；`contains` 仅支持非空白文本，不接受 `null`。
`equals` 去除两端空白后比较，`ignoreCase: true` 仅忽略 ASCII 大小写，不使用正则或全文响应扫描。
message 的包含比较只检查 UTF-8 安全截断的前 8 KiB，等值比较针对完整字段

HTTP 拒绝使用原始上游状态码；SSE 或 WS 流内失败只使用错误事件明确携带的状态码，没有则缺失，
不会用握手成功的 200/101 或网关投影后的状态补齐。规则不匹配请求正文、普通输出、本地超时、连接失败或插件 RPC 故障。
探测和预热不参与计数，同一 attempt 命中多条规则只累计一次，真实重试属于新的 attempt

完整成功的普通调用清除尚未形成冻结的同期证据，未匹配错误不续期也不清零；计数口径不是严格的“最近 N 秒事件数”。
规则或保护参数改变后重新累计，无关设置变更保留计数。已有冻结保存触发规则与恢复参数，不被后续编辑改写。
无探测时到期恢复，有探测时到期仍阻止调度，探测失败按冻结时的时长顺延。关闭保护或探测后，已有冻结等待当前冷却结束再解除。
保护到期或探测解除不清除独立的上游限流或模型冷却；账号手动恢复沿用强制清除本地状态的语义，使旧在途反馈和旧探测失效

设置更新不接受 `accountAutoFreeze*` 字段，缺少 `accountErrorProtection` 或混用新旧字段均拒绝，避免旧客户端覆盖已保存规则。
已有部署与备份的旧保护设置清理见[错误保护配置升级](../deploy/README.md#错误保护配置升级)

### Windows 客户端下载

Windows 离线包接口固定解析 Microsoft Store Product ID `9PLM9XGG6VKS` 的 Retail 包，不接受调用方提供
产品 ID、上游地址、ring 或文件名。后端只返回通过包名、架构、Microsoft CDN host/path、scheme 和失效
时间校验的 `x64` / `arm64` MSIX 直链，不代理安装包字节。Store 内容通道返回 HTTP/80 临时地址时保留
原始 scheme，不强制改写为该 host 不保证支持的 HTTPS。动态链接不足 10 分钟即失效时不会下发；某个架构
解析失败时只将该架构降级到 OpenAI 官方 HTTPS 稳定 MSIX，并通过 `warning` 说明。Store 解析最多等待
30 秒，官方回退包通过最多 3 秒的 HEAD 请求补充大小，大小查询失败不影响下载地址。响应形状为：

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
      "sizeBytes": 780402688,
      "downloadUrl": "http://dl.delivery.mp.microsoft.com/filestreamingservice/files/...",
      "expiresAt": "2026-09-01T07:30:00Z"
    }
  ]
}
```

`source` 为 `microsoft_store` 或 `official_openai`。Store 的四段 package version 只用于下载展示，不参与
Desktop 三段 SemVer 门禁，也不会自动回写最低版本设置。门禁规则见
[鉴权与公共约定](#1-鉴权与公共约定)，解析器职责见 [架构文档](architecture.md#11-生命周期安全与恢复)

## 9. 备份

全部备份端点位于 `/api/admin/settings/backups/*`，使用管理接口响应信封，字段为 camelCase，响应带 `Cache-Control: no-store`

归档为 `.sql.gz` 业务备份，包含账号、Key、设置、插件、已用额度和已发布用量汇总，不包含请求明细与诊断/审计日志。
汇总范围和离线恢复限制见 [备份内容与限制](../deploy/README.md#备份内容与限制)；没有在线恢复端点

统计重建或时间桶尚未发布完成时，任务以 `backup.analytics_not_ready` 失败；统计就绪后重新创建备份

| 方法 | 路由 | 请求 | 说明 |
| --- | --- | --- | --- |
| `GET` | `/api/admin/settings/backups` | 无 | 读取存储配置（含明文 Secret）、验证状态与调度配置 |
| `POST` | `/api/admin/settings/backups/storage/update` | S3 配置 | 更新存储配置；`secretAccessKey` 为空字符串会校验失败 |
| `POST` | `/api/admin/settings/backups/storage/test` | 无 | 测试已保存的存储配置（Put/Head/Get/Delete 探针） |
| `POST` | `/api/admin/settings/backups/schedule/update` | 调度配置 | 更新 Cron 与保留策略 |
| `GET` | `/api/admin/settings/backups/records` | 查询参数 | 分页查询备份记录 |
| `POST` | `/api/admin/settings/backups/create` | `{ expiresInDays? }` | 创建手动备份，返回 `202 Accepted`；`expiresInDays` 为过期天数（0 或缺省表示不过期） |
| `POST` | `/api/admin/settings/backups/download-url` | `{ backupId }` | 创建 5 分钟有效预签名下载地址（仅 completed） |
| `POST` | `/api/admin/settings/backups/delete` | `{ backupId }` | 请求删除（进入 `deleting`，由 Worker 收敛硬删除） |

读取设置响应（Secret 以明文返回）：

```text
storageRevision, endpoint, region, bucket, accessKeyId, secretAccessKey, prefix,
forcePathStyle, verified, scheduleEnabled, cronExpression,
retentionDays, retentionCount, nextRunAt, nextRunAtDisplay,
lastVerifiedAt, lastVerifiedAtDisplay, updatedAt, updatedAtDisplay
```

更新存储请求字段：

```text
endpoint, region, bucket, accessKeyId, secretAccessKey, prefix, forcePathStyle
```

`secretAccessKey` 为空字符串会校验失败；由于 GET 会回传已保存的明文 Secret，保存时始终整体提交当前值。已有备份记录时，endpoint/region/bucket/forcePathStyle 不允许变化（存储身份锁定，`409`）；只允许轮换凭据与修改 prefix

保存相同配置保留验证状态、定时计划及配置版本。存储配置实际变化时，会同时使验证失效、暂停定时计划并清空下次运行时间；连接测试通过后需重新启用计划

更新调度请求字段：

```text
scheduleEnabled, cronExpression, retentionDays, retentionCount
```

`cronExpression` 为 5 段格式，按部署 `host.timezone` 解释；不存在的本地时刻跳过，重复时刻只执行较早一次。
调度时区没有独立请求字段。`retentionDays`/`retentionCount` 为 0 表示禁用对应清理。启用计划前必须已保存完整存储配置且通过连接测试

记录列表查询参数：

```text
page, pageSize, status, trigger
```

`status` 可取 `queued/dumping/uploading/completed/failed/deleting`；`trigger` 可取 `manual/scheduled`。记录响应字段：

```text
id, triggerKind, status, scheduledAt, objectKey, sizeBytes, sha256, attemptCount,
errorCode, errorMessage, startedAt, completedAt, expiresAt, createdAt, updatedAt
```

记录中的时间点同时返回对应的 `*Display` 字段

`expiresAt` 在创建时确定：手动备份来自 `expiresInDays`，计划备份来自当时的
`retentionDays`；到期后由 Worker 进入删除流程

连接测试响应：

```text
{ ok, stage, code, message }
```

`stage` 为 `putObject/headObject/getObject/deleteObject`。探测成功后以 `storageRevision` CAS 写入 `lastVerifiedAt`；测试期间配置变化则丢弃结果

备份错误映射（`AdminErrorCode` 既有体系）：

| HTTP | 场景 |
| --- | --- |
| `400` | 配置、Cron 或状态参数无效 |
| `404` | 备份记录不存在 |
| `409` | 已有活跃任务、状态冲突或存储身份锁定 |
| `502` | S3 兼容服务返回无效或失败响应 |
| `503` | PostgreSQL、`pg_dump` 或对象存储暂不可用 |

审计动作：`backup.s3_config_updated`、`backup.s3_connection_tested`、`backup.schedule_updated`、`backup.created`、`backup.download_url_created`、`backup.delete_requested`。审计详情与记录表均不保存 Secret、数据库连接串或预签名 URL query

## 10. Dashboard、用量与错误

| 方法 | 路由 | 说明 |
| --- | --- | --- |
| `GET` | `/api/admin/dashboard/summary` | Dashboard 汇总；支持 `kind` 与统一时间范围参数 |
| `GET` | `/api/admin/dashboard/trend` | Dashboard 趋势；`kind=usage\|latency\|errors` |
| `GET` | `/api/admin/usage/records` | 请求记录分页列表 |
| `GET` | `/api/admin/usage/records/detail` | 按 `id` 查询请求详情 |
| `GET` | `/api/admin/usage/records/summary` | 当前筛选条件的请求汇总 |
| `GET` | `/api/admin/usage/insights/overview` | 用量、成本与成功率洞察 |
| `GET` | `/api/admin/usage/insights/diagnostics` | 按维度聚合诊断 |
| `GET` | `/api/admin/operations/errors` | 运维错误分页列表 |
| `GET` | `/api/admin/diagnostics/export` | 按 `requestId` 或 `errorId` 下载诊断 ZIP 附件 |

用量汇总的 `averageLatencyMsValue` 为可空毫秒数，前端按持续时长格式化；无延迟样本时为 `null`，
`averageLatencyMs` 保留为兼容展示字段

### Dashboard 趋势指标

趋势摘要 `summary[]` 返回稳定的 `metric`、展示数值 `value` 和可空的 `ratio`，指标名称由前端本地化

| kind | metric |
| --- | --- |
| `usage` | `inputTokens`、`outputTokens`、`cachedInputTokens` |
| `latency` | `firstTokenP95Ms`、`latencyP95Ms`、`outputThroughputP50` |
| `errors` | `errors`、`successRate`、`requests` |

图表颜色、系列身份和选中状态使用 `metric`，不根据译文或数组位置推断指标

`points[]` 提供趋势图与首页卡片使用的请求数、Tokens、缓存比例、错误数、成功率、
`latencyValue`、首 Token P95、总延迟 P95 和输出吞吐 P50，以及时间标签与展示值
请求并发与内部调度等待通过用量洞察的 `performance.load` 查询

### Dashboard 客户端身份

`wireProfiles[].attributes[]` 返回稳定的 `key` 和 `value`，字段名称由前端本地化

- `client_identifier`：上游客户端标识
- `client_mode`：客户端运行模式
- `token_auth`：认证协议
- `version_policy`：版本选择策略，值为 `fixed` 或 `latest`

页面按 `key` 取值；状态判断使用原始值，不依赖界面译文

### Dashboard 容量与账号用量

Dashboard 的 `capacityInfo` 统计普通容量，不含独立审批池；`maxConcurrentPerAccount` 为默认账号并发上限，`0` 表示不限制。
`capacityInfo.totalSlots` 为 `number | null`；可用账号池含无限并发账号时为 `null`，此时 `availableSlots` 也为 `null`。
`usedSlots` 仍表示实际在途数，Redis 不可用时为 `null`；没有可用账号时 `totalSlots` 为 `0`

Dashboard 的 `accountUsage[]` 返回稳定指标 `metric=requests|tokens`、可空原始数值 `metricValueRaw` 和
可空 RFC3339 `lastUsedAt`；前端负责指标名称和数字本地化，相对时间按[页面时间合同](#页面时间合同)翻译 `lastUsedRelative`。
`requests` 使用代表性额度窗口的本地请求数，`tokens` 使用今日 Token；未知数值为 `null`，零值保留 `0`。
`metricLabel`、`metricValue` 和 `lastUsed` 保留给兼容调用方，当前管理端不解析或显示这些字段

`accountUsage[].notes` 为账号当前备注，无备注时为 `null`

`usageWindow` 复用账号额度窗口合同，缺失额度事实时为 `null`；窗口标签、百分比、触顶状态、重置时间
和本地用量由 Provider/Admin 投影。套餐缺失不表示免费套餐，显示时舍入的百分比不用于判断触顶

滚动窗口使用相应时间范围的本地用量，独立于 Dashboard 的今日统计范围

### 请求字段与筛选

Dashboard 默认 `period=today`，用量、诊断和错误查询默认 `period=7d`，范围参数遵循[页面时间合同](#页面时间合同)。
Dashboard 返回原始 `asOf` 及 `asOfDisplay`，作为当前数据的查询锚点

请求明细列表接受 `currentPage`、`pageSize`、时间范围、`provider`、`clientApiKeyName`、`accountName`、`model` 和 `status`。
汇总和洞察只接受时间范围与 `provider`；诊断另接受 `dimension`、`model` 和 `statusCode`。
各接口拒绝未知参数，明细筛选不改变顶部统计范围

请求列表的 `status` 可取 `success`、`error`、`all`，默认 `success`；未知值返回 `400`。
`success` 沿用成功用量记录范围，`error` 包含最终 `failed`、`incomplete` 的请求，
`all` 合并成功用量记录及全部非成功请求，包括执行中和已取消请求。每个请求只返回一行，
重试中间错误不改变请求最终分类；`logicalOutcome` 保留真实结果。状态筛选只影响明细列表，
不改变顶部统计，独立运维事件仍通过 `/api/admin/operations/errors` 查询

请求列表的 `errorCode` / `errorMessage` 提供最终请求的错误码与错误信息，未记录时为 `null`

诊断 `dimension` 可取 `model`、`account`、
`apiKey`、`provider`、`transport`、`failureClass`、`status`。诊断按请求量降序返回最多 100 项，
同请求量按维度标识稳定排列

`requestShare` 的分母为该维度筛选后、截取前的全部请求数；`failureClass` 只在带错误类型的请求内计算占比
`retryCount` 为额外执行尝试次数之和，`retryRate` 为发生过重试的请求数占该组请求数的比例，同一请求多次重试只计一次

使用记录列表和错误排查列表的 `accountPlanType` / `accountPlanTypeDisplay` 来自记录保存的账号订阅快照。
账号维度诊断使用该账号最近请求的订阅快照；未保存订阅时为 `null`

管理端请求列表及 Dashboard 最近请求中的 `accountNotes` 来自请求保存的账号备注快照，未保存时为 `null`

`/api/admin/usage/records` 与 `/api/admin/operations/errors` 的 `clientApiKeyName` 来自请求保存的 Key 名称快照，
未保存时为 `null`，不影响记录返回，不包含密钥原文

查询不关联当前配置，改名、修改订阅或删除配置不改变已有快照。补写的历史 Key 名称与订阅取自补写时仍存在的配置，
不保证等于请求发生时的值；无法补齐的字段保持未知

运维错误记录的 `errorDetails` 为受控诊断文本，无详情时为 `null`。
错误来源快照以 JSON 文本保存：`causes.messages` 按外层到内层排列本地原因，`causes.truncated`
标记来源链截断；存在附属清理失败时，`causes.cleanup` 保存各自的原因快照。
`upstream` 保存上游正文，`redacted` 标记令牌等敏感上下文的移除；没有对应来源时为 `null`。
稳定错误分类、原始上游 code 和该详情分别保留，不以公共错误文案代替原始原因。
详情不出现在 Key 用量接口和普通错误信封中；[诊断导出](#诊断与恢复关联)在服务端重新脱敏后包含已存详情。
记录仍受异步观测写入与保留周期约束

管理端请求列表与详情保留 `requestedModel`（客户端请求）与 `upstreamModel`（网关发送）

请求明细的 `clientApiKeyName`、`accountName` 按字面量包含匹配请求保存的历史名称快照，区分大小写，
不解析为 ID，也不依赖当前配置是否存在。`model` 按字面量包含匹配请求模型或上游模型，`%`、`_` 和 `\` 按普通字符处理

运维错误列表的 `search` 使用字面量包含匹配，`%`、`_` 和 `\` 按普通字符处理，不支持密钥值或可见前缀搜索。
请求详情通过 `/api/admin/usage/records/detail?id=...` 查询

### 记录范围与统计口径

OpenAI 推理强度优先取 `input` 中最后一条 `type: "configuration_update"` 项的 `reasoning.effort`，
原生续接未再次更新时沿用已关联响应的配置，否则取请求顶层 `reasoning.effort`。
该字段是请求观测，不改写上游请求；网关未持有的续接历史无法用于推断档位

请求健康可用性与服务成功率按 `成功 /（成功 + 服务失败 + 未完成）` 计算
未完成保留独立计数，客户端取消与调用方错误不计入该分母，中间尝试失败不重复计为最终失败请求
健康时间线以 15 分钟分桶，可用性不低于 95% 且有效样本不少于 10 个时显示稳定
没有成功且服务失败与未完成合计不少于 3 个时显示不可达，其他不足 10 个有效样本的时段显示低样本
有效样本为零时显示无有效样本，尚未到达的时间格显示未来

已进入模型执行会话、但尚未登记执行尝试的失败请求也进入错误及详情查询，
包含无可用账号、准备失败、启动/准备超时与取消。此时 attempt 数为零，未确认的 Provider、账号及
上游传输为空；可用模型执行 ID 在“全部平台”下查询，不从路由候选推断实际调用平台。
已登记的尝试即使尚未收到首个上游事件，也会保留真实 attempt。
鉴权、解析、路由和准入等入口拒绝不属于该范围；请求观测仍是可能延迟或丢弃的异步投影

性能洞察的 `performance.load` 和 `performance.points[].load` 使用相同的负载结构，分别表示查询范围汇总与时间桶指标：

| 字段 | 含义 |
| --- | --- |
| `averageConcurrency` | 按有效时长加权的平均请求并发 |
| `peakConcurrency` | 最大重叠请求数 |
| `admissionDecisionP95Ms` | 准入判定耗时 P95，单位毫秒，无样本为 `null` |
| `accountSelectionWaitP95Ms` | 账号选择等待耗时 P95，单位毫秒，无样本为 `null` |

并发按逻辑请求的执行区间计算，不因重试 attempt 重复计数。
区间从 `started_at` 到 `completed_at`，执行中请求按查询范围末端仍在处理计算；与时间桶重叠的请求参与该桶，
平均值按重叠时长加权，峰值取桶内最大重叠请求数，无请求的时间桶为零。全范围平均按有效查询时长加权，峰值取各桶最大值

汇总与洞察中的请求数与 outcome 分布覆盖筛选范围内全部请求；token、缓存、延迟与成本聚合仅统计
已完整交付客户端的成功推理响应。OpenAI 的 `generate: false` 连接与上下文准备记录归类为
`requestKind: "prewarm"`，不进入用量列表、账号用量或额度预测的 Token / 费用覆盖统计，但仍可按
请求 ID 读取审计详情，响应中的额度观测仍可用于预测配对。此分类以实际 `generate` 字段为准，
不能仅凭客户端的同名 metadata 或输出 Token 为零排除普通推理；其他 Provider 不套用该规则

`latencyMs` 从模型执行会话开始计到终结，包含账号选择、重试及流交付等待，不包含此前的入口解析、路由和准入。
`firstTokenLatencyMs` 与它使用同一计时起点，首字边界由 Provider 协议定义。
OpenAI Responses 在 HTTP SSE 收到首个非空且非 `[DONE]` 的 `data` 载荷、
或 WebSocket 收到首个非空响应消息时记录首字，不要求实际输出内容；
xAI 采用首个非空文本、推理或工具参数输出。对应事件缺失时保留未知，不用其他事件补齐首字。
请求级首字统一通过 `firstTokenLatencyMs` 返回，由前端格式化。
`latencyDetails` 的首事件、首推理和首正文时间也使用请求级起点，首推理与首正文仍要求实际内容，
连接、响应头等传输阶段耗时独立计量，不能直接相加作为总耗时

`latencyDetails.upstreamResponseMs` 是上游返回的本次响应耗时，与网关请求计时独立。
OpenAI 仅使用同一条 `response.completed` 中的 `created_at` 与 `completed_at` 时间差，
将秒转换为毫秒保存，不拼接前导事件、会话累计计时或本地观测。
字段缺失、时间倒序或时间跨度为零时保持缺失；未采集官方计时的记录不会用网关耗时补齐。
返回整数秒时间戳时，短请求受秒级取整影响，不能用毫秒单位推断来源具有毫秒精度

`latencyDetails` 还可包含以下上游性能指标，单位为毫秒，保留上游返回的小数。
OpenAI 从当前响应的 `responsesapi.websocket_timing.timing_metrics` 读取，
WebSocket 握手不主动添加专项计时请求头，客户端显式提供的开关保留原值。
各字段独立可选；响应开始前、终态后的计时和明确属于其他响应的事件不写入当前请求，重试时清空

`critical_path` 仅采集 `scope=response`、`coverage=complete` 且边界为
`actionable_output_item_done` 的响应级指标：引擎耗时取 `engine_wall_ms`，API 开销为
`responses_pre_inference_ms` 与 `responses_other_ms` 之和，缺失分项不按零补齐。
外层 `logical_turn` 累计时间不进入请求指标，也不用于补齐引擎 TTFT 或 Token 间隔

| 字段 | 上游口径 |
| --- | --- |
| `upstreamApiOverheadMs` | API 排除引擎与客户端工具时间后的耗时 |
| `upstreamEngineMs` | 响应级引擎耗时，取完整 critical path 的墙钟时间或直接返回的 Engine Service 时间 |

以上指标按上游内部口径展示，不相加重建总耗时，不替换请求级 `firstTokenLatencyMs`。
它们仅补充性能诊断，不改变列表与聚合统计的速率分母

列表与性能统计的速率为 `outputTokens × 1000 / upstreamResponseMs`，只在两者为正时计算。
输出 Token 保留上游用量口径，OpenAI 的输出已包含推理 Token，不重复相加或扣除。
该值表示上游响应创建到完成期间的平均输出速率，按官方时间戳精度计算，
不表示模型内部的纯解码速度，也不用于替代网关观测的首个输出等待与请求总耗时

### 诊断与恢复关联

详情接口按 `id` 可读取成功、失败或未完成请求，普通详情与列表不传输时间线。
管理员通过 `GET /api/admin/diagnostics/export` 下载 ZIP 附件，必须且只能指定
`requestId` 或 `errorId`；`errorId` 指运维错误事件 ID，无模型请求的后台错误也能导出。
响应类型为 `application/zip`，设置 `Content-Disposition: attachment` 与 `Cache-Control: private, no-store`。
包内 `diagnostics.json` 保存完整结构化证据，`summary.txt` 汇总请求结果、模型、时延、用量与缺口，
`README.txt` 说明证据边界；三个文件均来自服务端脱敏后的文档

诊断包 `schemaVersion: 5` 用于故障分析，不是备份或导入格式。后端按目标 ID 收集请求、尝试、
完整已存时间线、错误原因链、原始上游错误及恢复关联；前端只负责下载。
`requests[].evidence` 保存详情、`trace`、`relatedRequests`、事发配置版本和尝试原因；
`requests[].errors` 保存同一请求的错误记录，`error` 保存按事件 ID 选中的错误，该事件不在请求内重复导出。
`environment.scope = export_time` 明确版本、构建与部署信息是导出时状态，不能代替事发环境

`request.routed.data.accountPolicy` 保存执行时冻结的账号策略与预算。
`attempt.failed.data.recovery` 保存 Provider 的重放安全性和恢复意图；`retry.decided` 保存当时
实际参与决策的门槛、剩余预算和发送状态，`retry.policy` 保存宿主及插件策略的停止事实。
已有的 `provider.precommit.released` 数据保留释放原因、预取字节数与等待时间，不等同于下游已提交

Trace v3 为路由、账号选择、失败、重试、响应边界及终态事实预留独立的 64 KiB 预算，
普通事件另有 64 KiB 预算，总事件数最多 128。包含元数据的完整 JSON 快照最多 128 KiB，
生成与持久化共用同一上限；完整编码超限时先淘汰普通事件，再淘汰关键证据。
`stageCounts` 记录至多 64 类已采集阶段的次数，
事件淘汰不抹除阶段计数；`droppedEvidenceEvents` 与 `stageCountOverflow` 分别报告关键证据淘汰和阶段统计超限

OpenAI Responses 的 `provider.request.input` 与 `provider.request.outbound` 分别记录 Provider 收到的
已解析正文和最终准备发送的正文指纹，包括指令、工具、缓存键、模型设置与有序输入。
输入保留前 16 项的独立指纹、每 16 项一块的前 32 块指纹及全量输入指纹，超出分段范围明确计数。
出站关联头仅保存指纹，`provider.request.account_scope` 保存账号归属依据及状态处理前后的指纹。
这些观测不重组转发正文，准备发送不等同于确认发送，指纹也不证明上游缓存仍存在

`provider.response.usage` 从已解析的上游终态单独提取已知数值字段，区分缺失、无效和明确的零，
同时保存共享 parser 的解析结果；`request.usage` 保存最终提交持久化前的网关归集值。
`provider.response.parse_gap` 标记 JSON 或 canonical 投影失败，原始 wire 仍按原有规则透传。
上述关键事实随请求终态保存；未采集的事实无法由导出补回

采集阶段仍按有界 trace 合同处理未知 JSON 键和值，未知事件名称保留摘要；导出不会补造被省略的原文。
下载端按 Provider 凭据、管理认证与传输头合同移除令牌、密钥、认证头、Cookie 和 URL 认证部分，
移除账号联系信息及客户端 IP，保留关联 ID、错误 code/message、套餐、状态和重试事实。
OAuth 字段依据 CPR 导出结构及官方 Codex `TokenData`，不按 `token / secret / credential` 等泛称删除业务字段。
原始错误中可解析的嵌套 JSON 展开后脱敏；任意业务内容不属于凭据识别范围

收集共享 20 秒预算，最多包含 8 个关联请求和 128 条错误记录，优先保留主请求的最终错误。
导出另有文本、节点与深度预算，正文截断仍保留格式版本、目标和缺口；超限会写入 `exportTruncated`、`redaction.truncatedValues` 和缺口记录。
`availability.gaps` 明示未记录、淘汰、截断、读取失败与旧记录缺少的决策事实，`omitted` 说明主动省略范围。
异步观测尚未落盘、已过期记录、未采集的请求正文与上游内部状态无法由导出补回，`null` 不表示没有发生错误

错误记录中的“已自动恢复”表示系统关联到了后续成功请求，不会把原来的失败记录改为成功。
`upstreamSendState = ambiguous` 表示无法确认该次上游执行结果，不代表后续恢复请求失败；
恢复关联也不等于逐字节验证过两次请求正文

### 费用与服务档位

OpenAI 与 xAI 的本地费用估算按实际发送给上游的请求模型（`upstreamModel`）查价，结合响应中的实际
用量计算；客户端请求 A、路由后发送 B 时按 B 计价，响应返回 C 不改变计价模型。实际发送模型缺少
定价时不估算，也不借用响应模型的价格。Provider 明确上报的已计费金额仍优先于本地估算

xAI 的 `cost_in_usd_ticks=0` 表示上游没有报告费用，不证明请求免费；正数才作为上游账单。
未报告时，只有已知价格且能覆盖本次费用的请求才使用本地估算，未知价格或无法覆盖的托管工具费用保持未知。
动态 latest 与 beta/internal 别名不借用具体型号的内置价格；显式配置的模型价格仍可用于估算

OpenAI Responses 用量记录的 `serviceTier` 与本地费用估算统一采用 Provider 最终发给上游的请求
`service_tier`，不使用响应档位覆盖或回退。例如发送 `priority`、响应回显 `default` 时，仍显示
`Fast` 并按 Priority 价格估算。未发送档位时，`serviceTier` 保持缺失，展示与估算按标准档处理。
计费展示把 `priority`/`fast` 映射为 `Fast`，`flex` 映射为 `Flex`，缺失或 `default`/`standard`
映射为 `Standard`；其他非空值以首字母大写展示。各模型、档位及长上下文区间使用明确登记的价格；缺少对应
价格时不估算（包括 `auto` 和未知档位），不以固定倍数兜底。展示的倍率由所选档位与标准档位费用之比
计算，托管工具调用费不随 Token 档位倍增。
Provider metadata 分别保留 `requestedServiceTier` 与 `upstreamServiceTier` 供诊断；发送给客户端的
原始 `response.service_tier` 不变。用量中的 Fast 仅表示发送档位，不能证明上游实际加速，本地费用
估算也不能代替官方账单。档位与费用在请求记录生成时确定；查询不会回填历史档位或重算已存储费用

本地计价使用[模型定价](#模型定价)的生效规则。缺少内置、同步及人工价格时不生成估价。本地
费用明细、有效单价、服务档位与自定义倍率随终态记录持久化，后续改价和清除覆盖不重算历史明细。
记录没有费用快照时，按内置规则核对总额后补充拆分，核对失败只显示原总额。
`billing.longContextBillingApplied` 表示已应用长上下文价格区间，与服务档位、自定义倍率独立；
费用快照未记录该事实或只有总额时返回 `false`，不按当前价格倒推历史标识。图像明细通过可选的
`billing.image` 返回 `inputAmountDisplay`、`cacheReadAmountDisplay`、`inputPriceDisplay` 和
`cacheReadPriceDisplay`；存在该字段时，普通输入与缓存字段仅表示文本输入，输出字段表示图像输出

## 11. 版本、更新与重启

| 方法 | 路由 | 主要 query/body | 说明 |
| --- | --- | --- | --- |
| `GET` | `/api/admin/system/version` | 无 | 当前构建、部署模式和可用更新 |
| `GET` | `/api/admin/system/update/detail` | `refresh=true\|false`、`channel?` | 按临时通道读取或刷新 Release 详情 |
| `GET` | `/api/admin/system/update/events` | 无 | SSE 更新事件流 |
| `POST` | `/api/admin/system/update` | `{ targetVersion, channel? }` | 受理后台在线更新，返回 `202` |
| `GET` | `/api/admin/system/update/status` | 无 | 查询当前更新或回滚状态 |
| `POST` | `/api/admin/system/rollback` | 无 | 回滚到保留的上一版本 |
| `GET` | `/api/admin/system/restart/check` | 无 | 只读检查重启目标与启用插件 |
| `POST` | `/api/admin/system/restart` | `{ confirmation? }` | 复核兼容性风险确认并重启，保留插件启用配置 |

在线更新遵循[版本命名与升级规则](../deploy/README.md#版本命名与升级规则)。版本接口的
`updateChannel` 由当前版本推导，取值为 `stable`、`alpha`、`beta`、`rc`、`exp`，无法识别时为 `unknown`。
详情接口的可选 `channel` 仅作用于本次查询，不保存实例偏好；省略时按当前运行版本推导。
响应的 `policy` 包含本次 `channel` 与 `availableChannels`，普通实例可选 `stable`、`rc`、`beta`、`alpha`，
实验实例仅允许 `exp`。不可用通道返回 `40901`，未知枚举值返回参数错误。
执行时应同时发送页面确认的 `channel` 与 `targetVersion`，服务端冻结该通道并重新复核远端目标；
省略通道时按运行版本推导。版本摘要接口始终检查运行通道，不受临时查询影响。
检查与执行使用同一规则，禁止的通道转换、跨实验线、跨大版本、降级或同版本重装均以 `40901` 拒绝。
`hasUpdate=true` 仅表示当前构建支持在线更新，且存在允许的更高版本；`latestVersion`、`releaseUrl` 和
`notes` 对应这个候选。没有可升级候选时，`hasUpdate=false`、`latestVersion` 为当前版本，
`releaseUrl` 和 `notes` 保留当前版本的已发布 Release 信息；找不到匹配当前版本的 Release 时为空。
当前构建不支持在线更新时不查询 Release，`hasUpdate=false`、`latestVersion` 为当前版本，
`releaseUrl` 和 `notes` 为空，不支持原因通过 `updateSupported=false`、`unsupportedReason` 返回。
强制检查失败时通过 `warning` 返回错误，`hasUpdate=false`，不以旧缓存或“没有更新”掩盖失败。
普通查询可复用 20 分钟内的结果，缓存按通道隔离，较慢的旧检查不能覆盖新检查结果。下载时仍会校验目标资产、校验和及归档

更新 POST 在本地校验目标版本并持久化任务后返回 `202`，数据包含 `operationId`、`targetVersion`、
`deploymentMode` 和 `message`，只表示已受理。Release 查询、远端目标复核、下载、校验及文件替换在后台
执行，结果通过 `/update/status` 的 `operation` 查询：`status` 为 `idle`、`running`、`succeeded` 或 `failed`，
终态包含 `finishedAt`，失败原因在 `error` 中。SSE 的 `operationId` 用于关联进度；终态事件发出前状态已落盘。
连接中断不取消已受理任务；响应丢失时先查询状态，不自动重复提交。打开更新页面时也会恢复最近一次任务。
下载并解包后校验发行身份，插件版本不匹配不阻止更新或回滚，兼容性风险在重启前提示。
两条路径在文件交换前后复核全局配置版本，文件替换失败或取消时成组恢复二进制、Web 资源和官方插件目录

客户端重启前调用 `restart/check`，返回 `targetVersion`、`releaseManifestSha256`、`configRevision` 和 `incompatiblePlugins`，
每项包含 `instanceId`、`name`、`reason`，检查不修改插件状态。源码运行的目标版本与发行摘要为空，检查当前宿主兼容性。
存在不兼容插件时，客户端展示列表并取得确认后，将完整检查结果作为 `confirmation` 提交重启请求。
服务端在重启锁内重新检查，目标或配置变化返回 `40901`，须重新检查并确认。确认后保留启用配置，重启时不兼容实例保持未启动，不挂载能力或故障占位；插件额外认证和拦截停止生效，宿主原生认证继续执行。
没有兼容性风险时可省略确认，不切换插件版本、接受其他制品或改写配置版本

重启受理后，旧服务拒绝更新、回滚和重复重启，返回 `40901`；客户端等待新进程就绪后重新连接

状态响应的 `currentVersion` 表示已安装文件的版本，运行中的版本仍以 `/version` 为准。
`needRestart=true` 表示已验证的安装文件尚未在当前进程生效，此时应调用重启接口，不能重复发起更新或切换通道。
最近一次 `operation` 仅表示操作历史，成功记录不等于待重启，也不改变远端 `hasUpdate`。
手动部署后按实际文件校准 `currentVersion`；无法核实的回滚备份不返回 `previousVersion`。
运行期间的外部文件变动或不完整安装返回错误，不伪装成安装成功。
Host 关闭或任务取消会记录失败终态；状态查询会收敛无执行锁的遗留 `running`。
异常退出留下的锁仍遵循 30 分钟过期规则，未过期前不会抢占其他进程的操作。
实例升级和仓库发版见 [部署文档](../deploy/README.md#镜像升级与源码构建)

## 12. 插件管理

除明确标注的公开入口外，以下接口需要管理身份。图标读取及 12.2 中标注的原始响应接口直接返回资源、插件响应
或模型响应，其余接口使用统一管理响应信封。制品按摘要记录用户的完整信任决定；首次安装某个插件时会尝试创建默认配置，
配置完整则启用，缺少必填项则保留为待配置。操作流程见 [插件使用](plugins.md)，开发合同见
[SDK](../backend/crates/gateway-plugin/sdk/README.md)。不提供官方商店或第三方市场订阅接口

插件通过类型化公开服务维护请求明细、聚合统计并试算费用，接口与数值合同见 [SDK 用量维护与计价](../backend/crates/gateway-plugin/sdk/docs/usage-data.md)。聚合参与原生总量与趋势，不增加请求明细列表条目

### 安装包与来源

| 方法 | 路径 | 行为 |
| --- | --- | --- |
| GET | `/api/admin/plugins/artifacts` | 列出制品的 `metadata`、固定 `source`、`installedAt` 与可为 `null` 的 `acceptedAt` |
| GET | `/api/admin/plugins/artifacts/{sha256}/icon?theme=light\|dark` | 管理身份；按不可变制品摘要读取已校验图标；无图标返回 404 |
| POST | `/api/admin/plugins/artifacts/upload` | 上传原始 tar.gz 包体，最多 32 MiB；查询参数 `sha256` 必须固定已解析的包；确认信任并完成安装 |
| POST | `/api/admin/plugins/artifacts/upload/verify` | 同样接收原始包体，只读解析并校验，返回 `metadata` 与 `source`，不安装 |
| POST | `/api/admin/plugins/artifacts/install` | 从 URL 或固定 GitHub Release 下载、校验，确认信任并完成安装 |
| POST | `/api/admin/plugins/artifacts/verify` | 按远程来源只读解析并校验完整包，返回 `metadata` 与固定 `source`，不持久化或启动插件 |
| POST | `/api/admin/plugins/artifacts/accept` | 严格请求 `{ "sha256": "<小写 SHA-256>" }`，确认信任已导入制品并完成安装 |
| POST | `/api/admin/plugins/artifacts/delete` | 请求 `{ "sha256": "<小写 SHA-256>" }`，删除未被引用的制品 |
| POST | `/api/admin/plugins/releases/query` | 查询指定 GitHub 仓库的最新稳定版或明确 tag |
| POST | `/api/admin/plugins/updates/check` | 按插件已保存的来源和策略查询 Release，只读，不下载或切换实例 |
| GET / POST | `/api/admin/plugins/source-credentials` | 列出公开授权范围，或创建独立下载凭据 |
| POST | `/api/admin/plugins/source-credentials/delete` | 请求 `{ "id": "<凭据 ID>" }`，删除未被来源引用的下载凭据 |
| GET / POST | `/api/admin/plugins/update-sources` | 查询或显式改变插件后续安装的允许来源及更新检查策略 |

删除制品时，同一事务会清理该制品使用过且已无任何制品引用的下载凭据；删除插件的最后一个版本时，
同时清理其来源规则、更新检查策略及出站代理引用。其他版本共享的凭据、代理本身和审计记录保留，
独立创建但未被该制品使用的下载凭据不会被顺带删除。完整卸载后重新安装不受旧来源规则限制

#### 包信息

安装包使用的版本与字段规则见 [SDK 清单](../backend/crates/gateway-plugin/sdk/docs/manifest.md)。
插件 ID 由 `publisher + "." + name` 派生；`displayName` 和可选 `author` 仅用于展示

制品 `metadata` 返回 `pluginId`、`name`、`displayName`、`publisher`、可为 `null` 的 `author`、`version`、
`description`、`license`、`sha256`、`platforms`、可为 `null` 的 `icon`、`contributes`、
`configurationSchema`、`secretFields` 和 `stateNamespaces`。`contributes` 是以稳定 snake_case capability 为 key 的对象；每个值包含
`id`、`version`、`stages`、`inputFormats` 和 `outputFormats`。`id` 是实例绑定引用的完整贡献项 ID，客户端必须通过
当前制品的 `contributes` 映射确定其 capability，不能解析 ID 文本推测能力

#### 图标读取

`metadata.icon` 为清单中的包内路径或 `{ "light": "...", "dark": "..." }` 对象；未声明时为 `null`。
配置示例、支持格式与文件限制见 [SDK 插件图标](../backend/crates/gateway-plugin/sdk/docs/manifest.md#插件图标)

读取浅色主题图标使用 `GET /api/admin/plugins/artifacts/{sha256}/icon?theme=light`：

- `sha256` 为制品的 64 位小写十六进制摘要；`theme` 必填，值为 `light` 或 `dark`。单路径图标在两种主题下返回相同内容
- 成功返回图片原始字节，`Content-Type` 为清单声明的 MIME，不使用 JSON 信封、不转换图片格式。制品不存在或未提供图标时返回 `404`
- 响应包含 `X-Content-Type-Options: nosniff` 和 `Cache-Control: private, max-age=31536000, immutable`；缓存按制品摘要与主题区分
- SVG 使用 sandbox CSP 隔离，允许内联样式和 data 图片，禁止脚本执行与外部资源请求

客户端应将接口地址用于 `<img src="…">`，不内联 SVG，也不把清单路径拼成文件系统路径或公开 URL。
没有图标时由客户端显示通用图标

#### 解析与确认安装

本地包先调用 `artifacts/upload/verify`，远程包先调用 `artifacts/verify`。校验只返回包信息与来源，
不保存、不确认信任、不启动插件，也不代表实例配置或私有状态已经兼容。
修改文件、地址、Release、摘要、凭据或代理选择后必须重新校验；确认安装时固定校验返回的身份、版本与摘要

远程解析请求包含 `location`、可选 `credentialIds` 和 `outboundProxyId`，不需要预先提供 ID 或版本。
更新已有插件时可传 `expectedPluginId` 锁定预期身份；首次安装从包内清单读取身份，并按该身份复核已保存的来源绑定。
URL 与 GitHub 的 `location.sha256` 在解析时均可省略，由服务端计算实际包摘要；提供摘要时必须通过匹配检查

确认安装时，将解析返回的 `metadata.pluginId`、`metadata.version` 和 `metadata.sha256` 分别填入
`pluginId`、`version`、`location.sha256`，与原来的来源、凭据和代理选择一起提交 `artifacts/install`。
这三个字段在远程安装时必填，确保实际安装内容与确认的包相同。清单身份、版本、平台、摘要和包结构
必须通过校验；宿主范围与能力版本的兼容提醒见[创建与更新](#创建与更新)。
来源类型、地址或代理选择变化返回冲突，需要先显式修改更新来源。相同包及来源重复安装为幂等操作；
同一插件版本的平台产物不允许替换成不同内容。安装新版本保留已安装旧版本

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
敏感字段不复制默认值；按下文[能力绑定规则](#能力绑定与范围)为适用贡献项及阶段建立空范围默认 binding。
缺少必填项时 `configurationRequired=true` 且实例保持停用；配置完整且版本兼容时直接启用，宿主范围或能力版本不匹配时保留停用，等待手动确认。
非法 schema、类型错误或把敏感字段放入普通配置会拒绝安装。已有该插件实例时不额外创建或切换实例，
`defaultInstanceId=null`。相同摘要的接受和默认实例创建可重试，不产生副本

宿主发行目录导入的制品只写入不可变包和来源，`acceptedAt=null`；确认来源与版本后调用 `artifacts/accept`。未接受制品不能用于创建、更新或启用实例

GitHub 的 `location` 使用 `kind: "github"`、`repository: "owner/repo"`、`tag`、`asset`、
`allow_prerelease` 和 `sha256`。解析时优先使用显式摘要或 GitHub 的 SHA-256，缺失时读取同一 Release 的
`checksums.txt`；都未提供时计算下载内容的摘要供确认安装使用。显式摘要与 GitHub 摘要冲突时拒绝下载。
摘要用于内容一致性检查，不证明发布者身份，只应安装可信来源的插件

#### GitHub Release 查询

查询请求为 `{ "query": { "repository": "owner/repo", "tag": null, "allowPrerelease": false }, "credentialIds": [], "outboundProxyId": null }`。
`tag: null` 查询最新稳定版；预发行版须指定 tag 并允许预发行。响应包含固定 tag、产物列表、`queriedAt`
和 `expiresAt`。显式查询刷新成功结果，同一批并发查询共享结果；下载固定 tag 的产物可复用 1 小时内的元数据，失败缓存 30 秒；限流返回错误，不转换成空列表

#### 下载认证与代理

下载凭据创建字段为 `name`、`origin`、`pathPrefix`、`purposes`、`authentication`。
`purposes` 可选 `metadata`、`artifact`；`authentication.kind` 可选 `github`、`bearer`、`basic`、`header`，
分别携带 `token`、`token`、`username/password`、`name/value`。列表与创建响应只返回 ID 和授权范围。
每次重定向都按 origin、路径段边界和用途重新匹配凭据，重叠授权拒绝执行。
来源 URL 使用 HTTPS，不包含用户信息、查询参数或片段；回环地址允许 HTTP，供本机来源使用

查询、校验和远程安装的 `outboundProxyId` 指向已保存的出站代理，与下载凭据独立，公开来源也可选择代理。
省略或 `null` 表示直连，不继承进程环境代理；代理失败不会回退直连。查询与下载期间代理配置变化时返回 `409`，需重新操作。
查询缓存与限流按凭据及代理身份隔离。制品历史 `source.outbound_proxy` 只保存下载时的 `{ id, revision }`，不返回代理地址或认证信息。
仍被更新来源或制品引用的代理不能删除；修改后续来源不会清除已安装制品的历史引用

#### 更新来源与检查

修改更新来源请求为 `{ "pluginId": "acme.example", "source": { "kind": "github", "repository": "owner/repo" }, "policy": { "kind": "stable" }, "outboundProxyId": null }`；
其他可选来源为 `upload` 或带 `url` 的 `url`。`policy` 省略时为 `manual`；GitHub 还支持 `stable`（最新稳定版）和
`{ "kind": "pinned", "tag": "v2.0.0-rc.1", "allow_prerelease": true }`。其他来源只支持 `manual`。
来源、策略和代理选择一同持久化与审计；代理字段省略或 `null` 明确保存为直连。`upload` 和 `builtin` 不接受代理。
变更不改变已安装包的历史来源，不自动下载、接受制品或切换实例版本

检查更新请求为 `{ "pluginId": "acme.example", "credentialIds": [] }`，返回 `binding` 和 `release`。
服务端使用已保存的来源、策略和代理，不接受临时替换仓库或代理；查询期间来源变更返回 `409`，不可访问或限流仍返回错误。
`release` 仅是来源元数据，不表示插件包已完成身份、版本、平台和摘要校验或兼容风险检查；选择产物后仍须显式调用安装接口，
安装成功也不会自动修改现有实例的固定制品。手动策略和不支持 Release 查询的来源返回明确错误，不伪装为没有更新

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
`secrets` 可选。输入严格拒绝未知字段。`artifactSha256` 必须指向
已接受的制品。`configuration` 是匹配插件 schema 的 JSON 对象；敏感字段必须
放入 `secrets`，空对象清除全部值。省略时同版本编辑保留当前值；跨版本编辑优先保留目标版本快照的密钥，没有快照则保留当前值

创建可传 `creationId`（标准小写 UUID），同一草稿重试复用该 ID，已保存且内容不同则返回 409，不创建副本或覆盖旧配置。
相同内容的重试仍会重新准备并发布，不保证配置 revision 不变。更新可传 `expectedRevision`，与实例列表的 `revision`
不一致时返回 409，管理员应重新加载后确认。创建不能传 `expectedRevision`，更新不能传 `creationId`。
删除实例会一并删除其 secret、私有状态与版本配置快照，停用则保留这些数据

保存启用配置时，可传 `replaceInstances: [{ "id": "<旧配置 ID>", "expectedRevision": 1 }]`，
明确确认同时停用的同插件配置（最多 256 项，不得包含当前配置或重复 ID）。同一插件最多启用一个实例，省略且已有其他启用配置时返回 409。
不能将已有实例切换成另一个插件。
目标配置准备通过后，停用旧配置与保存目标在同一事务提交，旧配置及其密钥、私有状态保留。
待停用配置必须仍启用且 revision 与确认时一致，否则返回 409，重新读取并确认后再提交

列表项包含 `id`、`name`、`artifactSha256`、`enabled`、`configurationRequired`、`configuration`、
`secretFields`、`bindings`、`revision`、`running`、`publishedRevision`、`runtime`、`compatibilityWarning`、`loadError` 和 `apiDeprecations`。
`compatibilityWarning` 表示宿主范围或已声明能力版本不匹配，不自动启动；手动确认后可尝试本次启动，成功运行后仍保留提醒。
`loadError` 表示包结构、清单、RPC 或平台等加载检查失败，非空时拒绝启用且不能通过确认覆盖；两项无对应问题时为 `null`，停用实例也返回检查结果。运行故障通过 `runtime.failure` 返回，不改写 `enabled`。
未通过启动准入的实例没有执行贡献或故障占位。恢复诊断使用 `runtime.failure.code=version_incompatible`（可确认尝试）或 `package_unloadable`（不能加载），`running=false`，不改写 `enabled`

手动启动软不兼容实例时，先读取该实例及目标包的 `version-plan`，向管理员展示 `compatibilityWarning`。
确认后通过 `instances/update` 提交 `enabled=true` 和原样返回的 `compatibilityConfirmation`，其中包含 `hostFingerprint`、`instanceId`、`instanceRevision` 和 `artifactSha256`。
服务端复核宿主、实例配置版本和目标包，缺少确认或确认过期返回 409；包无法加载返回 400。确认仅放行本次目标实例，不持久化，不放行其他实例，下次宿主启动重新检查。
创建软不兼容实例时先保存为停用，再读取计划确认启动；停用请求不接受确认信息

`configurationRequired` 由当前制品 schema 与已保存的普通/敏感配置实时派生，不写入数据库；它只表示仍缺少必填值。停用实例也不能保存
类型错误、非法 schema 或把敏感字段混入普通配置

`apiDeprecations` 为仍受支持的旧接口提示数组，包含 `capability`、`version`、`replacementVersion`、`introducedIn`、`remainingReleases` 和 `migration`。
`introducedIn` 在首次正式发布前为 `null`；`remainingReleases` 表示还会支持的后续正式版本数，不计预发行。提示不改变启停状态或阻止安装，无弃用提示时为空数组

#### 版本切换与私有状态

私有状态通过 [SDK 回调](../backend/crates/gateway-plugin/sdk/docs/capabilities.md#状态日志与迁移)访问，不提供任意读写的管理 HTTP 接口。
升级若需要迁移，实例可能暂时停用；迁移失败且发生并发配置变更时保持停用，不覆盖管理员的新配置。
版本切换只接受同一插件已安装且已接受的制品，`expectedRevision` 必须等于实例列表中的 `revision`，过期请求返回 409。
`version-plan` 返回 `instanceRevision`、`artifactSha256`、`configuration`、`secretFields`、`bindings`、`restored`、`compatibilityWarning`、`compatibilityConfirmation` 和 `loadError`。
`restored` 表示使用了目标版本的配置快照，兼容字段适用于当前包和版本切换目标；无软不兼容时确认信息为 `null`。没有快照时只补充缺失的 schema 默认值，保留当前显式值；功能范围按能力与阶段映射到目标声明，
新能力使用默认绑定，已关闭的既有能力继续关闭，客户端认证仍须显式身份映射。草稿不代表已通过校验

`switch-version` 重新生成草稿并执行准备与提交，名称和启停状态保持不变。不兼容设置返回 400，启用软不兼容目标但缺少风险确认返回 409，当前设置不变；
可读取草稿并通过 `instances/update` 携带原 `expectedRevision` 提交修正。不会推断字段改名或丢弃未知参数与密钥

每个实例、制品摘要保存一份最近启用时提交的配置快照，包含普通配置、secret 与 binding，停用草稿不覆盖快照。
`rollback` 只接受有快照的较早语义版本，恢复该版本设置并重新检查平台、配置与私有状态，保留当前名称与启停状态。
快照与实例修改共享事务，不从审计日志重建，也不恢复私有业务数据；删除实例或制品时删除对应快照

#### 完整信任与受管资源

安装并启用插件意味着完整信任其代码与行为，安全由安装者承担。清单与握手不包含权限声明，宿主回调不按访问域或阶段授权，参见[插件信任说明](plugins.md#完整信任)

回调保持类型、期限、取消、实例 revision、资源归属和事务一致性检查。模型调用可显式选择 Key，按所选 Key 的准入、预算、用量与计费合同执行；省略 Key 时继承现有模型父请求身份

受管 HTTP 不限制插件目标地址段。宿主解析 DNS，连接保留原始 Host 与 TLS 域名校验；代理沿用账号配置，失败不自动回退直连，重定向不自动跟随

#### 能力绑定与范围

`bindings` 通过 `contribution` 指定清单已声明的贡献项，并包含该贡献项声明支持的 `stage`、顺序和失败策略；
尚未声明、尚未注册或阶段不匹配的贡献项不能启用。
首次安装生成的默认实例会为适用贡献项的每个声明阶段创建 binding；`observer` 分别创建完成与 WebSocket 两类事件绑定。
默认 `order=0`、所有范围数组为空；观察阶段使用 `observe`，重试阶段使用 `delegate`，其他阶段使用 `reject`。
`model_catalog`、`management`、`command_line` 和 `maintenance` 按清单声明注册，不接受 binding；
`frontend_authentication` 必须由管理员显式配置身份映射，也不自动创建

`middleware` v6 可绑定 `http`、`websocket`、`service`、`request`、`attempt` 和 `outbound`，按声明的挂载阶段分别配置。
`http`、`websocket` 和 `service` 不使用 Key、分组、Provider 或模型范围，这些数组必须为空；插件在处理器内匹配路径、消息或服务操作。
`request` 与 `routing` 支持 Key、分组和公开模型范围，Provider 范围必须为空；`attempt`、`outbound`、`upstream`、`scheduling`、`retry` 与 `observation` 可使用 Provider 范围

`outbound` 准备原生 Provider 交给 transport 的请求，覆盖 OpenAI Responses HTTP / WS、Images、Search、Live 创建及 xAI 生成与压缩。
它使用最终上游模型匹配范围，下一层返回准备后的请求，不执行发送；字节、headers、HTTP 方法和目标的合同见 [SDK 出站请求准备](../backend/crates/gateway-plugin/sdk/docs/capabilities.md#上游出站准备)。
该阶段不包括 OAuth、额度刷新等后台请求，也不包括由 `upstream_adapter` 自行执行的发送

`observer` 固定使用 `observation/observe`，必填 `event` 为 `request_completed` 或 `websocket_response`。
每种事件最多一条绑定，可分别设置请求范围；同一实例两类事件的 `order` 必须相同。其他能力不接受非空 `event`。
完成事件完整提供终态、用量、费用与耗时，订阅不裁剪字段

```json
{
  "contribution": "acme.request-observer.observer",
  "stage": "observation",
  "event": "request_completed",
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
模型范围匹配客户端请求的公开模型 ID。配置发布不改变在途请求的匹配范围，同一实例不会因命中多个组而重复调用

`frontend_authentication` binding 只能使用 `authentication` 阶段、`reject` 或 `delegate` 失败策略，并通过
`identityBindings: [{ "principal": "...", "clientKeyId": "..." }]` 明确映射外部身份；其他范围数组必须为空。
普通 binding 不能携带 `identityBindings`。已停用、删除或不再授权的 Key 不会因映射恢复访问

实例不为管理页面或命令行预绑定模型执行 Key。管理页面经 12.2 的模型桥在每次调用时显式提交 `clientKeyId`；
CLI 插件也须在具体调用中选择当前 Key。管理员身份本身不提升为推理身份；模型桥用 Key ID 选择身份，不在桥参数中传递 Key 明文

观察失败只记录诊断，不改变响应、路由、标准 Usage 或账单。完成事件最多派发一次；上游 WebSocket 事件按请求保序，
原始帧通过独立载荷传递，完成通知不等待消息队列排空。过载可能丢弃事件，不承诺进程崩溃后的持久投递。
尚未通过重新鉴权、未建立冻结请求上下文的失败不触发完成观察；下游消息改写使用 `websocket` 中间件，普通响应流改写使用请求中间件

#### 发布状态

启用或修改时先准备候选进程及目录，再提交和发布；失败不替换正在运行的集合。
切换后在途请求保留原版本，其引用排空后回收旧进程

| 字段 | 含义 |
| --- | --- |
| `enabled` | 期望启用状态 |
| `publishedRevision` | 当前已发布的全局配置版本 |
| `running` | `runtime.status == "running"` 的派生值；不代表插件业务或外部服务健康 |
| `runtime.status` | 当前进程观察到的运行状态，独立于期望启用和发布版本 |
| `runtime.actualRevision` / `actualArtifactSha256` | 当前发布集合持有的实例版本与制品摘要，无对应会话时为 `null` |
| `runtime.failure` | 脱敏后的 `{ code, message }`；没有已知失败时为 `null` |
| `runtime.drainingRevisions` | 仍被在途工作持有的旧实例版本 |
| `runtime.load` | 当前发布会话的资源采样；无对应会话或诊断不可用时为 `null` |

`runtime.status` 包括 `disabled`、`awaiting_publication`、`preparing`、`running`、`blocked`、
`preparation_failed`、`faulted`、`draining` 和 `unknown`。`running` 要求配置匹配、当前会话就绪且发布集合可服务。
已启用实例缺少运行诊断时返回 `unknown`，不根据发布版本推断运行正常，也不制造失败原因；已停用配置仍可确定为 `disabled`

`runtime.load` 与 `actualRevision`、`actualArtifactSha256` 指向同一会话，不汇总其他排空版本。
以下计数来自现有调用表和容量资源，采样不是原子快照，各项可能重叠，不能相加作为调用总数：

| 字段 | 含义 |
| --- | --- |
| `activeCalls` | 总在途名额，包含排队、等待取消确认及仍由响应流持有的调用 |
| `executingCalls` | 首结果前占用的执行名额 |
| `queuedCalls` | 等待首次执行的名额 |
| `resumingCalls` | 已挂起、正在等待恢复授权的调用 |
| `pendingCallbacks` | 驻留回调，包含已返回但插件尚未消费的结果 |
| `activeStreams` | 已交付首结果、尚未收到 End 的流；End 后仍待消费的缓冲继续计入 `activeCalls` |

### 12.2 管理扩展页面与原始 API

`GET /api/admin/plugins/extensions` 需要管理身份，返回统一信封，`data` 为当前已发布的扩展数组。
每项包含 `target: { instanceId, artifactSha256, revision }`、`name`、`configurationSchema`、
`pages`、`routes`、`resources` 和 `callbacks`。schema 不是实例配置，不包含 secret 值。
页面目录的 `pages[].title` 与 `pages[].description` 分别用于宿主页面标题、副标题；未声明副标题时为 `null`。
目录来自冻结的插件注册结果，并与当前持久化实例复验；停用、换包或修改实例后旧目录项立即不再可用

以下用 `{target}` 表示 `{instanceId}/{artifactSha256}/{revision}`，`{path}` 表示包内已声明的相对路径：

| 方法 | 路径 | 身份与响应 |
| --- | --- | --- |
| 已注册方法 | `/api/admin/plugins/extensions/{target}/api/{path}` | 管理身份；插件原始响应，不套管理信封 |
| GET | `/api/admin/plugins/extensions/{target}/resources/{path}` | 管理身份；已校验的静态字节 |
| POST | `/api/admin/plugins/extensions/{target}/models/responses?clientKeyId=<ID>` | 管理身份；原生 Responses JSON 或 SSE，不套管理信封 |
| POST | `/api/admin/plugins/extensions/{target}/callback-tickets` | 管理身份；统一信封中的一次性回调票据 |
| GET | `/plugins/resources/{target}/{path}` | 无须登录，但资源须声明公开且制品已被接受 |
| GET | `/plugins/callbacks/{target}/{path}` | 无须登录，但必须携带有效且未消费的 `state` 票据 |

实例目标失效返回 409，调用方应刷新扩展目录，不重放原操作。尚无可用发布视图时返回 503。
所有入口都要求当前实例仍启用、制品已接受且版本匹配；公开资源与回调按插件注册内容开放。
公开入口不是读取任意包文件的路径。
单项静态资源最多 1 MiB，总量最多 8 MiB；资源必须同时出现在制品清单和注册结果中

管理 API 转交方法、相对路径、查询字符串、完整请求头、宿主请求关联与原始正文，保留 Cookie、认证头、
重复头与非 UTF-8 值。仅接受注册的方法、路径和内容类型；查询最多 8 KiB，正文最多 1 MiB 且受运行帧预算约束，
GET/HEAD 不接受正文。响应状态码为 200–599，内容类型元数据须在注册列表内，204/304 不允许正文；
插件返回的完整响应头在 HTTP 语法校验后覆盖默认值。插件操作失败可能发生在副作用之后，不自动重试

插件原始响应与模型协议响应带宿主保留的 `X-CPR-Plugin-Response: 1`，插件不能覆盖此字段。
宿主管理错误不带此标记；页面桥据此区分管理信封与插件正文，仅宿主 `40101` 触发[统一会话恢复](#统一登录与会话)

动态 API 与登录回调设置 `Cache-Control: no-store`。静态资源成功响应设置 `private, no-cache` 与内容 ETag，
浏览器可保存正文，但每次复用前必须重新校验管理员身份（公开资源除外）、实例版本和资源授权；
`If-None-Match` 匹配时返回无正文的 `304`。停用、版本或授权变更后，旧目标仍被拒绝，不返回缓存命中。
两类响应默认设置 nosniff、no-referrer 与限制性 CSP，动态处理器可显式覆盖默认响应头。管理端页面使用隔离 iframe，
仅允许脚本，不具有父页面同源权限；联网、弹窗、表单、嵌套页面和顶层导航均不开放。
页面访问已声明管理路由须经过固定目标的宿主桥，不能指定其他实例或版本。插件页面作者合同见
[SDK 管理 API 与页面](../backend/crates/gateway-plugin/sdk/docs/capabilities.md#管理页面公开入口与-cli)

模型桥只接受严格的 `clientKeyId` 查询参数和 Responses JSON 正文；传输正文与解压后的正文均最多 8 MiB。
服务端先复核固定目标与实例 revision，再按 ID 查询当前 Client Key，并使用普通 Responses HTTP/SSE 执行链；
因此仍执行当前 Key 的准入、账号范围、预算、用量、计费与请求插件链，包括发起页面所属插件自己的适用 hook。
这不是嵌套的宿主模型回调，管理身份本身也不提供推理身份，Key 明文不会离开宿主

等待首帧、非流式生成和流式正文交付期间约每秒复核一次固定目标与实例 revision。实例停用、制品或 revision
变化以及访问资格失效会取消底层执行并停止响应。成功或协议错误沿用 Responses 内容类型，并返回可检索的
`x-request-id` 与 `x-gateway-request-id`。模型插件读取完整请求头，响应头不按插件身份裁剪；
页面桥传递浏览器 `Headers` 可读取的全部字段，浏览器自身对 Cookie 等字段的规则仍适用。
内置页面桥最多并发 4 个模型请求，每次拉取最多 64 KiB，总期限 10 分钟；页面在已收到响应后 30 秒不继续拉取会取消，
等待首个响应本身不受该空闲计时误杀

票据请求为 `{ "path": "oauth", "ttlSeconds": 60 }`，有效期只允许 1–600 秒；响应数据为
`{ "state": "<一次性不透明票据>", "expiresAtMs": 0 }`。state 绑定签发管理身份、实例、制品、版本与路径，
不得记录或截图。公开回调只接受无正文的 GET，查询参数必须恰好包含一个 `state`；过期、错路径、跨实例、
并发重复或已消费票据均拒绝。宿主在执行前原子消费票据，因此超时或插件失败后不能重放。
公开调用使用 `public_management` 阶段，插件可以使用宿主回调；票据仍用于关联和消费一次登录流程。
票据用于受控接收第三方返回结果，不代替敏感管理操作的身份校验
