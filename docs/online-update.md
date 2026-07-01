# 在线更新方案

本文档定义 Codex Proxy RS 的应用包在线更新方案。目标是支持管理端检查更新，并在管理员点击按钮后完成一键更新。

## 目标

- 管理端可以显示当前版本、最新版本、更新说明和部署模式。
- 管理端可以手动检查更新。
- 管理端可以触发一键更新。
- Docker 部署是主要场景。
- 裸机二进制部署可以作为后续兼容场景。
- 更新过程必须保留数据卷、配置和日志，不覆盖运行数据。

## 非目标

- 不在运行中的 Rust 进程里热加载新代码。
- 不在 Docker 容器内部直接替换当前容器的二进制作为主方案。
- 不默认把宿主机 Docker socket 挂进主应用容器。
- 不在没有迁移机制前执行高风险数据库结构变更。

## 部署模式

### Docker 模式

Docker 模式下，一键更新应更新镜像并重建容器：

1. 管理端调用后端更新接口。
2. 后端完成管理员鉴权和并发锁控制。
3. 后端调用 updater。
4. updater 在宿主机或 sidecar 中执行镜像更新。
5. updater 拉取新镜像。
6. updater 重新创建 `codex-proxy-rs` 服务。
7. 数据卷继续挂载，SQLite、日志和配置保持不变。

推荐结构：

```text
browser
  -> codex-proxy-rs admin api
    -> updater sidecar / host updater
      -> docker pull
      -> docker compose up -d codex-proxy-rs
```

主应用容器不应直接拥有 Docker daemon 权限。updater 可以是一个独立容器，也可以是宿主机 systemd 服务。updater 只暴露给同一 Docker network 或 `127.0.0.1`，并使用共享 token 鉴权。

项目只保留生产用 `docker-compose.yml`，不维护本地 Docker 开发配置。compose 必须使用远端镜像，不能使用 `build: .`，否则管理端只能检查更新，无法安全地完成一键更新。

```yaml
services:
  codex-proxy-rs:
    image: ghcr.io/zyycn/codex-proxy-rs:latest
    ports:
      - "8080:8080"
    volumes:
      - ./data:/app/data
      - ./logs:/app/logs
      - ./config.yaml:/app/config.yaml:ro
    environment:
      CPR_DEPLOYMENT_MODE: docker
      CPR_VERSION: 0.1.0
      CPR_UPDATE_CHANNEL: stable
      CPR_UPDATER_URL: http://codex-proxy-rs-updater:8090
      CPR_UPDATER_TOKEN: ${CPR_UPDATER_TOKEN:?CPR_UPDATER_TOKEN is required}
```

### 二进制模式

二进制模式可以采用 Sub2API 类似方案：

1. 请求 GitHub Releases latest。
2. 匹配当前 OS/arch 的压缩包。
3. 下载 release asset。
4. 校验 `checksums.txt`。
5. 解压新二进制和 `web/dist`。
6. 备份旧二进制和旧 `web/dist`。
7. 原子替换。
8. 返回 `needRestart: true`。
9. 管理端触发重启，进程退出后由 systemd/supervisor 拉起。

二进制模式不是当前 Docker 部署的主路径，但可以复用检查更新、版本展示、更新锁和前端 UI。

## 版本与发布产物

发布时应同时产出 Docker 镜像和 release 元数据：

- Git tag：`v0.1.0`
- Docker image：`ghcr.io/zyycn/codex-proxy-rs:0.1.0`
- Docker image：`ghcr.io/zyycn/codex-proxy-rs:latest`
- Release archive：`codex-proxy-rs_0.1.0_linux_x86_64.tar.gz`
- Checksum：`checksums.txt`

后端应在构建时注入版本信息：

- `CPR_VERSION`
- `CPR_GIT_SHA`
- `CPR_BUILD_TIME`
- `CPR_DEPLOYMENT_MODE`
- `CPR_IMAGE_REPOSITORY`
- `CPR_IMAGE_TAG`

Docker 模式下，检查更新可以优先使用 GitHub Releases 的 tag 做语义版本比较；真正更新时由 updater 拉取指定镜像 tag 或 digest。

## 版本发布流程

版本发布以 Git tag 为唯一入口，tag、Docker 镜像、GitHub Release 和后端版本信息必须保持一致。

### 版本号规则

使用语义化版本：

```text
MAJOR.MINOR.PATCH
```

示例：

```text
0.2.0
0.2.1
1.0.0
```

Git tag 使用 `v` 前缀：

```text
v0.2.0
```

代码中的版本来源：

- `Cargo.toml` 的 `[package].version` 是本地构建默认版本。
- `web/package.json` 的 `version` 应与后端版本保持一致。
- CI 发布时以 Git tag 为准，并通过 build args 注入 `CPR_VERSION`。

发布前必须同步：

```text
Cargo.toml              version = "0.2.0"
web/package.json        "version": "0.2.0"
Git tag                 v0.2.0
Docker image tag        0.2.0
GitHub Release tag      v0.2.0
```

如果后续希望避免手动同步，可以增加 `scripts/release-version`，统一更新 `Cargo.toml`、`Cargo.lock` 和 `web/package.json`。

### 发布通道

推荐先只维护 `stable` 通道：

```text
latest -> 最新 stable 版本
0.2.0  -> 固定版本
sha-*  -> commit 固定构建
```

后续需要预发布时再增加：

```text
next
beta
rc
```

管理端检查更新默认只看 stable release。带 `prerelease: true` 的 GitHub Release 不应推送给普通更新检查，除非配置了 `CPR_UPDATE_CHANNEL=beta`。

### 发布前检查

发布前必须完成：

```bash
pnpm --dir web build
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --test main
```

发布前还应确认：

- `Cargo.toml` release profile 已关闭 debug symbols。
- Dockerfile 不复制本地 `config.yaml`。
- Dockerfile 在容器内构建 `web/dist`。
- `.dockerignore` 排除了 `target`、`node_modules`、数据库和日志。
- `docker-compose.yml` 使用远端 `image`，不是 `build: .`。
- release note 写明数据库迁移风险。

### 发布步骤

标准发布步骤：

```bash
VERSION=0.2.0

# 1. 更新版本号
# Cargo.toml: version = "0.2.0"
# web/package.json: "version": "0.2.0"

# 2. 更新 Cargo.lock 中的本包版本
cargo check --locked

# 3. 提交版本变更
git add Cargo.toml Cargo.lock web/package.json
git commit -m "chore: release v${VERSION}"

# 4. 创建 tag
git tag "v${VERSION}"
git push origin HEAD
git push origin "v${VERSION}"
```

tag 推送后由 `.github/workflows/release.yml` 自动构建并发布：

1. 构建前端。
2. 构建 Rust release。
3. 生成二进制压缩包。
4. 生成 `checksums.txt`。
5. 构建并推送 Docker 多架构镜像。
6. 构建并推送 updater 多架构镜像。
7. 创建 GitHub Release。
8. 上传 release assets。

### Release Note 格式

GitHub Release body 建议固定结构，方便管理端展示：

```markdown
## Highlights

- ...

## Changes

- ...

## Upgrade Notes

- ...

## Database

- Migration: none
- Backup required: no
```

如果需要用户备份，必须写明：

```markdown
## Database

- Migration: required
- Backup required: yes
```

后端检查更新时可以从 release body 或额外 manifest 中解析 `requiresBackup`，并在管理端一键更新前要求确认。

### 发布后验证

发布完成后验证：

```bash
docker pull ghcr.io/zyycn/codex-proxy-rs:0.2.0
docker run --rm ghcr.io/zyycn/codex-proxy-rs:0.2.0 --version
```

如果暂时没有 `--version` 命令，则使用管理端接口验证：

```bash
curl http://127.0.0.1:8080/api/admin/system/version
```

还需要验证：

- GitHub Release latest 指向新版本。
- `latest` 镜像 tag 指向新版本。
- 固定版本 tag 可以拉取。
- 管理端检查更新能看到新版本。
- Docker 一键更新能从旧版本升级到新版本。

## Docker 构建方案

Docker 镜像应由 CI 直接完成前端构建、后端构建和 runtime 镜像组装。发布镜像不能依赖本地已经存在的 `web/dist`，否则 CI、开发机和发布机之间容易产出不一致。

推荐 Dockerfile 结构：

```dockerfile
# syntax=docker/dockerfile:1.7

FROM node:24-bookworm-slim AS web-builder
WORKDIR /app/web
COPY web/package.json web/pnpm-lock.yaml web/pnpm-workspace.yaml ./
RUN corepack enable && pnpm install --frozen-lockfile
COPY web .
RUN pnpm build

FROM rust:1.95-bookworm AS rust-builder
WORKDIR /app
ARG CPR_VERSION=dev
ARG CPR_GIT_SHA=unknown
ARG CPR_BUILD_TIME=unknown
ENV CPR_VERSION=${CPR_VERSION}
ENV CPR_GIT_SHA=${CPR_GIT_SHA}
ENV CPR_BUILD_TIME=${CPR_BUILD_TIME}
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
COPY --from=web-builder /app/web/dist ./web/dist
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build --release --locked --bins && \
    cp /app/target/release/codex-proxy-rs /tmp/codex-proxy-rs && \
    cp /app/target/release/codex-proxy-rs-updater /tmp/codex-proxy-rs-updater && \
    strip /tmp/codex-proxy-rs /tmp/codex-proxy-rs-updater

FROM debian:bookworm-slim AS updater-runtime
ARG CPR_VERSION=dev
ARG CPR_GIT_SHA=unknown
ARG CPR_BUILD_TIME=unknown
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates docker.io docker-compose \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /workspace
ENV CPR_DOCKER_COMPOSE_BIN=docker-compose \
    CPR_VERSION=${CPR_VERSION} \
    CPR_GIT_SHA=${CPR_GIT_SHA} \
    CPR_BUILD_TIME=${CPR_BUILD_TIME}
COPY --from=rust-builder /tmp/codex-proxy-rs-updater /usr/local/bin/codex-proxy-rs-updater

EXPOSE 8090
CMD ["codex-proxy-rs-updater"]

FROM debian:bookworm-slim AS runtime
ARG CPR_VERSION=dev
ARG CPR_GIT_SHA=unknown
ARG CPR_BUILD_TIME=unknown
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
ENV CPR_DEPLOYMENT_MODE=docker \
    CPR_VERSION=${CPR_VERSION} \
    CPR_GIT_SHA=${CPR_GIT_SHA} \
    CPR_BUILD_TIME=${CPR_BUILD_TIME}
COPY --from=rust-builder /tmp/codex-proxy-rs /usr/local/bin/codex-proxy-rs

EXPOSE 8080
CMD ["codex-proxy-rs"]
```

构建上下文必须增加 `.dockerignore`，避免把本地编译产物、数据库、日志和依赖目录传入 Docker daemon：

```gitignore
.git
target
node_modules
web/node_modules
.runtime
data
logs
config.yaml
.env
*.sqlite
*.sqlite-*
*.db
```

发布镜像不应复制本地 `config.yaml`。配置文件应由 compose volume 挂载，避免镜像更新时覆盖用户配置：

```yaml
volumes:
  - ./config.yaml:/app/config.yaml:ro
  - ./data:/app/data
  - ./logs:/app/logs
```

后端 release profile 应关闭 debug symbols，并 strip 符号，避免 Docker 应用层过大：

```toml
[profile.release]
debug = false
strip = "symbols"
lto = "thin"
codegen-units = 1
panic = "abort"
```

如果后续仍需要 debug symbols，应把 symbols 作为单独 CI artifact 上传，不放进 runtime 镜像。

## Docker 发布方案

发布以 Git tag 为入口，例如 `v0.2.0`。CI 应完成以下产物：

1. 多架构 Docker 镜像。
2. GitHub Release。
3. 二进制 tar.gz 包。
4. `checksums.txt`。

推荐镜像标签：

```text
ghcr.io/zyycn/codex-proxy-rs:0.2.0
ghcr.io/zyycn/codex-proxy-rs:latest
ghcr.io/zyycn/codex-proxy-rs:sha-<git-sha>
```

推荐多架构平台：

```text
linux/amd64
linux/arm64
```

CI 发布命令形态：

```bash
docker buildx build \
  --platform linux/amd64,linux/arm64 \
  --build-arg CPR_VERSION="${VERSION}" \
  --build-arg CPR_GIT_SHA="${GIT_SHA}" \
  --build-arg CPR_BUILD_TIME="${BUILD_TIME}" \
  --label org.opencontainers.image.title="Codex Proxy RS" \
  --label org.opencontainers.image.version="${VERSION}" \
  --label org.opencontainers.image.revision="${GIT_SHA}" \
  --label org.opencontainers.image.source="https://github.com/zyycn/codex-proxy-rs" \
  -t "ghcr.io/zyycn/codex-proxy-rs:${VERSION}" \
  -t "ghcr.io/zyycn/codex-proxy-rs:latest" \
  --push \
  .
```

updater 镜像使用同一个 Dockerfile 的 `updater-runtime` target：

```bash
docker buildx build \
  --platform linux/amd64,linux/arm64 \
  --target updater-runtime \
  --build-arg CPR_VERSION="${VERSION}" \
  --build-arg CPR_GIT_SHA="${GIT_SHA}" \
  --build-arg CPR_BUILD_TIME="${BUILD_TIME}" \
  -t "ghcr.io/zyycn/codex-proxy-rs-updater:${VERSION}" \
  -t "ghcr.io/zyycn/codex-proxy-rs-updater:latest" \
  --push \
  .
```

GitHub Release 的 tag 是检查更新的权威来源。Release body 用于管理端展示更新说明。Docker 更新实际拉取的镜像应来自 release tag 对应的版本，不从用户输入拼接任意镜像地址。

生产 compose 使用仓库根目录的 `docker-compose.yml`。主服务镜像由 `.env` 的 `CPR_IMAGE` 控制，updater 更新时会改写这个变量；`.env` 是本机部署状态文件，不提交到仓库。

```dotenv
CPR_UPDATER_TOKEN=change-me
CPR_DEPLOY_DIR=/opt/codex-proxy-rs
CPR_IMAGE=ghcr.io/zyycn/codex-proxy-rs:latest
```

```yaml
services:
  codex-proxy-rs:
    image: ${CPR_IMAGE:-ghcr.io/zyycn/codex-proxy-rs:latest}
    container_name: codex-proxy-rs
    restart: unless-stopped
    ports:
      - "8080:8080"
    volumes:
      - ./config.yaml:/app/config.yaml:ro
      - ./data:/app/data
      - ./logs:/app/logs
    environment:
      CPR_DEPLOYMENT_MODE: docker
      CPR_UPDATE_REPOSITORY: zyycn/codex-proxy-rs
      CPR_IMAGE_REPOSITORY: ghcr.io/zyycn/codex-proxy-rs
      CPR_IMAGE_TAG: latest
      CPR_UPDATE_CHANNEL: stable
      CPR_UPDATER_URL: http://codex-proxy-rs-updater:8090
      CPR_UPDATER_TOKEN: ${CPR_UPDATER_TOKEN:?CPR_UPDATER_TOKEN is required}

  codex-proxy-rs-updater:
    image: ghcr.io/zyycn/codex-proxy-rs-updater:latest
    restart: unless-stopped
    working_dir: ${CPR_DEPLOY_DIR:?CPR_DEPLOY_DIR is required}
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock
      - ${CPR_DEPLOY_DIR:?CPR_DEPLOY_DIR is required}:${CPR_DEPLOY_DIR:?CPR_DEPLOY_DIR is required}
    environment:
      CPR_UPDATER_TOKEN: ${CPR_UPDATER_TOKEN:?CPR_UPDATER_TOKEN is required}
      CPR_ALLOWED_IMAGE_REPOSITORY: ghcr.io/zyycn/codex-proxy-rs
      CPR_COMPOSE_FILE: ${CPR_DEPLOY_DIR:?CPR_DEPLOY_DIR is required}/docker-compose.yml
      CPR_COMPOSE_ENV_FILE: ${CPR_DEPLOY_DIR:?CPR_DEPLOY_DIR is required}/.env
      CPR_COMPOSE_IMAGE_ENV: CPR_IMAGE
      CPR_COMPOSE_SERVICE: codex-proxy-rs
      CPR_UPDATER_STATE_FILE: ${CPR_DEPLOY_DIR:?CPR_DEPLOY_DIR is required}/.runtime/updater-state.json
```

`CPR_DEPLOY_DIR` 必须是宿主机上的绝对路径，并且 updater 容器内也挂载到同一个绝对路径。这样 updater 通过 Docker socket 调用 compose 时，`./config.yaml`、`./data`、`./logs` 等 bind mount 路径会被 host Docker 正确解析。

## Docker 在线一键更新流程

Docker 一键更新分为检查、触发、执行、恢复四个阶段。

### 1. 检查更新

后端调用 GitHub Releases latest：

```text
GET https://api.github.com/repos/zyycn/codex-proxy-rs/releases/latest
```

后端比较：

- 当前版本：构建时注入的 `CPR_VERSION`。
- 最新版本：GitHub Release tag。
- 当前部署模式：`CPR_DEPLOYMENT_MODE=docker`。
- 当前镜像仓库：`CPR_IMAGE_REPOSITORY`。
- updater 可用性：`CPR_UPDATER_URL` 和 `CPR_UPDATER_TOKEN` 是否存在。

只有满足以下条件时，`updateSupported` 才返回 `true`：

- 当前部署模式是 `docker`。
- 当前 compose 使用远端 `image`，不是 `build: .`。
- 已配置 updater URL 和 token。
- release tag 能映射到允许的镜像仓库。

### 2. 触发更新

管理员点击一键更新后，主应用后端：

1. 校验管理员会话或管理员 API Key。
2. 获取全局系统更新锁。
3. 重新检查 latest release，避免使用过期缓存。
4. 生成目标镜像，例如 `ghcr.io/zyycn/codex-proxy-rs:0.2.0`。
5. 调用 updater 的 `/update`。
6. 立即返回 `operationId` 和 `needReconnect: true`。

主应用后端不能接收前端传入的任意镜像地址。前端最多传 `targetVersion`，后端根据 release 信息和仓库白名单推导目标镜像。

### 3. 执行更新

updater 收到请求后：

1. 校验 bearer token。
2. 校验目标镜像仓库必须等于 `CPR_ALLOWED_IMAGE_REPOSITORY`。
3. 立即返回 `update started`，让主应用可以先响应前端。
4. 后台执行 `docker pull ghcr.io/zyycn/codex-proxy-rs:0.2.0`。
5. 将 `.env` 中的 `CPR_IMAGE` 改成目标镜像。
6. 执行 `docker compose up -d codex-proxy-rs`。
7. 记录当前和上一个镜像，供回滚使用。

updater 只允许执行固定动作，不提供通用 shell 执行能力。

### 4. 服务恢复

Docker 重建主应用容器时，前端连接会中断。管理端 UI 应：

1. 显示更新中。
2. 每 2-3 秒轮询 `/api/admin/system/version`。
3. 服务恢复后比较返回版本。
4. 版本等于目标版本时显示更新成功。
5. 超时后提示用户查看容器日志。

推荐超时时间：120 秒。

## 更新失败与回滚

Docker 模式下的回滚以镜像标签或 digest 为单位：

1. 更新前记录当前镜像 digest。
2. 更新失败时 updater 可以重新部署旧 digest。
3. 如果服务已启动但健康检查失败，也回滚到旧 digest。
4. 回滚只替换容器镜像，不回滚 SQLite 数据。

因此涉及数据库结构变更的版本必须谨慎发布。若 release 声明需要备份，管理端一键更新前应显示确认状态，后端可要求请求带上 `confirmBackup: true`。

## 后端 API

建议新增管理端接口：

```http
GET  /api/admin/system/version
GET  /api/admin/system/check-updates?force=true
POST /api/admin/system/update
POST /api/admin/system/rollback
POST /api/admin/system/restart
```

所有接口必须要求管理员会话或管理员 API Key。

`GET /api/admin/system/version` 返回：

```json
{
  "version": "0.1.0",
  "gitSha": "abc1234",
  "buildTime": "2026-07-01T00:00:00Z",
  "deploymentMode": "docker",
  "image": "ghcr.io/zyycn/codex-proxy-rs:0.1.0"
}
```

`GET /api/admin/system/check-updates` 返回：

```json
{
  "currentVersion": "0.1.0",
  "latestVersion": "0.2.0",
  "hasUpdate": true,
  "deploymentMode": "docker",
  "releaseUrl": "https://github.com/zyycn/codex-proxy-rs/releases/tag/v0.2.0",
  "notes": "...",
  "cached": false,
  "updateSupported": true,
  "unsupportedReason": null
}
```

`POST /api/admin/system/update` 在 Docker 模式下返回：

```json
{
  "operationId": "sysop-...",
  "deploymentMode": "docker",
  "message": "Update started",
  "needReconnect": true
}
```

如果当前部署不支持一键更新，应返回明确原因：

```json
{
  "updateSupported": false,
  "unsupportedReason": "Docker one-click update requires CPR_UPDATER_URL and a remote image deployment"
}
```

## Updater 服务

Docker 一键更新的关键是 updater。updater 负责真正操作 Docker：

```http
POST /update
Authorization: Bearer <CPR_UPDATER_TOKEN>
Content-Type: application/json
```

请求：

```json
{
  "service": "codex-proxy-rs",
  "image": "ghcr.io/zyycn/codex-proxy-rs:0.2.0",
  "composeProject": "codex-proxy-rs"
}
```

updater 行为：

1. 校验 token。
2. 校验 image 仓库白名单。
3. 执行 `docker pull ghcr.io/zyycn/codex-proxy-rs:0.2.0`。
4. 将 `CPR_COMPOSE_ENV_FILE` 中的 `CPR_COMPOSE_IMAGE_ENV` 更新为目标镜像。
5. 执行 `docker compose up -d codex-proxy-rs` 或 `docker-compose up -d codex-proxy-rs`。
6. 记录当前和上一个镜像，用于回滚。

updater 不应暴露到公网。若使用 sidecar 容器，建议挂载 Docker socket 到 updater，而不是主应用：

```yaml
services:
  codex-proxy-rs-updater:
    image: ghcr.io/zyycn/codex-proxy-rs-updater:latest
    working_dir: ${CPR_DEPLOY_DIR:?CPR_DEPLOY_DIR is required}
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock
      - ${CPR_DEPLOY_DIR:?CPR_DEPLOY_DIR is required}:${CPR_DEPLOY_DIR:?CPR_DEPLOY_DIR is required}
    environment:
      CPR_UPDATER_TOKEN: ${CPR_UPDATER_TOKEN:?CPR_UPDATER_TOKEN is required}
      CPR_ALLOWED_IMAGE_REPOSITORY: ghcr.io/zyycn/codex-proxy-rs
      CPR_COMPOSE_FILE: ${CPR_DEPLOY_DIR:?CPR_DEPLOY_DIR is required}/docker-compose.yml
      CPR_COMPOSE_ENV_FILE: ${CPR_DEPLOY_DIR:?CPR_DEPLOY_DIR is required}/.env
      CPR_COMPOSE_IMAGE_ENV: CPR_IMAGE
      CPR_UPDATER_STATE_FILE: ${CPR_DEPLOY_DIR:?CPR_DEPLOY_DIR is required}/.runtime/updater-state.json
    networks:
      - default
```

如果后续不想维护自研 updater，可以评估 Watchtower 的 HTTP API 模式，但仍需要后端把“检查更新、按钮触发、操作结果展示”封装到管理端。

## 安全约束

- 所有更新接口必须走管理员鉴权。
- 更新任务必须有全局锁，避免并发更新。
- GitHub release 下载 URL 必须限制为可信 HTTPS host。
- Docker image 仓库必须白名单校验。
- 下载包必须限制大小。
- 二进制包必须校验 checksum。
- Docker 更新必须保留 `data`、`logs`、`config.yaml` 挂载。
- 不允许通过管理端传入任意 shell 命令。
- updater token 只能通过环境变量或密钥文件配置，不能写入前端。

## 前端交互

管理端系统页建议包含：

- 当前版本。
- 部署模式。
- 最新版本。
- 更新说明。
- 检查更新按钮。
- 一键更新按钮。
- 更新中状态。
- 更新后重连提示。
- 不支持一键更新时的明确原因。

Docker 更新会导致容器重建，前端连接会短暂断开。UI 应在触发更新后轮询 `/api/admin/system/version`，服务恢复后提示已更新。

## 数据库迁移约束

当前项目启动时执行 `schema.sql`，主要依赖 `create table if not exists`。在加入在线更新前，建议补充迁移版本表，例如：

```sql
create table if not exists schema_migrations (
  version integer primary key,
  applied_at text not null
);
```

发布新版本时：

- 向前兼容的字段新增可以自动迁移。
- 删除字段、重命名字段、不可逆数据迁移需要显式 release note。
- 一键更新前可以在检查结果里标注 `requiresBackup: true`。

## 推荐落地顺序

1. 建立 GitHub Release 和 GHCR 镜像发布流程。
2. 为后端注入版本、commit、构建时间和部署模式。
3. 增加系统版本与检查更新 API。
4. 增加管理端系统更新页。
5. 增加 Docker updater sidecar。
6. 接入一键更新按钮。
7. 补充数据库迁移机制。
8. 再考虑二进制模式的原子替换和 rollback。
