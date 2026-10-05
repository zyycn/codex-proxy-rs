# 构建、联调与验证

仅在准备工程、构建或验证插件时读取对应章节，分发规则只在准备分发时读取
这里给出执行入口，不复制 SDK 协议或安装 API 的字段手册

## 工程与依赖

| 位置 | 用途 |
| --- | --- |
| 主仓 `backend/crates/gateway-plugin/sdk/` | Rust 公开合同、会话辅助与能力文档 |
| 主仓 `backend/apps/plugin-cli/` | `cpr-plugin package`，只校验与打包，不编译插件源码 |
| [codex-proxy-plugins](https://github.com/zyycn/codex-proxy-plugins) 的 `examples/workbench/` | 基础能力体验与文本工作流示例，按需选择功能，不整体复制 |
| [codex-proxy-ui](https://github.com/zyycn/codex-proxy-ui) | 可选的 Vue UI 库、组件文档与 playground |

先检查用户给定工程、`modules/plugins` 子模块或同级仓库是否存在，读取实际仓库约定及当前步骤所需的 README 章节、依赖与锁文件，不预读无关前端配置
只需中间件时不复制示例的管理 API 和页面；CLI 等能力从 SDK 对应合同起步，不给示例添加无关能力

本仓库子模块联调使用[开发文档](../../../../docs/development.md#源码联调)中的 source 模式和本地 SDK 验证入口，不改写正式依赖或锁文件
其他工程按其自身联调配置临时使用包级链接或 override，路径须真实可解析，不从业务源码跨仓 `import .../src/...`

对外交付前核实 UI／SDK 的可用版本和安装来源：仅引用确实存在的发布版本，或包含所需合同的固定 Git commit；
不要把浮动 `main` 当版本，也不要自动发布依赖以补齐条件。本地 override 尚未移除时，明确说明独立安装缺口。
所用工具链以目标项目的配置为准，不因本机安装了更新版本而顺手升级。

## 复用示例

找到 `codex-proxy-plugins` 后，只读根目录约定与 `examples/workbench/README.md` 中当前能力或构建所需的章节
该仓库使用 `bash scripts/package` 构建页面、Rust 二进制并打包；前端依赖和工具配置位于示例的 `frontend/`，根目录不维护 Node 工程。执行前核对脚本适用的插件路径和目标平台。

| 任务 | 示例入口 |
| --- | --- |
| 身份、声明与资源 | `examples/workbench/plugin.json` |
| 会话启动与处理器组合 | `examples/workbench/backend/src/main.rs`、`app.rs` |
| 管理接口 | `examples/workbench/backend/src/management/` |
| 页面与主题 | `examples/workbench/frontend/src/` |
| 管理接口调用 | `examples/workbench/frontend/src/api/modules/` |
| 包内静态资源构建 | `examples/workbench/frontend/vite.config.ts` |

若派生新插件，同时修改包名、二进制名、插件身份、扩展项 ID、注册描述和构建目标，不只改展示名称。
保留当前项目确实需要的依赖、资源和能力声明；不要把示例中的全请求范围直接用到共享环境

## 构建与打包

先运行目标工程已有的测试和构建入口。Rust 检查示意如下，路径、工具链和 target 按实际工程替换：

```bash
cargo fmt --manifest-path /path/to/plugin/Cargo.toml -- --check
cargo clippy --manifest-path /path/to/plugin/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo test --manifest-path /path/to/plugin/Cargo.toml --locked
cargo build --manifest-path /path/to/plugin/Cargo.toml --release --locked --target x86_64-unknown-linux-gnu
```

新工程尚无锁文件时先生成并检查锁文件；不要把缺少锁文件的 `--locked` 失败当作源码问题。
有页面时另跑该前端项目的检查与生产构建，确认产物路径、资源清单和 MIME 类型相符；无页面的插件无需 Node 项目。

先检查实际调用的 `cpr-plugin` 路径与构建来源。已有工具只有在支持目标作者清单时才复用；
遇到派生字段被要求手写或支持能力不符时，对照目标 SDK 和 CLI 合同，不通过改写清单迁就过期工具。
需要从目标宿主源码构建工具时，在主仓根目录运行以下命令，把 `--help` 换成实际参数即可，不必全局安装：

```bash
cargo +1.97.0 run --manifest-path backend/Cargo.toml -p codex-proxy-plugin-cli --locked -- package --help
```

直接打包的示意命令：

```bash
cpr-plugin package \
  --manifest /path/to/plugin/plugin.json \
  --binary /path/to/plugin/target/x86_64-unknown-linux-gnu/release/plugin \
  --target x86_64-unknown-linux-gnu \
  --resource-map web=frontend/dist \
  --output-dir /path/to/plugin/dist
```

没有 `web` 资源时去掉 `--resource-map`。映射源相对于作者清单目录，而非命令执行目录；资源必须留在插件工程内。
目标平台指运行宿主的服务器／容器，不是访问管理端的浏览器所在电脑；平台与二进制必须匹配，不能只改 target 参数伪装交叉编译。支持平台以 [CLI 文档](../../../../backend/apps/plugin-cli/README.md)及实际 `--help` 为准。

输出为 `.tar.gz` 与 `.sha256`，不是源码 zip、npm 包或页面目录。归档根目录应有生成后的 `plugin.json`、可执行文件和声明资源。
安装时以本次输出的包摘要为准，核对包内版本、平台、能力和 `engines` 与实际运行宿主匹配；
源码已更新不代表 `dist` 中旧包已重建。版本要求未满足时使用兼容宿主或确有依据的兼容声明，不跳过宿主检查。
本地同版本反复试验与对外发布区分处理；发布新内容使用明确的新版本，不覆盖已发布版本。

## 发布与安装来源

仅在用户要求发布或准备分发时执行；沿用目标插件仓库的发布流程，核对源码提交、包内版本、目标平台与产物摘要。

- GitHub 安装读取 Release 附件，发布时上传实际生成的 `.tar.gz`／`.tgz` 和校验文件；仅推送 tag 或保留 GitHub 自动生成的源码归档，不能提供可安装插件
- 稳定版应发布为非 Draft、非 Pre-release；需要支持留空标签安装时，再核对最新稳定版查询能返回预期 Release。版本号没有预发行后缀，不代表 GitHub 的发布状态已经是正式版
- 当前宿主在标签留空时只查询最新稳定版；预发行版安装需要同时指定标签并允许预发行，不把该开关解释成自动发现预发行版
- 多平台或多插件附件由用户按实际发布内容选择，文件名帮助辨识，包内清单才是平台、版本与兼容范围的依据
- 发布完成后，从公开下载来源核对附件和摘要，再按已授权的目标环境验证安装；本地包验证不等于远程发行附件已经验证

## 验证与交付

按[插件使用](../../../../docs/plugins.md)验证“校验安装包 → 确认来源并接受完整信任 → 自动准备默认配置 → 实际使用”；只在缺少业务必填值时补配置，不添加权限或资源选择向导
只有获得目标环境操作授权才安装和启用；不把修改源码的授权扩展到生产实例或真实凭据操作。
需要 API 自动化时查[插件管理 API](../../../../docs/api.md#12-插件管理)，不要依据记忆拼路由。

按实际实现选择验证，不为不存在的能力增加测试任务：

| 实现范围 | 应验证的行为 |
| --- | --- |
| 中间件 | 匹配／不匹配范围、单次 `next`、未改写字节保留、下游错误与取消；支持流式时检查多帧、终态和断开；若涉及续链，使用真实目标客户端检查 HTTP/SSE、WS 及后续轮次 |
| 上游适配器 | 绑定匹配、账号认证与代理、HTTP/SSE 或 WS 事件解析、发送状态、用量和终态；支持续接时核对 Key、账号、凭据版本与代次隔离，失败不能透明重放已发送操作 |
| 模型路由／调度 | 真实入口能命中规则，未匹配请求保持原有行为；使用官方客户端正常携带的业务头和会话别名，确认投影不会拒绝合法请求，也不暴露身份字段 |
| 生命周期／用量／WS 观察 | 完成一次实际模型请求并核对对应事件；WS 观察需确认实际上游采用 WS，HTTP 请求成功不能证明 WS 观察生效 |
| 管理页面 | 从宿主打开，标题／副标题、明暗主题、加载与错误状态；管理桥请求成功；停用或版本切换后旧入口失效 |
| 入口认证／CLI | 对应身份和账号范围、缺少授权、取消与错误；不把帮助输出、注册成功或一次健康探测当完整验收 |
| 持久状态或升级 | 读写版本冲突、停用再启用后的持久状态、升级兼容性及迁移失败；版本配置恢复与私有状态迁移分别验证，不覆盖并发配置或把回滚版本误当恢复历史数据 |

真实请求按用户选择的客户端、模型和测试预算执行；没有这些能力或授权时列为缺口，不模拟成功。
外部示例测试若依赖未提供的仓库或制品，应记录为未执行；当前示例可用于实际能力联调，但不能把被忽略的测试计为通过。
临时账号、Key 或配置只在明确的测试范围内创建，记录并清理自己创建的对象，不输出密钥、原始请求或插件 secret。

交付简述：源码与产物位置、兼容范围、所需能力与绑定、执行过的命令及结果、尚未验证的场景
只有确实生成了产物才报告“已打包”，只有目标环境业务验证成功才报告“已生效”；提交、推送与发布按用户授权另行执行。
