# 受管上游适配器

[返回 SDK](../README.md) · [清单声明](manifest.md#扩展项简写) · [能力与回调](capabilities.md)

`upstream_adapter` v5 处理内置 `openai` / `xai` 的生成请求，独立图像和压缩端点使用原生实现。
插件负责 Base URL、路径、请求编码和响应解析。宿主负责身份、ACL、选号、租约、凭据、代理、重试和账本。仅需加工请求或响应时使用[中间件](capabilities.md#洋葱中间件)

## 声明与绑定

清单声明 `upstream_adapter`、版本 `5`，以及匹配的 `inputFormats` / `outputFormats`；固定阶段 `upstream` 由作者清单规范化入口生成。
使用 `PluginBuilder::on` 注册
`methods::UPSTREAM_ADAPTER_REGISTER` 与 `methods::UPSTREAM_ADAPTER_EXECUTE`

注册返回 `UpstreamAdapterRegistration`，最多包含 16 个适配器：

| 字段 | 含义 |
| --- | --- |
| `id` | 插件提供的适配器标识，不是 Provider 或账号类型 |
| `provider` | `openai` 或 `xai` |
| `base_url` | 解析请求目标的基址，路径拼接遵循 URL join 规则 |
| `paths` | 请求路径与 `inference` 或 `auxiliary` 用途的对应关系 |
| `authentication_kinds` | 可使用的现有认证类型，宿主复核已选账号 |
| `transport` | `http_json`、`http_sse` 或 `websocket` |
| `protocol` | 请求和客户端 wire 使用的协议 |
| `models` | 公开请求模型的精确匹配，空集合沿用绑定范围 |
| `account_ids` | 上游账号 ID 范围，必填；空数组匹配所有账号 |

实例只允许一个 `upstream` 绑定，故障策略为 `reject`。Key、账号组、Provider 和模型限定请求范围，插件注册的 `account_ids` 只能进一步收窄账号范围

同一模型可按账号使用不同适配器。每个账号按发布顺序取首个匹配声明，后续声明的故障不影响已匹配的适配器。未匹配时使用原生上游，命中但不可用时拒绝执行

取得租约前，宿主排除适配器不可用或认证不兼容的账号。xAI 原生编码不支持请求时，仅排除原生账号，匹配的适配器账号仍可使用。宿主按亲和与调度策略取得租约后，从同一发布代次解析适配器

停用或卸载实例会移除绑定。维护前先停用依赖该适配器的账号并排空请求

能力、清单、进程 RPC 和宿主兼容声明分别校验各自版本，见[清单](manifest.md)。
Provider、业务协议和传输名称不能写入宿主兼容清单的 capability / RPC 版本位置

## 执行与受管出站

`TypedCall<UpstreamAdapterRequest>` 包含只读的 Key、账号、凭据版本、模型、协议、Header 和宿主续接投影；
原始请求正文位于 `call.payload`，原始账号凭据不进入插件输入。宿主首次消费冷流才调用插件

`fast_mode` 是本次模型执行的有效设置，取值为 `default`、`enabled` 或 `disabled`。OpenAI 在 attempt 中间件之前将 Fast 设置应用到正文基线，
适配器收到经过 attempt 中间件处理的正文；attempt 中间件显式改写档位后，宿主不会在发送前再次应用 Fast 策略。
其他请求设置与 Key 作用域的覆盖规则见[模型请求与 attempt](capabilities.md#模型请求与-attempt)

HTTP 请求使用 `call.host.upstream_http(request, body).await?`，返回 `HostHttpResponse`：

```rust,ignore
let response = call.host.upstream_http(UpstreamHttpRequest {
    method: "POST".into(), path: "responses".into(), query: vec![], headers: vec![],
}, encode_request(&call.payload)?).await?;
let status = response.status;
let mut body = response.body;
while let Some(bytes) = body.read().await? {
    // 分段不等于 SSE 事件；插件按自己的上游协议增量解码。
    decode_upstream(status, bytes)?;
}
```

正文每次 `read` 至多 64 KiB，SDK 按需读取并管理流 ID。JSON 可用 `collect(maximum_bytes)`，SSE 须跨读取块增量解析。
显式 `close`、收集超限或父调用结束、取消时释放正文。HTTP 版本、字节头及 trailers 见[网络](capabilities.md#网络)

WebSocket 使用 `call.host.upstream_websocket(request).await?`，返回
`UpstreamWebSocketUpgrade::Connected { http_version, headers, connection }` 或 `Rejected { status, http_version, headers, body, trailers }`。
连接提供 `send(kind, body)`、`read()`、`close()`，支持完整文本与二进制消息，Ping/Pong 由宿主处理。
读写可同时进行，等待上游输出时可以发送控制消息；有先后依赖的消息须顺序等待 `send`。
同一时刻只允许一个读取者；取消一次 `read()` 等待后，再次读取继续同一次接收，不跳过消息。
`close` 会唤醒在途操作并释放连接，并发关闭等待同一次完成；读取超时或连接错误也会关闭连接。
握手拒绝保留 HTTP 状态、正文和 trailers；连接复用返回该连接最初的握手版本和响应头。底层 `host.upstream.*` 是 SDK 使用的资源协议

```rust,ignore
let (message, ()) = tokio::try_join!(
    connection.read(),
    connection.send(WebSocketMessageKind::Text, control_message),
)?;
```

宿主先注入已选账号认证，插件显式提供的同名头覆盖默认值。路径支持相对基址或完整 HTTP(S) URL；
声明路径用于标记发送用途，不是访问白名单。出站复用账号代理，未绑定可用代理时使用部署默认出口，代理失败不能回退直连，重定向不自动跟随。
`auxiliary` 路径的出站计入请求副作用水位，防止附件上传等操作在失败后被透明重复执行

## 事件、结算和续接

执行返回 `TypedReply<Empty>` 的有界 `ResponseStream`。每个 chunk 使用 `UpstreamAdapterEvent::encode()`，
包含标准事实与可选原始 wire；事件需符合开始、内容、工具、用量和唯一结束的顺序。
原始 JSON / SSE / WebSocket 字节与标准事实分开传递，错误使用 `UpstreamFailure`；宿主网络观测决定发送状态，插件不能将其降为未发送
`status`、`code`、可选 `error_type` 和 `message` 保留实际上游错误字段，缺失类型保持空值；流内失败不能使用成功握手状态或从业务码推导状态

宿主只对一次受管 `inference` HTTP 调用明确返回的 429 提供有限重放证明：插件必须报告
`rate_limited` 和 `status: 429`，且此前及失败事件中均无业务事实、wire 或续接材料。
调用结束时先撤销回调、回收资源并封存未完成发送，再将错误交给 Core；发送状态仍为 `sent`。
多次或并发请求、辅助请求、其他宿主能力调用（日志除外）、受管回调失败以及取消都不能取得该证明。
观测事件和该次 HTTP 的读流、关流不单独阻断；HTTP200 流内报错、WebSocket 拒绝及其他状态码不适用。
Core 仍复核续接归属、外部副作用、交付与重试预算；这不保证一定换号，也不证明完整信任插件绕过宿主执行的操作可重放

初始 RPC 回复默认限时 120 秒。持续接收与解析应放在返回的流中，遵循模型请求期限。受管 HTTP 正文每次读取单独计时，读取超时、客户端取消或流结束时回收资源

用量提交标准 `Usage`，费用由内置 Provider 按已冻结的上游模型与价格计算，响应回显模型不切换计价。
`observation.service_tier` 记录上游回显的最新档位，允许上游在完成时确定实际值；OpenAI 计价沿用请求策略，回显档位仅作观测。
未知用量或价格保留未知。宿主确认 RPC 正常结束后才发布完成事实，插件不能独立结算或组织换号重试

`UpstreamAdapterEvent.observation` 接收 `ResponseObservation` 原始观测：HTTP 版本、状态、请求 ID、回显模型、
服务档位、字节响应头、Provider JSON 元数据及 `ResponseTimings`。`transport` 与注册传输类型一致；
已提供字段覆盖同次执行的旧值，缺失字段或空 headers 保留已有观测，允许独立的观测事件及失败事件携带观测。
`ExecutionEvent.host` 是宿主结果的只读快照，不能用于回写费用、发送状态或续接

`first_*_ms` 相对 Core 请求计时起点，包含本次请求的排队和重试等待。
`request_elapsed_ms` 提供宿主发送本次适配器调用前已耗的时间，插件自行计时须对齐该基线；
没有报告时 Runtime 在收到首事件、非空正文／推理／工具增量时记录相应时间。
开始、心跳和空增量不构成首个输出，显式报告的计时优先于宿主缺省观测。
`upstream_response_ms`、`upstream_api_overhead_ms`、`upstream_engine_ms` 只能来自上游原始计时，
没有来源时保持缺失。观测合同不包含总耗时、速率或计算费用，这些由宿主业务层消费原始值计算

成功终态可附带 `UpstreamContinuation`，包含上游响应 ID、不超过 32 KiB 的私有续接材料和作用域：

- `persisted`：续接材料随宿主会话保存，仍受实例、代次、Key、账号和凭据版本限制
- `connection_local`：还必须保有本次已建立的 WebSocket；宿主在成功终态后保存同一连接，续接时独占取回

此处代次指插件进程的握手身份；配置未变化的实例在宿主发布集合之间复用时，该身份保持不变。
连接内状态不能跨账号、Key、凭据版本、插件进程或代次复用，失效时拒绝续接，不重新建立连接假装原生续接成功。
每个适配器代次最多保留 64 条闲置连接，闲置期限 30 分钟；保留连接不占用上一轮账号租约。
显式 `close()` 放弃连接内续接。普通成功、错误、取消与退出释放未保留连接
