<p align="center">
  <img src="frontend/public/favicon.svg" width="88" height="88" alt="Codex Proxy RS" />
</p>

<h1 align="center">Codex Proxy RS</h1>

<p align="center">
  ChatGPT / Codex 账号池网关，提供 OpenAI 兼容接口和本地管理控制台。
</p>

<p align="center">
  <img alt="Rust" src="https://img.shields.io/badge/Rust-1.95-000?style=flat-square&logo=rust" />
  <img alt="Axum" src="https://img.shields.io/badge/Axum-0.8-2f7d95?style=flat-square" />
  <img alt="Vue" src="https://img.shields.io/badge/Vue-3-42b883?style=flat-square&logo=vuedotjs&logoColor=white" />
  <img alt="SQLite" src="https://img.shields.io/badge/SQLite-local-003b57?style=flat-square&logo=sqlite" />
  <img alt="License" src="https://img.shields.io/badge/License-MIT-blue?style=flat-square" />
</p>

<p align="center">
  <code>/v1/responses</code>
  ·
  <code>/v1/chat/completions</code>
  ·
  <code>/v1/models</code>
  ·
  <code>/api/admin</code>
</p>

## ✦ 功能清单

| 部分 | 内容 |
| --- | --- |
| ⚡ OpenAI 兼容接口 | `responses`、`chat completions`、模型列表、流式输出 |
| 🔐 账号池 | OAuth、refresh token、CPR / Sub2API 导入、状态管理 |
| 🧭 调度 | 账号轮转、并发限制、请求间隔、会话亲和性 |
| 📡 上游传输 | Codex Desktop 风格 headers、HTTP SSE、WebSocket、连接池 |
| 📊 记录 | 使用记录、Token、成本、延迟、失败原因、请求轨迹 |
| 🛠 管理端 | 账号、API Key、仪表盘、使用记录、运行时设置 |
| 💾 本地持久化 | SQLite 数据库，运行数据默认写入 `.runtime` |

## ✦ 快速启动

本地源码启动：

```bash
cargo run --manifest-path backend/Cargo.toml
```

默认监听：

```text
http://0.0.0.0:8080
```

默认配置文件：

```text
config.yaml
```

首次启动会初始化管理端账号。默认账号来自运行目录下的 `config.yaml`。

Docker 首次本地构建和启动：

```bash
cp deploy/config.example.yaml deploy/config.yaml
docker compose -f deploy/docker-compose.yml build
docker compose -f deploy/docker-compose.yml up -d
```

`deploy/config.example.yaml` 是 Docker 部署样例配置，复制为 `deploy/config.yaml` 后再修改。默认挂载关系：

```text
deploy/config.yaml -> /app/config.yaml
deploy/data        -> /app/data
deploy/logs        -> /app/logs
```

样例配置中的默认管理端账号：

```yaml
admin:
  default_username: admin
  default_password: admin
```

正式使用前请改掉默认密码。

## ✦ 管理控制台

前端代码在 `frontend/`。

开发模式：

```bash
pnpm --dir frontend dev
```

生产构建：

```bash
pnpm --dir frontend build
cargo run --manifest-path backend/Cargo.toml
```

构建后的控制台由后端直接提供，API 路由优先于静态资源。

## ✦ OpenAI 兼容入口

```http
POST /v1/responses
POST /v1/chat/completions
GET  /v1/models
GET  /v1/models/catalog
```

示例：

```bash
curl http://127.0.0.1:8080/v1/responses \
  -H 'Authorization: Bearer <client-api-key>' \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "gpt-5.5",
    "input": "Say hello",
    "stream": true
  }'
```

## ✦ config.yaml

示例：

```yaml
server:
  host: '0.0.0.0'
  port: 8080

api:
  base_url: 'https://chatgpt.com/backend-api'

database:
  url: 'sqlite://.runtime/data/codex-proxy-rs.sqlite'

logging:
  directory: .runtime/logs
  retention_days: 14
```

运行数据：

```text
.runtime/data/
.runtime/logs/
```

账号、API Key、模型路由和运行时设置在管理端修改，不写进 YAML。

## ✦ 项目结构

```text
backend/
  build/      构建脚本与版本元信息
  src/        Rust 后端源码
  tests/      集成测试与 fixture

frontend/     Vue 管理控制台
deploy/       Dockerfile、Compose、部署样例配置
docs/         设计与迁移文档
```

## ✦ 开发命令

```bash
cargo fmt --manifest-path backend/Cargo.toml --check
cargo clippy --manifest-path backend/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo test --manifest-path backend/Cargo.toml --test main
pnpm --dir frontend build
```

## ✦ License

MIT
