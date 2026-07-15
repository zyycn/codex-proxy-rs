<div align="center">

  <img src="frontend/public/favicon.svg" alt="Codex Proxy RS" width="80" height="80" />

# Codex Proxy RS

OpenAI Responses 多账号网关。

[![CI](https://github.com/zyycn/codex-proxy-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/zyycn/codex-proxy-rs/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/zyycn/codex-proxy-rs?display_name=tag&sort=semver)](https://github.com/zyycn/codex-proxy-rs/releases)
[![GHCR](https://img.shields.io/badge/GHCR-codex--proxy--rs-2496ED?logo=docker&logoColor=white)](https://github.com/zyycn/codex-proxy-rs/pkgs/container/codex-proxy-rs)
[![MIT](https://img.shields.io/badge/License-MIT-blue.svg)](#许可)

[部署](deploy/README.md) · [架构](docs/architecture.md) · [发布](https://github.com/zyycn/codex-proxy-rs/releases)

</div>

## 边界

- OpenAI Responses：HTTP、SSE、官方 WebSocket。
- 多账号调度、故障切换、额度与身份隔离。
- 会话级 WebSocket 池与共享 HTTP/2 后备。
- PostgreSQL 持久化，Redis 运行态协调。
- Vue 管理端：账号、客户端 Key、用量、费用、延迟与错误。

不提供 `/v1/chat/completions`。

## 部署

要求 Docker Engine 与 Docker Compose Plugin。

```bash
git clone https://github.com/zyycn/codex-proxy-rs.git
cd codex-proxy-rs

mkdir -p .runtime/data .runtime/logs
install -d -m 0750 .runtime/postgres .runtime/redis
cp deploy/config.example.yaml deploy/config.yaml
sudo chown "$(id -u):10001" deploy/config.yaml
chmod 0640 deploy/config.yaml
```

分别生成 PostgreSQL、Redis 和管理员密码：

```bash
openssl rand -hex 24
```

写入 `deploy/config.yaml`：

- `x-cpr.database.password`
- `x-cpr.redis.password`
- `x-cpr.admin.default_password`

前两个值必须是 48 位十六进制字符。管理员密码至少 12 位，且不能包含 `$`。

Linux 需要开放容器组写权限：

```bash
sudo chown -R "$(id -u):10001" .runtime/data .runtime/logs
chmod 0770 .runtime/data .runtime/logs
```

启动：

```bash
docker compose -f deploy/compose.yaml config --quiet
docker compose -f deploy/compose.yaml pull
docker compose -f deploy/compose.yaml up -d --no-build
curl -i http://127.0.0.1:8080/healthz
```

`204 No Content` 表示应用、PostgreSQL 与 Redis 均可用。管理端位于
`http://127.0.0.1:8080`。

完整部署与密码轮换规则见 [deploy/README.md](deploy/README.md)。

## 初始化

1. 使用 `admin@cpr.local` 与配置中的初始密码登录。
2. 添加 Codex 账号并执行连接测试。
3. 创建 `sk_...` 客户端 Key。
4. 复制 Codex CLI 配置，或导入 CCSwitch。

`admin.default_password` 只在首次创建管理员时生效。

## 客户端

| 配置     | 值                                       |
| -------- | ---------------------------------------- |
| Base URL | `http://127.0.0.1:8080/v1`               |
| API Key  | 管理端创建的 `sk_...`                    |
| 鉴权     | `Authorization: Bearer <client-api-key>` |

```bash
curl http://127.0.0.1:8080/v1/models \
  -H 'Authorization: Bearer <client-api-key>'

curl http://127.0.0.1:8080/v1/responses \
  -H 'Authorization: Bearer <client-api-key>' \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "<model-id>",
    "input": "Reply with pong.",
    "stream": false
  }'
```

| 路由                        | 用途                |
| --------------------------- | ------------------- |
| `POST /v1/responses`        | JSON 或 SSE         |
| `GET /v1/responses`         | Responses WebSocket |
| `POST /v1/responses/review` | Review              |
| `GET /v1/models`            | 模型列表            |
| `GET /v1/models/catalog`    | 模型目录            |

所有 `/v1/*` 路由都需要客户端 API Key。

## 运维

```bash
# 升级
docker compose -f deploy/compose.yaml pull codex-proxy-rs
docker compose -f deploy/compose.yaml up -d --no-build

# 日志
docker compose -f deploy/compose.yaml logs -f codex-proxy-rs

# 从源码构建
docker compose -f deploy/compose.yaml build codex-proxy-rs
docker compose -f deploy/compose.yaml up -d
```

运行状态保存在 `.runtime/`。删除该目录会清除数据库、Redis、身份密钥和日志。

Compose 默认只绑定 `127.0.0.1`。公网接入应使用 HTTPS 反向代理，并转发 WebSocket
upgrade 与真实客户端 IP；不要暴露 PostgreSQL 或 Redis。

## 许可

[MIT](LICENSE)
