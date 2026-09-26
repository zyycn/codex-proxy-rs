# 插件能力与回调

[返回 SDK](../README.md) · [清单与图标](manifest.md)

`contributes` 声明插件提供什么功能，`permissions` 声明插件需要访问哪些宿主资源。两者不是同一件事：
例如，管理页面声明 `management` 能力，需要调用模型时再声明 `models` 访问域。

## 类型化作者入口

启用 `io` feature 后，组合插件优先使用 `client::PluginBuilder`：

```rust,ignore
use gateway_plugin_sdk::client::{PluginBuilder, methods};

let plugin = PluginBuilder::from_json(include_bytes!("../plugin.json"))?
    .management(management_registration(), handle_management)?
    .command_line(command_registration(), handle_command)?
    .on(methods::ROUTE_MODEL, route_model)?
    .build()?;

session.run(plugin).await?;
```

`PluginBuilder::from_json` 与 `cpr-plugin package` 使用同一个作者清单规范化入口。构建器负责：

- 自动响应 `plugin.register`，注册内容直接来自规范化后的清单
- 按方法合同解码控制参数和二进制载荷，并在进入业务处理器前检查调用阶段
- 保留 `TypedCall` 中的调用上下文、宿主客户端和取消信号
- 按方法合同编码 `TypedReply`，流式结果继续复用有界 `ResponseStream`
- 在 `build()` 时核对能力与必需处理器，避免清单与分派表漂移

`management`、`command_line` 和 `middleware` 是常用组合入口；其余能力使用
`on(methods::..., handler)`。方法常量固定请求、响应、阶段与载荷位置，作者不手写 RPC 方法名。
状态迁移不是 capability：清单任一 `state[]` 声明了 `migratesFrom` 时，注册
`methods::STATE_MIGRATE`，构建器也会要求该处理器存在。

类型化合同故意区分三种数据：普通控制元数据、可能敏感的 JSON 二进制载荷、原始字节。
例如 CLI 参数、账号凭据和状态迁移批次使用二进制 JSON，管理 HTTP 正文保持原始字节。
错误地把敏感载荷放进控制参数会在业务处理器运行前被拒绝。

## 能力与方法

Provider 固定为宿主内置的 OpenAI 与 xAI。插件提供以下扩展能力：

| 能力 | 类型化方法 |
| --- | --- |
| `middleware` | `PluginBuilder::middleware` / `middleware.handle` |
| `model_router` | `methods::ROUTE_MODEL` |
| `model_catalog` | `PluginBuilder::model_catalog` / `methods::MODEL_CATALOG_REGISTER` |
| `retry_policy` | `methods::RETRY_DECISION` |
| `scheduler` | `methods::SCHEDULE_ACCOUNT` |
| `request_lifecycle`、`usage` | `methods::OBSERVE_REQUEST`，同一实例的终态订阅合并调用 |
| `web_socket_observer` | `methods::OBSERVE_WEBSOCKET`，线方法为 `websocket.response_event` |
| `frontend_authentication` | `methods::FRONTEND_IDENTIFIER`、`methods::FRONTEND_AUTHENTICATE` |
| `management` | `PluginBuilder::management`；公开回调另用 `methods::MANAGEMENT_CALLBACK` |
| `command_line` | `PluginBuilder::command_line` |
| `maintenance` | `methods::RECONCILE`；无需功能绑定 |

## 访问域

安装时接受清单声明的访问域，不存在另一套逐方法、逐用途或资源 ID 白名单。宿主仍会检查父调用、阶段、
期限、实例代次、资源归属，以及 Key、账号和 Provider 自身的业务规则。

| 权限 | 开放的宿主能力 |
| --- | --- |
| `network` | 通过 `host.http.*` 使用宿主受管出站网络；仍受统一代理、超时、大小和流控规则约束 |
| `models` | 列出非秘密 Key、按所选 Key 查询模型，以及通过 `host.model.*` 调用模型；调用可能产生消耗 |
| `accounts` | 查询账号、读取原始凭据及创建或替换账号；写入仍经过 revision CAS、审计和发布事务 |
| `data` | 在管理／命令／维护阶段只读全部账号的最小基础信息及已有额度观测，不包含凭据、写入或预测 |
| `requests` | 参与请求／响应处理、路由、调度、观察及亲和查询 |
| `public_endpoints` | 提供无需登录即可访问的已声明静态资源或一次性票据回调 |
| `groups` | 创建本实例分组，允许将所有现有及未来新增账号加入或移出这些分组，保留其他分组关系 |
| `keys` | 创建仅绑定本实例分组的 Key，返回非秘密身份；不授予其他 Key 的修改或明文读取权限 |

`host.log` 和本插件声明的 `host.state.*` 是基础设施，不需要额外 permission。公开调用阶段不开放任何
宿主回调；权限也不能把一个父调用的句柄、流或上下文转移到另一个调用。

## 宿主资源回调

所有回调都通过当前 `TypedCall.host` 发起，并受父调用截止时间和取消信号约束。

### 基础事实

纯展示页面声明 `management` 和 `data` 即可，不需要请求处理 binding 或 `accounts` 权限。
`data` 是独立的管理员授权域，只允许 `management`、`command_line`、`maintenance` 阶段使用；请求链、注册及公开回调均拒绝。
它可读取全部账号的下列最小事实，不继承或授予客户端 Key 的模型执行权。

| SDK 方法 | 回调 | 查询与结果 |
| --- | --- | --- |
| `call.host.account_facts(query)` | `host.data.accounts.list` | `AccountFactsQuery`：可选 `provider_id`、`cursor`，必填 `limit`（1～200）；按账号 ID 升序，`next_cursor=null` 表示本页已结束 |
| `call.host.quota_facts(query)` | `host.data.quota.get` | `QuotaFactsQuery { account_id }`：读取 Provider 现有观测，不访问上游刷新 |

类型在 `call::data`。控制参数为 `{}`，查询和结果使用二进制 JSON；结果固定 `schema_version=1`。
账号仅返回 `account_id`、`provider_id`、`group_ids`、`enabled` 和 `updated_at_ms`，不附带姓名、邮箱、令牌或代理信息。
额度仅返回观测时间与窗口的 `key`、`window_seconds`、`used_percent`、`reset_at_ms`。
时间均为 UTC Unix 毫秒，比例为百分数；未知值保留 `null`，不能解释为 0。`observed_at_ms=null` 表示没有可用观测时间，
不保证当前缓存新鲜，不提供历史快照或多个查询之间的原子一致性。

```rust,ignore
use gateway_plugin_sdk::call::data::{AccountFactsQuery, QuotaFactsQuery};
let page = call.host.account_facts(AccountFactsQuery {
    provider_id: Some("openai".into()), cursor: None, limit: 100,
}).await?;
for account in page.accounts {
    let quota = call.host.quota_facts(QuotaFactsQuery { account_id: account.account_id }).await?;
    // 插件自行解释样本的新鲜度并计算展示或预测结果。
}
```

已有 `usage` 观察提供请求最终用量、时间和结算事实；该投递有界、不是历史补偿接口。
本接口不提供 SQL、池汇总、健康分、额度预测或历史使用记录查询；插件结果保存在自身 `host.state.*` 中。

### 账号与凭据

`accounts` 域开放 `host.auth.list/get_runtime/get/save`：

- `list` 可用 `provider_id` 过滤并分页，返回不含原始凭据的运行投影
- `get` 的原始凭据只放在二进制载荷中
- `save` 新建时显式提供 Provider，由宿主生成账号 ID；替换时必须提交精确 credential revision

插件可以自行选择其业务需要的账号和 Provider，不需要安装器预先配置允许列表。宿主仍校验账号归属、资格、
凭据版本和并发事务；原始凭据不得进入日志、审计正文或控制元数据。

### 自有资源与维护

声明 `maintenance` 并注册 `methods::RECONCILE`，宿主会在实例启用发布、进程恢复及配置变更后调用
`plugin.reconcile`，并每 30 秒补偿一次。处理器必须幂等；调用可能重复，配置通知会合并，不表示逐条账号事件。
每个实例串行执行，不同实例相互独立；调用限时 30 秒，失败后等待 5 秒重试。维护失败不回滚已经提交的资源，
下一次对账继续补齐。只读校验、准备候选与 CLI 帮助不会启动维护；停用、替换和宿主关闭时取消旧任务。

维护阶段允许日志、私有状态，以及已授权的 `data`、`groups`、`keys` 回调；不开放网络、凭据或模型执行。
其他管理／命令入口也可使用下列资源方法：

| SDK 方法 | 参数与行为 |
| --- | --- |
| `call.host.ensure_group(GroupEnsureRequest)` | `resource_key`、`name`、`color`、可选 `description`；首次创建，之后返回已有 `{id,name,enabled}` |
| `call.host.change_group_members(GroupMembersChange)` | `resource_key`、`add`、`remove`；两列表合计最多 200 个账号 ID，不得重复或交叉，返回实际 `added`／`removed` 数量 |
| `call.host.ensure_key(KeyEnsureRequest)` | `resource_key`、`name`、非空 `group_resource_keys`（最多 64 个）、`max_concurrency`、`requests_per_minute`、`daily_limit_usd`、`weekly_limit_usd`；返回 `{id,name,enabled}`，预算零值沿用原生无限制语义 |

资源键使用 1～64 位小写字母、数字、`_`、`.`、`-`，首位为字母或数字，在实例和资源类型内唯一。
归属由宿主签发，插件不能指定其他实例。`ensure` 只在首次创建时使用属性，不覆盖管理员对名称、启用状态或限制的修改；
同名的管理员资源不会被接管，名称冲突会失败。账号已被删除时增量加入会忽略该 ID。
无实际变化时不会写审计或递增配置版本。创建与归属登记、成员变更、授权复验均在同一事务完成。

插件升级沿用实例资源；停用保留分组和 Key，其原生启用状态保持不变。删除实例解除归属，资源仍由管理员管理；
重新安装为新实例不会接管旧资源。管理员删除自有资源后，仍启用的维护逻辑可在下次对账重新创建。
Key 明文仍通过宿主管理面查看，插件模型调用使用返回的 Key ID。

典型处理器先确保分组存在，再通过 `data` 分页查询账号并增量补齐成员，最后确保 Key 存在。
安装 `groups` 域即授权纳入全部当前及未来账号；插件可按自己的配置筛选账号，但该筛选不构成宿主的权限边界。

### Key、模型与模型调用

`models` 域开放：

- `host.keys.list`：只返回 `{id, name, enabled}`，不返回 Key 明文、前缀或隐藏 scope
- `host.models.list`：以显式 `client_key_id` 查询该 Key 当前可见的模型目录
- `host.model.execute[_stream]`：交给 Core 的路由、准入、租约、重试、账本和计费链路执行
- `host.model.stream_read/close`：读取或关闭当前父调用创建的有界流

`management`、`command_line`、`authentication` 和 `observation` 阶段调用模型时，在
`ModelExecuteRequest.client_key_id` 中显式选择执行 Key；`request`、`attempt`、`routing` 和
`scheduling` 阶段将该字段留空并继承父请求身份，不能切换 Key。Key 的启用状态、模型规则、账号组、并发和预算
始终使用调用时的当前事实。
插件不能读取 Key 明文，也不能用一个 Key 查询结果替另一个 Key 执行。

模型事件使用 `call::model::ExecutionEvent` 的 `GPE1` 封套，将 canonical 事实与原始 wire 分段编码。
非流式模型结果通过 `ModelEventBatch` 的 `HME1` 有界二进制批次返回；流式句柄只在创建它的父调用内有效。
发起回调的插件实例在子调用中跳过，Core 还会限制递归深度、总数和并发，避免插件调用自身形成环。

`host.affinity.lookup` 属于 `requests` 域，只查询 Provider 已持久化的真实亲和键。命中只是账号偏好，
后续执行仍重新检查当前 Key 规则、账号资格和租约。

### 网络

`network` 域通过 `host.http.do/do_stream` 和流读取／关闭方法提供受管 HTTP。URL、header 和正文可能含敏感数据，
正文使用独立二进制载荷。宿主统一施加代理、超时、帧与流限制；插件不能绕过宿主端口取得额外调用身份。
地址被拒绝返回 `permission_denied`，期限耗尽返回 `timeout`，解析、连接或响应读取失败返回 `upstream`。
这些 HTTP 业务错误的 `message` 是宿主生成的安全分类提示，不包含目标 URL、凭据或上游错误正文，
插件可以在相应操作中展示；协议故障不应直接展示内部消息。Fake-IP 环境按[部署排障](../../../../../deploy/README.md#插件取文与-fake-ip-dns)
调整 DNS，不通过放宽地址范围解决。

## 请求扩展

### 洋葱中间件

`middleware` 的 `request` mount 在逻辑请求外层执行一次，`attempt` mount 在每次选定 Provider 和账号后重建。
作者必须在清单中显式选择一个或两个阶段。`PluginBuilder::middleware` 接收 `MiddlewareCall`，其中
`next.run(request)` 是 single-use continuation；响应按相反顺序穿过中间件。

请求正文和响应帧受 `requests` 域保护。没有正文时仍要区分“保持原正文”和“替换为空”：默认使用
`MiddlewareRequestBody::Preserve`，只有 `replace_body(Vec::new())` 才表示清空。Header 修改采用增量
remove/append，保留合法多值。`MiddlewareBody::map_frames` 复用会话的流控和唯一终态，不建立插件全局流表。

**不改写就 1:1 保留**：同一网关处理边界上，默认 `next.run(request)` 保留原始正文、未修改的 header 和响应流字节及顺序。
只解析 `request.body` 不会触发替换；SDK 比较原投影，宿主持有的隐藏 header 不会被脱敏视图覆盖。
只观察响应可使用 `response.body.inspect_frames(|frame| { /* 读取，不修改 */ })?`，保留来源信封、背压和取消语义。
这一保证比较插件前后的同一边界，Provider 自身的协议适配、两次调用的随机值和网络分段不在比较范围内。

需要声明功能转换时，在清单选择 `middleware.version=2`，普通 v1 插件仍可原样运行。
`request.declare_capabilities(CapabilityDeclaration { handled, required })` 随单次 `next` 发出，必须同时显式替换正文且具有 `requests` 权限。
当前仅允许 OpenAI 生成请求的 request 阶段，HTTP 与 WebSocket 共用规则；不扩展端点允许的协议或流模式。
`handled` 只允许列举改写前确实存在、改写后已从实际请求移除的 `tools`、`vision`、`reasoning`、`json_schema`，
插件须负责对应的提示词转换及普通响应、错误、流式响应还原。`required` 为额外上游需求，不能覆盖正文推导出的要求。
重复项、同一项同时出现在两组、空声明、v1 使用声明、attempt 阶段使用声明均会拒绝。
原始语义中未由插件承担的功能继续约束路由；实际正文重新加入某功能时，该需求仍生效。
原生续接不能由声明豁免，attempt 阶段的既有冻结与保护字段检查继续生效。

```rust,ignore
use gateway_plugin_sdk::call::middleware::{CapabilityDeclaration, RequestFeature};
// 将 JSON Schema 转为提示词前保留原 schema，响应返回时按它验证并还原。
request.replace_body(convert_schema_to_prompt(&request.body)?);
request.declare_capabilities(CapabilityDeclaration {
    handled: vec![RequestFeature::JsonSchema], required: vec![],
});
let response = next.run(request).await?;
restore_and_validate_response(response).await
```

仅实现中间件时也可使用轻量的 `MiddlewarePlugin`；需要与管理或其他方法组合时使用
`PluginBuilder`，两者复用相同的 `MiddlewareCall` 和 `MiddlewareResponse`。

### 模型目录

`model_catalog` v1 在 registration 阶段一次提交 `ModelCatalogRegistration { models: Vec<ModelAlias> }`，不需要请求级 binding。
每条 `ModelAlias { id, provider, model }` 定义公开 ID、内置 Provider（`openai` 或 `xai`）及直接上游目标。
上游目标须已存在于该 Provider 的当前目录；同名原生模型、静态映射、其他插件条目、别名链和循环均拒绝发布。
每实例最多 256 条，整个二进制 JSON 最多 64 KiB；不允许覆盖原生元数据或自行宣称目标不支持的功能。

```rust,ignore
use gateway_plugin_sdk::call::catalog::{ModelAlias, ModelCatalogRegistration};
let plugin = PluginBuilder::from_json(include_bytes!("../plugin.json"))?
    .model_catalog(ModelCatalogRegistration { models: vec![ModelAlias {
        id: "team-default".into(), provider: "openai".into(), model: "gpt-target".into(),
    }] })?
    .build()?;
```

有效目录、路由和目标能力随同一个不可变快照发布，`/v1/models`、详情、原生目录和 `host.models.list` 共用该事实及现有 Key／账号模型范围。
原生别名继承目标的完整可公开对象；更新失败保留旧快照，禁用后新请求不再使用该别名，在途请求持有旧代次。
v1 是直接别名，动态选择可组合 `model_router`；实际路由仍须通过宿主的权限和能力校验，不提供多目标虚拟模型注册合同。

### 重试决策

`retry_policy` v1 固定绑定 `retry` 阶段，故障策略只能为 `delegate`。使用 `methods::RETRY_DECISION` 处理 `RetryDecisionRequest`。
输入包含失败分类、必要状态码、发送状态、attempt 序号、Provider／上游模型、剩余路由次数和期限，以及 `allowed_actions`。
不发送账号凭据、请求正文或上游错误转储。路由次数不等同于 Provider 自有传输恢复预算。

返回 `RetryDecision::Delegate`、`Stop` 或 `Retry`。`Retry` 必须出现在宿主允许动作中，并且仅继续宿主已经选定的恢复路径；
不能指定账号、延迟、目标或提高预算。响应已交付、发送不明确、外部副作用及续接绑定等既有安全门不可绕过。
Core 处理建立响应流前与流中的失败，并在策略返回后再次检查期限、取消和副作用。

```rust,ignore
use gateway_plugin_sdk::{client::TypedReply, call::policy::RetryDecision};
let plugin = PluginBuilder::from_json(include_bytes!("../plugin.json"))?
    .on(methods::RETRY_DECISION, |call| async move {
        let decision = if call.request.upstream_status == Some(429) {
            RetryDecision::Stop
        } else { RetryDecision::Delegate };
        Ok(TypedReply::new(decision))
    })?
    .build()?;
```

按 binding 顺序、插件 ID、实例 ID 确定委托链，首个合法非委托结果生效。整条链最多等待 2 秒，并受请求剩余期限约束。
退出、超时、未知字段及不允许的动作委托后续处理，最终回到宿主决定；故障不改写原始上游错误。
本阶段宿主只开放日志与插件私有状态回调，不允许额外网络、模型调用或账号访问。middleware 的 `next` 仍最多调用一次。

### 路由、调度与观察

`policy.route_model` 和 `policy.schedule_account` 只提出决定，宿主随后复核 Key 规则、模型能力、账号资格和租约。
`policy.observe_request` 接收一次最终请求事实；`request_lifecycle` 提供终态，`usage` 提供最终标准化用量、费用和耗时。
缺失值表示宿主没有该事实，不等于零，也不能由插件重新计价。

`websocket.response_event` 只读观察真实上游 WS 事件，元数据与原始帧分开传输。事件序号在请求内单调递增；
有界队列丢弃可能产生间隔。观察超时、过载或插件失败不改变业务响应，插件也不能修改 WS 帧。

观察计划在请求开始时冻结，最终事件最多派发一次。`Handshake`、`Message`、`Frame` 和 `PluginFault` 的
调试输出会隐藏配置、载荷与错误正文；插件自己的日志同样不能输出凭据或完整请求。

## 状态、日志与迁移

清单 `state[]` 声明命名空间、`schemaVersion`、JSON schema、记录与字节配额。运行时用
`host.state.get/put/delete` 访问；创建要求键不存在，更新和删除必须提供精确版本。状态始终绑定当前
插件实例、制品和 schema，不能直接读写宿主数据库。

不兼容升级由宿主停用并排空旧会话，再调用 `plugin.state.migrate` 分批迁移到暂存状态。插件逐项返回
keep、replace 或 delete；全部完成后宿主才原子提交新配置与状态，失败时丢弃暂存结果。

`host.log` 接收固定事件名、级别和有界字段。宿主脱敏并限制速率；`recorded: true` 只表示已交给有界日志任务。
日志和私有状态无需 permission，但注册、配置与公开管理阶段仍受各自的回调生命周期限制。

## 管理页面、公开入口与 CLI

`PluginBuilder::management` 同时冻结 `ManagementRegistration` 并绑定 `management.handle`。请求元数据在控制参数，
原始 HTTP 正文在二进制载荷；响应同样分离。宿主不转交管理 Cookie、Authorization 或任意 header，插件也不能
返回任意响应 header。账号操作通过 `accounts` 域调用宿主，资源由调用参数选择，不在路由注册中固定 Provider。

页面运行在隔离 iframe 中，静态资源必须同时出现在清单 `resources` 和管理注册中。标记为公开的资源及一次性
票据回调需要 `public_endpoints`；`management.callback` 使用 `public_management` 阶段，并且不能调用账号、
网络、模型、状态或日志端口。

宿主自动注入尺寸监听与上报脚本，为 `body` 设置扣除页面头部和外侧留白后的最小高度，整页滚动由管理端承接。
插件无需调用 resize、发送尺寸消息或安装桥接依赖；Vue 等框架的根容器需要延续最小高度时，使用普通 CSS `min-height: inherit`。
页面使用自然文档流，内容增加或减少时自动同步高度；不使用 `100vh`、`height: 100%`、整页固定定位或内部滚动容器代替文档流。
编辑器、日志等局部区域可自行限高滚动，脱离文档流的浮层不会用于撑高页面。

### 页面宿主桥 v2

宿主注入只读的 `window.codexProxyPlugin`。`request({ method, path, query?, contentType?, body? })` 只能访问当前版本
已注册的管理路由，返回 `{ status, contentType, body: ArrayBuffer }`；`callbackTicket({ path, ttlSeconds })` 为已注册
回调签发 1–600 秒的一次性地址；`resourceUrl(path)` 只解析同一制品已注册的非 HTML 资源。`theme` 为当前
`light` / `dark` 值，变化时触发 `codex-proxy-themechange`。页面不能指定其他实例、制品、版本、URL 或请求 header。

普通模型请求使用标准 `Response` 接口：

```ts
interface PluginModelsBridge {
  responses(input: {
    clientKeyId: string
    body: Record<string, unknown>
    signal?: AbortSignal
  }): Promise<Response>
}

interface CodexProxyPluginBridge {
  readonly version: 2
  readonly models: PluginModelsBridge
}

declare global {
  interface Window {
    readonly codexProxyPlugin: CodexProxyPluginBridge
  }
}

const controller = new AbortController()
const response = await window.codexProxyPlugin.models.responses({
  clientKeyId: selectedClientKeyId,
  body: { model: 'gpt-5', input: 'Hello', stream: true },
  signal: controller.signal,
})
if (!response.ok)
  throw new Error(await response.text())
for await (const chunk of response.body!) {
  // chunk 是原生 Responses JSON 或 SSE 字节流的一部分。
}
```

`clientKeyId` 是管理端 Key 标识，不是 Key 原文；页面可通过自己的已注册管理路由调用 `host.keys.list` 和
`host.models.list` 获取 Key 候选及其可见模型，但桥不会读取或返回秘密。该请求进入普通 Responses 数据面，
因此当前实例自己的 middleware、router、scheduler 和
observer 也按常规计划执行；它不是插件执行期间的嵌套 `host.model` 回调。正文最多 8 MiB，每页最多 4 个并发模型
请求，总时限 10 分钟；响应以不超过 64 KiB 的单块 pull/ack 传输。页面 30 秒不继续消费响应、调用
`AbortController.abort()` / `ReadableStream.cancel()`，或页面被切换、停用、换版时，宿主都会取消请求。桥不接受
任意 URL、header 或 Key 原文；只转交响应状态、安全 header 以及原生 JSON / SSE 正文。

`PluginBuilder::command_line` 冻结命令帮助并绑定执行函数。注册和执行的 JSON 都位于二进制载荷，控制信封为 `{}`。
命令若返回待保存账号，需要 `accounts` 域；只有退出码为零时宿主才按顺序执行各自独立的 CAS 提交。
模型命令通过 `host.keys.list` 展示非秘密 Key，再显式选择 Key 查询模型和执行，不依赖安装时预绑定。

## 数据面入口认证

`frontend_auth.identifier` 返回稳定认证器标识；`frontend_auth.authenticate` 接收敏感的 Authorization 载荷并返回
外部 principal。插件不能指定 Client Key，宿主根据当前入口配置完成映射，并继续检查 Key 的启用状态、模型规则、
账号组、并发和预算。该能力只处理数据面身份，不接管管理端登录。
