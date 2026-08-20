<!-- prettier-ignore -->
<div align="center">

<img src="frontend/public/favicon.svg" alt="Codex Proxy RS" width="80" height="80" />

# Codex Proxy RS

**基于 Rust 的多 Provider 透明 AI 网关**

[![CI](https://github.com/zyycn/codex-proxy-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/zyycn/codex-proxy-rs/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/zyycn/codex-proxy-rs?display_name=tag&sort=semver&style=flat-square)](https://github.com/zyycn/codex-proxy-rs/releases)
[![GHCR](https://img.shields.io/badge/GHCR-codex--proxy--rs-2496ED?logo=docker&logoColor=white&style=flat-square)](https://github.com/zyycn/codex-proxy-rs/pkgs/container/codex-proxy-rs)
[![MIT](https://img.shields.io/badge/License-MIT-blue.svg?style=flat-square)](https://opensource.org/license/mit)

[快速开始](#快速开始) · [客户端接入](#客户端接入) · [运维](#运维) · [部署](deploy/README.md) · [接口](docs/api.md) · [架构](docs/architecture.md)

</div>

> [!NOTE]
> 只支持 OpenAI Responses API，不提供 `/v1/chat/completions`。

## 能力

| 领域     | 实现                                                                                                  |
| -------- | ----------------------------------------------------------------------------------------------------- |
| 协议     | OpenAI Responses JSON、SSE、WebSocket 与模型目录                                                       |
| Provider | 固定 OpenAI、xAI 两个 Provider；各自管理 OAuth、账号、额度与模型目录                                    |
| 路由     | Client Key 绑定 Provider、全局模型映射、会话亲和与同 Provider 安全 fallback                             |
| 透明边界 | OpenAI 请求保留未知字段和字段顺序，SSE 与 WebSocket 原样转发业务字节；xAI 在 Provider 内做协议转换          |
| 延续     | OpenAI native/replay continuation；xAI 使用客户端提交的完整历史                                          |
| 账号     | 导入、OAuth、credential 刷新、主动额度刷新，以及正常响应中的被动额度/状态观测                              |
| 管理     | Client Key、账号、模型目录、设置、观测、系统更新与 OAuth                                                 |
| 计量     | 模型请求 Token、费用、延迟、账号与 Provider 归因，并记录响应确认的实际 `service_tier`                      |

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

分别生成 PostgreSQL 与 Redis 密码：

```bash
openssl rand -hex 24
openssl rand -hex 24
```

把两个结果写入 `deploy/config.yaml`，并单独设置管理员初始密码：

| 配置                       | 约束                         |
| -------------------------- | ---------------------------- |
| `store.database.password`  | 48 位十六进制                 |
| `store.redis.password`     | 48 位十六进制                 |
| `admin.default_password`   | 至少 12 位，不能包含 `$`      |

PostgreSQL、Redis 容器和应用通过同一份 `config.yaml` 使用这两个基础设施密码，不需要额外
导出环境变量。管理员密码只在首次创建管理员时生效。

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
2. 在账号页选择对应 Provider：OpenAI 支持 OAuth、账号文件与 Agent Identity；xAI 支持 OAuth
   和兼容账号文件导入。
3. 按需配置客户端模型到上游模型的全局映射；未命中映射的模型名原样透传。
4. 创建 `sk_...` Client Key，绑定 `openai` 或 `xai`，并设置速率与并发限制。

OpenAI 账号导入和首次 OAuth 创建在 credential 提交后会立即尝试一次额度观测；额度接口临时失败不会
回滚已提交的账号。Free、K12 等套餐共用同一套额度与状态转换逻辑，套餐字段只作为账号事实并隔离
对应的模型目录 cache。credential 刷新、额度刷新和正常请求中的被动额度观测是三条独立链路，详见
[账号接口](docs/api.md#5-账号)。

> [!IMPORTANT]
> xAI 使用 OAuth session，不支持把 xAI API Key 作为上游 credential。

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

| 路由                               | 用途                           |
| ---------------------------------- | ------------------------------ |
| `POST /v1/responses`               | JSON 响应或 SSE Responses 流    |
| `GET /v1/responses`                | Responses WebSocket 升级        |
| `POST /v1/responses/review`        | review 子代理请求               |
| `POST /v1/images/generations`      | 图像生成 JSON 透明转发          |
| `POST /v1/images/edits`            | 图像编辑 JSON 透明转发          |
| `GET /v1/models`                   | 当前 Client Key 可用的模型列表   |
| `GET /v1/models/catalog`           | Codex 客户端展示用模型目录      |
| `GET /v1/models/{model_id}/info`   | Codex 客户端展示用模型详情      |
| `GET /v1/models/{model_id}`        | OpenAI 兼容模型详情             |

所有 `/v1/*` 路由都需要客户端 API Key。

</details>

完整的客户端与管理端路由、鉴权和 mutation 规则见 [接口文档](docs/api.md)。

## 运维

```bash
# 实例升级
docker compose -f deploy/compose.yaml pull codex-proxy-rs
docker compose -f deploy/compose.yaml up -d --no-build

# 日志
docker compose -f deploy/compose.yaml logs -f codex-proxy-rs

# 从源码构建
docker compose -f deploy/compose.yaml build codex-proxy-rs
docker compose -f deploy/compose.yaml up -d
```

> [!IMPORTANT]
> `.runtime/` 保存 PostgreSQL、Redis、日志、OpenAI 会话锚点密钥和自更新状态。删除该目录会永久清除
> 运行状态。

> [!WARNING]
> 不支持跨大版本在线升级。升级到新的大版本时，请使用全新的 `.runtime/` 数据目录重新部署，
> 并重新导入或重新授权 Provider 账号与客户端 Key。

Compose 默认只绑定 `127.0.0.1`。公网接入应使用 HTTPS 反向代理，转发 WebSocket
upgrade 与真实客户端 IP；不要暴露 PostgreSQL 或 Redis。

## 维护与发布

发布仓库版本必须从干净、已同步上游的发布分支运行：

```bash
release/publish <version>
```

脚本依赖已登录的 GitHub CLI（`gh auth login`）。它会更新 `release/version.yaml`、创建约定提交与带
注释的 `v<version>` tag，原子推送分支和 tag，再从发布分支触发 Release 构建，使构建缓存可以跨版本
复用。不要手工修改版本后单独打 tag。该脚本不会登录服务器，也不会改变任何运行实例；源码提交、
仓库发版和实例升级是三个独立状态。

## 文档

- [部署](deploy/README.md)
- [客户端与管理端接口](docs/api.md)
- [架构与数据边界](docs/architecture.md)
- [Release](https://github.com/zyycn/codex-proxy-rs/releases)
- [容器镜像](https://github.com/zyycn/codex-proxy-rs/pkgs/container/codex-proxy-rs)
