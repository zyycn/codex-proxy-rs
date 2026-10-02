# 受管上游适配器

`upstream_adapter` v2 是现有生成请求洋葱链的终端，只挂到内置 `openai` / `xai`，不替换独立图像或压缩端点。
插件定义 Base URL、业务路径、请求编码和响应解析；宿主继续持有身份、ACL、选号、租约、凭据、代理、重试和账本。
普通请求或响应加工使用 [middleware](capabilities.md#洋葱中间件)

## 声明与绑定

清单声明 `upstream_adapter`、版本 `2`，以及匹配的 `inputFormats` / `outputFormats`；固定阶段 `upstream` 由作者清单规范化入口生成。
使用 `PluginBuilder::on` 注册
`methods::UPSTREAM_ADAPTER_REGISTER` 与 `methods::UPSTREAM_ADAPTER_EXECUTE`

注册返回 `UpstreamAdapterRegistration`，包含 1 至 16 个适配器：

| 字段 | 含义 |
| --- | --- |
| `id` | 实例内唯一标识，不是 Provider 或账号类型 |
| `provider` | `openai` 或 `xai` |
| `base_url` | 以 `/` 结尾的 HTTP(S) 基址，无用户信息、查询串或 fragment |
| `paths` | 不以 `/` 开头的基址相对路径，含 `inference` 或 `auxiliary` 用途，无通配符 |
| `authentication_kinds` | 可使用的现有认证类型，宿主复核已选账号 |
| `transport` | `http_json`、`http_sse` 或 `websocket` |
| `protocol` | 请求和客户端 wire 使用的协议，须在清单输入、输出格式内 |
| `models` | 公开请求模型的精确匹配，空集合沿用绑定范围 |

实例绑定沿用 Key、账号组、Provider、模型范围，只允许一次 `upstream` 绑定和 `reject` 故障策略。
同一请求不能同时命中两个适配器；配置准备时拒绝重叠，执行时再次复核。适配器未命中时仍使用原生上游，
已命中但不可用时拒绝请求。安装、配置、启停和升级使用已有插件管理

upstream_adapter v1 在[弃用窗口](manifest.md#接口弃用)内仍使用旧布尔投影，不能仅修改清单版本而继续使用旧 DTO。能力版本、清单版本、进程 RPC 版本和宿主兼容声明格式版本分别判断，见[清单](manifest.md)。
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

正文与普通 `HostClient::http` 共用 `read`、`collect(maximum_bytes)` 和 `close`；每次读取至多 64 KiB，
不后台预读，不向作者暴露流 ID。提前关闭及收集超限会释放正文，父调用结束或取消时宿主兜底回收。
JSON 可有界收集，SSE 应增量解析，不能把读取块当成完整事件

WebSocket 使用 `call.host.upstream_websocket(request).await?`，返回
`UpstreamWebSocketUpgrade::Connected { headers, connection }` 或 `Rejected { status, headers, body }`。
连接提供 `send(kind, body)`、`read()`、`close()`，支持完整文本与二进制消息，Ping/Pong 由宿主处理。
读写可同时进行，等待上游输出时可以发送控制消息；有先后依赖的消息须顺序等待 `send`。
同一时刻只允许一个读取者；取消一次 `read()` 等待后，再次读取继续同一次接收，不跳过消息。
`close` 会唤醒在途操作并释放连接，并发关闭等待同一次完成；读取超时或连接错误也会关闭连接。
握手拒绝保留 HTTP 状态和正文。底层 `host.upstream.*` 是 SDK 使用的资源协议

```rust,ignore
let (message, ()) = tokio::try_join!(
    connection.read(),
    connection.send(WebSocketMessageKind::Text, control_message),
)?;
```

宿主先注入已选账号认证，插件显式提供的同名头覆盖默认值。路径支持相对基址或完整 HTTP(S) URL；
声明路径用于标记发送用途，不是访问白名单。出站复用账号代理，代理失败不能回退直连，重定向不自动跟随。
`auxiliary` 路径的出站计入请求副作用水位，防止附件上传等操作在失败后被透明重复执行

## 事件、结算和续接

执行返回 `TypedReply<Empty>` 的有界 `ResponseStream`。每个 chunk 使用 `UpstreamAdapterEvent::encode()`，
包含标准事实与可选原始 wire；事件需符合开始、内容、工具、用量和唯一结束的顺序。
原始 JSON / SSE 字节与标准事实分开传递，错误使用 `UpstreamFailure`；宿主网络观测决定发送状态，插件不能将其降为未发送

用量提交标准 `Usage`，费用由内置 Provider 按已冻结的上游模型与价格计算，响应回显模型不切换计价。
`service_tier` 记录上游回显的最新档位，允许上游在完成时确定实际值；OpenAI 计价沿用请求策略，回显档位仅作观测。
未知用量或价格保留未知。宿主确认 RPC 正常结束后才发布完成事实，插件不能独立结算或组织换号重试

成功终态可附带 `UpstreamContinuation`，包含上游响应 ID、不超过 32 KiB 的私有续接材料和作用域：

- `persisted`：续接材料随宿主会话保存，仍受实例、代次、Key、账号和凭据版本限制
- `connection_local`：还必须保有本次已建立的 WebSocket；宿主在成功终态后保存同一连接，续接时独占取回

连接内状态不能跨账号、Key、凭据版本、插件进程或代次复用，失效时拒绝续接，不重新建立连接假装原生续接成功。
每个适配器代次最多保留 64 条闲置连接，闲置期限 30 分钟；保留连接不占用上一轮账号租约。
显式 `close()` 放弃连接内续接。普通成功、错误、取消与退出释放未保留连接
