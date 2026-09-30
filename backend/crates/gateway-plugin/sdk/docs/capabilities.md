# 插件能力与回调

[返回 SDK](../README.md) · [清单与图标](manifest.md)

`contributes` 声明插件提供的处理器。安装者完整信任插件，宿主回调不需要权限声明或阶段授权，参见[完整信任](manifest.md#完整信任)

| 开发任务 | 入口 |
| --- | --- |
| 组合处理器 | [作者入口](#类型化作者入口) · [能力与方法](#能力与方法) |
| 使用宿主资源 | [基础事实](#基础事实) · [额度刷新](#额度观测刷新) · [账号](#账号与凭据) · [自有资源](#自有资源与维护) · [Key 预算](#client-key-预算) · [模型](#key模型与模型调用) · [网络](#网络) |
| 扩展请求链 | [中间件](#洋葱中间件) · [上游适配器](upstream-adapters.md) · [模型目录](#模型目录) · [重试](#重试决策) · [路由、调度与观察](#路由调度与观察) · [入口认证](#数据面入口认证) |
| 管理与交互 | [状态、日志与迁移](#状态日志与迁移) · [管理页面](#管理页面公开入口与-cli) · [页面宿主桥](#页面宿主桥-v2) · [命令行](#命令行) |

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
`methods::STATE_MIGRATE`，构建器也会要求该处理器存在

类型化合同故意区分三种数据：普通控制元数据、可能敏感的 JSON 二进制载荷、原始字节。
例如 CLI 参数、账号凭据和状态迁移批次使用二进制 JSON，管理 HTTP 正文保持原始字节。
错误地把敏感载荷放进控制参数会在业务处理器运行前被拒绝

## 能力与方法

Provider 固定为宿主内置的 OpenAI 与 xAI。当前宿主开放以下 11 类扩展能力；`middleware` 使用 v3，其余使用 v1：

| 能力 | 类型化方法 |
| --- | --- |
| `middleware` | `PluginBuilder::middleware` / `middleware.handle` |
| `upstream_adapter` | `methods::UPSTREAM_ADAPTER_REGISTER`、`methods::UPSTREAM_ADAPTER_EXECUTE`，见[受管上游](upstream-adapters.md) |
| `model_router` | `methods::ROUTE_MODEL` |
| `model_catalog` | `PluginBuilder::model_catalog` / `methods::MODEL_CATALOG_REGISTER` |
| `retry_policy` | `methods::RETRY_DECISION` |
| `scheduler` | `methods::SCHEDULE_ACCOUNT` |
| `observer` | `methods::OBSERVE` / `observer.observe`，按事件类型订阅 |
| `frontend_authentication` | `methods::FRONTEND_IDENTIFIER`、`methods::FRONTEND_AUTHENTICATE` |
| `management` | `PluginBuilder::management`；公开回调另用 `methods::MANAGEMENT_CALLBACK` |
| `command_line` | `PluginBuilder::command_line` |
| `maintenance` | `methods::RECONCILE`；无需功能绑定 |

## 宿主资源回调

所有回调都通过当前 `TypedCall.host` 发起，关联父调用及其取消信号。普通调用使用父调用剩余期限；已经建立的资源流按[会话生命周期](../README.md#帧与会话)回收，网络操作单独计时

### 基础事实

基础事实接口提供账号分页、Key 分组绑定及额度观测。查询不进入模型执行链，也不触发计费；账号凭据使用[账号接口](#账号与凭据)读取

| SDK 方法 | 回调 | 查询与结果 |
| --- | --- | --- |
| `call.host.account_facts(query)` | `host.data.accounts.list` | `AccountFactsQuery`：可选 `provider_id`、`cursor`，必填 `limit`（1～200）；按账号 ID 升序，`next_cursor=null` 表示本页已结束 |
| `call.host.quota_facts(query)` | `host.data.quota.get` | `QuotaFactsQuery { account_id }`：读取 Provider 现有观测，不访问上游刷新 |
| `call.host.key_facts(query)` | `host.data.keys.get` | `ClientKeyFactsQuery { client_key_id }`：读取当前 Key 的启用状态及显式分组 ID，不返回密钥 |

类型在 `call::data`。控制参数为 `{}`，查询和结果使用二进制 JSON；结果固定 `schema_version=1`。
SDK 解析响应时忽略未知字段，包含账号分页、账号及额度窗口；已知字段按声明校验类型与必填性，查询拒绝未知字段。
插件须通过 `engines.codex-proxy-rs` 限定支持所用接口和字段的宿主版本

`key_facts` 每次读取当前管理数据，返回 `client_key_id`、`enabled`、`group_ids`；不存在的 Key 沿用事实接口的 `rejected` 错误

`group_ids` 表示显式绑定，包含停用分组，不是最终可路由账号集合；空绑定也不代表单账号范围。通过 `account_facts` 关联账号时需完整遍历分页，结果包含停用账号。跨查询关联及同步策略由插件负责，这些调用不构成跨查询事务，管理员修改绑定后应重新检查。
账号仅返回 `account_id`、`provider_id`、`name`、`email`、`group_ids`、`enabled` 和 `updated_at_ms`，不附带令牌或代理信息。
`name` 和 `email` 来自宿主已保存的账号资料；没有邮箱时 `email=null`，读取不会请求上游个人信息。
额度仅返回观测时间与窗口的 `key`、`window_seconds`、`used_percent`、`reset_at_ms`。
时间均为 UTC Unix 毫秒，比例为百分数；未知值保留 `null`，不能解释为 0。`observed_at_ms=null` 表示没有可用观测时间，
不保证当前缓存新鲜，不提供历史快照或多个查询之间的原子一致性

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

`observer` 的 `request_completed` 事件提供请求最终用量、时间和结算事实；该投递有界、不是历史补偿接口。
本接口不提供 SQL、池汇总、健康分、额度预测或历史使用记录查询；插件结果保存在自身 `host.state.*` 中

### 额度观测刷新

- `call.host.quota_facts(query)` / `host.data.quota.get`：读取已有快照，不访问上游
- `call.host.refresh_account_quota(query)` / `host.quota_observations.refresh`：请求 Provider 刷新观测

两个方法均使用 `call::data::QuotaFactsQuery` 和 `QuotaFacts`，控制参数为 `{}`，输入输出在二进制 JSON 中。刷新复用原生 Provider 的凭据、代理、超时及观测落库规则
刷新观测不会消费额度重置券，也不会自动重置任何 Key；Provider 仍可能依据真实上游响应更新账号可用性观测

接口不承诺每次得到不同的周期或样本，也不保证结果对应一个账号重置事件。不存在账号返回 `rejected`；
Provider 不支持刷新（如 OpenAI URL + Key 账号）返回 `invalid_input`，其他失败沿用原生管理错误映射。
刷新受父调用期限和取消约束；结果未知不表示观测未落库。SDK 不自动重复调用，插件应控制刷新频率并处理失败，
不能把失败、空值或旧快照当作额度恢复

### 账号与凭据

账号接口使用 `host.auth.list/get_runtime/get/save`：

- `list` 可用 `provider_id` 过滤并分页，返回不含原始凭据的运行投影
- `get` 的原始凭据只放在二进制载荷中
- `save` 新建时显式提供 Provider，由宿主生成账号 ID；替换时必须提交精确 credential revision

插件可以自行选择其业务需要的账号和 Provider，不需要安装器预先配置允许列表。宿主仍校验账号归属、资格、
凭据版本和并发事务；原始凭据不得进入日志、审计正文或控制元数据

### 自有资源与维护

声明 `maintenance` 并注册 `methods::RECONCILE`，宿主会在实例启用发布、进程恢复及配置变更后调用
`plugin.reconcile`，并每 30 秒补偿一次。处理器必须幂等；调用可能重复，配置通知会合并，不表示逐条账号事件。
每个实例串行执行，不同实例相互独立；调用限时 30 秒，失败后等待 5 秒重试。维护失败不回滚已经提交的资源，
下一次对账继续补齐。只读校验、准备候选与 CLI 帮助不会启动维护；停用、替换和宿主关闭时取消旧任务

维护与其他处理器共用以下资源方法：

| SDK 方法 | 参数与行为 |
| --- | --- |
| `call.host.ensure_group(GroupEnsureRequest)` | `resource_key`、`name`、`color`、可选 `description`；首次创建，之后返回已有 `{id,name,enabled}` |
| `call.host.change_group_members(GroupMembersChange)` | `resource_key`、`add`、`remove`；两列表合计最多 200 个账号 ID，不得重复或交叉，返回实际 `added`／`removed` 数量 |
| `call.host.ensure_key(KeyEnsureRequest)` | `resource_key`、`name`、非空 `group_resource_keys`（最多 64 个）、`max_concurrency`、`requests_per_minute`、`daily_limit_usd`、`weekly_limit_usd`；返回 `{id,name,enabled}`，预算零值沿用原生无限制语义 |

资源键使用 1～64 位小写字母、数字、`_`、`.`、`-`，首位为字母或数字，在实例和资源类型内唯一。
归属由宿主签发，插件不能指定其他实例。`ensure` 只在首次创建时使用属性，不覆盖管理员对名称、启用状态或限制的修改；
同名的管理员资源不会被接管，名称冲突会失败。账号已被删除时增量加入会忽略该 ID。
无实际变化时不会写审计或递增配置版本。创建与归属登记、成员变更、实例版本复验均在同一事务完成

插件升级沿用实例资源；停用保留分组和 Key，其原生启用状态保持不变。删除实例解除归属，资源仍由管理员管理；
重新安装为新实例不会接管旧资源。管理员删除自有资源后，仍启用的维护逻辑可在下次对账重新创建。
Key 明文仍通过宿主管理面查看，插件模型调用使用返回的 Key ID

典型处理器先确保分组存在，再通过 `data` 分页查询账号并增量补齐成员，最后确保 Key 存在。
插件可将当前及未来新增账号纳入分组，筛选和同步策略由插件决定

### Client Key 预算

预算接口可查询、设置日／周金额上限及清零全部 Client Key 的当前周期用量，包括管理员和其他插件创建的 Key

类型位于 `call::key_budgets`；下列预算方法控制参数为 `{}`，输入输出为二进制 JSON：

| SDK 方法 / 回调 | 输入与结果 |
| --- | --- |
| `get_key_budget` / `host.keys.get_budget` | `{client_key_id}`；返回 `KeyBudget`，包含 ID、日／周上限及已用金额、日／周重置时间 |
| `update_key_budget_limits` / `host.keys.update_budget_limits` | `{client_key_id, daily_limit_usd?, weekly_limit_usd?}`；至少指定一项；返回 `{client_key_id}` |
| `reset_key_budget` / `host.keys.reset_budget` | `{client_key_id, period}`；`period` 必填，取 `daily`、`weekly` 或 `all`；返回 `{client_key_id}` |

`list_keys` / `host.keys.list` 仍可查询非秘密目录，仅返回 `{id,name,enabled}` 和游标，沿用目录的控制参数编码。
上述操作均接受 Key ID，不接受 Key 明文，也不要求先查询目录

金额为非负十进制字符串，沿用原生金额精度；上限 `"0"` 表示不限。`KeyBudget` 的字段为
`daily_limit_usd`、`weekly_limit_usd`、`daily_used_usd`、`weekly_used_usd`、`daily_resets_at_ms`、`weekly_resets_at_ms`；
时间为 UTC Unix 毫秒，`null` 表示尚未使用或窗口已过期。读取不触发准入、开启窗口或清零，停用的 Key 仍可管理

上限更新仅写入提供的日／周金额；省略或 `null` 的项保持不变。它保留已用金额、窗口到期时间、费用历史及其他 Key 配置。
写入复用 Key 行锁，与结算串行；实际变化时实例版本复验、修改、配置 revision 和审计在同一事务提交，并通知原生配置发布。
相同值再次赋值不产生新 revision 或审计；并发更新按事务顺序生效，同一字段由后提交的值覆盖，接口不提供调用去重或比较交换。
将非零上限降低到已用金额及以下会拒绝后续准入，提高限额可恢复准入，在途请求仍按原生规则完成和结算

重置清零所选周期并清除到期时间，保留额度上限、密钥和历史费用事件；所选窗口下次使用时按原生规则重新开启。
费用按完成时间归属，重置前完成但延迟落盘的费用不会重新扣入已重置周期，重置后完成的在途请求仍正常扣额。
实例版本复验、清零和审计在同一事务提交，不推进配置 revision。每次调用均执行新重置，可能清掉两次调用之间的新消费

SDK 不自动重试。超时或断连不能证明写入未提交；重试上限赋值也可能覆盖期间其他调用的修改。
不存在的 Key 返回 `rejected`；写入事务复验发现实例停用或版本变化时返回 `conflict`，
非法输入返回 `invalid_input`。账号关联、预算分配和重置触发由插件决定，宿主不自动串联上述接口

### Key、模型与模型调用

模型调用方法：

- `host.keys.list`：只返回 `{id, name, enabled}`，不返回 Key 明文、前缀或隐藏 scope
- `host.models.list`：以显式 `client_key_id` 查询该 Key 当前可见的模型目录
- `host.model.execute[_stream]`：交给 Core 的路由、准入、租约、重试、账本和计费链路执行
- `host.model.stream_read/close`：读取或关闭当前父调用创建的有界流

`ModelExecuteRequest.client_key_id` 可显式选择执行 Key，不与触发插件的父请求 Key 求交。省略时继承现有模型父请求身份；没有模型父请求的调用必须提供 Key。所选 Key 的启用状态、模型规则、账号组、并发和预算继续由 Core 执行

模型事件使用 `call::model::ExecutionEvent` 的 `GPE2` 封套，将 canonical 事实、宿主事实和原始 wire 分段编码。`host` 包含费用及定价明细、完整上游观测、会话状态和改写来源；当前 wire 与原始 wire 都保留二进制字节。读取或回传快照不触发第二次计费、续接提交或发送状态更新。
非流式模型结果通过 `ModelEventBatch` 的 `HME1` 有界二进制批次返回；流式句柄只在创建它的父调用内有效。
发起回调的插件实例在子调用中跳过，Core 在单次执行的子调用图内限制递归深度、总数和并发，避免插件调用自身形成环。
显式 Key 的身份快照随父调用保留，每个模型执行独立计时并建立调用图；父调用结束会取消尚未完成的执行，连接空闲不消耗后续执行的期限

`host.affinity.lookup` 查询 Provider 已持久化的真实亲和键。命中只是账号偏好，
后续执行仍重新检查当前 Key 规则、账号资格和租约

### 网络

`HostClient::http(request, body)` 返回响应头与 `HostHttpBody`。正文用 `read()` 按需读取，
用 `collect(maximum_bytes)` 有界收集，提前结束用 `close()`；EOF 与重复关闭不再调用宿主。
父调用取消或结束时宿主回收未关闭的流，SDK 不后台预读。取消一次 `read()` 等待后，再次读取会继续同一次操作，不丢弃已经返回的分块。
账号上游使用同一正文对象，使用方式见[受管上游](upstream-adapters.md)

`host.http.do/do_stream` 和流读取／关闭方法提供受管 HTTP。正文使用独立二进制载荷，Host 统一执行代理、超时和背压，不按插件身份限制目标地址段。DNS 由宿主解析，连接保留目标 Host 与 TLS 域名校验；重定向不会自动跟随。
普通调用使用父调用剩余期限；已建立的受管 HTTP 流或 WebSocket 会话按每次网络操作计时，仍受父资源取消约束

期限耗尽返回 `timeout`，解析、连接或响应读取失败返回 `upstream`。错误消息提供分类提示，响应正文仍由插件读取；协议故障不应直接展示内部消息

## 请求扩展

### 洋葱中间件

`middleware.version` 为 `3`，所有挂载使用同一个 `ctx + next` 组合器。清单显式选择处理边界，绑定决定顺序；
不按 URL、消息类型或每个内部步骤增加处理器。仅处理一个边界可直接声明其调用类型；跨边界插件使用 `MiddlewareCall`
在一个处理器内匹配类型化视图，不感兴趣的调用交给 `call.forward().await`

| 挂载 | SDK 视图 | 默认终端 |
| --- | --- | --- |
| `http` | `HttpCall` | 路由匹配、认证、正文解析与既有 HTTP handler |
| `websocket` | `WebSocketCall` | 入站业务消息分派／出站连接写入 |
| `service` | `ServiceCall` / `TypedServiceCall<O>` | 已登记的公开业务服务 |
| `request` | `RequestCall` | 一次逻辑模型请求 |
| `attempt` | `RequestCall` | 本次已选 Provider 和账号的执行 |

`PluginBuilder::middleware` 与 `MiddlewarePlugin::new` 共用 `MiddlewareInput` / `MiddlewareOutput`。
`next.run(input)` 消费自身，最多执行一次；结果按相反顺序经过外层。重试或独立子请求通过宿主执行服务发起

HTTP 总入口统一包裹原生路由，`dispatch_http` 复用同一路由与组合能力；无需为每个 HTTP 接口登记新的挂载点。
`service` 用于不经过 HTTP 的类型化主动调用，具体公开范围见下节

#### 公开服务

服务合同位于 `call::services`，`Operation` 关联稳定名称、输入和输出类型。
`HostClient::service::<O>(input)` 主动调用服务；`ServiceCall::is::<O>()` 匹配操作，
`into_typed::<O>()` 取得可修改输入、单次 `next`、父调用标识及宿主客户端。
返回 `ServiceResponse::from_result::<O>(result)` 可以改写结果、短路或恢复业务错误；
不感兴趣的操作调用 `forward()`。公开值完整传递，类型校验不承担字段权限控制

当前登记 `settings` 的 11 个操作：`Load`、`Replace`、`ApiKeyExists`、`RegenerateApiKey`、`DeleteApiKey`、
`Pricing`、`PreviewPricingSync`、`Sync`、`Update`、`ClientProfileOptions`、`PreviewClientProfile`。
插件回调和 CLI 经注册表主动调用时统一进入一次 `service` 组合；网关原生设置接口由 HTTP 总入口包裹。
各入口共用同一业务实现，业务方法内部调用不重复进入 `service` 组合。
其他管理接口复用既有 HTTP 路由，不要求按内部方法另建服务目录；既有账号、Key 等资源回调不自动进入 `service` 组合

`Load` 返回完整运行设置与 `config_revision`。`ReplaceRuntimeSettings::from(settings)` 保留该版本并构造更新命令；
`Replace` 接收 `(MutationContext, ReplaceRuntimeSettings)`，通过已有校验、事务、审计和快照发布保存设置。
版本过期返回 `ServiceError { kind: "conflict", message }`。读取结果的普通改写不会持久化，也不会回滚已经提交的事务

服务错误保留分类与完整业务文案，子调用继承冻结发布计划及取消信号。
插件显式再次调用服务时继承发起实例链并跳过相同实例，防止无限递归；无 service 绑定时直接分派业务实现，仍保留空计划快照、父子关联和取消信号。
原生业务方法保留类型化调用，不为服务组合增加 JSON 编解码

#### HTTP 入口

`HttpCall.request` 提供 method、URI（含 query）、HTTP version、完整多值 headers、`settings`、`timeout_ms` 和 `HttpBody`。
调用发生在路由匹配之前，管理接口、模型接口和未知路径都可参与。`settings` 是同一请求快照中的宿主运行设置，
插件改写后用于后续认证、版本检查、正文解压及执行；不新增设置业务的中间件入口。`timeout_ms` 使用宿主 HTTP 超时基线，
插件可改写或用 `None` 清除；终端按有效值执行，不恢复宿主默认值。宿主快照不可用时 `settings` 为 `null`

`HttpCall.settings_sources` 返回 `config_revision`、宿主 `host` 基线、运行设置的 `overrides`、Key 作用域的 `execution` 与 `http_timeout`。
每条改写记录含 `instance_id`、`order` 和 `value`；来源由宿主记录，修改查询结果不会写入设置。
宿主比较提交前后的值，仅记录实际改变的设置项；同值赋值不新增改写记录。每个顶层设置项整体替换，`false`、`0`、
空映射及合同允许的 `null` 都是值，不进行隐式深合并。改写只属于当前调用，不修改持久设置

`HttpBody::read` 按需读取数据帧或 trailers；`map_frames` 包装上传或响应。透传只搬运句柄，保留原始分块、trailers、
request/response extensions 与连接升级状态。上传转换使用单帧有界管道；下游可在上传完成前返回响应，剩余生产随响应正文驱动，
提前拒绝不要求继续读完整个上传。每次调用最多保留 16 个活动上传管道，写入 EOF、显式关闭或消费端退出即归还名额。
读取、发送和生产都随父调用取消，不启动脱离调用的生产任务

取消一次宿主正文读取的等待不会丢帧，再次读取继续原操作；有未完成读取时转交 `HttpBody`，SDK 按输出背压保留该读取并转发剩余正文。读取量遵守宿主 Credit 窗口，已经在途的较大块会拆分保留

`HttpResponse::new` 创建短路结果。`next` 返回响应后可改写状态、headers 或包装正文；返回响应头不代表流已结束。
`host.middleware.next` 与 `host.http.body_*` 复用当前调用的 HTTP 资源池；已消费的 next 或正文句柄不能再消费一次。
读取 EOF 或转交正文会消费读取句柄；SDK 在 EOF 后继续读取返回 `None`，重复关闭不发起 RPC。
`body.close().await` 只关闭正文，仍可替换正文并返回原响应；响应部件保留到返回响应或调用资源池释放。
插件进程已经启动本次 HTTP 调用后，故障不会自动重放下游；无法启动且绑定为 `delegate` 时才直接委托

`HostClient::dispatch_http(HttpRequest)` 主动调用宿主相对路径，复用相同的路由、HTTP 洋葱链与惰性正文资源，不通过网络回环。
`HttpCall.call_id` 和 `parent_call_id` 表达父子关系，`context.request_id` 保持同一请求关联；子调用沿已冻结的计划运行并跳过所有发起祖先实例，最多嵌套 4 层。
主动管理请求使用宿主签发的插件身份和系统审计，不依赖父请求的管理员 Cookie；模型请求仍需提供用于模型范围与结算的 Key。普通 HTTP 请求头不能声明插件身份。
子调用继承当前配置快照与显式改写，`HttpRequest.settings = null` 表示沿用；提供对象时按设置项比较并替换。
切换 Key 后重新解析目标 Key 的默认画像和限额，父 Key 的默认值不会带入；运行设置改写继续生效，Fast、Key 限额和模型超时的改写只对原 Key 生效。同级调用的修改互不回写
主动 HTTP 分派计入模型父请求的副作用水位，避免下游失败后透明重放子调用。父调用取消会终止等待中的子请求与响应读取；转交正文不因父 RPC End 提前取消。消费完成会释放原正文，提前结束时调用 `body.close().await` 释放正文句柄

#### 自定义 WebSocket 会话

HTTP 中间件可调用 `call.upgrade(protocols, handler).await` 接管当前请求，返回仍经过外层 HTTP 中间件。
`protocols` 按插件给出的顺序与客户端协商子协议；握手不合法或当前请求不支持升级时明确失败。
默认路由不会再执行，路径选择、认证、协议解析及每条消息产生哪些模型调用由插件处理

`handler` 接收 `WebSocketSession`，使用 `receive().await` 读取完整消息，复制 `sender` 后可在接收等待期间独立发送。
在 `select!` 中取消一次 `receive` 等待不会丢弃消息，下一次等待继续同一次接收。
`close(code, reason).await` 发送关闭帧；处理函数返回、失败、插件退出或客户端断开时，宿主回收连接与调用资源。
会话的收发也经过已冻结的 `websocket` 消息组合，主动写入与接收不共享跨等待的独占锁

握手受初始调用期限约束。成功返回响应头后，会话持有同一发布代次及父调用，直至连接或资源终结；
101 正文完成不会取消会话。外层中间件替换响应会丢弃未提交的会话，改变已选择的升级状态码会被判为无效响应。
一个会话可通过捕获 `call.host` 发起多个独立模型执行；模型关联、用量和费用仍归各自执行所有

#### 下游 WebSocket 消息

`WebSocketCall` 提供连接 ID、握手完整 headers、`direction`、原始 `message`、单次 `next`、`sender` 和取消信号。
文本、二进制、Ping、Pong 和 Close 均携带原始正文；消息类型不由宿主预先按第三方协议筛选。
`WebSocketPayload` 按需读取，透传不读取、不复制正文；插件需要完整内容时自行 `collect().await`。
不再使用的句柄通过 `payload.close().await` 释放；读取到 EOF 或转交消息也会消费资源。
取消一次读取等待后可继续读取；未完成的读取须先继续消费或关闭，直接转交该消息会报错，避免静默丢弃在途正文。
单次调用最多保留 16 个未消费的消息正文，达到容量后需先消费或关闭已有句柄

`next.run(message)` 返回可继续修改的 `Option<WebSocketMessage>`；返回 `None` 丢弃当前消息。
`sender.send(message).await` 是当前连接的直接写入能力，等待实际 transport 完成，不递归进入消息中间件。
入站插件在响应进行中仍可处理未知控制消息并主动回写；是否转发到上游及其协议语义由插件决定。
消息正文可改写，实际写入、错误和取消事实由连接 owner 记录，丢弃消息不会记作写入成功

Ping 的自动 Pong 与关闭握手属于底层 WebSocket 协议事实；插件观察消息时，transport 可能已经产生应答。
重写消息不能撤销已经发生的发送。文本必须是 UTF-8，控制帧长度和关闭格式仍遵守 WebSocket 协议。
每条消息组合有独立 RPC 期限；自定义会话持有一个受管流式调用。连接结束会取消仍在处理的消息与发送

#### 模型请求与 attempt

`RequestCall.request.head` 包含完整 Client Key 与分组归属。OpenAI 在进入 attempt 前应用宿主档位设置，插件显式改写后不再恢复默认值。
OpenAI 与 xAI 的上游 headers 先由账号和画像生成，插件同名字段覆盖基线，同名多值按插件顺序保留；不存在按认证、Cookie 或会话名称划分的保护列表。
实际账号租约、凭据版本与计费归属仍记录真实选中事实，不随插件修改 header 名称而伪造

request 阶段的 `request.head.settings` 是本次调用的有效设置对象，包含 `runtime`、`disable_fast`、`client_limits` 和 `timeout_ms`。
`runtime` 使用宿主快照的设置字段，画像已合并 Client Key 设置；可以修改映射、画像、调度、位置及计价等执行参数。
SDK 仅在对象改变时提交完整替换，`false`、`0`、空映射和合同允许的 `null` 不表示跳过；不做隐式深合并。
多个插件按入站顺序读取前层改写后的值，终端在执行前从同一配置代次重算，改写不发布全局配置。
`timeout_ms` 从本次模型请求开始计时，改写不会清零已用时间；HTTP 处理期限与 RPC 操作期限仍各自生效

此视图在客户端认证及初始协议解码之后出现：不会追回已经完成的版本准入或入口解压校验。
Responses 终端重新解码插件正文时读取改写后的正文上限；attempt 阶段的 `settings` 为 `null`，已经完成的准入不能经该入口重做。
更早的改写使用上述 HTTP 请求的 `settings`。`request.head.settings_sources` 使用相同的来源结构，`execution.input` 表示进入当前 Key 作用域时的有效值。
原生 Responses WebSocket 每轮请求读取当前宿主快照，重新认证并解析 Key 策略，同时保留握手阶段的显式改写；已开始的执行保持自己的冻结值

`middleware.version` 必须为 `3`。插件可读取和改写完整请求、响应 headers、正文与响应帧，不要求
`requests` 权限，也不按字段名称过滤认证、Cookie、会话或连接头。安装者承担插件读取与修改这些数据的风险。
类型、HTTP 格式、大小及资源生命周期仍需符合合同

默认使用 `MiddlewareRequestBody::Preserve` 保持正文；`replace_body(Vec::new())` 明确清空。
Header 修改采用增量 remove/append，保留合法多值。`MiddlewareBody::map_frames` 复用会话的流控和唯一终态

`response.metadata` 提供 Provider、实际模型、账号、上游请求 ID 和账号选择观测。
`response.body.with_facts()?.inspect_frames(...)` 或 `.map_frames(...)` 通过 `frame.facts` 读取完整事件快照；纯事实帧可没有正文。
快照按需读取，未开启读取或未消费正文时不复制事件载荷；来源帧完成后对应句柄失效。
修改快照不会改变宿主确认的原执行事实，响应改写仍通过 headers、正文和受管调用完成

`next` 和模型子调用返回的 `PluginFault` 保留具体消息、发送状态、HTTP 状态及 `details`。
域错误详情包含分类、上游 type / code、重试提示和诊断；Provider 错误还包含请求标识、原始错误、HTTP 响应字节与多值 headers、连接及续接恢复事实。
公开服务的 `ServiceError.details` 保留跨进程错误详情。错误快照供插件读取，宿主继续持有原错误处理重试、发送与账本事实

**不改写就 1:1 保留**：同一网关处理边界上，默认 `next.run(request)` 保留原始正文、未修改的 header 和响应流字节及顺序。
只解析 `request.body` 不会触发替换；SDK 比较完整原始输入，只发送实际修改。
只观察响应可使用 `response.body.inspect_frames(|frame| { /* 读取，不修改 */ })?`，保留来源信封、背压和取消语义。
这一保证比较插件前后的同一边界，Provider 自身的协议适配、两次调用的随机值和网络分段不在比较范围内

`request.declare_capabilities(CapabilityDeclaration { handled, required })` 随单次 `next` 发出，必须同时显式替换正文。
当前仅允许 OpenAI 生成请求的 request 阶段，HTTP 与 WebSocket 共用规则；不扩展端点允许的协议或流模式。
`handled` 只允许列举改写前确实存在、改写后已从实际请求移除的 `tools`、`vision`、`reasoning`、`json_schema`，
插件须负责对应的提示词转换及普通响应、错误、流式响应还原。`required` 为额外上游需求，不能覆盖正文推导出的要求。
重复项、同一项同时出现在两组、空声明、attempt 阶段使用声明均会拒绝。
原始语义中未由插件承担的功能继续约束路由；实际正文重新加入某功能时，该需求仍生效。
原生续接不能由声明豁免，attempt 已取得的账号租约与真实发送事实仍由原 owner 持有

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
`PluginBuilder`，两者复用相同的类型化调用与结果合同

### 扩展的组合边界

请求处理沿 `request middleware → 原生选路与选号 → attempt middleware → 原生上游或 upstream_adapter` 推进，
响应按相反顺序返回；适配器是现有洋葱链的终端，不能再次调用 `next` 或组织换号重试

| 工作 | 组合方式 |
| --- | --- |
| 请求头／正文改写、提前拒绝、响应头处理 | `middleware` 包裹一次 `next.run(request)` |
| 响应逐帧转换、观察 | 返回惰性 `map_frames` / `inspect_frames`，不能在 `next` 返回时就把流视为结束 |
| 新上游路径、协议编码、错误及用量解析 | `upstream_adapter` 终端，复用已选账号和原生结算 |
| 账号代理、默认凭据注入、网络发送状态 | 宿主网络外层，插件可覆盖默认请求头，普通 HTTP 与账号 HTTP 共用发送和正文资源实现 |
| 路由、调度、重试决策 | 各自的决策端口，Core 保持最终裁决和预算 |
| RPC Credit、取消、流关闭、WebSocket 续接 | 会话及资源生命周期，不作为可重排的中间件 |
| 终态观察、管理与维护 | 原有观察／管理／维护入口，不嵌入请求 `next` 链 |

### 模型目录

`model_catalog` v1 在 registration 阶段一次提交 `ModelCatalogRegistration { models: Vec<ModelAlias> }`，不需要请求级 binding。
每条 `ModelAlias { id, provider, model }` 定义公开 ID、内置 Provider（`openai` 或 `xai`）及直接上游目标。
上游目标须已存在于该 Provider 的当前目录；同名原生模型、静态映射、其他插件条目、别名链和循环均拒绝发布。
每实例最多 256 条，整个二进制 JSON 最多 64 KiB；不允许覆盖原生元数据或自行宣称目标不支持的功能

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
v1 是直接别名，动态选择可组合 `model_router`；实际路由仍须通过宿主的权限和能力校验，不提供多目标虚拟模型注册合同

### 重试决策

`retry_policy` v1 固定绑定 `retry` 阶段，故障策略只能为 `delegate`。使用 `methods::RETRY_DECISION` 处理 `RetryDecisionRequest`。
输入包含失败分类、必要状态码、发送状态、attempt 序号、Provider／上游模型、剩余路由次数和期限，以及 `allowed_actions`。
不发送账号凭据、请求正文或上游错误转储。路由次数不等同于 Provider 自有传输恢复预算

返回 `RetryDecision::Delegate`、`Stop` 或 `Retry`。`Retry` 必须出现在宿主允许动作中，并且仅继续宿主已经选定的恢复路径；
不能指定账号、延迟、目标或提高预算。响应已交付、发送不明确、外部副作用及续接绑定等既有安全门不可绕过。
Core 处理建立响应流前与流中的失败，并在策略返回后再次检查期限、取消和副作用

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
middleware 的 `next` 最多调用一次，重试与额外模型执行通过独立调用创建

### 路由、调度与观察

`policy.route_model` 和 `policy.schedule_account` 只提出决定，宿主随后复核 Key 规则、模型能力、账号资格和租约。
`observer` v1 通过 `methods::OBSERVE` 接收 `call::observation::Event`，固定绑定 `observation/observe`。
每条绑定以 `event` 选择 `request_completed` 或 `websocket_response`，可分别配置请求范围；同一实例每种事件至多一条绑定，两类绑定使用相同 `order`。
事件选择只决定是否投递，不裁剪字段

- `Event::RequestCompleted` 包含一次最终请求事实，`terminal` 与 `usage` 始终存在；用量、费用与耗时的缺失值表示宿主没有该事实，不等于零
- `Event::WebSocketResponse` 观察实际上游 WS 事件，原始帧放在 `TypedCall.payload`，元数据放在 `request`；序号在请求内单调递增，有界队列丢弃可能产生间隔

两类事件共用 `observer.observe` 方法及同一处理器。观察计划在请求开始时冻结；完成事件最多派发一次，WebSocket 事件按请求保序；完成通知不等待 WebSocket 观察队列排空。
观察超时、过载或插件失败不改变业务响应，返回值不用于修改帧或重新计价。需要改写响应时使用洋葱中间件

```rust,ignore
use gateway_plugin_sdk::call::observation::Event;
let plugin = PluginBuilder::from_json(include_bytes!("../plugin.json"))?
    .on(methods::OBSERVE, |call| async move {
        match call.request {
            Event::RequestCompleted(request) => record_completion(&request),
            Event::WebSocketResponse(response) => record_frame(&response, &call.payload),
        }
        Ok(TypedReply::new(Empty {}))
    })?
    .build()?;
```

`Handshake`、`Message`、`Frame` 和 `PluginFault` 的调试输出隐藏配置、载荷与错误正文；插件自己的日志同样不能输出凭据或完整请求

## 状态、日志与迁移

清单 `state[]` 声明命名空间、`schemaVersion`、JSON schema、记录与字节配额。运行时用
`host.state.get/put/delete` 访问；创建要求键不存在，更新和删除必须提供精确版本。状态始终绑定当前
插件实例、制品和 schema；该接口操作插件状态，不代替宿主业务事务

不兼容升级由宿主停用并排空旧会话，再调用 `plugin.state.migrate` 分批迁移到暂存状态。插件逐项返回
keep、replace 或 delete；全部完成后宿主才原子提交新配置与状态，失败时丢弃暂存结果

`host.log` 接收固定事件名、级别和有界字段。宿主脱敏并限制速率；`recorded: true` 只表示已交给有界日志任务。
日志、状态与其他宿主回调都属于当前父调用，父调用结束后不能继续使用其资源

## 管理页面、公开入口与 CLI

`PluginBuilder::management` 同时冻结 `ManagementRegistration` 并绑定 `management.handle`。请求元数据在控制参数，
原始 HTTP 正文在二进制载荷；响应同样分离。`ManagementRequest.headers` 完整保留原始请求头，
`ManagementResponse.headers` 可显式覆盖宿主默认响应头；两者使用 `MiddlewareHeader`，保留重复项与非 UTF-8 值。
宿主只校验 HTTP 语法，不按 Cookie、Authorization 等字段名称裁剪。账号操作通过宿主账号接口执行，
资源由调用参数选择，不在路由注册中固定 Provider

页面运行在隔离 iframe 中，静态资源必须同时出现在清单 `resources` 和管理注册中。标记为公开的资源及一次性
票据回调按注册内容开放；`management.callback` 使用 `public_management` 阶段，与其他处理器共用宿主回调

宿主自动注入尺寸监听与上报脚本，为 `body` 设置扣除页面头部和外侧留白后的最小高度，整页滚动由管理端承接。
插件无需调用 resize、发送尺寸消息或安装桥接依赖；Vue 等框架的根容器需要延续最小高度时，使用普通 CSS `min-height: inherit`。
页面使用自然文档流，内容增加或减少时自动同步高度；不使用 `100vh`、`height: 100%`、整页固定定位或内部滚动容器代替文档流。
编辑器、日志等局部区域可自行限高滚动，脱离文档流的浮层不会用于撑高页面

### 页面宿主桥 v2

宿主注入只读的 `window.codexProxyPlugin`。`request({ method, path, query?, contentType?, body? })` 只能访问当前版本
已注册的管理路由，返回 `{ status, contentType, body: ArrayBuffer }`；`callbackTicket({ path, ttlSeconds })` 为已注册
回调签发 1–600 秒的一次性地址；`resourceUrl(path)` 只解析同一制品已注册的非 HTML 资源。`theme` 为当前
`light` / `dark` 值，变化时触发 `codex-proxy-themechange`。页面不能指定其他实例、制品、版本、URL 或请求 header

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
`host.models.list` 获取 Key 候选及其可见模型；该便捷桥用 ID 选择推理身份，不需要页面提供 Key 原文。该请求进入普通 Responses 数据面，
因此当前实例自己的 middleware、router、scheduler 和
observer 也按常规计划执行；它不是插件执行期间的嵌套 `host.model` 回调。正文最多 8 MiB，每页最多 4 个并发模型
请求，总时限 10 分钟；响应以不超过 64 KiB 的单块 pull/ack 传输。页面 30 秒不继续消费响应、调用
`AbortController.abort()` / `ReadableStream.cancel()`，或页面被切换、停用、换版时，宿主都会取消请求。桥不接受
任意 URL、header 或 Key 原文；转交响应状态、浏览器 `Headers` 可读取的完整响应头以及原生 JSON / SSE 正文。
这些页面辅助方法不限制插件进程在统一 HTTP 中间件中读取、改写请求，或通过 `dispatch_http` 调用其他宿主接口

### 命令行

`PluginBuilder::command_line` 冻结命令帮助并绑定执行函数。注册和执行的 JSON 都位于二进制载荷，控制信封为 `{}`。
命令返回待保存账号时，只有退出码为零时宿主才按顺序执行各自独立的 CAS 提交。
模型命令通过 `host.keys.list` 展示非秘密 Key，再显式选择 Key 查询模型和执行，不依赖安装时预绑定

## 数据面入口认证

`frontend_auth.identifier` 返回稳定认证器标识；`frontend_auth.authenticate` 接收敏感的 Authorization 载荷并返回
外部 principal。插件不能指定 Client Key，宿主根据当前入口配置完成映射，并继续检查 Key 的启用状态、模型规则、
账号组、并发和预算。该能力只处理数据面身份，不接管管理端登录
