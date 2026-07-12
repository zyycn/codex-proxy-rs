<div align="center">
  <h1>Codex Proxy RS</h1>
  <p>OpenAI Responses 兼容的 Codex 多账号网关</p>
  <p>
    <a href="https://www.rust-lang.org/"><img alt="Rust 1.97" src="https://img.shields.io/badge/Rust-1.97-000?style=flat-square&amp;logo=rust" /></a>
    <a href="https://github.com/tokio-rs/axum"><img alt="Axum 0.8" src="https://img.shields.io/badge/Axum-0.8-2f7d95?style=flat-square" /></a>
    <a href="https://vuejs.org/"><img alt="Vue 3.5" src="https://img.shields.io/badge/Vue-3.5-42b883?style=flat-square&amp;logo=vuedotjs&amp;logoColor=white" /></a>
    <a href="https://vite.dev/"><img alt="Vite 8" src="https://img.shields.io/badge/Vite-8-646cff?style=flat-square&amp;logo=vite&amp;logoColor=white" /></a>
    <a href="https://www.postgresql.org/"><img alt="PostgreSQL 18" src="https://img.shields.io/badge/PostgreSQL-18-4169e1?style=flat-square&amp;logo=postgresql&amp;logoColor=white" /></a>
    <a href="https://redis.io/"><img alt="Redis 8" src="https://img.shields.io/badge/Redis-8-ff4438?style=flat-square&amp;logo=redis&amp;logoColor=white" /></a>
    <a href="https://opensource.org/license/mit"><img alt="MIT License" src="https://img.shields.io/badge/License-MIT-blue?style=flat-square" /></a>
  </p>
  <p>
    <a href="#快速部署">快速部署</a>
    ·
    <a href="#客户端接入">客户端接入</a>
    ·
    <a href="docs/architecture.md">架构说明</a>
  </p>
</div>

Codex Proxy RS 为多个 Codex 账号提供统一的 Responses API。客户端使用同一个服务地址和 API Key；账号调度、并发限制、额度检查、故障换号和会话续接都在服务端完成。

## 功能

- 兼容 `/v1/responses`、SSE、官方 Responses WebSocket 和模型查询。
- 客户端与上游均可使用 HTTP/SSE 或 WebSocket。
- 提供 `smart`、额度重置优先、轮询和粘性四种账号调度策略。
- 单个账号发生额度、认证、模型或传输故障时，继续使用账号池中的其他可用账号。
- 支持跨账号会话续接，并隔离不同账号的会话状态。
- 提供账号、API Key、模型映射、运行参数、用量、错误记录和系统更新管理界面。
- 使用 PostgreSQL 保存权威业务数据，使用 Redis 保存管理会话、租约、模型快照和响应归属。
- 内置日志轮转、遥测保留期清理和 PostgreSQL/Redis 健康检查。

除账号调度和会话恢复外，请求与响应尽量保持原有语义。

## 快速部署

需要 Docker Engine 和 Docker Compose Plugin。下面的 Compose 默认使用：

- 应用镜像：`ghcr.io/zyycn/codex-proxy-rs:latest`
- PostgreSQL 18
- Redis 8
- 管理端地址：`http://127.0.0.1:8080`

### 1. 获取项目

```bash
git clone https://github.com/zyycn/codex-proxy-rs.git
cd codex-proxy-rs
```

### 2. 创建配置

```bash
mkdir -p .runtime/data .runtime/logs
cp deploy/config.example.yaml .runtime/config.yaml
cp deploy/.env.example deploy/.env
chmod 0600 deploy/.env
```

编辑 `deploy/.env`：

```dotenv
CPR_ADMIN_DEFAULT_PASSWORD='<管理员密码，至少 12 个字符>'
CPR_POSTGRES_PASSWORD='<PostgreSQL 密码>'
CPR_REDIS_PASSWORD='<Redis 密码>'
```

`deploy/.env` 已被 Git 忽略。应用会安全地把 PostgreSQL 和 Redis 密码写入运行时连接 URL，不需要把真实密码填入 `.runtime/config.yaml`。

Linux 宿主机需要保证容器用户 `10001:10001` 可以读写数据和日志目录：

```bash
sudo chown -R "$(id -u):10001" .runtime/data .runtime/logs
chmod 0770 .runtime/data .runtime/logs
sudo chown "$(id -u):10001" .runtime/config.yaml
chmod 0640 .runtime/config.yaml
```

### 3. 启动

```bash
docker compose --env-file deploy/.env -f deploy/docker-compose.yml config
docker compose --env-file deploy/.env -f deploy/docker-compose.yml pull
docker compose --env-file deploy/.env -f deploy/docker-compose.yml up -d --no-build
```

检查服务：

```bash
docker compose --env-file deploy/.env -f deploy/docker-compose.yml ps
curl -i http://127.0.0.1:8080/healthz
```

`/healthz` 正常时返回 `204 No Content`。随后打开 <http://127.0.0.1:8080>：

- 用户名：`admin@cpr.local`
- 密码：`deploy/.env` 中的 `CPR_ADMIN_DEFAULT_PASSWORD`

首次登录后导入账号，在“API 密钥”页面创建客户端 Key。

### 本地构建镜像

需要验证当前源码时，不拉取 GHCR 镜像，直接构建：

```bash
docker compose --env-file deploy/.env -f deploy/docker-compose.yml build codex-proxy-rs
docker compose --env-file deploy/.env -f deploy/docker-compose.yml up -d
```

## 客户端接入

OpenAI 兼容客户端填写：

- Base URL：`http://127.0.0.1:8080/v1`
- API Key：管理端创建的 `sk_...` 客户端 Key

Responses 请求示例：

```bash
curl http://127.0.0.1:8080/v1/responses \
  -H 'Authorization: Bearer <client-api-key>' \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "<model-id>",
    "input": "Reply with pong.",
    "stream": false
  }'
```

可用接口：

| 路由 | 说明 |
| --- | --- |
| `POST /v1/responses` | Responses 非流式或 SSE 流式请求 |
| `GET /v1/responses` | 官方 Responses WebSocket |
| `POST /v1/responses/review` | Review 请求 |
| `GET /v1/models` | 模型列表 |
| `GET /v1/models/catalog` | 模型目录 |
| `GET /v1/models/{model_id}` | 模型详情 |
| `GET /v1/models/{model_id}/info` | 模型运行信息 |

Compose 默认只监听宿主机 `127.0.0.1`。需要从其他设备访问时，应在前面配置 HTTPS 反向代理，不要直接暴露 PostgreSQL 和 Redis 端口。

## 管理端

管理端提供以下页面：

- Dashboard：请求趋势、服务状态、账号概览和最近请求。
- 账号：导入、导出、OAuth、连接测试、quota 和 token 刷新。
- API 密钥：创建、停用、删除和客户端接入信息。
- 用量：请求记录、token、延迟、模型分布和运维错误。
- 设置：模型别名、账号调度策略、单账号并发、请求间隔和刷新参数。
- 系统：版本检查、更新、重启和回滚。

管理端登录会话保存在 Redis，并按 `admin.session_ttl_minutes` 自动过期。

## 配置

启动配置位于 `.runtime/config.yaml`，模板见 [deploy/config.example.yaml](deploy/config.example.yaml)。

| 配置段 | 用途 |
| --- | --- |
| `server` | HTTP 监听地址和端口 |
| `api` | Codex 上游地址 |
| `database`、`redis` | PostgreSQL 与 Redis 连接 |
| `quota` | quota 刷新周期和耗尽账号过滤 |
| `tls` | 上游 HTTP 协议偏好 |
| `ws_pool` | WebSocket 连接池与首事件超时 |
| `fingerprint` | Codex Desktop 请求指纹 |
| `admin` | 默认管理员和登录会话 TTL |
| `logging` | 日志级别、目录、大小和保留期 |
| `telemetry` | 请求事实记录开关 |

模型别名、调度策略、单账号并发、请求间隔和刷新参数由管理端保存到 PostgreSQL，可在运行中更新。请求事实、运维错误和聚合时间桶由后台任务按各自保留期自动清理。

## 数据与升级

| 数据 | 默认位置 |
| --- | --- |
| 账号、API Key、设置、用量和错误记录 | Compose 命名卷 `postgres-data` |
| 管理会话、租约、模型快照和响应归属 | Compose 命名卷 `redis-data` |
| 身份派生密钥和更新状态 | `.runtime/data` |
| 文件日志 | `.runtime/logs` |
| 启动配置 | `.runtime/config.yaml` |
| Docker 密钥 | `deploy/.env` |

普通升级只替换应用镜像：

```bash
docker compose --env-file deploy/.env -f deploy/docker-compose.yml pull codex-proxy-rs
docker compose --env-file deploy/.env -f deploy/docker-compose.yml up -d --no-build
```

更新镜像不会删除数据库卷，但仍应在升级前备份 PostgreSQL 和 `.runtime/data`。

不要使用 `docker compose down -v` 做普通升级，该命令会删除 PostgreSQL 与 Redis 命名卷。

`.runtime/data/identity_hmac_secret` 必须长期保留。丢失该文件会改变全部账号的 installation ID 和账号作用域身份。

## 查看状态

```bash
# 容器状态
docker compose --env-file deploy/.env -f deploy/docker-compose.yml ps

# 应用日志
docker compose --env-file deploy/.env -f deploy/docker-compose.yml logs -f codex-proxy-rs

# PostgreSQL 和 Redis 日志
docker compose --env-file deploy/.env -f deploy/docker-compose.yml logs postgres redis

# 展开并检查最终 Compose 配置
docker compose --env-file deploy/.env -f deploy/docker-compose.yml config
```

`/healthz` 返回 `503` 时，先检查 PostgreSQL 和 Redis 的健康状态。容器反复重启时，再检查 `.runtime/config.yaml`、`deploy/.env` 和 `.runtime/data`、`.runtime/logs` 的权限。

## 架构

模块边界、请求链路、存储归属和后台任务见 [架构说明](docs/architecture.md)。

## License

MIT
