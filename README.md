<!-- prettier-ignore -->
<div align="center">

<img src="frontend/public/favicon.svg" alt="Codex Proxy RS" width="80" height="80" />

# Codex Proxy RS

**基于 Rust 的多 Provider 透明 AI 网关**

[![CI](https://github.com/zyycn/codex-proxy-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/zyycn/codex-proxy-rs/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/zyycn/codex-proxy-rs?display_name=tag&sort=semver&style=flat-square)](https://github.com/zyycn/codex-proxy-rs/releases)
[![GHCR](https://img.shields.io/badge/GHCR-codex--proxy--rs-2496ED?logo=docker&logoColor=white&style=flat-square)](https://github.com/zyycn/codex-proxy-rs/pkgs/container/codex-proxy-rs)
[![MIT](https://img.shields.io/badge/License-MIT-blue.svg?style=flat-square)](https://opensource.org/license/mit)

[快速开始](#快速开始) · [客户端接入](#客户端接入) · [运行维护](#运行维护) · [部署](deploy/README.md) · [接口](docs/api.md) · [架构](docs/architecture.md)

</div>

> [!NOTE]
> 数据面支持 OpenAI Responses、Images 和模型目录协议，不提供 `/v1/chat/completions`。

## 功能

| 领域 | 当前能力 |
| --- | --- |
| 数据面 | Responses JSON、SSE、WebSocket、review 子代理、Images 生成/编辑与模型目录 |
| Provider | 固定 `openai`、`xai` 两个 Provider，各自拥有 OAuth、credential、额度、目录与 transport |
| 路由 | Client Key 限定账号分组；按模型能力编译 Provider 候选；支持全局精确模型映射、会话亲和和安全 fallback |
| 透明边界 | OpenAI 保留未知请求字段及字段顺序，并原样转发 SSE、WebSocket 与 Images 业务字节；xAI 在 Provider 内转换协议 |
| 账号 | OAuth、OpenAI AT/RT 与账号文件导入，credential/额度刷新、账号恢复及主动额度重置卡 |
| 管理 | 账号与分组、Client Key、运行设置、用量/错误观测、S3/R2 备份、系统更新与回滚 |
| 计量 | Token、图片、费用、延迟、账号与 Provider 归因，并记录上游响应确认的实际 `service_tier` |

架构边界、状态所有权和请求不变量见 [架构文档](docs/architecture.md)。

## 快速开始

需要 Docker Engine 与 Docker Compose Plugin。

### 1. 准备配置和目录

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

把两个结果分别写入 `deploy/config.yaml`，再设置管理员初始密码：

| 配置 | 约束 |
| --- | --- |
| `store.database.password` | 48 位十六进制 |
| `store.redis.password` | 48 位十六进制 |
| `admin.default_password` | 至少 12 位，不能包含 `$` |

PostgreSQL、Redis 和应用从同一份 `config.yaml` 读取基础设施密码，不需要额外导出环境变量。
管理员密码只在首次创建管理员时生效。

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

`204 No Content` 表示应用、PostgreSQL 与 Redis 均可用。管理端地址为
`http://127.0.0.1:8080`。

### 3. 初始化网关

1. 使用 `admin@cpr.local` 与初始密码登录。
2. 导入或授权 Provider 账号：OpenAI 支持 OAuth、逐行 AT/RT 与 OAuth JSON；
   xAI 支持 OAuth 和账号 JSON。
3. 创建账号分组并按需配置全局模型映射。未命中的模型名原样交给候选 Provider。
4. 创建 `sk_...` Client Key，选择可用账号分组，并设置速率和并发限制；空分组范围表示全部账号。

OpenAI RT 导入会先向官方 token endpoint 换取 AT；仅 AT 的账号不具备自动续期能力，
能否参与调度仍取决于 token 中可验证的身份与后续上游事实。账号导入和首次 OAuth 创建会尽力读取一次额度，
额度上游暂时失败不会回滚已提交的账号。

OpenAI 账号的主动重置卡只在用户打开相应弹窗或手工刷新时查询。消费成功后管理端重新查询卡片与额度；
系统不会通过修改本地 `reset_at` 伪造重置。完整账号合同见 [账号接口](docs/api.md#5-账号)。

> [!IMPORTANT]
> xAI 使用 OAuth session，不支持把 xAI API Key 作为上游 credential。

完整的权限、密码轮换、备份和升级规则见 [部署文档](deploy/README.md)。

## 客户端接入

| 配置 | 值 |
| --- | --- |
| Base URL | `http://127.0.0.1:8080/v1` |
| API Key | 管理端创建的 `sk_...` |
| 鉴权 | `Authorization: Bearer <client-api-key>` |

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
<summary>公开数据面路由</summary>

| 路由 | 用途 |
| --- | --- |
| `POST /v1/responses` | JSON 响应或 SSE Responses 流 |
| `GET /v1/responses` | Responses WebSocket 升级 |
| `POST /v1/responses/review` | review 子代理请求 |
| `POST /v1/images/generations` | OpenAI Images 生成 JSON 透明转发 |
| `POST /v1/images/edits` | OpenAI Images 编辑 JSON 透明转发 |
| `GET /v1/models` | 当前 Client Key 账号范围内的模型列表 |
| `GET /v1/models/catalog` | Codex 客户端展示用模型目录 |
| `GET /v1/models/{model_id}/info` | Codex 客户端展示用模型详情 |
| `GET /v1/models/{model_id}` | OpenAI 兼容模型详情 |

所有 `/v1/*` 路由都需要 Client Key。Images 是 OpenAI Provider 自有端点，不读取模型字段，也不参与
文本模型映射。

</details>

完整的客户端与管理端路由、鉴权、请求字段和 mutation 语义见 [接口文档](docs/api.md)。

## 运行维护

```bash
# 同一大版本内升级发布镜像
docker compose -f deploy/compose.yaml pull codex-proxy-rs
docker compose -f deploy/compose.yaml up -d --no-build

# 查看日志
docker compose -f deploy/compose.yaml logs -f codex-proxy-rs

# 从当前源码构建
docker compose -f deploy/compose.yaml build codex-proxy-rs
docker compose -f deploy/compose.yaml up -d
```

> [!IMPORTANT]
> `.runtime/` 保存 PostgreSQL、Redis、日志、OpenAI 会话锚点和在线更新状态。删除该目录会永久清除
> 本地运行状态。

> [!WARNING]
> 不支持跨大版本在线升级。升级到新的大版本时请使用全新的 `.runtime/` 数据目录，并重新导入或授权
> Provider 账号与 Client Key。

Compose 默认只绑定 `127.0.0.1`。公网接入应使用 HTTPS 反向代理，转发 WebSocket upgrade 与真实客户端
IP；不要暴露 PostgreSQL 或 Redis。当前运行拓扑为单副本，不能通过复制应用容器扩容。

## 维护与发布

仓库版本只能从干净、已同步上游的发布分支发布：

```bash
release/publish <version>
```

脚本依赖已登录的 GitHub CLI（`gh auth login`），会更新 `release/version.yaml`、创建约定提交和带注释的
`v<version>` tag，原子推送分支与 tag，再触发 Release 构建。源码提交、仓库发版和运行实例升级是三个
独立状态；脚本不会登录服务器或改变任何运行实例。

## 文档

- [部署与运维](deploy/README.md)
- [客户端与管理端接口](docs/api.md)
- [系统架构](docs/architecture.md)
- [数据库迁移规则](backend/migrations/README.md)
- [Release](https://github.com/zyycn/codex-proxy-rs/releases)
- [容器镜像](https://github.com/zyycn/codex-proxy-rs/pkgs/container/codex-proxy-rs)
