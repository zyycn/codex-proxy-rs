# 开发与源码联调

本文说明开发环境、服务启动与跨仓库联调，命令均从宿主仓库根目录执行

协作与检查要求见 [贡献与审查](../CONTRIBUTING.md)，部署实例的配置与数据维护见 [部署与运维](../deploy/README.md)

| 任务 | 入口 |
| --- | --- |
| 修改宿主前后端 | [环境与依赖](#环境与依赖) → [启动宿主](#启动宿主) |
| 定位模块与共享实现 | [源码入口](#源码入口) |
| 同时修改 UI 或插件 | [源码联调](#源码联调) → [启动与检查](#启动与检查) |
| 验证本地 SDK | [本地 SDK 验证](#本地-sdk-验证) |
| 提交跨仓库改动 | [编辑与提交](#编辑与提交) |

## 环境与依赖

需要 Node.js 24、pnpm、[Rust 工具链](architecture.md#验证命令)、PostgreSQL 和 Redis

各前端的 `packageManager` 固定 pnpm 版本，宿主 CI 和 Docker 从 `frontend/package.json` 读取

Rust 工具链声明位于 `backend/rust-toolchain.toml`，从仓库根目录运行 Cargo 时显式选择该工具链；
`--manifest-path` 只选择 manifest，不切换 rustup 按当前目录选定的工具链

```bash
git clone https://github.com/zyycn/codex-proxy-rs.git
cd codex-proxy-rs
pnpm --dir frontend install --frozen-lockfile
```

前端依赖、锁文件与 ESLint 配置由 `frontend/` 管理，根目录不维护 Node 包

- **版本固定**：`@codex-proxy/ui` 使用固定 GitHub 标签或提交，锁文件记录实际提交、源码归档地址及完整性摘要
- **安装与构建**：pnpm 按 UI 自身锁文件准备依赖，执行 `prepack` 生成 JS、CSS 和类型声明，普通 `dev`、`build`、发行 CI 和 Docker 使用编译产物，无需初始化子模块
- **构建许可**：`frontend/pnpm-workspace.yaml` 的 `allowBuilds` 按仓库放行自有 UI 包脚本，升级时核对源码并更新锁文件，无需逐次修改许可，不开放全部依赖脚本

## 启动宿主

使用源码仓库自带的 Compose 与配置模板，按[手动安装](../deploy/README.md#手动安装)中的配置与权限要求准备 `deploy/config.yaml` 和 `.runtime/` 目录

数据库及 Redis 可由 Compose 启动：

```bash
docker compose -f deploy/compose.yaml up -d postgres redis
cargo +1.97.0 run --manifest-path backend/Cargo.toml -p codex-proxy-rs --locked
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

后端职责和依赖方向见 [Workspace 边界](architecture.md#3-workspace-边界)，页面与共享逻辑见 [前端模块职责](architecture.md#34-前端模块职责)。
先按所属模块定位，再跟踪请求、状态与展示的调用关系

## 源码联调

宿主、组件库和官方插件保留独立仓库，通过 Git 子模块在同一目录开发：

| 目录 | 仓库与职责 |
| --- | --- |
| `frontend` / `backend` | 宿主管理端与 Rust 网关 |
| `modules/ui` | `codex-proxy-ui`，共用组件与主题 |
| `modules/plugins` | `codex-proxy-plugins`，官方插件示例 |

### 检出与安装

首次检出可同时下载子模块：

```bash
git clone --recurse-submodules https://github.com/zyycn/codex-proxy-rs.git
cd codex-proxy-rs
```

已有仓库运行 `git submodule update --init --recursive`。子模块固定到具体提交；拉取宿主更新后，同样用此命令对齐。执行前先保存子模块中的改动，避免切换正在开发的分支

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
源码联调 CI 显式检出子模块、核对 UI 提交锁定并验证这两个命令

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
    cargo +1.97.0 test --manifest-path "$cpr_sdk_check_dir/backend/Cargo.toml" \
    --config "patch.\"https://github.com/zyycn/codex-proxy-rs.git\".gateway-plugin-sdk.path=\"$PWD/backend/crates/gateway-plugin/sdk\""
)
```

## 编辑与提交

直接打开宿主目录即可开发，分别在对应仓库目录操作 Git

1. 在要修改的子模块内先创建分支，例如 `git -C modules/ui switch -c feat/select`。初次初始化的子模块通常处于 detached HEAD
2. 在子模块运行自身检查，并在宿主根目录运行所需联调检查
3. 分别提交、推送并合并组件库或插件 PR。宿主只记录子模块提交号，不能替代子仓库的提交与推送
4. UI 更新后，将管理端和插件页面的 UI 依赖同步到同一固定标签或提交，重新生成各自锁文件，再更新宿主的子模块指针；CI 会核对三处是否一致
5. 提交宿主改动及 `modules/ui`、`modules/plugins` 指针。指针必须指向远程可取得的提交，不能只存在于开发机

更新子模块指针不会自动升级消费方依赖。正式依赖不能写成开发机路径或浮动分支；SDK 仍由插件仓库固定到包含所需合同的宿主提交
