# Codex Proxy RS 项目指南

## 目录

- [Codex Proxy RS 项目指南](#codex-proxy-rs-项目指南)
  - [目录](#目录)
  - [事实来源](#事实来源)
  - [仓库结构](#仓库结构)
  - [后端边界](#后端边界)
  - [存储边界](#存储边界)
  - [前端规范](#前端规范)
  - [Docker 和运行时](#docker-和运行时)
  - [发布和在线更新](#发布和在线更新)
  - [文档边界](#文档边界)
  - [验证矩阵](#验证矩阵)
  - [项目约束](#项目约束)

## 事实来源

1. 当前源码、配置和测试是行为事实。
2. `docs/architecture.md` 是目录、依赖方向、请求链路和存储归属的权威说明。
3. 本文件记录开发流程和高频约束，不复制完整架构。
4. 文档与代码冲突时先核对实现，再在同一提交中修正文档。

## 仓库结构

- `backend/src/`：Rust 1.97 / Axum 后端生产代码。
- `backend/tests/`：后端集成测试和 fixtures。测试代码禁止放进 `backend/src/`。
- `backend/build.rs`：把版本、Git SHA、构建时间和构建类型写入编译期环境变量。
- `frontend/`：Vue 3、Vite 8、Tailwind v4、Pinia、Vue Router、Axios、Lucide 和 ECharts 管理端。
- `deploy/`：Dockerfile、Compose、`config.example.yaml` 和 `.env.example`。
- `docs/architecture.md`：唯一长期架构文档。
- `release/`：版本、目标平台和发布脚本。
- `skills/`：项目本地 Codex skills。

仓库根目录没有 Cargo workspace manifest。Rust 命令必须显式使用 `backend/Cargo.toml`。

## 后端边界

项目没有 `application` 层。`bootstrap` 是装配根，业务规则留在各领域模块。

| 目录 | 职责 |
| --- | --- |
| `backend/src/api` | 入站 HTTP、鉴权提取、响应映射和静态资源 |
| `backend/src/bootstrap` | 配置加载、服务装配、启动关闭和后台任务 |
| `backend/src/dispatch` | Responses 编排、账号失败处理、流生命周期和会话恢复 |
| `backend/src/fleet` | 账号、账号池、调度、quota、刷新、Cookie 和管理操作 |
| `backend/src/upstream/openai` | OpenAI/Codex 协议、HTTP/WebSocket 传输、token 和指纹 |
| `backend/src/telemetry` | 成功/失败事实、聚合桶、账号用量和 Dashboard 查询 |
| `backend/src/keys` | 客户端 API Key |
| `backend/src/auth` | 管理员用户和登录会话 |
| `backend/src/settings` | PostgreSQL 运行时设置和 watch 广播 |
| `backend/src/models` | 模型目录和 Redis 快照 |
| `backend/src/update` | Release 查询、下载、替换、回滚和更新状态 |
| `backend/src/infra` | PostgreSQL、Redis、日志、身份和通用工具 |

关键入口：

- 总路由：`backend/src/api/router.rs`
- 客户端路由：`backend/src/api/client/router.rs`
- 管理端路由：`backend/src/api/admin/router.rs`
- 服务装配：`backend/src/bootstrap/services.rs`
- 非流式调度：`backend/src/dispatch/service.rs`
- 流式调度：`backend/src/dispatch/stream/lifecycle.rs`
- 账号调度：`backend/src/fleet/scheduler/mod.rs`

对应测试：

- 系统更新：`backend/tests/api/admin/system_routes/mod.rs`
- 代理调度：`backend/tests/dispatch/service.rs` 及其子目录
- 静态资源和健康检查：`backend/tests/api/assets.rs`

启动服务：

```bash
cd backend
cargo run -- serve
```

`main.rs` 还提供内部维护命令 `rebuild-buckets`，用于从保留期内的请求事实重建聚合桶。

## 存储边界

| 存储 | 数据 |
| --- | --- |
| PostgreSQL | 管理员、客户端 Key、运行时设置、账号、quota、累计用量、请求事实、错误事实、聚合桶、Cookie 和指纹 |
| Redis | 管理会话、token 刷新租约、模型计划快照、会话亲和与 replay |
| 本地数据目录 | `identity_hmac_secret`、更新状态、锁和临时文件 |
| 文件日志目录 | 轮转后的结构化日志 |

约束：

- PostgreSQL migration 必须新增版本，禁止修改已应用 SQL；启动会校验版本顺序、名称和 checksum。
- Redis 业务键统一使用 `cpr:` 前缀；会话、租约和 affinity 依赖 TTL。
- `identity_hmac_secret` 决定账号作用域身份和 installation ID，换镜像时必须保留。
- 模型别名、调度策略、并发、请求间隔和刷新参数保存到 PostgreSQL，不放回 YAML。
- 遥测和 affinity 写入失败不能改变已经取得的代理响应语义。

## 前端规范

- 使用 Vue 3 Composition API 和 `<script setup lang="ts">`。
- API 客户端位于 `frontend/src/api/modules`。
- 基础 UI 位于 `frontend/src/components/base`。
- 页面私有组件和 composables 放在对应 `frontend/src/views/*` 下。
- 优先使用 Tailwind v4 utilities；需要 CSS 时复用 `frontend/src/styles/tokens.css`。
- 保持亮色和暗色主题一致，有 token 时不硬编码一次性颜色。
- 按钮和紧凑操作优先使用 Lucide 图标。
- 弹窗工作流沿用现有 modal/component 模式，不为一次性操作增加路由。

## Docker 和运行时

- Runtime 基于 `debian:bookworm-slim`，只安装 `ca-certificates` 和健康检查所需的 `curl`。
- 镜像包含单个后端二进制和 `web/dist`，以非 root 用户 `10001:10001` 运行。
- Builder 使用 Node 24 和 Rust 1.97，构建工具不得进入 runtime。
- 默认应用镜像是 `ghcr.io/zyycn/codex-proxy-rs:latest`。

`deploy/.env.example` 是 Docker 密钥模板，包含：

- `CPR_ADMIN_DEFAULT_PASSWORD`
- `CPR_POSTGRES_PASSWORD`
- `CPR_REDIS_PASSWORD`

`deploy/config.example.yaml` 是启动配置模板。真实密码只放在 `deploy/.env`，应用通过环境变量覆盖连接 URL 的密码。

首次部署：

```bash
mkdir -p .runtime/data .runtime/logs
cp deploy/config.example.yaml .runtime/config.yaml
cp deploy/.env.example deploy/.env
# 设置 deploy/.env 后执行
docker compose --env-file deploy/.env -f deploy/docker-compose.yml config
docker compose --env-file deploy/.env -f deploy/docker-compose.yml up -d
```

挂载和卷：

- `.runtime/config.yaml` -> `/app/config.yaml`，只读
- `.runtime/data` -> `/app/data`
- `.runtime/logs` -> `/app/logs`
- `postgres-data` -> PostgreSQL 权威数据
- `redis-data` -> Redis AOF

普通升级不得执行 `docker compose down -v`。该命令会删除 PostgreSQL 和 Redis 命名卷。

## 发布和在线更新

发布契约：

- 发布入口：`release/publish <version>`
- 产品版本：`release/version.yaml`
- 目标平台：`release/platforms.yaml`
- Release workflow：`.github/workflows/release.yml`
- `v*` tag 触发 Release
- Docker 平台：`linux/amd64`、`linux/arm64`

Release asset：

```text
codex-proxy-rs_<version>_linux_amd64.tar.gz
codex-proxy-rs_<version>_linux_arm64.tar.gz
codex-proxy-rs_<version>_darwin_arm64.tar.gz
codex-proxy-rs_<version>_windows_amd64.zip
checksums.txt
```

GHCR 发布 `<version>` 和 `sha-<git-sha>` tag；只有稳定版本更新 `latest`，预发布版本不得覆盖 `latest`。

在线更新接口位于 `/api/admin/system/*`，前端入口是
`frontend/src/layout/components/SystemUpdateModal.vue`。Docker 环境使用：

- `CPR_DEPLOYMENT_MODE=docker`
- `CPR_UPDATE_REPOSITORY=zyycn/codex-proxy-rs`
- `CPR_UPDATE_CHANNEL=stable`
- `CPR_UPDATE_TEMP_DIR=/app/data/update-tmp`
- `CPR_UPDATE_STATE_FILE=/app/data/update-state.json`
- `CPR_UPDATE_LOCK_FILE=/app/data/update.lock`
- `CPR_WEB_DIST_DIR=/app/web/dist`
- `CPR_ENABLE_SELF_RESTART=true`

更新会校验 checksum，替换二进制和 `web/dist`，再触发进程重启。文件替换必须保留跨文件系统的 copy-and-remove fallback。

## 文档边界

- 根 `README.md` 面向部署者和 API 使用者，只写当前产品能力、部署、配置、持久化、升级和排障。
- README 不写内部发布脚本、CI 门禁、审计过程或内测迁移流程。
- `docs/` 只保留 `architecture.md`，用于说明当前目录、依赖、请求链路、调度恢复、存储和后台任务。
- 维护者专用流程写入本 skill，不新增过程型 Markdown 文档。

## 验证矩阵

Rust：

```bash
cd backend
cargo fmt --check
cargo clippy --all-targets --all-features --locked
cargo test --test main --locked
```

完整测试需要 PostgreSQL 和 Redis。通过以下变量提供连接：

```text
CPR_TEST_DATABASE_URL
CPR_TEST_REDIS_URL
```

使用部署 Compose 时，测试 URL 的密码必须与 `deploy/.env` 一致并正确进行 URL 编码。不要在日志或提交中输出真实密码。

前端：

```bash
pnpm --dir frontend format:check
pnpm --dir frontend build
```

Compose：

```bash
docker compose --env-file deploy/.env -f deploy/docker-compose.yml config
```

聚焦测试应使用现有集成测试模块过滤；只有跨模块契约、存储或用户流程变化时才扩大到完整门禁。发布与镜像改动还需核对 `.github/workflows/_quality.yml`、`.github/workflows/_container.yml` 和 `.github/workflows/release.yml`。

## 项目约束

- 新代码和重构不保留兼容别名、临时 shim、重复旧 API 或补丁式旁路。
- 不把测试写入 `backend/src`。
- 不为 Cargo `target` 添加仓库级重定向；运行时数据和日志放在 `.runtime`。
- 已确认且属于当前范围的问题应先修复，再继续后续测试。
- README、架构、skill、workflow、Dockerfile 和真实命令必须一致。
- UI 保持安静、紧凑的运维控制台风格，避免营销式布局、卡片套卡片和重复说明。
- 不创建冗余 Docker 配置、第二套版本源或重复存储路径。
- CodeGraph 用于定位；只有后续判断依赖已修改的索引且工具需要手动刷新时才同步索引。
