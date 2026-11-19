# 开发与源码联调

除克隆仓库外，命令均在宿主仓库根目录执行。协作与验收见 [贡献与审查](../CONTRIBUTING.md)，部署配置与数据维护见 [部署与运维](../deploy/README.md)

| 任务 | 入口 |
| --- | --- |
| 修改宿主前后端 | [环境与依赖](#环境与依赖) → [启动宿主](#启动宿主) |
| 定位模块与共享实现 | [源码入口](#源码入口) |
| 同时修改 UI 或插件 | [源码联调](#源码联调) → [启动与检查](#启动与检查) |
| 验证本地 SDK | [本地 SDK 验证](#本地-sdk-验证) |
| 提交跨仓库改动 | [编辑与提交](#编辑与提交) |

## 环境与依赖

使用 Node.js 26.13.1、PostgreSQL、Redis 和各前端 `packageManager` 指定的 pnpm。宿主 CI 与 Docker 读取 `frontend/package.json`。
在仓库根目录运行 Cargo 时，用 `+版本号` 选择 `backend/rust-toolchain.toml` 指定的[工具链](architecture.md#验证命令)

```bash
git clone https://github.com/zyycn/codex-proxy-rs.git
cd codex-proxy-rs
pnpm --dir frontend install --frozen-lockfile
```

宿主前端的依赖、锁文件与 ESLint 配置位于 `frontend/`

- **版本固定**：`@codex-proxy/ui` 使用固定 GitHub 标签或提交，锁文件记录实际提交、源码归档地址及完整性摘要
- **安装与构建**：pnpm 按 UI 自身锁文件准备依赖，执行 `prepack` 生成 JS、CSS 和类型声明，普通 `dev`、`build`、发行 CI 和 Docker 使用编译产物，无需初始化子模块
- **构建许可**：`frontend/pnpm-workspace.yaml` 的 `allowBuilds` 按仓库放行自有 UI 包脚本。升级时核对源码并更新锁文件

## 启动宿主

使用源码仓库自带的 Compose 与配置模板，按[手动安装](../deploy/README.md#手动安装)中的配置与权限要求准备 `deploy/config.yaml` 和 `.runtime/` 目录

数据库及 Redis 可由 Compose 启动：

```bash
docker compose -f deploy/compose.yaml up -d postgres redis
cargo +1.99.1 run --manifest-path backend/Cargo.toml -p codex-proxy-rs --locked
```

后端从当前目录向上查找 `deploy/config.yaml`，本机运行时数据库与 Redis 地址应指向可访问的本机端口

`host.runtime_data_dir` 和日志的相对路径以该配置文件所在目录解析

另开终端启动管理端：

```bash
pnpm --dir frontend dev
```

后端代理由 `frontend/vite.config.ts` 配置，验证 WebSocket 时直接连接后端

前后端检查见 [贡献与审查](../CONTRIBUTING.md#验证)，数据库集成测试使用[专用测试库](../backend/migrations/README.md#本地测试库)

## 源码入口

[后端 Workspace 边界](architecture.md#3-workspace-边界) · [前端模块职责](architecture.md#34-前端模块职责) · [共享 UI 组件架构](../modules/ui/playground/guide/architecture.md)

Messages 与 Responses 的纯协议转换位于 `backend/crates/gateway-protocol/src/anthropic/`，
HTTP 入口位于 `backend/crates/gateway-api/src/openai/messages.rs`，
账号协议分派、HTTP 执行和计数位于 `backend/crates/providers/openai/src/provider/messages/`。
模块职责见 [Provider 与协议边界](architecture.md#5-provider-与协议边界)

联调协议时分别准备使用 `responses` 和 `messages` 的 API Key 账号，并通过账号分组与 Client Key 固定上游范围，
避免混合账号池使一次请求选中其他协议。对 Responses、Chat 和 Messages 三个入口分别检查 JSON 与 SSE，
再核对工具往返、取消、错误和输入计数；请求格式及六种组合见 [模型数据面](api.md#3-模型数据面与模型目录)。
Messages 账号只配置 HTTP；本地计数估算不能替代上游计费核对

OpenAI 的 `transport::tls::` 测试在本机回环地址采集 ClientHello，覆盖默认 HTTP、自定义 CA 的 HTTP 和 WebSocket。运行方式见[后端验证命令](architecture.md#验证命令)

## 源码联调

宿主、组件库和官方插件保留独立仓库，通过 Git 子模块在同一目录开发：

| 目录 | 仓库与职责 |
| --- | --- |
| `frontend` / `backend` | 宿主管理端与 Rust 网关 |
| `modules/ui` | `codex-proxy-ui`，共用组件与主题 |
| `modules/plugins` | `codex-proxy-plugins`，官方插件示例 |

各仓库的 CI 使用锁定依赖。联调前确认所选提交的公开接口兼容

### 检出与安装

首次检出可同时下载子模块：

```bash
git clone --recurse-submodules https://github.com/zyycn/codex-proxy-rs.git
cd codex-proxy-rs
```

在已有仓库中，先保存子模块改动，再用 `git submodule update --init --recursive` 检出宿主指定的提交。拉取宿主更新后也用此命令对齐

在宿主根目录安装各项目依赖：

```bash
pnpm --dir modules/ui install --frozen-lockfile
pnpm --dir frontend install --frozen-lockfile
pnpm --dir modules/plugins/examples/workbench/frontend install --frozen-lockfile
```

### 启动与检查

在各自终端启动所需服务：

```bash
pnpm --dir frontend dev:source                            # 管理端，引用本地 UI
pnpm --dir modules/plugins/examples/workbench/frontend dev:source  # 插件页面，引用本地 UI
pnpm --dir modules/ui dev                                 # 组件库交互示例
```

管理端需要[已启动的网关](#启动宿主)。插件独立预览的宿主桥使用模拟数据；已安装插件读取包内静态资源，修改后需重新构建、打包和安装

`dev:source` 使用 Vite 的 `source` 模式，由 `@codex-proxy/ui/vite` 的 `CodexProxyUI` 适配器将公开入口解析到 `modules/ui`，并统一处理共享依赖、源码预构建排除与开发服务的文件访问范围。入口映射以 UI 包 `exports` 中的 `codex-proxy-source` 条件为准，由 Vite 热更新。插件单独检出时仍用普通 `dev`；`source` 模式需要上述子模块目录结构

```bash
pnpm --dir frontend build:source
pnpm --dir modules/plugins/examples/workbench/frontend build:source
```

`build:source` 用各项目的 `tsconfig.source.json` 检查源码类型，管理端构建到 `frontend/node_modules/.vite/source-dist`，示例构建到其前端项目的 `.vite/source-dist`，不改写正式依赖、锁文件或 `dist`。

### 本地 SDK 验证

插件正式 SDK 依赖仍由其 Cargo 锁文件固定。需要验证尚未发布的 SDK 修改时，在临时副本运行 Cargo patch，避免改写插件锁文件。以下命令从宿主根目录执行：

```bash
(
  set -e
  cpr_sdk_check_dir=$(mktemp -d)
  trap 'rm -rf "$cpr_sdk_check_dir"' EXIT
  mkdir "$cpr_sdk_check_dir/backend"
  cp modules/plugins/examples/workbench/backend/Cargo.{toml,lock} "$cpr_sdk_check_dir/backend/"
  cp -R modules/plugins/examples/workbench/backend/{src,tests} "$cpr_sdk_check_dir/backend/"
  cp modules/plugins/examples/workbench/plugin.json "$cpr_sdk_check_dir/"
  RUST_MIN_STACK=16777216 CARGO_TARGET_DIR="$PWD/backend/target/plugin-development" \
    cargo +1.99.1 test --manifest-path "$cpr_sdk_check_dir/backend/Cargo.toml" \
    --config "patch.\"https://github.com/zyycn/codex-proxy-rs.git\".gateway-plugin-sdk.path=\"$PWD/backend/crates/gateway-plugin/sdk\""
)
```

独立插件的构建、打包和安装后功能验证见 [Workbench 示例](../modules/plugins/examples/workbench/README.md)。
宿主插件集成测试的数据库与 Redis 配置见[本地测试库](../backend/migrations/README.md#本地测试库)

## 编辑与提交

1. 在要修改的子模块内先创建分支，例如 `git -C modules/ui switch -c feat/select`。初次初始化的子模块通常处于 detached HEAD
2. 在子模块运行自身检查，并在宿主根目录运行所需联调检查
3. 在对应仓库提交、推送并合并组件库或插件 PR
4. 需要升级依赖时，在管理端或插件仓库固定 UI 标签或提交并更新锁文件。插件 SDK 固定到提供所需合同的宿主提交
5. 在宿主仓库提交代码和子模块指针。指针须可从远程检出，更新指针不会升级消费方依赖
