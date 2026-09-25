# 源码联调

宿主、组件库和官方插件保留独立仓库，通过 Git 子模块在同一目录开发：

| 目录 | 仓库与职责 |
| --- | --- |
| `frontend` / `backend` | 宿主管理端与 Rust 网关 |
| `modules/ui` | `codex-proxy-ui`，共用组件与主题 |
| `modules/plugins` | `codex-proxy-plugins`，官方插件示例 |

需要 Node.js 24、pnpm，以及 [宿主约定的 Rust 工具链](architecture.md#12-修改与验收)。各前端的 `packageManager` 分别固定 pnpm 版本。

## 检出与安装

首次检出可同时下载子模块：

```bash
git clone --recurse-submodules https://github.com/zyycn/codex-proxy-rs.git
cd codex-proxy-rs
```

已有仓库运行 `git submodule update --init --recursive`。子模块固定到具体提交；拉取宿主更新后，同样用此命令对齐。执行前先保存子模块中的改动，避免切换正在开发的分支。

在宿主根目录安装各项目依赖：

```bash
pnpm --dir modules/ui install --frozen-lockfile
pnpm --dir frontend install --frozen-lockfile
pnpm --dir modules/plugins/examples/workbench/frontend install --frozen-lockfile
```

## 启动与检查

在各自终端启动所需服务：

```bash
pnpm --dir frontend dev:source                            # 管理端，引用本地 UI
pnpm --dir modules/plugins/examples/workbench/frontend dev:source  # 插件页面，引用本地 UI
pnpm --dir modules/ui dev                                 # 组件库交互示例
```

管理端沿用 `frontend/vite.config.ts` 的后端代理，需按原有方式启动网关。插件独立预览的宿主桥使用模拟数据；已安装插件读取包内静态资源，修改后仍需重新构建、打包和安装。

`dev:source` 使用 Vite 的 `source` 模式，在各自配置中将 UI 的公开入口解析到 `modules/ui`，由 Vite 热更新。插件单独检出时仍用普通 `dev`；`source` 模式需要上述子模块目录结构。

```bash
pnpm --dir frontend build:source
pnpm --dir modules/plugins/examples/workbench/frontend build:source
```

`build:source` 用各项目的 `tsconfig.source.json` 检查源码类型，再构建到各自 `.vite/source-dist`，不改写正式依赖、锁文件或 `dist`。正常 `dev`、`build`、宿主发行 CI 和 Docker 仍消费锁定的 UI 包，无需初始化子模块。CI 另有源码联调检查，显式检出子模块、核对 UI 提交锁定并验证这两个命令。

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
    cargo test --manifest-path "$cpr_sdk_check_dir/backend/Cargo.toml" \
    --config "patch.\"https://github.com/zyycn/codex-proxy-rs.git\".gateway-plugin-sdk.path=\"$PWD/backend/crates/gateway-plugin/sdk\""
)
```

## 编辑与提交

直接打开宿主目录即可开发，分别在对应仓库目录操作 Git。

1. 在要修改的子模块内先创建分支，例如 `git -C modules/ui switch -c feat/select`。初次初始化的子模块通常处于 detached HEAD。
2. 在子模块运行自身检查，并在宿主根目录运行所需联调检查。
3. 分别提交、推送并合并组件库或插件 PR。宿主只记录子模块提交号，不能替代子仓库的提交与推送。
4. UI 更新后，将管理端和插件页面的 UI 依赖同步到同一固定标签或提交，重新生成各自锁文件，再更新宿主的子模块指针；CI 会核对三处是否一致。
5. 提交宿主改动及 `modules/ui`、`modules/plugins` 指针。指针必须指向远程可取得的提交，不能只存在于开发机。

更新子模块指针不会自动升级消费方依赖。正式依赖不能写成开发机路径或浮动分支；SDK 仍由插件仓库固定到包含所需合同的宿主提交。
