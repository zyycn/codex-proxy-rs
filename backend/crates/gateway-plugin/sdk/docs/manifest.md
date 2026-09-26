# 插件清单

[返回 SDK](../README.md) · [能力与回调](capabilities.md)

`Manifest` 对应包内 `plugin.json`。作者源清单由 `Manifest::from_author_slice` 读取，CLI 打包也调用同一入口；
因此本地构建器、生成的安装清单和运行注册共享一份规范化结果。

| 字段 | 规则 |
| --- | --- |
| `publisher` + `name` | 发布者命名空间与机器短名；`plugin_id()` 派生唯一 ID，如 `acme.echo` |
| `displayName` / `author` | 人类可读名称 / 可选作者文字，不参与身份与授权 |
| `version` / `engines` | 插件业务版本 / 宿主版本范围；安装包不能使用全版本通配 `*` |
| `contributes` | 插件提供的扩展能力；作者可省略可推导字段 |
| `permissions` | 需要访问的宿主资源域；安装即接受该版本声明的域 |
| `configurationSchema` / `secretFields` | 普通设置的 schema / 单独保存的敏感字段 |
| `main` / `resources` | 包内可执行文件 / 资源路径与 MIME 类型 |
| `state` | [私有状态](capabilities.md#状态日志与迁移)的命名空间、schema 与配额 |
| `package` | CLI 生成的协议版本、单一目标平台与文件摘要，不需作者手写 |

作者清单不能包含 `package`。使用 `Manifest::from_author_slice()` 校验并规范化；直接反序列化的 `Manifest`
代表完整合同，不会隐式补全作者字段。打包后，`main` 和所有资源都必须进入 `package.files`；宿主用
`package_for(host, os, architecture)` 检查版本与平台。运行模式仅为 `trustedProcess`：插件拥有与宿主
相同的系统身份，**不是进程沙箱**。

## 扩展项简写

普通作者声明可以省略 `id`、`version` 和固定阶段：

```json
{
  "contributes": {
    "management": {},
    "command_line": {},
    "middleware": {
      "stages": ["request"],
      "inputFormats": ["openai"],
      "outputFormats": ["openai"]
    }
  }
}
```

默认扩展项 ID 为 `<publisher>.<name>.<capability-kebab>`，版本默认为 `1`；使用能力需求声明的 `middleware` 选择 `2`。除 `middleware` 外，阶段由
capability 固定并由工具生成：

| 阶段 | 能力 |
| --- | --- |
| `authentication` / `routing` / `scheduling` | `frontend_authentication` / `model_router` / `scheduler` |
| `registration` / `retry` | `model_catalog` / `retry_policy` |
| `observation` | `request_lifecycle`、`web_socket_observer`、`usage` |
| `management` / `command_line` | `management` / `command_line` |
| `maintenance` | `maintenance` |

`middleware` 必须显式选择 `request`、`attempt` 或两者；协议格式等真实业务选择也不能省略。
安装清单若携带不同的固定阶段会被拒绝，而不是在加载时静默改写。

## 权限

权限使用以下稳定访问域：

| 标识 | 含义 |
| --- | --- |
| `network` | 使用宿主受管网络 |
| `models` | 查询非秘密 Key 与模型并调用模型，可能产生消耗 |
| `accounts` | 查询、读取原始凭据和修改账号 |
| `data` | 仅在管理、命令与维护阶段只读全部账号的基础信息和已有额度观测 |
| `requests` | 查看和处理请求、响应、路由、调度与观察事实 |
| `groups` | 创建本实例分组并管理所有当前及未来账号在这些分组中的成员关系 |
| `keys` | 创建绑定本实例分组的 Key，不读取密钥明文 |
| `public_endpoints` | 提供无需登录即可访问的资源或回调 |

安装时统一接受清单声明的域，不再填写逐方法、用途、Key、账号或 Provider 白名单。日志与清单声明的
本插件私有状态是基础设施，无需单独 permission。权限并不替代方法阶段、父调用、Key 规则、账号 revision、
资源归属和流生命周期校验，具体接口见[能力与回调](capabilities.md#访问域)。

## 插件图标

`icon` 指定管理端展示的插件图标。值可以是一个包内文件路径，也可以是包含 `light`、`dark` 两个路径的对象。
使用单个路径时，浅色和深色主题共用同一张图；使用主题对象时，管理端按当前主题选择，两项都必须填写。
不填写 `icon` 时显示通用图标。图标路径不是远程 URL，也不接受内置图标名称。

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
以图片方式展示，不将 SVG 源码插入页面 HTML。
