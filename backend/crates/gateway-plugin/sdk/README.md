# Gateway Plugin SDK

用于编写 Codex Proxy 插件的 Rust 合同与可选会话辅助。插件只依赖本包，不依赖宿主 Core、Admin、Store 或 Host。
SDK 包版本为 `0.1.0`，独立于网关版本，处于实验阶段。能力是否可用取决于宿主支持、清单声明与实际注册

清单、进程协议和各项能力分别版本化，当前版本与声明规则统一见[插件清单](docs/manifest.md)

开发顺序：编写[清单](docs/manifest.md) → 实现[能力处理器](docs/capabilities.md) → 用 [CLI](../../../apps/plugin-cli/README.md)
打包 → [安装、配置与启用](../../../../docs/plugins.md)。完整可运行示例位于独立仓库
`codex-proxy-plugins` 的 `examples/workbench`

使用 AI 协作开发时，可调用仓库技能 [`$cpr-plugin-dev`](../../../../.agents/skills/cpr-plugin-dev/SKILL.md)，按任务定位合同、示例和验证入口

## 模块与依赖

| 入口 | 职责 |
| --- | --- |
| 根门面 | 清单、能力声明、调用上下文、消息与帧、公开错误 |
| `call::frontend_authentication` | 数据面认证信封、认证器标识与外部 principal 结果 |
| `call::model` | 宿主模型调用的 canonical 事实、原生 wire 与有界事件编解码 |
| `call::middleware` | HTTP、WebSocket 消息、公开服务、模型请求、attempt 与上游出站准备的洋葱调用、单次 next 与惰性正文合同 |
| `call::services` | 公开服务的类型化操作、输入输出及错误合同；`usage` 提供[用量维护与计价](docs/usage-data.md) |
| `call::upstream_adapter` | 内置 OpenAI / xAI 的[受管上游适配器](docs/upstream-adapters.md)注册、执行、出站与续接合同 |
| `call::policy` | 模型路由、账号调度与重试决策合同 |
| `call::catalog` | 固定 Provider 的模型别名注册合同 |
| `call::data` | 最小账号事实与已有额度观测的只读合同 |
| `call::observation` | 请求完成、用量、费用与实际上游 WebSocket 响应事件的统一观察合同 |
| `call::host` | 账号、私有状态、受管 HTTP、模型、亲和、日志及调用内流的宿主回调合同 |
| `call::management` | 管理 API、页面与资源声明、公开回调，以及 CLI 命令与待保存账号 |
| `client` | 通过 `io` feature 开启的 `PluginBuilder`、会话、中间件与异步帧收发 |

`call/` 按业务能力组织数据，`message.rs` 定义消息与帧。已有请求链的请求／响应改写、协议转换和思考参数映射使用
`call::middleware`；需要指定业务上游并解析结果时使用 `call::upstream_adapter`，作为洋葱链终端复用宿主账号与结算。
下游 WebSocket 改写使用 `websocket` 中间件；上游发送前的请求准备使用 `outbound` 中间件，响应观察接口保留只读合同

插件使用的导入路径例如：

```rust
use gateway_plugin_sdk::{Frame, Message, PluginFault};
use gateway_plugin_sdk::call::{host::ModelEventBatch, model::ExecutionEvent};
// 需要 Cargo 依赖声明启用 features = ["io"]。
use gateway_plugin_sdk::client::{read_frame, write_frame};
```

默认 feature 不引入异步运行时。`io` 除帧编解码外，还提供 `PluginBuilder`、
`PluginSession::accept/run` 和 `PluginHandler`。组合插件从作者清单建立类型化方法，SDK 自动生成
`plugin.register`；作者不需要复制贡献项或手写方法名、阶段和敏感载荷编解码。
会话管理握手、有界并发、父调用关联、回调、流信用、取消、排空与关闭，容量与计时见[并发与期限](#并发与期限)

`ResponseStream::channel` 提供有界流生产入口，`ResponseStream::pull` 按消费进度驱动生产者；`PluginCall` 的宿主客户端只代表当前父调用，
不能把一个调用的资源误用到另一个调用。插件程序自行选择运行时和启动入口，不会启动网关。
`SessionConfig.maximum_stream_chunk_bytes` 默认 1 MiB，只约束单个业务流分块；
流式响应还需满足宿主实际授予的信用窗口。普通调用与回调的正文不受此预算限制。
普通业务处理器仍接收完整正文，内存占用随正文增长；HTTP 与下游 WebSocket 中间件使用惰性正文句柄，透传不复制正文

## 通信边界

启用 `io` 时优先用 [`PluginBuilder`](docs/capabilities.md#类型化作者入口) 组合业务处理器，再交给
`PluginSession::run`。仅实现中间件时也可直接使用
[`MiddlewarePlugin`](docs/capabilities.md#洋葱中间件)

```mermaid
sequenceDiagram
  participant H as 宿主 Runtime
  participant P as 插件会话
  H->>P: Hello
  P-->>H: Ready（相同 incarnation）
  H->>P: 注册能力，核对清单
  P-->>H: 注册结果
  H->>P: Call（ID、上下文、载荷）
  opt 需要宿主资源
    P->>H: 回调（关联父调用 ID）
    H-->>P: 回调结果
  end
  P-->>H: 结果或错误；流以 End 结束
```

### 并发与期限

每个宿主插件会话默认允许 64 个执行中的 RPC 调用、64 个初次排队调用和 256 个总在途上下文。
初次排队和等待 `next` 后的恢复排队各限 30 秒，不扣插件自身默认上限 120 秒的处理预算。
恢复等待占总在途名额，不占初次排队名额。
等待 `next` 时释放执行名额并暂停处理预算，返回后重新取得名额，继续同一次调用；
已返回响应头的流继续占用在途预算，不占首结果执行名额。具体组合行为见[洋葱中间件](docs/capabilities.md#洋葱中间件)

插件侧 `SessionConfig.maximum_calls` 默认 256，限制包含下游等待和响应流在内的在途调用；
`maximum_callbacks` 默认 64，限制普通宿主回调。宿主普通回调的默认上限同样为 64，
`next` 使用总在途上限对应的独立回调预算，不挤占普通回调容量

实际容量由宿主预算与 SDK 会话配置共同限制，较小的插件侧上限会提前拒绝新调用。
达到容量或等待上限时，中间件返回过载错误；HTTP 响应为 503（`middleware_capacity_unavailable`），
不跳过插件或重放请求。调整 SDK 会话配置需重新构建插件包，容量应结合插件耗时、正文和响应流的资源占用评估

### 帧与会话

`CallContext.timeout_ms` 限定初始处理期限。宿主对由流或连接 owner 管理的调用设置 `resource_stream`：
首个流式结果成功返回后，正文、会话及关联回调由资源取消与终态回收，不再沿用初始期限。
普通调用继续使用原期限；资源模式仍保留并发容量、Credit、父调用关联及取消约束

协议 v5 的帧头由大端 `u32` 元数据长度和大端 `u64` 载荷长度组成，随后依次传输 JSON 元数据和二进制载荷。
元数据保留 64 KiB 上限；正文总量不由插件传输层额外限制。传输内部以 256 KiB 为单次 I/O 块，
不逐块发起 RPC，也不要求插件配置与宿主同步。读取按进度扩展缓冲区；截断正文必须报错，不能作为完整消息交付

JSON 元数据只承载消息类别、调用关联及分段长度。载荷依次包含入口快照 JSON、业务 JSON 和原始二进制正文：

- `request_context_bytes` 指定 `CallContext.request` 的快照长度；无快照时省略
- `data_bytes` 指定 `Hello.configuration`、`Call.params`、`Callback.params` 或 `Result.result` 的 JSON 长度；这些消息必须携带该段，其他消息省略
- 两个 JSON 段之后的剩余字节是 `Frame.payload`，其内容不经过 JSON 编码

入口快照的 `headers[].value` 使用标准带填充 base64 字符串，保留重复头与任意字节；Rust API 仍为 `Vec<u8>`。这个表示法仅用于 `RequestContext`，不改变中间件业务数据的头值合同

SDK 帧读写器自动拆装这些字段，不拼接复制二进制正文。`Message` 与 `Handshake` 的 serde 表示只包含控制字段；完整消息应通过 `read_frame` / `write_frame` 读写

帧写入队列前用 `EncodedFrame::new` 完成编码和校验，写泵通过 `EncodedFrame::write` 复用编码结果，
避免重复序列化或让本地输入错误关闭共享传输。编码帧不保留业务 JSON 与入口快照的原始副本，
`EncodedFrame::message` 仅供关联消息类别和 ID。只需校验时可使用 `validate_frame`。
协议校验不会自动重试业务调用；部分读写中断后不能把当前位置当作新帧边界。
底层帧读写方法不是取消安全的；自行编排会话时，不能在 `select!` 的其他分支完成后
重建半途中的帧读写。`PluginSession` 会在处理调用完成通知时保留同一个读取 future，直到完整帧到达或会话关闭

插件须校验 `Hello` 的协议版本。一次 `Call` 的结果、错误和流帧按 ID 关联；实例、代次、阶段、账号或资源 ID 用于关联调用和管理资源生命周期

## 本地验证

以下命令面向宿主仓库中的 SDK 维护。独立插件的构建、安装与能力验收见[开发技能](../../../../.agents/skills/cpr-plugin-dev/SKILL.md)及所用示例

### 生成服务合同

设置与账号自动重置服务的 SDK 类型和操作由宿主领域类型及[操作声明](../../gateway-admin/src/public_service)生成，SDK 构建不依赖宿主源码。
修改宿主合同后，在仓库根目录更新生成文件；Admin 合同测试会检查生成结果是否同步：

```bash
CPR_UPDATE_SERVICE_CONTRACT=1 RUST_MIN_STACK=16777216 cargo +1.99.1 test --manifest-path backend/Cargo.toml -p gateway-admin --test main sdk_settings_contract_matches_host_declarations --locked
CPR_UPDATE_SERVICE_CONTRACT=1 RUST_MIN_STACK=16777216 cargo +1.99.1 test --manifest-path backend/Cargo.toml -p gateway-admin --test main sdk_accounts_contract_matches_host_declarations --locked
cargo +1.99.1 fmt --all --manifest-path backend/Cargo.toml
```

用量服务合同由 Admin 与 SDK 分别持有，`sdk_wire_types_match_host_contracts` 检查数值、明细、聚合、查询和计价类型一致性；修改合同需同步两侧类型与 [使用说明](docs/usage-data.md)

### 验证命令

```bash
RUST_MIN_STACK=16777216 cargo +1.99.1 test --manifest-path backend/Cargo.toml -p gateway-plugin-sdk --no-default-features --locked
RUST_MIN_STACK=16777216 cargo +1.99.1 test --manifest-path backend/Cargo.toml -p gateway-plugin-sdk --all-features --locked
RUST_MIN_STACK=16777216 RUSTDOCFLAGS='-D warnings' cargo +1.99.1 doc --manifest-path backend/Cargo.toml -p gateway-plugin-sdk --all-features --no-deps --locked
```

源码中的类型和字段是当前合同；宿主接入、目录边界、内置 Provider 与 Runtime 集成验证见
[系统架构](../../../../docs/architecture.md#31-插件扩展)。
单元测试与文档构建不代替目标平台上的安装、配置及实际能力验证
