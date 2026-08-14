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
- OpenAI 数据面是透明代理：请求 body 逐字段（含未知字段与顺序）原样透传，HTTP SSE 与 WebSocket 的上游业务事件逐字节原样转发；xAI 是 Grok wire 与 Responses wire 的翻译层，向客户端转发结构化上游错误 message/code/type，message 先擦除 UUID 账号指纹。
- 换号只由 Core 在下游 commit 前按重放安全性决定。
- fallback 只允许同一 Provider kind 内的账号；不跨 Provider，也不存在 Provider Instance 层。
- continuation 升级链由 Core 冻结：native（原生 handle 绑定原账号）→ replay owner（原账号完整 transcript 重放）→ replay any（transcript 已可携带，允许换账号）；两个 Provider 都实现全链，xAI 的 transcript 由不透明 session state 承载，跨账号重放前做脱敏投影。
- Provider wire profile 以配置为启动基线，并由共享运行时状态统一发布。OpenAI Desktop 读取官方 appcast，并从同一 ZIP 的内嵌 Core 做有界 Range 探测；xAI CLI 读取 `@xai-official/grok`。发现新版本后自动更新对应版本字段，所有消费边界不得维护独立常量。
- `downstream_committed_at` 是不可撤回交付承诺，不是首字节已经写达的证明。

## 存储

- PostgreSQL 业务表只有 `0001_initial.sql` 定义的七张；该文件直接表达当前大版本的完整新库基线，
  包括 bytes response ID、响应观测 service tier 与额度耗尽冷却约束。
- 同一大版本内已应用迁移按字节冻结：`backend/migrations/.frozen-sha256` 是冻结清单，CI 相对 PR base 做 append-only 校验；schema 变更一律新增编号迁移，规则见 `backend/migrations/README.md`。
- `config_revision` 只用于会改变调度快照或安全配置的管理 mutation。
- quota、cooldown、catalog generation、自动 refresh 不推进全局 revision。
- refresh 只推进账号 `credential_revision`；Redis cooldown 是可丢失热缓存。
- OAuth pending 使用 Provider 域隔离 SHA-256 key；基础 Hash 保存 owner、过期时间和 Provider payload，
  complete 先原子 claim，失败释放 claim，账号事务提交后才消费。
- 数据面 PostgreSQL execution observation 与 Redis admission release/circuit feedback 使用有界 OpsFlush
  队列；队列可丢失、可观测且不是权威状态，不得让 Store 失败改写客户端协议结果。native
  continuation 直接写 Redis，并限制每次记录的索引清理量。
- 真实 secret 不进入日志、Debug、fixture、文档或 audit details；默认测试使用合成值，外部 access-token
  签名检查是唯一显式 opt-in 例外。

## 前端

- Vue 3 Composition API 与 `<script setup lang="ts">`。
- API 位于 `frontend/src/api`，页面状态留在对应 view/composable。
- 复用现有基础组件和主题 token，保持紧凑低噪声。

## 验证

```bash
cd backend
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --test main --locked
cd ..
pnpm --dir frontend format:check
pnpm --dir frontend build
docker compose -f deploy/compose.yaml config --quiet
```

每个 package 的集成测试统一挂在单一 `main` 测试目标下（`autotests = false`）。
`gateway-store` 的 PostgreSQL/Redis 集成测试需要 `CPR_TEST_DATABASE_URL`、
`CPR_TEST_REDIS_URL`；本地未设置时静默跳过，CI 缺失则直接失败。

自动化测试不发送真实对话，也不轮换生产 refresh token。OpenAI identity 有一个可选的
`CODEX_REAL_ACCOUNT_FILE` 外部文件签名检查，只验证官方 JWKS，不经过 selector 或数据面；未设置时
直接跳过。线上链路验收使用隔离的操作员请求，并按 request ID 对照服务日志和观测记录。
