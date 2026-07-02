# Codex Proxy RS 项目指南

## 目录

- [仓库结构](#仓库结构)
- [后端规范](#后端规范)
- [前端规范](#前端规范)
- [Docker 和运行时](#docker-和运行时)
- [发布和版本契约](#发布和版本契约)
- [在线更新链路](#在线更新链路)
- [验证矩阵](#验证矩阵)
- [已知偏好和约束](#已知偏好和约束)

## 仓库结构

- `backend/`：Rust 1.95 Axum 后端，包含 SQLite 存储、代理调度、管理端 API、web 静态资源托管、在线更新逻辑。
- `backend/build/`：构建脚本和 `VERSION`；release workflow 会写入 `backend/build/VERSION`。
- `backend/tests/`：集成测试和 fixtures。测试代码只放这里，禁止放进 `backend/src`。
- `frontend/`：Vue 3 管理端 SPA，使用 Vite 8、Tailwind v4、Pinia、Vue Router、Axios、lucide icons、ECharts。
- `deploy/`：Dockerfile、Dockerfile 专用 ignore 文件、Compose 文件、`config.example.yaml`。
- `docs/`：只在用户要求时放设计说明或迁移文档。
- `skills/`：项目本地 Codex skills。

## 后端规范

- 使用 `cargo --manifest-path backend/Cargo.toml`，避免在仓库根目录生成意外构建产物。
- 开发启动主服务使用：

```bash
cargo run --manifest-path backend/Cargo.toml --bin codex-proxy-rs
```

- 不要建议在仓库根目录直接 `cargo run`；多 binary 或布局变化时容易失败或生成根目录 `target/`。
- Rust 代码保持 idiomatic 和显式；现有 lint 禁止 unsafe，并 deny 常见 clippy 问题。
- 路由组合位置：
  - `backend/src/admin/router.rs`
  - `backend/src/proxy/router.rs`
  - `backend/src/http/router.rs`
- 运行时服务在 `backend/src/runtime/services.rs` 构造，并通过 `AppState` 传递。
- 管理端接口返回 `AdminEnvelope` / `AdminResponse`，错误使用已有 `AdminError` 构造函数。
- 测试放到受影响领域附近：
  - 系统更新：`backend/tests/admin/system/mod.rs`
  - 代理调度：`backend/tests/proxy/dispatch/...`
  - web 静态资源：`backend/tests/http/web_assets/mod.rs`
- 优先写聚焦测试；只有跨模块契约或用户流程变化时才扩大覆盖面。

## 前端规范

- 使用 Vue 3 Composition API 和 `<script setup lang="ts">`。
- 遵循现有组件边界：
  - API 客户端：`frontend/src/api/modules/*.ts`
  - 基础 UI：`frontend/src/components/base`
  - 布局和系统更新弹窗：`frontend/src/layout/components`
  - 页面私有组件和 composables：各自 `views/*` 目录下。
- 优先使用 Tailwind v4 utilities；遇到无法使用或代价很大的场景，再写 scoped CSS 或复用现有全局 CSS。
- 颜色接入 `frontend/src/styles/tokens.css` 的主题 token，例如：
  - `--cp-bg-*`
  - `--cp-text-*`
  - `--cp-border-*`
  - `--cp-info`
  - `--cp-success`
  - `--cp-warning`
  - `--cp-danger`
- 保持亮色/暗色主题一致；有 token 时不要硬编码一次性颜色。
- 按钮和紧凑操作优先使用 lucide 图标。
- 弹窗级工作流不要新增路由；用户要求“弹窗、不单开路由”时沿用现有 modal/component 模式。
- Vite 构建中刻意设置了 `rolldownOptions.treeshake.annotations = false`，用于压掉 Rolldown 对部分依赖 pure annotation 的警告。

## Docker 和运行时

- runtime 镜像保持最小：`debian:bookworm-slim` + `ca-certificates` + 单个后端二进制 + `web/dist`。
- builder 阶段可以使用 `node:24-bookworm-slim` 和 `rust:1.95-bookworm`；不要把构建工具带进 runtime 阶段。
- Compose 文件是 `deploy/docker-compose.yml`，默认镜像是 `ghcr.io/zyycn/codex-proxy-rs:latest`。
- `deploy/config.example.yaml` 是唯一示例配置；除非用户明确要求，不要重新引入 `.env.example`。
- 本地 Docker 首次部署命令：

```bash
cp deploy/config.example.yaml deploy/config.yaml
docker compose -f deploy/docker-compose.yml build
docker compose -f deploy/docker-compose.yml up -d
```

- 运行时挂载关系：
  - `deploy/config.yaml` -> `/app/config.yaml`
  - `deploy/data` -> `/app/data`
  - `deploy/logs` -> `/app/logs`

## 发布和版本契约

- Release workflow：`.github/workflows/release.yml`。
- `v*` tag 触发发布。
- release 时版本来源是去掉 `v` 前缀的 Git tag；workflow 会写入 `backend/build/VERSION`。
- 运行时版本接口展示编译期元数据：`CPR_VERSION`、`CPR_GIT_SHA`、`CPR_BUILD_TIME`。
- 当前发布的 Docker 平台是 `linux/amd64`，因为 release asset 目前只有 `linux_x86_64`。
- Release asset 命名必须匹配在线更新选择器：

```text
codex-proxy-rs_<version>_linux_x86_64.tar.gz
checksums.txt
```

- release 会推送这些 GHCR tag：
  - `ghcr.io/zyycn/codex-proxy-rs:<version>`
  - `ghcr.io/zyycn/codex-proxy-rs:latest`
  - `ghcr.io/zyycn/codex-proxy-rs:sha-<git-sha>`

## 在线更新链路

- 后端接口：
  - `GET /api/admin/system/version`
  - `GET /api/admin/system/check-updates`
  - `GET /api/admin/system/update-events`
  - `GET /api/admin/system/update-status`
  - `POST /api/admin/system/update`
  - `POST /api/admin/system/restart`
- 前端更新 UI：`frontend/src/layout/components/SystemUpdateModal.vue`。
- SSE 日志由后端生成中文消息；前端只负责展示，不做状态文案映射。
- 前端不要做伪交互：不存在后端真实能力前，不展示“回滚”“备份确认”等按钮或说明。
- 更新弹窗不单开路由；入口放在现有布局/版本区域，进度通过日志卡片实时展示。
- Docker 模式使用这些环境变量：
  - `CPR_DEPLOYMENT_MODE=docker`
  - `CPR_UPDATE_REPOSITORY=zyycn/codex-proxy-rs`
  - `CPR_UPDATE_STATE_FILE=/app/data/update-state.json`
  - `CPR_UPDATE_LOCK_FILE=/app/data/update.lock`
  - `CPR_WEB_DIST_DIR=/app/web/dist`
  - `CPR_ENABLE_SELF_RESTART=true`
- 更新会替换本地二进制和 `web/dist`；随后调用 restart，依靠 Docker `restart: unless-stopped` 拉起新进程。
- 必须保留跨设备安全替换逻辑。Docker/容器文件系统里 `rename` 可能报 `Invalid cross-device link`，更新代码需要支持 copy-and-remove fallback。

## 验证矩阵

- Rust 格式：

```bash
cargo fmt --manifest-path backend/Cargo.toml --check
```

- Rust lint：

```bash
cargo clippy --manifest-path backend/Cargo.toml --all-targets --all-features --locked -- -D warnings
```

- 聚焦后端测试：

```bash
cargo test --manifest-path backend/Cargo.toml --test main admin::system --locked
```

- 全量后端集成测试：

```bash
cargo test --manifest-path backend/Cargo.toml --test main --locked
```

- 前端构建：

```bash
pnpm --dir frontend build
```

- Compose 配置校验：

```bash
docker compose -f deploy/docker-compose.yml config
```

## 已知偏好和约束

- 保持项目布局稳定：`backend`、`frontend`、`deploy`、`docs`、`skills`。
- 不创建冗余本地 Docker 开发配置。
- 不把测试代码放进 `src`。
- README、docs、发布和更新说明要和真实命令、真实行为一致。
- UI 保持当前安静、偏运维控制台的风格；避免营销式布局、卡片套卡片、图标过大、信息重复。
- 版本统一从发布 tag 到编译期元数据再到前端显示；不要引入第二个运行时版本源。
- 历史迁移文档可能包含阶段性方案；实现和 README、workflow、Dockerfile 不一致时，以当前代码和真实命令为准。
