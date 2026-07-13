<div align="center">

  <img src="frontend/public/favicon.svg" alt="Codex Proxy RS" width="88" height="88" />

# Codex Proxy RS

**基于 Rust 的高性能 Codex 多账号网关代理**

[![CI](https://github.com/zyycn/codex-proxy-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/zyycn/codex-proxy-rs/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/zyycn/codex-proxy-rs?display_name=tag&sort=semver)](https://github.com/zyycn/codex-proxy-rs/releases)
[![Rust 1.97](https://img.shields.io/badge/Rust-1.97-000?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Vue 3.5](https://img.shields.io/badge/Vue-3.5-42b883?logo=vuedotjs&logoColor=white)](https://vuejs.org/)
[![Vite 8](https://img.shields.io/badge/Vite-8-646cff?logo=vite&logoColor=white)](https://vite.dev/)
[![GHCR](https://img.shields.io/badge/GHCR-codex--proxy--rs-2496ED?logo=docker&logoColor=white)](https://github.com/zyycn/codex-proxy-rs/pkgs/container/codex-proxy-rs)
[![MIT](https://img.shields.io/badge/License-MIT-blue.svg)](#开源许可)

[快速开始](#快速开始) · [首次使用](#首次使用) · [客户端接入](#客户端接入) · [常见问题](#常见问题) · [部署文档](deploy/README.md)

</div>

> [!NOTE]
> 当前兼容 OpenAI Responses API，不提供 `/v1/chat/completions`。

## 功能

- 支持 Responses HTTP、SSE 和官方 WebSocket 请求。
- 提供智能调度、额度重置优先、轮询和粘滞四种账号策略。
- 某个账号遇到额度、认证、模型或传输错误时，继续尝试其他账号。
- 在网页中管理账号、客户端 Key、模型别名和运行参数。
- 按请求、模型、账号或客户端 Key 查看 Token、费用、缓存、TTFT、延迟和错误。
- 支持账号 OAuth、Token 导入以及 CPR、Sub2API、CLIProxyAPI JSON 导入。
- 可直接生成 Codex CLI 配置，或通过 deeplink 导入 CCSwitch。

## 快速开始

Docker Compose 是最简单的启动方式。开始前请安装 Docker Engine 和 Docker Compose Plugin。

### 1. 准备配置

```bash
git clone https://github.com/zyycn/codex-proxy-rs.git
cd codex-proxy-rs

mkdir -p .runtime/data .runtime/logs
install -d -m 0750 .runtime/postgres .runtime/redis
cp deploy/config.example.yaml deploy/config.yaml
sudo chown "$(id -u):10001" deploy/config.yaml
chmod 0640 deploy/config.yaml
```

分别执行三次以下命令生成密码：

```bash
openssl rand -hex 24
```

把结果填入 `deploy/config.yaml` 的 `database.password`、`redis.password` 和
`admin.default_password`。PostgreSQL 与 Redis 密码必须是 48 位十六进制字符；管理员密码
至少 12 位且不能包含 `$`。

> [!IMPORTANT]
> 应用容器以非 root 用户 `10001:10001` 运行。Linux Bind Mount 会保留宿主机权限，因此需要允许容器用户组读取配置并写入数据和日志目录：
>
> ```bash
> sudo chown -R "$(id -u):10001" .runtime/data .runtime/logs
> chmod 0770 .runtime/data .runtime/logs
> ```
>
> 配置文件通过 Compose `configs` 只读挂载；普通 Compose 会保留宿主机权限，因此由
> 当前用户持有、仅向容器组开放读取权限。
> macOS 和 Windows 的 Docker Desktop 通常也不需要调整数据目录权限。

### 2. 启动服务

```bash
docker compose -f deploy/compose.yaml config --quiet
docker compose -f deploy/compose.yaml pull
docker compose -f deploy/compose.yaml up -d --no-build
```

启动后检查：

```bash
docker compose -f deploy/compose.yaml ps
curl -i http://127.0.0.1:8080/healthz
```

`/healthz` 返回 `204 No Content` 即表示服务正常。管理端地址为 <http://127.0.0.1:8080>。

## 首次使用

1. 使用 `admin@cpr.local` 和 `x-cpr.admin.default_password` 中设置的密码登录。
2. 打开“账号”，用 OAuth、Access/Refresh Token 或 JSON 文件添加账号。
3. 点一次连接测试，能看到模型和额度即可。
4. 打开“API 密钥”，创建一个 `sk_...` 客户端 Key。
5. 点击“使用密钥”复制 Codex CLI 配置，或一键导入 CCSwitch。

> [!TIP]
> `admin.default_password` 只用于首次创建管理员。已有管理员后修改该字段不会重置登录密码。

## 客户端接入

| 配置     | 值                                       |
| -------- | ---------------------------------------- |
| Base URL | `http://127.0.0.1:8080/v1`               |
| API Key  | 管理端创建的 `sk_...` 客户端 Key         |
| 鉴权     | `Authorization: Bearer <client-api-key>` |

先查询当前可用模型：

```bash
curl http://127.0.0.1:8080/v1/models \
  -H 'Authorization: Bearer <client-api-key>'
```

发送一个非流式 Responses 请求：

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

<details>
<summary>查看全部客户端 API 路由</summary>

| 路由                             | 说明                            |
| -------------------------------- | ------------------------------- |
| `POST /v1/responses`             | Responses 非流式或 SSE 流式请求 |
| `GET /v1/responses`              | 官方 Responses WebSocket        |
| `POST /v1/responses/review`      | Review 请求                     |
| `GET /v1/models`                 | OpenAI 格式的模型列表           |
| `GET /v1/models/catalog`         | 完整模型目录                    |
| `GET /v1/models/{model_id}`      | 模型详情                        |
| `GET /v1/models/{model_id}/info` | 模型运行信息                    |

所有 `/v1/*` 路由都需要客户端 API Key。

</details>

## 管理端

| 页面     | 内容                                                                   |
| -------- | ---------------------------------------------------------------------- |
| 概览     | 查看服务状态、请求趋势、Token、账号池容量、请求健康和最近调用          |
| 账号     | 添加、导入、导出、测试、启停账号，以及刷新 Token 和额度                |
| API 密钥 | 创建、复制、启停或删除 Key，生成 Codex CLI 配置并导入 CCSwitch         |
| 使用统计 | 按时间、模型、账号和客户端 Key 查看用量、费用、缓存、延迟和错误        |
| 系统设置 | 配置模型别名、调度策略、单账号并发、请求间隔、刷新参数和管理员 API Key |
| 系统更新 | 查看版本和发布说明；Release 版本可以在线更新并重启                     |

## 常用操作

### 升级

```bash
docker compose -f deploy/compose.yaml pull codex-proxy-rs
docker compose -f deploy/compose.yaml up -d --no-build
```

### 查看日志

```bash
docker compose -f deploy/compose.yaml logs -f codex-proxy-rs
```

### 从当前源码构建

```bash
docker compose -f deploy/compose.yaml build codex-proxy-rs
docker compose -f deploy/compose.yaml up -d
```

> [!WARNING]
> PostgreSQL、Redis、应用数据和文件日志都保存在仓库根目录 `.runtime/`。普通
> `docker compose down` 不会删除这些绑定目录；删除 `.runtime/` 才会清除数据。升级前请备份
> 整个目录，数据库备份中包含客户端凭据，应按敏感数据保管。

## 常见问题

### `/healthz` 返回 `503`

检查 PostgreSQL 和 Redis 容器是否健康：

```bash
docker compose -f deploy/compose.yaml ps
docker compose -f deploy/compose.yaml logs postgres redis
```

### 容器反复重启

检查 `deploy/config.yaml` 的三个密码是否填写完整，以及 `.runtime/data`、`.runtime/logs`
的权限是否正确。

### 需要从其他设备访问

Compose 默认仅绑定 `127.0.0.1`。请在服务前配置 HTTPS 反向代理，并正确转发 WebSocket 升级和真实客户端 IP 请求头。不要直接暴露 PostgreSQL 或 Redis。

## 相关链接

- [发布版本](https://github.com/zyycn/codex-proxy-rs/releases)
- [容器镜像](https://github.com/zyycn/codex-proxy-rs/pkgs/container/codex-proxy-rs)
- [详细部署说明](deploy/README.md)
- [架构说明](docs/architecture.md)

Docker 镜像支持 Linux amd64/arm64。Release 压缩包另有 macOS arm64 和 Windows amd64 版本。

## 开源许可

Codex Proxy RS 基于 [MIT License](https://opensource.org/license/mit) 开源。
