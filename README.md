<h1 align="center">Codex Proxy RS</h1>

<p align="center">
  OpenAI 兼容的 ChatGPT / Codex 多账号代理网关
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Rust-1.95-000?style=flat-square&logo=rust" />
  <img src="https://img.shields.io/badge/Axum-0.8-2f7d95?style=flat-square" />
  <img src="https://img.shields.io/badge/Vue-3-42b883?style=flat-square&logo=vuedotjs&logoColor=white" />
  <img src="https://img.shields.io/badge/SQLite-local-003b57?style=flat-square&logo=sqlite" />
  <img src="https://img.shields.io/badge/License-MIT-blue?style=flat-square" />
</p>

## 环境

- Rust ≥ 1.95
- Node ≥ 20，pnpm ≥ 9（仅开发管理端前端时需要）

## 快速开始

```bash
cargo run --manifest-path backend/Cargo.toml
```

启动后监听 `http://0.0.0.0:8080`，配置从运行目录下的 `config.yaml` 读取。首次启动会自动创建管理端账号（默认用户名密码见配置文件）。

Docker 部署：

```bash
cp deploy/config.example.yaml deploy/config.yaml
# 编辑 deploy/config.yaml，至少改掉 admin.default_password
docker compose -f deploy/docker-compose.yml build
docker compose -f deploy/docker-compose.yml up -d
```

挂载关系：`deploy/config.yaml` → `/app/config.yaml`，`deploy/data` → `/app/data`，`deploy/logs` → `/app/logs`。

## API

所有接口通过 Authorization Bearer header 鉴权，API Key 在管理端创建和管理。

| 端点 | 说明 |
| --- | --- |
| `POST /v1/responses` | OpenAI responses 接口 |
| `POST /v1/chat/completions` | Chat completions 接口 |
| `GET /v1/models` | 模型列表 |
| `GET /v1/models/catalog` | 模型目录 |
| `/api/admin/*` | 管理端 API（前端调用） |

```bash
curl http://127.0.0.1:8080/v1/responses \
  -H 'Authorization: Bearer <your-api-key>' \
  -H 'Content-Type: application/json' \
  -d '{"model": "gpt-5.5", "input": "Say hello", "stream": true}'
```

## 管理控制台

前端是 Vue 3 单页应用，源码在 `frontend/`。开发时：

```bash
pnpm --dir frontend dev
```

生产构建后由后端直接托管：

```bash
pnpm --dir frontend build
cargo run --manifest-path backend/Cargo.toml
```

管理端功能包括账号管理、API Key 管理、用量仪表盘、请求记录和运行时设置。账号、Key、模型路由等运行时配置在管理端操作，不写在 YAML 里。

## 配置

最小 `config.yaml`：

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

完整配置项见 `deploy/config.example.yaml`，包括 TLS、WebSocket 连接池、请求指纹、账号额度策略等。

运行数据目录：

```text
.runtime/data/   # SQLite 数据库
.runtime/logs/   # 日志文件
```

## 架构

Codex Proxy RS 是单主服务架构：Rust 后端同时承载 OpenAI 兼容代理、管理端 API、SQLite 持久化、前端静态资源托管和在线更新。详细模块边界、请求链路、发布和更新流程见 [架构说明](docs/architecture.md)。

## 项目结构

```text
backend/
  src/        Rust 源码
  tests/      集成测试
  build/      构建脚本
frontend/     Vue 3 管理控制台
deploy/       Dockerfile、Compose、示例配置
docs/         架构与维护文档
```

## 开发

```bash
cargo fmt --manifest-path backend/Cargo.toml --check
cargo clippy --manifest-path backend/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo test --manifest-path backend/Cargo.toml --test main
pnpm --dir frontend build
```

## License

MIT
