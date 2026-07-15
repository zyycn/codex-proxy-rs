<!-- prettier-ignore -->
<div align="center">

<img src="frontend/public/favicon.svg" alt="Codex Proxy RS" width="80" height="80" />

# Codex Proxy RS

**基于 Rust 的高性能 Codex 多账号网关代理**

[![CI](https://github.com/zyycn/codex-proxy-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/zyycn/codex-proxy-rs/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/zyycn/codex-proxy-rs?display_name=tag&sort=semver&style=flat-square)](https://github.com/zyycn/codex-proxy-rs/releases)
[![GHCR](https://img.shields.io/badge/GHCR-codex--proxy--rs-2496ED?logo=docker&logoColor=white&style=flat-square)](https://github.com/zyycn/codex-proxy-rs/pkgs/container/codex-proxy-rs)
[![MIT](https://img.shields.io/badge/License-MIT-blue.svg?style=flat-square)](https://opensource.org/license/mit)

[快速开始](#快速开始) · [客户端接入](#客户端接入) · [运维](#运维) · [部署文档](deploy/README.md) · [架构](docs/architecture.md)

</div>

> [!NOTE]
> 只支持 OpenAI Responses API，不提供 `/v1/chat/completions`。

## 能力

| 领域     | 实现                                             |
| -------- | ------------------------------------------------ |
| 协议     | Responses HTTP、SSE、官方 WebSocket              |
| 传输     | 会话级 WebSocket 池、共享 HTTP/2 后备            |
| 调度     | 智能调度、额度优先、轮询、粘滞                   |
| 故障处理 | 额度、认证、封禁、模型与传输故障切换             |
| 身份     | 账号身份隔离、稳定 installation ID、会话拓扑透传 |
| 管理     | 账号、客户端 Key、模型别名、运行设置与在线更新   |
| 遥测     | Token、费用、缓存、TTFT、延迟与错误              |

## 快速开始

需要 Docker Engine 与 Docker Compose Plugin。

### 1. 准备

```bash
git clone https://github.com/zyycn/codex-proxy-rs.git
cd codex-proxy-rs

mkdir -p .runtime/data .runtime/logs
install -d -m 0750 .runtime/postgres .runtime/redis
cp deploy/config.example.yaml deploy/config.yaml
sudo chown "$(id -u):10001" deploy/config.yaml
chmod 0640 deploy/config.yaml
```

分别执行三次：

```bash
openssl rand -hex 24
```

将结果写入 `deploy/config.yaml`：

| 配置                           | 约束                     |
| ------------------------------ | ------------------------ |
| `x-cpr.database.password`      | 48 位十六进制            |
| `x-cpr.redis.password`         | 48 位十六进制            |
| `x-cpr.admin.default_password` | 至少 12 位，不能包含 `$` |

Linux 需要允许容器组写入运行目录：

```bash
sudo chown -R "$(id -u):10001" .runtime/data .runtime/logs
chmod 0770 .runtime/data .runtime/logs
```

### 2. 启动

```bash
docker compose -f deploy/compose.yaml config --quiet
docker compose -f deploy/compose.yaml pull
docker compose -f deploy/compose.yaml up -d --no-build
curl -i http://127.0.0.1:8080/healthz
```

`204 No Content` 表示应用、PostgreSQL 与 Redis 均可用。管理端地址：
`http://127.0.0.1:8080`。

### 3. 初始化

1. 使用 `admin@cpr.local` 与初始密码登录。
2. 添加 Codex 账号并执行连接测试。
3. 创建 `sk_...` 客户端 Key。
4. 复制 Codex CLI 配置，或导入 CCSwitch。

> [!NOTE]
> `admin.default_password` 只在首次创建管理员时生效。

完整部署、权限和密码轮换规则见 [deploy/README.md](deploy/README.md)。

## 客户端接入

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

<details>
<summary>API 路由</summary>

| 路由                        | 用途                |
| --------------------------- | ------------------- |
| `POST /v1/responses`        | JSON 或 SSE         |
| `GET /v1/responses`         | Responses WebSocket |
| `POST /v1/responses/review` | Review              |
| `GET /v1/models`            | 模型列表            |
| `GET /v1/models/catalog`    | 模型目录            |
| `GET /v1/models/{model_id}` | 模型详情            |

所有 `/v1/*` 路由都需要客户端 API Key。

</details>

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

> [!IMPORTANT]
> `.runtime/` 保存数据库、Redis、身份密钥和日志。删除该目录会永久清除运行状态。

Compose 默认只绑定 `127.0.0.1`。公网接入应使用 HTTPS 反向代理，转发 WebSocket
upgrade 与真实客户端 IP；不要暴露 PostgreSQL 或 Redis。

## 文档

- [部署](deploy/README.md)
- [架构](docs/architecture.md)
- [Release](https://github.com/zyycn/codex-proxy-rs/releases)
- [容器镜像](https://github.com/zyycn/codex-proxy-rs/pkgs/container/codex-proxy-rs)
