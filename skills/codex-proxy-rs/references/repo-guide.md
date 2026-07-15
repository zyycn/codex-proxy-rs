# Codex Proxy RS 项目指南

## 权威来源

- 行为：当前源码、配置和测试。
- 架构：`docs/architecture.md`。
- 部署：`deploy/config.example.yaml`、`deploy/compose.yaml`、`deploy/README.md`。
- 发布：`release/version.yaml`、`release/platforms.yaml`、`.github/workflows/release.yml`。

本文件只记录高频开发入口，不复制完整架构。

## 结构

| 路径                          | 责任                                          |
| ----------------------------- | --------------------------------------------- |
| `backend/src/api`             | HTTP/WebSocket 契约、鉴权、响应编码、静态资源 |
| `backend/src/dispatch`        | `v1/*` Request → Attempt → Stream 编排        |
| `backend/src/fleet`           | 账号、调度、quota、token、Cookie、管理操作    |
| `backend/src/upstream/openai` | Codex 协议、HTTP/2、WebSocket、指纹           |
| `backend/src/telemetry`       | usage、错误事实、聚合和查询                   |
| `backend/src/infra`           | PostgreSQL、Redis、日志、路径、身份           |
| `backend/src/bootstrap`       | 配置、装配、后台任务、关闭                    |
| `backend/tests`               | 集成、契约、fixture；生产源码内禁止测试       |
| `frontend`                    | Vue 管理端                                    |
| `deploy`                      | 唯一 Compose 与配置模板                       |
| `docs/architecture.md`        | 唯一长期架构文档                              |
| `release`                     | 版本与平台元数据                              |

仓库根目录没有 Cargo manifest。后端命令使用
`--manifest-path backend/Cargo.toml`，或先进入 `backend/`。

## 后端所有权

- `api` 只做入站/出站适配，不写业务规则。
- `dispatch/lifecycle` 只编排顺序、retry、commit 和 finalize。
- `dispatch/controllers` 中每个功能只能有一个 owner，controller 不互调。
- `fleet/account_failure.rs` 统一解释跨入口账号失败和状态 effect。
- `upstream/openai/failure.rs` 只生成 typed failure facts。
- `upstream/openai/transport` 只建立连接、收发协议和产出 transport metrics。
- `telemetry` 保存已经确定的事实，不参与调度。
- `bootstrap` 是 composition root，不解释业务错误。

提交边界后不得自动换 transport、账号或重放 payload。账号身份按 attempt 重建；客户端
session/thread/turn 等拓扑字段原样透传。

## Responses 传输

- 热 WS 立即复用。
- 冷 WS 前台预算为 800ms；超时后当前请求使用同账号 HTTP/2。
- 未完成的 WS 只在后台继续握手，不发送原 payload；成功后进入 Idle。
- `Connecting` 按 key 单飞并沿用原绝对 deadline。
- 账号驱逐和 shutdown 取消 opening；迟到连接不能重新入池。
- warmup 与 connection-local continuation 必须使用精确 WS。
- breaker 按 origin/TLS profile 统计退化 opening，同一次 opening 只计一次。

完整 typestate、pool、breaker 和指标契约见 `docs/architecture.md`。

## 存储

| 位置            | 数据                                                   |
| --------------- | ------------------------------------------------------ |
| PostgreSQL      | 管理员、Key、账号、quota、设置、Cookie、指纹、遥测事实 |
| Redis           | 管理会话、刷新租约、模型快照、响应归属、短期会话状态   |
| `.runtime/data` | 身份 HMAC secret、更新状态和锁                         |
| `.runtime/logs` | 结构化轮转日志                                         |

规则：

- migration 只能新增；禁止修改已应用 SQL。
- Redis 业务键使用 `cpr:` 前缀和明确 TTL。
- `identity_hmac_secret` 必须随部署保留。
- 遥测或 affinity 写入失败不能改变已取得的代理响应。
- 账号明确失效先更新内存并驱逐 WS，再异步持久化。

## 前端

- Vue 3 Composition API、`<script setup lang="ts">`。
- API：`frontend/src/api/modules`。
- 基础组件：`frontend/src/components/base`。
- 页面状态和副作用：对应 `views/*/composables`。
- 使用现有主题 token、Tailwind utilities 和 Lucide。
- 独立功能使用独立 loading；旧请求不得覆盖较新响应。
- 保持紧凑、低噪声的运维界面。

## 运行与部署

- 配置入口只有 `deploy/config.yaml`；项目不使用 `.env`。
- Compose 运行 PostgreSQL、Redis 和非 root 应用容器 `10001:10001`。
- 持久状态全部位于 `.runtime/`。
- 应用默认绑定 `127.0.0.1:8080`；公网使用 HTTPS 反向代理。
- Docker runtime 只包含后端二进制、前端产物和运行依赖。

不要输出 `docker compose config`、`docker inspect` 或真实连接 URL 中的密码。

## 验证

```bash
cargo +1.97.0 fmt --manifest-path backend/Cargo.toml -- --check
cargo +1.97.0 clippy --manifest-path backend/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo +1.97.0 test --manifest-path backend/Cargo.toml --locked
pnpm --dir frontend format:check
pnpm --dir frontend build
docker compose -f deploy/compose.yaml config --quiet
```

聚焦改动先运行对应模块。跨领域、存储、生命周期或发布改动执行完整门禁。

## 发布

1. 更新 `release/version.yaml`。
2. 提交版本变更。
3. 创建同版本带注释 `vX.Y.Z` tag。
4. 原子推送 `main` 与 tag。
5. 等待 Release workflow 完成。
6. 核对远端 `main`、tag target、GitHub Release、assets 和 GHCR tags。

Release 包含 Linux amd64/arm64、macOS arm64、Windows amd64；稳定版本更新 GHCR
`latest`。在线更新必须验证 checksum，并保留跨文件系统 copy-and-remove fallback。
