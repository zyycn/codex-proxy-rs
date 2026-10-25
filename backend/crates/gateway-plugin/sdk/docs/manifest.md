# 插件清单

[返回 SDK](../README.md) · [能力与回调](capabilities.md)

`Manifest` 对应包内 `plugin.json`。作者源清单由 `Manifest::from_author_slice` 读取，CLI 打包也调用同一入口；
因此本地构建器、生成的安装清单和运行注册共享一份规范化结果

| 字段 | 规则 |
| --- | --- |
| `publisher` + `name` | 发布者命名空间与机器短名；`plugin_id()` 派生唯一 ID，如 `acme.echo` |
| `displayName` / `author` | 人类可读名称 / 可选作者文字，不参与身份与授权 |
| `version` / `engines` | 插件业务版本 / 宿主版本范围；安装包不能使用全版本通配 `*` |
| `contributes` | 插件提供的扩展能力；作者可省略可推导字段 |
| `configurationSchema` / `secretFields` | 普通设置的 schema / 单独保存的敏感字段 |
| `main` / `resources` | 包内可执行文件 / 资源路径与 MIME 类型 |
| `state` | [私有状态](capabilities.md#状态日志与迁移)的命名空间、schema 与配额 |
| `package` | CLI 生成的协议版本、单一目标平台与文件摘要，不需作者手写 |

作者清单不能包含 `package`。使用 `Manifest::from_author_slice()` 校验并规范化；直接反序列化的 `Manifest`
不会执行作者清单的 ID 与固定阶段推导，声明的可选字段仍使用其默认值。打包后，`main` 和所有资源都必须进入 `package.files`；宿主用
`package_for(host, os, architecture)` 检查版本与平台。运行模式为 `trustedProcess`，信任边界见[完整信任](#完整信任)

宿主实际开放的合同由 [plugin-host-compatibility.json](../../runtime/plugin-host-compatibility.json)
声明，SDK 的 `Capability::contract_versions()` 只表示 SDK 能描述的行为版本，不能代替宿主支持检查

| 版本字段 | 当前值 | 对应合同 |
| --- | --- | --- |
| `manifestVersion` / `manifest_schema_versions` | `2` | 插件清单格式 |
| `package.protocolVersion` / `protocol_versions` | `6` | 插件进程 RPC 封装，不是 OpenAI 或 xAI 的业务协议 |
| `contributes.<capability>.version` / `capabilities[].versions` | 见[扩展项简写](#扩展项简写) | 指定扩展能力的行为合同 |
| 宿主声明的 `schema_version` | `2` | 宿主兼容声明自身的格式 |

`capabilities` 使用扩展能力标识；Provider、模型和账号 ID 不属于这个集合

清单不接受 `permissions` 字段；插件按[完整信任](#完整信任)运行

RPC v6 的帧布局、编码与会话生命周期见[通信边界](../README.md#通信边界)。插件使用 SDK 构建，并由 CLI 生成包含协议版本的安装包

安装包清单校验与已安装元数据读取是两个边界：清单拒绝未知字段，并检查必需字段、类型和版本；
数据库元数据的读取规则见[制品与发布](../../../../../docs/architecture.md#制品配置与发布)，不改变清单或 RPC 合同。
`engines.codex-proxy-rs` 声明插件所需的宿主范围。范围或能力版本不匹配时持续提示风险，允许尝试启动；包结构、平台与 RPC 封装校验失败阻止加载。CLI 严格校验作者声明是否符合 SDK 支持的合同

## 接口弃用

当前支持范围由[宿主兼容声明](../../runtime/plugin-host-compatibility.json)给出，弃用状态、剩余兼容窗口及作者说明以[弃用清单](../../runtime/plugin-api-deprecations.json)为准，管理端和加载日志展示同一提示

兼容窗口自替代合同首次正式发布起至少覆盖 7 个后续正式版本；主版本、次版本和补丁版本各计一次，预发行与同标签重试不计。
窗口内提供适配，窗口外不保证兼容；是否使用过特定字段不改变判断，版本号差值也不能代替清单中的计数

## 扩展项简写

作者声明可以省略 `id` 和固定阶段。版本字段省略时为 `1`，其余版本必须显式声明：

| 能力 | 当前版本 |
| --- | --- |
| `middleware` | `6` |
| `scheduler` | `3` |
| `upstream_adapter` | `5` |
| `frontend_authentication`、`model_router`、`retry_policy`、`observer`、`management` | `2` |
| `model_catalog`、`command_line`、`maintenance` | `1` |

例如，仅声明管理页面、命令行和模型请求中间件：

```json
{
  "contributes": {
    "management": { "version": 2 },
    "command_line": {},
    "middleware": {
      "version": 6,
      "stages": ["request"],
      "inputFormats": ["openai"],
      "outputFormats": ["openai"]
    }
  }
}
```

默认扩展项 ID 为 `<publisher>.<name>.<capability-kebab>`。除 `middleware` 外，阶段由
capability 固定并由工具生成：

| 阶段 | 能力 |
| --- | --- |
| `authentication` / `routing` / `scheduling` | `frontend_authentication` / `model_router` / `scheduler` |
| `registration` / `retry` | `model_catalog` / `retry_policy` |
| `observation` | `observer` |
| `upstream` | `upstream_adapter` |
| `management` / `command_line` | `management` / `command_line` |
| `maintenance` | `maintenance` |

`observer` 使用一个处理器接收完成与上游 WebSocket 事件，实例绑定按 `event` 选择订阅类型；具体合同见[观察事件](capabilities.md#路由调度与观察)

`middleware` 必须从 `http`、`websocket`、`service`、`request`、`attempt`、`outbound` 中显式选择挂载；同一处理器可覆盖多个边界，协议格式等真实业务选择也不能省略。
安装清单若携带不同的固定阶段会被拒绝，而不是在加载时静默改写

## 完整信任

安装并启用插件意味着信任其全部代码和行为。插件与宿主使用相同的系统身份，可以访问数据、凭据和网络；安全由安装者承担，宿主不提供插件安全沙箱

清单只声明处理器、配置和资源。宿主回调不需要权限声明，也不按调用阶段授予访问域；类型校验、期限、取消、实例 revision、事务及流资源生命周期仍然生效

## 插件图标

`icon` 指定管理端展示的插件图标。值可以是一个包内文件路径，也可以是包含 `light`、`dark` 两个路径的对象。
使用单个路径时，浅色和深色主题共用同一张图；使用主题对象时，管理端按当前主题选择，两项都必须填写。
不填写 `icon` 时显示通用图标。图标路径不是远程 URL，也不接受内置图标名称

在 `plugin.json` 中同时设置 `icon` 和对应的 `resources` 项。以下是单图标配置片段：

```json
{
  "icon": "assets/icon.svg",
  "resources": {
    "assets/icon.svg": "image/svg+xml"
  }
}
```

浅色和深色主题可以使用不同格式的图片：

```json
{
  "icon": {
    "light": "assets/icon-light.svg",
    "dark": "assets/icon-dark.webp"
  },
  "resources": {
    "assets/icon-light.svg": "image/svg+xml",
    "assets/icon-dark.webp": "image/webp"
  }
}
```

文件扩展名不区分大小写，文件内容、扩展名与声明的 MIME 类型必须一致：

| 格式 | 扩展名 | `resources` 中的 MIME 类型 |
| --- | --- | --- |
| SVG | `.svg` | `image/svg+xml` |
| PNG / APNG | `.png` | `image/png` |
| JPEG | `.jpg`、`.jpeg`、`.jfif` | `image/jpeg` |
| WebP | `.webp` | `image/webp` |
| GIF | `.gif` | `image/gif` |
| ICO | `.ico` | `image/vnd.microsoft.icon` 或 `image/x-icon` |
| BMP | `.bmp` | `image/bmp` 或 `image/x-ms-bmp` |

资源要求：

- 每个图标文件不超过 512 KiB；位图宽、高各为 1–4096 像素
- GIF、APNG 和 WebP 可以包含动画，每个文件最多 256 帧，累计解码数据不超过 128 MiB
- SVG 使用 UTF-8 编码，根元素为 SVG 命名空间中的 `<svg>`，XML 节点不超过 16384 个；不使用 DTD 实体或 XML 处理指令
- SVG 保留矢量、渐变、内联样式和 data URL 内嵌图片；图标不能依赖脚本、外部图片、外部样式或外部字体

将图标文件与清单一同交给 [插件 CLI](../../../../apps/plugin-cli/README.md) 打包。工具收集 `resources` 声明的文件，
生成 `package.files` 摘要；宿主在安装时验证图标内容。管理端通过 [图标读取接口](../../../../../docs/api.md#12-插件管理)
以图片方式展示，不将 SVG 源码插入页面 HTML
