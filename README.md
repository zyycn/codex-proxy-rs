<!-- prettier-ignore -->
<div align="center">

<img src="frontend/public/favicon.svg" alt="Codex Proxy RS" width="80" height="80" />

# Codex Proxy RS

面向 Codex 的自托管多账号 AI 网关。

[![CI](https://github.com/zyycn/codex-proxy-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/zyycn/codex-proxy-rs/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/zyycn/codex-proxy-rs?display_name=tag&sort=semver&style=flat-square)](https://github.com/zyycn/codex-proxy-rs/releases)
[![GHCR](https://img.shields.io/badge/GHCR-codex--proxy--rs-2496ED?logo=docker&logoColor=white&style=flat-square)](https://github.com/zyycn/codex-proxy-rs/pkgs/container/codex-proxy-rs)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg?style=flat-square)](LICENSE)

[功能](#功能) · [快速开始](#快速开始) · [客户端接入](#客户端接入) · [文档](#文档) · [社区](#社区) · [许可证](#许可证)

</div>

将 OpenAI、xAI 账号接入同一个服务，通过网页管理账号、分配客户端密钥和查看使用情况。
客户端使用统一的访问地址与代理密钥，上游账号凭据由服务端管理。

## 功能

- **账号管理**：OAuth 授权、账号导入、凭据刷新与额度查看；支持独立出站代理及 sub2api OpenAI 账号代理导入。
- **访问控制**：通过账号分组分配密钥权限，设置日/周 USD 限额、并发和请求频率。
- **客户端接入**：支持 Codex CLI、桌面端与 Responses API 客户端，提供 HTTP 流式响应和 WebSocket。
- **请求诊断**：查看请求记录、用量、费用、延迟和错误详情。
- **部署运维**：Docker Compose 部署、S3/R2 数据库备份、同大版本在线更新与回滚。

图片生成、编辑和 Codex 独立搜索由 OpenAI 账号提供；模型与功能的可用性取决于上游账号权限。

> [!NOTE]
> 本项目提供 Responses API，不支持 `/v1/chat/completions`。接入前请确认客户端支持 Responses 协议。

## 快速开始

使用 Docker Compose 部署发布镜像 `ghcr.io/zyycn/codex-proxy-rs:latest`，同时启动 PostgreSQL 和 Redis。
以下命令适用于 Linux amd64/arm64，需要 Docker Engine、Docker Compose Plugin、curl 和 OpenSSL。已有部署请先看
[升级说明](deploy/README.md#镜像升级与源码构建)，不要覆盖原配置。

### 1. 下载部署文件并配置

```bash
mkdir -p codex-proxy-rs/deploy
cd codex-proxy-rs

curl -fsSL https://raw.githubusercontent.com/zyycn/codex-proxy-rs/main/deploy/compose.yaml \
  -o deploy/compose.yaml
curl -fsSL https://raw.githubusercontent.com/zyycn/codex-proxy-rs/main/deploy/config.example.yaml \
  -o deploy/config.example.yaml

mkdir -p .runtime/data .runtime/logs
install -d -m 0750 .runtime/postgres .runtime/redis
cp deploy/config.example.yaml deploy/config.yaml
sudo chown "$(id -u):10001" deploy/config.yaml
chmod 0640 deploy/config.yaml
sudo chown -R "$(id -u):10001" .runtime/data .runtime/logs
chmod 0770 .runtime/data .runtime/logs
```

分别生成数据库和 Redis 密码：

```bash
openssl rand -hex 24
openssl rand -hex 24
```

编辑 `deploy/config.yaml`，填好以下三项：

| 配置项 | 填写内容 |
| --- | --- |
| `store.database.password` | 第一个生成的 48 位十六进制密码 |
| `store.redis.password` | 第二个生成的 48 位十六进制密码 |
| `admin.default_password` | 管理员初始密码，至少 12 位，不能包含 `$` |

不需要创建 `.env`。管理员初始密码只在首次创建账号时使用。

### 2. 启动服务

```bash
docker compose -f deploy/compose.yaml config --quiet
docker compose -f deploy/compose.yaml pull
docker compose -f deploy/compose.yaml up -d --no-build --wait
curl -i http://127.0.0.1:8080/healthz
```

健康检查返回 `204 No Content` 后，打开 `http://127.0.0.1:8080`，
使用 `admin@cpr.local` 和刚设置的管理员密码登录。

默认地址只能在服务器本机访问。从其他设备使用时，需要配置
[HTTPS 反向代理](deploy/README.md#公网访问)。

### 3. 添加账号与客户端密钥

1. 在「账号」中添加 OpenAI 或 xAI 账号，完成授权或导入。
2. 按需建立账号分组，再创建客户端密钥并选择可用分组。**不选分组表示可使用全部账号**。
3. 打开密钥的「使用密钥」，复制客户端配置。

## 客户端接入

**Codex CLI / 桌面端**：在「使用密钥」中按操作系统复制配置，或通过 CCSwitch 导入。
合并到客户端配置后重启 Codex。完整步骤、生图配置与排障见[客户端配置](deploy/README.md#客户端配置)。

**其他 Responses API 客户端**：填写以下信息。

| 配置 | 值 |
| --- | --- |
| Base URL | `http://127.0.0.1:8080/v1`；远程接入使用服务器的 HTTPS 地址 |
| API Key | 管理端创建的 `sk_...` 客户端密钥 |

可用模型以该密钥查询到的模型列表为准：

```bash
curl http://127.0.0.1:8080/v1/models \
  -H 'Authorization: Bearer <client-api-key>'
```

## 文档

- [客户端接入与生图](deploy/README.md#客户端配置)
- [部署、备份与恢复](deploy/README.md)
- [API 参考](docs/api.md)
- [系统架构](docs/architecture.md)
- [管理端主题](docs/theme.md)
- [数据库迁移](backend/migrations/README.md)

## 社区

感谢 [LINUX DO](https://linux.do) 社区提供开放、友善的技术交流平台。

## 许可证

本项目基于 [Apache License 2.0](LICENSE) 开源。
