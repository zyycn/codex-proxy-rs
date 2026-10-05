# 清单与处理器实现

仅在编写或排查插件清单、Rust 处理器时读取，涉及 Rust 时使用[开发入口](../../cpr-dev-guide/SKILL.md)的 Rust 分支
选择能力时只读 SDK 的[能力与方法](../../../../backend/crates/gateway-plugin/sdk/docs/capabilities.md#能力与方法)及所需能力章节

## 清单与处理器

- `publisher` 与机器短名 `name` 派生插件 ID，`displayName` 用于展示；复制示例后同时调整扩展项 ID 与注册结果
- `contributes` 每种能力最多一项，普通贡献项可省略派生 ID、能力版本和固定阶段；中间件必须声明版本 `3` 并显式选择阶段。自定义完整扩展项 ID 属于自己的插件命名空间，运行注册与规范化清单一致
- 作者清单用 `Manifest::from_author_slice` 规范化，不手写 `package`、固定阶段或重复注册；CLI 生成平台、协议与资源摘要，版本范围不能使用全版本通配
- 安装后宿主会按 `configurationSchema` 准备默认配置，配置完整即可启用；默认绑定的空范围不限制请求。需要业务参数时声明真实必填项，通过 `secretFields` 声明敏感字段，不依赖用户再走一遍自建安装向导
- Rust 插件只依赖公开 `gateway-plugin-sdk`；需要异步会话辅助时开启 `io`，使用 `PluginSession` 管理握手、回调关联、流控、取消和关闭
- 优先用 `PluginBuilder::from_json` 组合 `.middleware`、`.management`、`.command_line` 与 `.on(methods::..., handler)`，由构建器生成注册并检查处理器；单一中间件也可用 `MiddlewarePlugin`
- stdout 是二进制协议通道，不能用 `println!` 输出诊断。使用基础设施 `host.log`，不输出 secret 或完整请求

## 中间件与宿主服务

- `request` 包裹整个逻辑请求，`attempt` 在每次选定账号的尝试中执行；根据作用范围选择，避免重试时重复副作用
- `next` 只能消费一次。使用 SDK 的正文保留与流式映射能力，不自行拼一套 SSE／取消／流控机制；HTTP、WebSocket、服务和模型请求按各自的类型化视图处理
- 持久化状态使用已声明命名空间的 `host.state.*`，账号读写使用 `host.auth.*`，不直接访问宿主数据库。见[账号与凭据](../../../../backend/crates/gateway-plugin/sdk/docs/capabilities.md#账号与凭据)和[状态、日志与迁移](../../../../backend/crates/gateway-plugin/sdk/docs/capabilities.md#状态日志与迁移)
- 按[完整信任](../../../../backend/crates/gateway-plugin/sdk/docs/manifest.md#完整信任)使用宿主回调，不添加权限或访问域声明；资源选择属于插件业务，父调用、期限、取消、实例 revision 与事务校验仍须满足
- 模型、网络和亲和查询使用当前父调用的受管回调。管理／CLI 等独立入口调用模型时传明确的 Key ID；请求处理阶段继承父身份。见[Key、模型与模型调用](../../../../backend/crates/gateway-plugin/sdk/docs/capabilities.md#key模型与模型调用)
- SDK 操作名不是管理员 HTTP 路由，不能据此拼接口地址；主动调用 HTTP 使用 `dispatch_http`，类型化服务使用 `HostClient::service`，实际执行与账单事实仍由宿主持有
- 路由与调度只读取宿主投影的请求事实，身份头可能被隐藏；使用已有会话和亲和合同，不依赖原始认证头或根据缓存键补造客户端身份
- 需要自定义上游编码、路径与响应解析时使用[受管上游适配器](../../../../backend/crates/gateway-plugin/sdk/docs/upstream-adapters.md)，它是已选号后的终端，不调用 `next` 或自行组织换号重试；普通请求和响应加工仍使用中间件
