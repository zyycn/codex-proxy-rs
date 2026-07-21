# Codex Proxy RS 项目指南

## 权威来源

- 行为：当前源码和测试。
- 架构与数据边界：`docs/architecture.md`。
- 部署：`deploy/config.example.yaml`、`deploy/compose.yaml`、`deploy/README.md`。
- 发布：`release/version.yaml`、`release/platforms.yaml`、Release workflow。

## 结构与 owner

| 路径 | 责任 |
| --- | --- |
| `backend/apps/gateway` | composition root、server、worker 启动 |
| `backend/crates/gateway-core` | operation、routing、attempt coordinator、policy、accounting |
| `backend/crates/gateway-protocol` | wire contract、canonical event |
| `backend/crates/gateway-admin` | 管理领域、用例和抽象端口 |
| `backend/crates/gateway-store` | PostgreSQL/Redis adapter |
| `backend/crates/gateway-api` | Responses/Admin HTTP adapter |
| `backend/crates/gateway-host` | host、update、system 能力 |
| `backend/crates/providers/openai` | OpenAI OAuth、catalog、transport |
| `backend/crates/providers/xai` | xAI/Grok OAuth session、catalog、transport |
| `frontend` | Vue 管理端 |
| `deploy` | Compose、镜像与配置模板 |
| `docs/architecture.md` | 唯一长期架构文档 |

仓库根目录没有 Cargo manifest；后端命令进入 `backend/` 或传入 `--manifest-path backend/Cargo.toml`。

## 执行与 Provider 边界

- `gateway-api` 不写具体 Provider 分支。
- Core 冻结 RuntimeSnapshot、RoutePlan、retry/fallback 与 downstream commit 边界。
- Provider 每次 `execute` 只选择一个 credential 并返回 cold canonical stream。
- 换号只由 Core 在下游 commit 前按重放安全性决定。
- fallback 只允许同 instance 账号和同 Provider kind instance；不跨 Provider kind。
- OpenAI continuation 为 native → replay owner → replay any；xAI 使用客户端完整历史。
- Provider wire profile 以配置为启动基线，并由共享运行时状态统一发布。OpenAI CLI 读取 `@openai/codex`、Desktop 读取官方 appcast、xAI CLI 读取 `@xai-official/grok`；发现新版本后自动更新对应版本字段，所有消费边界不得维护独立常量。
- `downstream_committed_at` 是不可撤回交付承诺，不是首字节已经写达的证明。

## 存储

- PostgreSQL 只有 `0001_initial.sql` 定义的八张终态业务表。
- `config_revision` 只用于会改变调度快照或安全配置的管理 mutation。
- quota、cooldown、catalog generation、自动 refresh 不推进全局 revision。
- refresh 只推进账号 `credential_revision`；Redis cooldown 是可丢失热缓存。
- OAuth pending 使用 Provider 域隔离 SHA-256 key、固定三字段 Hash 和原子一次消费。
- secret 不进入日志、Debug、fixture、文档或 audit details。

## 前端

- Vue 3 Composition API 与 `<script setup lang="ts">`。
- API 位于 `frontend/src/api`，页面状态留在对应 view/composable。
- 复用现有基础组件和主题 token，保持紧凑低噪声。

## 验证

```bash
cargo +1.97.0 fmt --manifest-path backend/Cargo.toml -- --check
cargo +1.97.0 clippy --manifest-path backend/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo +1.97.0 test --manifest-path backend/Cargo.toml --workspace --all-targets --locked
pnpm --dir frontend format:check
pnpm --dir frontend build
docker compose -f deploy/compose.yaml config --quiet
```

真实账号测试只从仓库外路径读取，不打印或复制 credential。

真实数据面验证使用两个显式 ignored 测试：

- `admin::real_openai_conversation_crosses_production_provider_boundaries`
- `admin::real_xai_conversation_crosses_production_provider_boundaries`

两者覆盖真实 catalog、selector、生产 SSE transport、canonical event、usage 和 completed；xAI 测试还要求 `XAI_ALLOW_DESTRUCTIVE_FIXTURE_REFRESH=1`，因为验证过程可能轮换 refresh token。
