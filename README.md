<h1 align="center">Codex Proxy RS</h1>

<p align="center">
  OpenAI 兼容的 ChatGPT / Codex 多账号代理网关
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Rust-1.95-000?style=flat-square&logo=rust" />
  <img src="https://img.shields.io/badge/Axum-0.8-2f7d95?style=flat-square" />
  <img src="https://img.shields.io/badge/Vue-3.5-42b883?style=flat-square&logo=vuedotjs&logoColor=white" />
  <img src="https://img.shields.io/badge/Vite-8-646cff?style=flat-square&logo=vite&logoColor=white" />
  <img src="https://img.shields.io/badge/PostgreSQL-18-4169e1?style=flat-square&logo=postgresql&logoColor=white" />
  <img src="https://img.shields.io/badge/Redis-8-ff4438?style=flat-square&logo=redis&logoColor=white" />
  <img src="https://img.shields.io/badge/License-MIT-blue?style=flat-square" />
</p>

## 概览

Codex Proxy RS 是一个单进程网关：

- Rust/Axum 后端提供 Responses 兼容代理、管理端 API、PostgreSQL 持久化、Redis 运行态存储和静态前端托管。
- Vue 管理端负责账号导入、API Key、用量统计、请求记录、运行参数、模型映射和系统更新。
- 应用本地文件默认写入仓库 `.runtime/`；PostgreSQL 与 Redis 数据由各自服务管理。

## 环境

- Rust 1.95
- Node 24 或兼容版本，pnpm 11（只在开发前端或构建镜像时需要）
- PostgreSQL 18、Redis 8
- Docker / Docker Compose（推荐用于启动依赖、部署或验证镜像）

## 本地运行

本地直接运行后端时，配置文件由 `CPR_CONFIG_FILE` 指定；未指定时读取当前工作目录下的 `config.yaml`。

```bash
cp deploy/config.example.yaml .runtime/config.yaml
```

如果直接用本地二进制而不是 Docker，确保 `.runtime/config.yaml` 使用宿主机可访问的连接地址与日志路径：

```yaml
database:
  url: 'postgres://codex_proxy:codex_proxy@127.0.0.1:5432/codex_proxy'

redis:
  url: 'redis://127.0.0.1:6379/'

logging:
  level: info
  stdout: true
  file:
    enabled: true
    directory: .runtime/logs
    retention_days: 14
    max_file_size_mb: 20
    max_files: 20

telemetry:
  enabled: true

admin:
  default_password: '<set-a-long-random-password>'
```

启动：

```bash
mkdir -p .runtime/data .runtime/logs
CPR_CONFIG_FILE=.runtime/config.yaml cargo run --manifest-path backend/Cargo.toml
```

服务默认监听 `0.0.0.0:8080`。本机访问通常使用 `http://127.0.0.1:8080`。

## Docker 部署

Compose 同时启动 PostgreSQL、Redis 和主服务。主服务默认把宿主机 `.runtime` 映射进容器：

- `.runtime/config.yaml` -> `/app/config.yaml`
- `.runtime/data` -> `/app/data`（installation id 与在线更新状态，不含数据库）
- `.runtime/logs` -> `/app/logs`

PostgreSQL 与 Redis 分别使用 `postgres-data`、`redis-data` 命名卷。

初始化配置：

```bash
mkdir -p .runtime/data .runtime/logs
cp deploy/config.example.yaml .runtime/config.yaml
sudo chown -R "$(id -u):10001" .runtime/data .runtime/logs
chmod 0770 .runtime/data .runtime/logs
sudo chown "$(id -u):10001" .runtime/config.yaml
chmod 0640 .runtime/config.yaml
cp deploy/.env.example deploy/.env
chmod 0600 deploy/.env
# 编辑 deploy/.env，设置管理员、PostgreSQL 与 Redis 密码
```

`deploy/.env` 是 Docker 部署的密钥配置文件，已被 Git 忽略。Compose 要求其中三项密码均为非空；
应用会用管理员密码覆盖 `admin.default_password`，并安全覆盖数据库 URL 的密码部分，不需要手工编码或
同步连接串。编辑密码时保留示例中的单引号。

Docker 配置文件里路径保持容器路径：

```yaml
database:
  url: 'postgres://codex_proxy:codex_proxy@postgres:5432/codex_proxy'

redis:
  url: 'redis://:codex_proxy@redis:6379/'

logging:
  level: info
  stdout: true
  file:
    enabled: true
    directory: /app/logs
    retention_days: 14
    max_file_size_mb: 20
    max_files: 20

telemetry:
  enabled: true
```

构建并启动：

```bash
docker compose --env-file deploy/.env -f deploy/docker-compose.yml build
docker compose --env-file deploy/.env -f deploy/docker-compose.yml up -d
```

默认只绑定宿主机 `127.0.0.1:8080`。如需覆盖路径：

```bash
CPR_CONFIG_FILE=/path/to/config.yaml \
CPR_DATA_DIR=/path/to/data \
CPR_LOG_DIR=/path/to/logs \
docker compose --env-file deploy/.env -f deploy/docker-compose.yml up -d
```

## API

客户端 API Key 在管理端创建。OpenAI 兼容接口通过 `Authorization: Bearer <key>` 鉴权。

| 端点 | 说明 |
| --- | --- |
| `POST /v1/responses` | OpenAI Responses 兼容接口 |
| `POST /v1/responses/review` | Review 模型请求入口 |
| `POST /v1/responses/compact` | Compact 请求入口 |
| `GET /v1/models` | 模型列表 |
| `GET /v1/models/{id}` | 模型详情 |
| `GET /v1/models/{id}/info` | 模型运行信息 |
| `GET /v1/models/catalog` | 管理端可见模型目录 |
| `/api/admin/*` | 管理端 API |

示例：

```bash
curl http://127.0.0.1:8080/v1/responses \
  -H 'Authorization: Bearer <your-api-key>' \
  -H 'Content-Type: application/json' \
  -d '{"model":"gpt-5.5","input":"Say hello","stream":true}'
```

## 管理端

生产环境前端由后端直接托管，不需要单独 Node 服务。开发前端时：

```bash
pnpm --dir frontend install
pnpm --dir frontend dev
```

生产构建：

```bash
pnpm --dir frontend build
CPR_CONFIG_FILE=.runtime/config.yaml cargo run --manifest-path backend/Cargo.toml
```

管理端功能：

- 账号导入、OAuth 授权、连接测试、quota 刷新、token 刷新。
- 客户端 API Key 管理，完整 Key 可长期复制并导入 CCSwitch。
- Dashboard、请求记录、Token 用量、模型用量和账号额度。
- 运行参数、模型别名、账号选择策略。
- 版本检查、在线更新、更新日志、重启和回滚。

## 配置与运行数据

配置文件只承载启动必需项。账号、API Key、模型映射、账号选择策略和多数运行参数由管理端写入 PostgreSQL；管理会话、刷新租约、会话亲和与模型缓存位于 Redis。

`.runtime` 约定：

```text
.runtime/config.yaml                # 本地或 Docker 启动配置，路径按运行环境写
.runtime/data/installation_id       # 上游 installation id
.runtime/data/update-state.json     # 在线更新状态
.runtime/data/update.lock           # 在线更新锁
.runtime/logs/                      # 日志
```

## 发布与更新

当前版本记录在 `release/version.yaml`，发布平台记录在 `release/platforms.yaml`。

发布命令：

```bash
release/publish 1.0.4
```

该命令会更新版本文件、生成版本提交、创建 `v<version>` tag 并推送。GitHub Actions 随后构建 Release 归档和 GHCR 多平台镜像。

在线更新由主服务处理，管理端调用 `/api/admin/system/*`：

- 查询 GitHub Release。
- 下载当前平台对应的归档和 `checksums.txt`。
- 校验 checksum 后替换二进制和 `web/dist`。
- 重启时，Docker 模式依赖 `restart: unless-stopped` 拉起新容器进程；非 Docker 模式会先安排新进程延迟启动，再关闭当前进程。

## 项目结构

```text
backend/       Rust/Axum 后端、PostgreSQL migration、SQLite 导入工具、集成测试
frontend/      Vue 3 管理端
deploy/        Dockerfile、Compose、部署配置模板
docs/          架构和维护文档
release/       版本、平台和发布脚本
skills/        项目本地 Codex skill
```

## 开发检查

```bash
cargo fmt --manifest-path backend/Cargo.toml --check
cargo clippy --manifest-path backend/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo test --manifest-path backend/Cargo.toml --test main --locked
pnpm --dir frontend build
docker compose --env-file deploy/.env -f deploy/docker-compose.yml config
```

## License

MIT
