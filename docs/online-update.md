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

当前 `docker-compose.yml` 使用 `build: .`，这种模式无法可靠在线更新到远端版本。要支持 Docker 一键更新，compose 应改为使用远端镜像：

```yaml
services:
  codex-proxy-rs:
    image: ghcr.io/<owner>/codex-proxy-rs:latest
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
      CPR_UPDATER_TOKEN: ${CPR_UPDATER_TOKEN}
```

如果继续使用 `build: .`，管理端只能检查上游是否有新版本，不能安全地完成远端包更新。

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
- Docker image：`ghcr.io/<owner>/codex-proxy-rs:0.1.0`
- Docker image：`ghcr.io/<owner>/codex-proxy-rs:latest`
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
  "image": "ghcr.io/<owner>/codex-proxy-rs:0.1.0"
}
```

`GET /api/admin/system/check-updates` 返回：

```json
{
  "currentVersion": "0.1.0",
  "latestVersion": "0.2.0",
  "hasUpdate": true,
  "deploymentMode": "docker",
  "releaseUrl": "https://github.com/<owner>/codex-proxy-rs/releases/tag/v0.2.0",
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
  "image": "ghcr.io/<owner>/codex-proxy-rs:0.2.0",
  "composeProject": "codex-proxy-rs"
}
```

updater 行为：

1. 校验 token。
2. 校验 image 仓库白名单。
3. 执行 `docker pull ghcr.io/<owner>/codex-proxy-rs:0.2.0`。
4. 执行 `docker compose up -d codex-proxy-rs` 或 Docker API 等价操作。
5. 记录当前镜像 digest，支持失败诊断。

updater 不应暴露到公网。若使用 sidecar 容器，建议挂载 Docker socket 到 updater，而不是主应用：

```yaml
services:
  codex-proxy-rs-updater:
    image: ghcr.io/<owner>/codex-proxy-rs-updater:latest
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock
      - ./docker-compose.yml:/workspace/docker-compose.yml:ro
    environment:
      CPR_UPDATER_TOKEN: ${CPR_UPDATER_TOKEN}
      CPR_ALLOWED_IMAGE_REPOSITORY: ghcr.io/<owner>/codex-proxy-rs
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

