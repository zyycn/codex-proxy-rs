# 部署与 CI 改造设计

> 设计基线：`e4b274a2a6095f22a7f526732ba4886f60441218`
>
> 适用范围：`deploy/`、`.github/workflows/`、`release/publish`、运行时配置加载与部署说明。

本文是本轮部署与 CI 改造的权威规格。实现必须保持 feature 分支 push 检查，消除同一提交的重复镜像构建，保证发布提交经过完整质量门禁，并用真实 PostgreSQL、Redis 和应用容器验证镜像能够无警告、无错误地启动和退出。

## 1. 已确认的现状

### 1.1 重复构建真实存在，但原审计定级和耗时不准确

- `ci.yml` 的 `deploy` 与 `security-scan.yml` 的 `docker-security` 会在每次 push 和 pull request 中分别构建一次 `runtime` 镜像。
- 已成功运行的远端记录显示，两次构建合计约占 8 分钟 runner 时间；两个 workflow 并行执行，不能按墙钟时间简单相加为“十余分钟乘二”。
- 最近两次失败分别在 9 秒和 6 秒落到同一个镜像构建错误，证明失败路径也被重复执行。
- GitHub Actions 不支持给普通 job 直接配置 `paths`。路径判断必须由独立 job 产生输出，或使用 workflow 级路径过滤。

### 1.2 Compose 密码与应用连接串没有真正同步

- PostgreSQL 和 Redis 服务端密码来自 `CPR_POSTGRES_PASSWORD`、`CPR_REDIS_PASSWORD`。
- 应用当前只读取 YAML 中的固定 URL；仅修改 Compose 环境变量会导致服务端密码和客户端连接串不一致。
- Redis 部署说明要求人工同步 URL，PostgreSQL 没有同等说明；这种双写方式必须删除。

### 1.3 发布门禁与发布镜像扫描缺失

- `release/publish` 会生成新的版本提交和 tag，并原子推送分支与 tag。这个新提交此前没有经过 CI。
- 通过查询 check-runs 判断“曾经绿过”存在竞态，也无法证明版本提交本身通过测试。
- Security Scan 扫描的是源码构建的 `runtime`，Release 发布的是由预编译产物组装的 `runtime-prebuilt`。二者不是同一镜像。

### 1.4 CI 没有运行态烟测

当前 deploy job 只执行 Compose 配置解析和 Docker 构建，不能发现以下问题：

- entrypoint 或动态链接错误；
- UID/GID `10001:10001` 无法写入 bind mount；
- PostgreSQL 迁移或 Redis 连接失败；
- `/healthz` 无法通过；
- 启动或关闭期间产生 WARN、ERROR、panic；
- PostgreSQL、Redis 数据卷在容器替换后丢失。

### 1.5 发布标签会被预发布版本污染

当前 SemVer 校验允许 `1.2.0-rc.1`，但镜像 manifest 无条件更新 `latest`，GitHub Release 也没有显式标记 prerelease。预发布版本会错误占用稳定通道。

### 1.6 供应链输入没有固定

- workflow 中所有 action 均使用可移动 tag，而不是完整 commit SHA。
- Dockerfile、Compose 和 CI service container 均使用可移动镜像 tag。
- 仓库没有 Dependabot 配置，固定后的依赖无法形成自动更新闭环。

## 2. 目标与验收口径

本轮实现必须同时满足以下条件：

1. feature 分支 push 与 pull request 都保留；同一分支的两个事件共享 concurrency group，后到事件取消先到事件，不并行重复执行完整检查。
2. 普通 CI 对同一提交最多构建一次源码版 `runtime` 镜像；该镜像同时用于 Trivy 门禁和 Compose 烟测。
3. Release 对 tag 指向的精确 SHA 重新执行完整质量门禁，不依赖历史 check-runs。
4. Release 扫描实际推送的每个平台 `runtime-prebuilt` digest，扫描通过后才能创建多架构 manifest 和 GitHub Release。
5. Rust format、clippy、测试，前端格式和构建，workflow 静态检查全部为 0 错误、0 警告。
6. Compose 烟测使用真实 PostgreSQL 和 Redis，应用健康检查返回 HTTP 204，三个容器日志中不出现 WARN、WARNING、ERROR、FATAL、PANIC 严重级别。
7. 应用容器以 UID/GID `10001:10001` 运行，数据和日志目录可写，SIGTERM 后在 30 秒内以退出码 0 结束。
8. PostgreSQL 和 Redis 密码只需在 Compose 环境中设置一次；包含 URL 特殊字符的密码也能正确连接，错误信息不得泄露密码。
9. PostgreSQL 和 Redis 的命名卷在服务容器强制重建后仍保留探针数据。
10. 所有 action 和容器镜像输入固定到不可变 SHA 或 digest，并由 Dependabot 持续提出更新。

## 3. 非目标

以下内容不在本轮实施范围内：

- 不删除 PostgreSQL、Redis 的 `127.0.0.1` 宿主端口映射；本地集成测试仍依赖它们。
- 不凭经验给 PostgreSQL、Redis 增加 CPU、内存或 `shm_size` 上限。资源预算应基于真实负载单独评审。
- 不改变 PostgreSQL、Redis 的命名卷持久化方案，也不把数据库数据迁移到应用的 `.runtime/` bind mount。
- 不改变日志表和统计表保留策略；该范围已由日志与保留策略改造处理。
- 不引入兼容配置键、旧密码路径或 YAML 与环境变量双写逻辑。

## 4. 工作流架构

目标工作流由两个可复用门禁和三个入口组成：

```text
CI push / pull_request
  -> changes
  -> _quality.yml(ref)
  -> _container.yml(ref), 仅镜像相关路径变化时执行

Security Scan push / pull_request
  -> Cargo 与 pnpm 依赖审计

Security Scan schedule
  -> Cargo 与 pnpm 依赖审计
  -> _container.yml(ref)

Release tag / workflow_dispatch
  -> release-info
  -> _quality.yml(tag SHA, 上传 frontend-dist)
  -> 平台二进制与 release assets
  -> runtime-prebuilt 平台镜像
  -> 精确 digest Trivy 门禁
  -> 多架构 manifest、签名、attestation、GitHub Release
```

### 4.1 `.github/workflows/_quality.yml`

`_quality.yml` 只通过 `workflow_call` 调用，接收：

- `ref`：必填的完整提交 SHA；所有 checkout 都显式使用它。
- `upload_frontend`：布尔值，默认 `false`；Release 传 `true`。

它包含以下边界清晰的 job：

- `backend`：固定 Rust 1.95，执行 `cargo fmt --check`、`cargo clippy --all-targets --all-features --locked -- -D warnings`、`cargo test --test main --locked`。
- `frontend`：固定 Node 24 和 pnpm 11.7.0，执行 frozen install、`format:check`、`build`。`build` 已包含 `vue-tsc`。
- `workflow-lint`：对 `.github/workflows/*.yml` 执行 actionlint。

Release 请求 artifact 时，`frontend` job 上传由同一 SHA 产生的 `frontend-dist`。Release 不再维护第二套前端构建 job。

`backend`、`frontend`、`workflow-lint` 的 timeout 分别为 30、15、5 分钟。Rust 测试使用固定 digest 的 PostgreSQL 18 和 Redis 8 service container，并显式传递测试连接串。actionlint 使用固定版本和 digest，不在运行时下载浮动的 latest 二进制。

### 4.2 `.github/workflows/_container.yml`

`_container.yml` 只通过 `workflow_call` 调用，接收必填的完整提交 SHA，job timeout 为 45 分钟。单次调用按以下顺序执行：

1. checkout 精确 SHA；
2. 校验 `docker compose config`；
3. 使用 Buildx、GHA cache 构建一次 `runtime` 并 `load` 到本地 tag；
4. 对该本地镜像生成 SARIF；
5. 对同一镜像执行 HIGH/CRITICAL、`ignore-unfixed` 的表格门禁，命中即失败；
6. 使用同一镜像执行 Compose 烟测；
7. 无论成功或失败都收集容器状态和日志，最后执行 `down -v --remove-orphans`。

SARIF 通过固定 SHA 的 CodeQL upload action 写入 GitHub Security。来自 fork 的 pull request 没有 `security-events: write`，因此只跳过 SARIF 上传，不跳过表格漏洞门禁。

### 4.3 `.github/workflows/ci.yml`

CI 保留所有分支的 `push` 与 `pull_request`。两种事件统一使用 head repository 与 branch name 组成 concurrency group，并启用 `cancel-in-progress`，使同一 feature 分支的 push 和 PR 不同时跑两套完整门禁。

入口统一计算 `source_sha`：push 使用 `github.sha`，pull request 使用 `pull_request.head.sha`。质量门禁和容器门禁必须 checkout 同一个值。

`changes` job 的 timeout 为 5 分钟，使用固定 SHA 的 paths-filter，仅决定是否调用 `_container.yml`。镜像相关路径固定为：

- `backend/**`
- `frontend/**`
- `deploy/**`
- `.dockerignore`
- `release/version.yaml`
- `.github/workflows/_container.yml`
- `.github/workflows/ci.yml`

`_quality.yml` 始终执行；不能因只改文档而跳过格式、编译和测试。

### 4.4 `.github/workflows/security-scan.yml`

push 和 pull request 只执行 Cargo 与 pnpm 依赖审计，不再构建镜像。周一定时任务执行两类依赖审计，并额外调用 `_container.yml` 扫描当前默认分支的精确 SHA。

push 与 pull request 使用和 CI 相同的 head repository、branch name concurrency 策略，避免同一 feature 分支同时执行两套依赖审计。

依赖审计保留 HIGH 级别门禁，backend、frontend timeout 分别为 20、15 分钟。`cargo-audit` 使用显式版本和 `--locked` 安装，不为节省几十秒新增不必要的 action。

### 4.5 权限边界

入口 workflow 默认只有 `contents: read`。仅以下 job 提升权限：

- SARIF 上传需要 `security-events: write`，fork pull request 不执行上传；
- Release 平台镜像 job 需要 `packages: write` 与 `id-token: write`；
- Release publish job 需要 `contents: write`、`packages: write`、`id-token: write`、`attestations: write`。

任何执行 pull request 中不可信脚本的 job 都不能获得 package、content 或 attestation 写权限。

## 5. 运行时密码与配置

### 5.1 单一配置来源

`AppConfig` 保持当前 YAML 结构，新增三个可选的运行时环境变量：

- `CPR_ADMIN_DEFAULT_PASSWORD`
- `CPR_POSTGRES_PASSWORD`
- `CPR_REDIS_PASSWORD`

加载顺序固定为：

1. 严格解析完整 YAML；
2. 读取三个可选环境变量；
3. 直接覆盖 `admin.default_password`，使用 `url::Url` 仅替换 `database.url`、`redis.url` 的 password 部分；
4. 返回最终 `AppConfig`。

禁止用字符串拼接或替换 URL。用户名、scheme、host、port、path、query 均保持 YAML 原值。密码按原始字节内容交给 URL API 编码，不 trim；空值视为无效配置。URL 解析失败、URL 不支持密码或环境变量为空时，返回明确但不包含密码内容的配置错误。

`AppConfig::load()` 与 `AppConfig::load_from_dir()` 使用同一加载路径，不允许产生两套行为。相关测试全部放在 `backend/tests/bootstrap/config/`，环境变量场景使用隔离子进程，禁止在 `backend/src` 写测试代码。

### 5.2 Compose 传递

Compose 对数据库服务与应用服务使用完全相同的展开值：

- 管理员初始密码来自必填的 `CPR_ADMIN_DEFAULT_PASSWORD`；
- PostgreSQL 的 `POSTGRES_PASSWORD` 与应用的 `CPR_POSTGRES_PASSWORD` 都来自必填的 `CPR_POSTGRES_PASSWORD`；
- Redis 的 `--requirepass`、healthcheck 与应用的 `CPR_REDIS_PASSWORD` 都来自必填的 `CPR_REDIS_PASSWORD`。

`deploy/.env` 定义为 Docker 部署唯一的密钥配置文件，必须被 Git 忽略且 mode 为 `0600`。三项变量任一缺失或为空时，Compose 在创建容器前失败。仓库提供仅含空值的 `deploy/.env.example`，不再人工编辑 YAML 密码；环境变量在加载后覆盖管理员密码和两个 URL 的 password。

### 5.3 bind mount 权限

部署文档必须给出 Linux 首次初始化命令，并明确以下权限：

- `.runtime/data`、`.runtime/logs` 归宿主部署用户所有，group 为容器 GID 10001，mode 为 `0770`；宿主文件共享进程可访问，容器用户可通过组权限写入，其他用户无权限；
- `.runtime/config.yaml` 对容器 GID 10001 可读，对其他用户不可读；
- 修改配置后保持原 owner、group 和 mode。

这不是文档提示项，而是应用以非 root 用户启动的前置条件；Compose 烟测必须从新建目录验证该条件。

## 6. Compose 烟测

烟测使用临时工作目录、专用 Compose project 和 `deploy/docker-compose.smoke.yml` override，不能复用开发机 `.runtime/` 数据。override 移除 PostgreSQL、Redis 的宿主端口，按 project name 隔离应用容器名，并把应用映射到专用烟测端口。配置文件直接复制模板，管理员密码和数据库密码均通过专用的 CI 环境变量注入。

PostgreSQL 与 Redis 密码必须包含 `@`、`:`、`/`、`?`、`#`、`%` 中的多种字符，以证明 URL 覆盖正确编码。执行顺序如下：

1. 创建 owner 为 runner UID、group 为 10001、mode 为 `0770` 的数据和日志目录；配置文件 mode 为 `0640`，owner 为 runner UID，group 为 10001；
2. 只启动 PostgreSQL、Redis 并等待健康；
3. 分别写入 PostgreSQL 表探针和 Redis key；
4. 强制重建 PostgreSQL、Redis 服务容器；
5. 验证两个探针仍存在，再删除探针；
6. 以刚构建的本地镜像和 `--no-build` 启动应用；
7. 验证 `/healthz` 返回 HTTP 204；
8. 验证容器配置和进程实际 UID/GID 均为 `10001:10001`；
9. 验证 `/app/data`、`/app/logs` 可写；
10. 按日志级别扫描三个容器完整日志，不允许出现 WARN、WARNING、ERROR、FATAL、PANIC；应用检查结构化 `level`，PostgreSQL 检查 severity 前缀，Redis 检查 `#` warning marker 与独立严重度前缀，字段名中的 `ops_error_logs`、`bf-error-rate` 等普通文本不构成失败；
11. 向应用发送 SIGTERM，验证 30 秒内退出且退出码为 0。

失败时必须上传 Compose config、`docker compose ps -a`、inspect 结果和三个容器日志。清理步骤使用 `if: always()`，不得因前序失败遗留容器或命名卷。

## 7. Release 设计

### 7.1 精确 SHA 质量门禁

`release-info` 继续校验 tag 与 `release/version.yaml` 一致，并输出 tag 指向的完整 `git_sha`。新增 `quality` reusable job，以该 SHA 调用 `_quality.yml` 且上传 `frontend-dist`。

所有二进制、release asset 和镜像 job 都依赖 `quality`。任一格式、警告、测试、前端构建或 workflow 检查失败，Release 在产生架构暂存镜像或版本级发布产物前终止。禁止查询历史 check-runs 代替该门禁。

### 7.2 精确发布镜像扫描

每个平台的 `build-and-push` 输出 digest 必须被实际使用。平台 job 按以下顺序执行：

1. 从同一 release SHA 的二进制和 `frontend-dist` 组装 `runtime-prebuilt`；
2. 推送架构级 `sha-<git_sha>-<arch>` 暂存 tag；
3. 使用 `registry/image@sha256:...` 扫描 Buildx 返回的精确 digest；
4. 仅扫描通过的架构级 tag 可进入 manifest 汇总。

漏洞门禁失败时允许留下不可变 SHA 暂存 tag，但不得创建版本 manifest、`latest`、签名、attestation 或 GitHub Release。

### 7.3 稳定版与预发布版

`release-info` 增加明确的 `prerelease` 布尔输出。标签规则如下：

| 发布类型 | 多架构 tag | GitHub Release |
| --- | --- | --- |
| 稳定版 | `<version>`、`sha-<git_sha>`、`latest` | `prerelease=false`，设为 latest |
| 预发布版 | `<version>`、`sha-<git_sha>` | `prerelease=true`，不得设为 latest |

Cosign 只签名本次实际创建的 tag。预发布版本不得在 manifest、签名参数或 Release metadata 中引用 `latest`。

### 7.4 发布清理

- `release-info`、`package-assets`、`publish` timeout 分别为 10、20、20 分钟；二进制矩阵 job 为 90 分钟，平台镜像 job 为 45 分钟。
- 删除重复的 release asset 目录创建。
- Dockerfile 的 `release-asset` target 在 `CPR_VERSION` 为空时立即失败，不能产生双下划线文件名。
- `build-and-push` 的 step id 保留并用于 digest 扫描，不再是死标识。

## 8. 供应链固定与自动更新

### 8.1 不可变输入

以下输入全部固定：

- 所有 `uses:`，包括 `actions/*`，使用完整 40 位 commit SHA，同行注释保留可读版本号；
- `dtolnay/rust-toolchain` 固定 action SHA，并显式设置 `toolchain: '1.95'`，不再依赖 ref 名携带工具链版本；
- Dockerfile syntax frontend、全部 `FROM`、Compose service image、CI service container 和 release build container 使用 `tag@sha256:digest`；
- pnpm 继续固定为 11.7.0，Rust 继续固定为 1.95。

禁止在安全修复中把可移动 tag 换成另一个可移动 tag。

### 8.2 Dependabot

新增 `.github/dependabot.yml`，每周检查：

- `/` 的 `github-actions`；
- `/backend` 的 Cargo；
- `/frontend` 的 npm/pnpm；
- `/deploy` 的 Docker。

同生态系统的小版本和补丁更新分组，减少 PR 噪声。Dependabot 更新后仍必须经过相同质量、漏洞和烟测门禁。

## 9. 验证矩阵

实现完成后必须按顺序验证，发现问题先修复再继续：

1. `git diff --check`，并扫描 `TBD`、`TODO`、未固定 action tag、未固定容器 tag；
2. `cargo fmt --manifest-path backend/Cargo.toml -- --check`；
3. `cargo clippy --manifest-path backend/Cargo.toml --all-targets --all-features --locked -- -D warnings`；
4. 使用带密码 PostgreSQL、Redis 执行 `cargo test --manifest-path backend/Cargo.toml --test main --locked`；
5. `pnpm --dir frontend install --frozen-lockfile`、`format:check`、`build`、生产依赖 audit；
6. Cargo audit；
7. actionlint 与 YAML 解析；
8. 使用特殊字符密码执行 `docker compose config`；
9. 构建 `runtime`，对同一镜像执行 Trivy HIGH/CRITICAL 门禁；
10. 执行完整 Compose 烟测、持久化探针、日志扫描和 SIGTERM 验证；
11. 构建 `release-asset`，分别验证缺少版本时失败、提供版本时文件名正确；
12. 静态验证稳定版和预发布版的 tag、签名和 GitHub Release 条件没有交叉污染；
13. 扫描 `backend/src`，确认没有新增 `#[test]` 或 `#[cfg(test)]`。

本地验证通过后再推送，由远端 CI 证明 reusable workflow 权限、SARIF、GHA cache、artifact 和 Release job 依赖在 GitHub Actions 环境中同样成立。

## 10. 完成定义

只有满足以下全部条件，本轮改造才算完成：

- 普通 CI 不再重复构建同一源码镜像；
- Release 的质量门禁和发布镜像扫描针对同一个精确 SHA；
- 默认版与预发布版标签行为正确；
- 密码单点配置和特殊字符密码真实启动通过；
- PostgreSQL、Redis 重建后数据仍存在；
- 应用以非 root 身份健康启动并优雅退出；
- Rust、前端、workflow、漏洞扫描和运行日志均达到本文定义的 0 错误、0 警告；
- 所有测试代码位于 `backend/tests`，没有兼容路径和重复实现。

## 11. 实施验证结果

2026-07-11 本地验证结果：

- `git diff --check`、Rust fmt、全目标全特性 clippy `-D warnings`、actionlint 与 YAML 解析通过；
- 后端使用真实 PostgreSQL、Redis 完成 `634 passed, 0 failed`；前端格式、类型检查、构建与生产依赖审计通过；Cargo audit 无漏洞；
- 空的 `deploy/.env.example` 会在 Compose 插值阶段失败，三项特殊字符密码能够完整传递；
- `runtime` 镜像构建成功，镜像 ID 为 `sha256:bc6c2ae76be06e2ac7b64b7629413c6f3c8a214100175d3fcc416ff578b7af24`，Trivy HIGH/CRITICAL 为 0；
- 独立 Compose 烟测得到 `health=204`、UID/GID `10001:10001`、PostgreSQL/Redis 持久化通过、退出码 0、日志无警告和错误；
- 本机部署已迁移到 mode `0600` 的 `deploy/.env`，YAML 管理员密码为空；只替换应用容器，PostgreSQL、Redis 容器和数据未变。

远端 reusable workflow 权限、SARIF、GHA cache、artifact 与 Release job 依赖仍须在本次提交推送后由 GitHub Actions 证明。
