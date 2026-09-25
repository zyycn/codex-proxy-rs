---
name: plugin-dev
description: 开发 Codex Proxy RS 网关插件，包括选择扩展能力、编写 plugin.json、实现公开 SDK 处理器、管理页面、打包与本地验证。用于新建插件、修改插件或排查插件接入；不用于 OpenAI Codex 的 .codex-plugin 插件、Vue 插件或仅修改宿主插件管理功能。
---

# plugin-dev

面向插件作者。插件在独立项目中实现，通过公开 SDK 与宿主通信，不向宿主业务模块添加分支。
仅修改宿主安装器、Runtime 或管理端时，使用仓库开发指南，不按本技能创建插件工程。

## 先建立正确的模型

```text
选择能力 → 作者清单 + SDK 处理器 → 构建二进制／页面 → cpr-plugin package
                                                       ↓
实际业务验证 ← 查看权限并安装、自动准备配置 ← tar.gz + sha256
```

- `contributes` 声明提供什么，`permissions` 声明访问域；安装接受记录派生授权，`bindings` 只决定功能挂载和请求匹配
- 安装包、运行配置和已启用功能不是同一件事；安装成功不能代替功能验证
- `trustedProcess` 与宿主使用相同系统身份，不是操作系统沙箱

## 开始工作

1. 确认插件用途、已有工程／输出目录、目标宿主版本与平台。已有项目沿用其结构；没有指定工程时先确认目录，不把插件源码塞进宿主 workspace
2. 先读 [SDK 入口](../../../backend/crates/gateway-plugin/sdk/README.md) 和[清单](../../../backend/crates/gateway-plugin/sdk/docs/manifest.md)，再按下表只选本次需要的能力章节
3. 核对实际运行宿主的版本、目标版本的 [SDK Cargo.toml](../../../backend/crates/gateway-plugin/sdk/Cargo.toml) 与[宿主支持清单](../../../backend/crates/gateway-plugin/runtime/plugin-host-compatibility.json)。同时确认打包 CLI 使用相同作者清单合同；不要只看源码版本或依赖包版本号。清单版本、通信版本、能力版本各自独立
4. 需要构建、联调或交付时读取 [构建与验证](references/development.md)，先检查实际依赖和脚本，不假设包已公开发布；通过 GitHub 交付时另读其中的[发布与安装来源](references/development.md#发布与安装来源)

本技能相对链接以各文件所在目录解析；主仓根目录是本文件所在目录向上三级。插件工程或同级示例不在磁盘上时，请用户提供位置或使用选定版本的公开资料，不猜开发机绝对路径，也不自动克隆或创建远程仓库。

## 按任务选择能力

| 用户要做什么 | 优先使用 | 需要读取的合同 |
| --- | --- | --- |
| 改写请求／响应、处理流式内容、协议转换 | `middleware` | [洋葱中间件](../../../backend/crates/gateway-plugin/sdk/docs/capabilities.md#洋葱中间件) |
| 改变模型选择或账号排序 | `model_router` / `scheduler` | [策略类型](../../../backend/crates/gateway-plugin/sdk/src/call/policy/mod.rs)，宿主继续复验范围、资格和租约 |
| 发布模型别名 | `model_catalog` | [模型目录](../../../backend/crates/gateway-plugin/sdk/docs/capabilities.md#模型目录)，注册时冻结直接目标，不需要请求绑定 |
| 读取基础数据制作管理页面 | `management` + `data` 权限 | [基础事实](../../../backend/crates/gateway-plugin/sdk/docs/capabilities.md#基础事实)，不需要账号凭据授权 |
| 选择宿主允许的重试动作 | `retry_policy` | [重试决策](../../../backend/crates/gateway-plugin/sdk/docs/capabilities.md#重试决策)，不能修改预算或自行重复调用 next |
| 统计成功失败、用量或观察 WS 帧 | `request_lifecycle` / `usage` / `web_socket_observer` | [路由、调度与观察](../../../backend/crates/gateway-plugin/sdk/docs/capabilities.md#路由调度与观察)，观察不改写业务结果 |
| 接受外部客户端身份 | `frontend_authentication` | [数据面入口认证](../../../backend/crates/gateway-plugin/sdk/docs/capabilities.md#数据面入口认证)，映射现有 Client Key，不接管后台登录 |
| 增加插件页面与管理操作 | `management` | [管理页面、公开入口与 CLI](../../../backend/crates/gateway-plugin/sdk/docs/capabilities.md#管理页面公开入口与-cli) |
| 提供终端命令 | `command_line` | [管理页面、公开入口与 CLI](../../../backend/crates/gateway-plugin/sdk/docs/capabilities.md#管理页面公开入口与-cli) |

只声明实现了的能力。Provider 固定为宿主内置的 OpenAI 与 xAI；插件只能扩展已开放的处理与管理能力。
网络、账号、模型调用和私有状态属于宿主回调与授权，不重复声明为顶层能力。

## 实现时的关键选择

### 清单与处理器

- `publisher` 与机器短名 `name` 派生插件 ID，`displayName` 用于展示；复制示例后同时调整扩展项 ID 与注册结果
- `contributes` 每种能力最多一项，普通贡献项可省略派生 ID、能力版本和固定阶段；中间件阶段仍需显式选择。自定义完整扩展项 ID 属于自己的插件命名空间，运行注册与规范化清单一致
- 作者清单用 `Manifest::from_author_slice` 规范化，不手写 `package`、固定阶段或重复注册；CLI 生成平台、协议与资源摘要，版本范围不能使用全版本通配
- 安装后宿主会按 `configurationSchema` 准备默认配置，配置完整即可启用；默认绑定的空范围不限制请求。需要业务参数时声明真实必填项，通过 `secretFields` 声明敏感字段，不依赖用户再走一遍自建安装向导
- Rust 插件只依赖公开 `gateway-plugin-sdk`；需要异步会话辅助时开启 `io`，使用 `PluginSession` 管理握手、回调关联、流控、取消和关闭
- 优先用 `PluginBuilder::from_json` 组合 `.middleware`、`.management`、`.command_line` 与 `.on(methods::..., handler)`，由构建器生成注册并检查处理器；单一中间件也可用 `MiddlewarePlugin`
- stdout 是二进制协议通道，不能用 `println!` 输出诊断。使用基础设施 `host.log`，不输出 secret 或完整请求

### 中间件与宿主服务

- `request` 包裹整个逻辑请求，`attempt` 在每次选定账号的尝试中执行；根据作用范围选择，避免重试时重复副作用
- `next` 只能消费一次。使用 SDK 的正文保留与流式映射能力，不自行拼一套 SSE／取消／流控机制；请求、响应及策略处理使用 `requests` 域
- 持久化状态使用已声明命名空间的 `host.state.*`，账号读写使用 `host.auth.*`，不直接访问宿主数据库。见[账号与凭据](../../../backend/crates/gateway-plugin/sdk/docs/capabilities.md#账号与凭据)和[状态、日志与迁移](../../../backend/crates/gateway-plugin/sdk/docs/capabilities.md#状态日志与迁移)
- 权限只声明 `network`、`models`、`accounts`、`data`、`requests`、`public_endpoints`；日志和自身状态无需授权项。安装器不配置 Key／账号白名单，资源选择属于插件业务
- 模型、网络和亲和查询使用当前父调用的受管回调。管理／CLI 等独立入口调用模型时传明确的 Key ID；请求处理阶段继承父身份。见[Key、模型与模型调用](../../../backend/crates/gateway-plugin/sdk/docs/capabilities.md#key模型与模型调用)
- SDK 操作名不是管理员 HTTP 路由，不能据此拼接口地址；宿主身份、账单事实和调用授权也不能由插件覆盖
- 路由与调度只读取宿主投影的请求事实，身份头可能被隐藏；使用已有会话和亲和合同，不依赖原始认证头或根据缓存键补造客户端身份

### 只有需要页面时才创建前端

- 优先参考独立示例仓库的 `examples/workbench/frontend/src/api/`：业务路由放在 `modules/`，`request.ts` 封装公开宿主桥；按需使用 `@codex-proxy/ui` 包出口，不跨仓导入宿主或 UI 的内部源码
- Vue 页面保留 SFC，逻辑、脚本和配置使用 TypeScript；只实现业务内容，标题和副标题由 `ManagementPage` 交给宿主显示
- 隔离页面通过 `window.codexProxyPlugin.request` 调用已注册管理路由；`models.responses` 通过普通请求链交付 JSON/SSE，支持取消且不暴露 Key 明文。不直接 `fetch` 宿主、不读取管理 Cookie 或借用宿主 Vue 实例
- 管理路由的响应正文由插件业务定义，不默认套用宿主管理 API 的 `{ code, message, data }`；页面与处理器共享实际合同，GET 无正文时不附带 JSON 正文声明
- `host.model.*` 子请求跳过发起插件，不能用它证明本插件中间件／路由已参与；需要完整请求链的示例使用页面模型桥或真实客户端
- JS、CSS、图标等资源随包构建和声明；沿用宿主桥的主题同步。Vite 本地预览不等于已安装插件的热更新，宿主验证仍需重新构建、打包和切换版本

## 交付

按[构建与验证](references/development.md#验证与交付)提供源码位置、能力／权限／绑定摘要、实际运行结果及验证缺口。
开发插件不默认授权安装并启用本机进程、调用真实上游、发布依赖或推送仓库；超出当前授权时交付可安装包及操作步骤。
