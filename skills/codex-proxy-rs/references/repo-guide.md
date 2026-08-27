# Codex Proxy RS 项目指南

这是面向仓库修改者的导航，不复制完整 API 或架构说明。行为以当前源码和测试为准；长期边界见
`docs/architecture.md`。

## 权威入口

| 主题 | 入口 |
| --- | --- |
| 顶层装配与启动顺序 | `backend/apps/gateway/src/bootstrap.rs` |
| 客户端路由 | `backend/crates/gateway-api/src/openai/router.rs` |
| 管理路由 | `backend/crates/gateway-api/src/admin/` |
| 请求生命周期 | `backend/crates/gateway-core/src/engine/` |
| 快照与路由计划 | `backend/crates/gateway-core/src/routing/snapshot.rs` |
| OpenAI Provider | `backend/crates/providers/openai/src/` |
| xAI Provider | `backend/crates/providers/xai/src/` |
| PostgreSQL/Redis adapter | `backend/crates/gateway-store/src/` |
| Worker 贡献与监督 | 各 Bundle 的 `worker_contributions`、`gateway-host/src/workers/` |
| Vue 管理端 | `frontend/src/views/`、`frontend/src/components/base/` |
| 部署与发布 | `deploy/`、`release/` |

存在 `.codegraph/` 时，先查询符号和调用链；CodeGraph 未覆盖的目标再用 `rg` 和定点读取补齐。

## Package owner

| Package | 拥有的事实 | 不应拥有 |
| --- | --- | --- |
| `gateway-protocol` | 共享 wire/canonical contract | 数据库、Provider、业务策略 |
| `gateway-core` | operation、routing、attempt、commit/send、accounting | HTTP DTO、SQL、具体上游协议 |
| `gateway-admin` | 管理命令、用例、审计、备份策略、抽象端口 | Axum、具体数据库实现 |
| `gateway-api` | Axum route、鉴权解码、wire presenter、响应交付 | SQL、具体 Provider 分支 |
| `gateway-store` | PostgreSQL、Redis、S3、`pg_dump` adapter | 路由/重试/Provider 业务规则 |
| `gateway-host` | 配置、日志、serve/drain、worker、update | 数据面路由与 Provider 逻辑 |
| `provider-openai` | OpenAI credential/quota/catalog/transport | Client Key admission、跨 Provider fallback |
| `provider-xai` | xAI credential/quota/catalog/transport 与协议转换 | Client Key admission、跨 Provider fallback |
| `apps/gateway` | 具体实现装配 | 业务状态机 |

仓库根目录没有 Cargo manifest。后端 workspace 是 `backend/Cargo.toml`，Rust 版本为 1.97，edition 2024。

## 数据面入口

公开路由由 `gateway-api/src/openai/router.rs` 固定：

- Responses：`POST /v1/responses`、WebSocket `GET /v1/responses`、`POST /v1/responses/review`；
- Images：`POST /v1/images/generations`、`POST /v1/images/edits`；
- 模型：`/v1/models`、catalog、info、detail。

Responses adapter 解析参与路由的最少语义，并保留原始请求；Images adapter 不解析模型或重建 JSON，
直接创建 `GenerateImage` operation。两者都进入同一 Core execution lifecycle。

定位请求问题时按以下顺序追踪：

```text
gateway-api adapter
  -> ExecutionService
  -> RuntimeSnapshot::plan / plan_provider_endpoint
  -> AttemptCoordinator
  -> Provider::execute
  -> API delivery
  -> asynchronous observations
```

重点边界：

- `UpstreamSendState` 区分 `not_sent/sent/ambiguous`；
- downstream commit 后不能重试；
- Provider execute 一次只返回一个账号的一条冷流；
- account retry 与跨 Provider 候选推进由 Core 管理；
- response ID 和原始 wire 不因观测模型而重写。

OpenAI 保持 Responses/SSE/WS/Images 业务字节透明；xAI 在自己的 Provider 内完成 Grok/Responses 转换。
Images 固定调用 OpenAI Provider 自有端点，候选不带 `upstream_model`。

## 路由与 continuation

`RuntimeSnapshot` 聚合运行设置、账号目录、分组 membership、Client Key scope、Provider 模型目录和
`config_revision`。Client Key 零分组表示全部账号，非空分组表示已启用成员并集；空池 fail closed。

文本模型先做全局精确映射，再在 scope 内按 Provider 能力生成候选。普通 retry 需要同时满足发送状态、
重放安全、attempt 预算和未 commit；传输不明确不自动重放。

OpenAI session affinity 是粘性提示；native continuation pin 才固定 Client Key、scope、Provider 与账号。
OpenAI continuation 按 native/replay-owner/replay-any 推进，xAI 使用客户端提交的完整历史。

## 账号控制面

账号路由和 DTO 在 `gateway-api/src/admin/accounts.rs`，业务编排在
`gateway-admin/src/use_case/accounts.rs`，Provider 解析/验证后由 Store 事务提交。

### 导入与授权

- 公共导入合同始终是 `{ provider, data }`，其中 `data` 为 Provider-owned JSON object。
- OpenAI 支持 OAuth JSON、`accounts` 数组、AT-only、RT-only 和 AT+RT+可选 ID Token；
  最多 200 项。RT-only 先交换 AT，AT-only 不会凭空获得刷新能力。
- OpenAI OAuth 身份来自 `parse_chatgpt_jwt_claims`，不信任导入 object 顶层的 user/account ID。
- 前端 AT/RT 标签只把“每行一个 token”转换成 `accounts` JSON，没有第二条导入 API。
- xAI 接受 OAuth 账号 object/数组，逐项验证；API Key 不是 credential。
- 新账号未分组；匹配既有上游身份的导入/重新授权保留调度事实。

### Credential、quota 与恢复

- credential refresh、quota refresh、正常请求的 rate-limit 观测是独立链路。
- 账号五态由 credential/quota + Redis cooldown 统一投影，不保存重复 `status`。
- `/accounts/recover` 是管理员清除本地错误、quota 和 cooldown 的强制动作，不证明上游已经恢复。
- 展开区用量按代表性额度窗口 `[reset_at - window_seconds, reset_at)` 聚合，不回退到全历史累计。

### 主动重置卡

- `GET/POST /api/admin/accounts/reset-credits` 只对 OpenAI 有效。
- 列表和消费直接访问上游；不建 PostgreSQL/Redis 卡库存。
- POST 使用 canonical UUIDv4 `redeemRequestId`，账号级串行；401 credential refresh 后复用同一命令。
- 发送后结果不明确时，调用方只能复用原键重试；成功后另行刷新卡列表和 quota。
- 前端只在弹窗打开/手工刷新时查询，最近结果仅缓存于当前浏览器会话。

## 存储与迁移

PostgreSQL 业务 schema 由 `0001_initial.sql` 建立。`.frozen-sha256` 覆盖全部迁移，
已应用文件不可回改。

主要状态边界：

- PostgreSQL：账号、credential、quota、分组、Client Key、设置、请求、审计、备份；
- Redis：admission、credential/worker lease、cooldown、circuit、session affinity、continuation、OAuth
  pending、catalog/cache 和 runtime change；
- `.runtime/data`：会话身份、更新状态与备份暂存；
- `.runtime/logs`：普通日志和含原始 AT/RT 的独立 OAuth 恢复文件集。

数据面 observation 使用有界 OpsFlush 队列；它可丢失、可观测，但不是第二份业务权威。native
continuation 等正确性依赖的协调写入不经该 best-effort 队列。

## Worker 与生命周期

组合根按 Store → Core → OpenAI → xAI → Admin 收集 worker contributions，Host 统一校验、监督、健康
暴露和关闭：

- Store：stale request recovery、retention、PostgreSQL/Redis flush daemons；
- Core：snapshot reconciliation 和 change subscription；
- OpenAI/xAI：OAuth refresh、quota/catalog 和版本/etag 检查；
- Admin：单个备份 daemon。

应用只支持单副本。周期 lease 是单周期互斥，不是集群 leader election；备份和在线更新同样依赖此边界。
关闭先 drain HTTP/WS，再 join workers。

## 前端定位

- API DTO/请求：`frontend/src/api/modules/`；
- 页面状态：对应 `views/*/composables/`；
- 页面展示转换：对应组件或相邻 presenter/util；
- 通用交互：`components/base/`；
- 主题：`styles/tokens.css` 与既有 `cp-*` token。

不要把页面状态塞进 API module，也不要为单一页面创造第二套通用层。修改共享组件前检查所有调用方；
异步上游动作必须区分 loading、明确失败和结果不确定。

## 验证与发布

```bash
cargo +1.97.0 fmt --all --manifest-path backend/Cargo.toml -- --check
cargo +1.97.0 clippy --manifest-path backend/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo +1.97.0 test --manifest-path backend/Cargo.toml --test main --locked
pnpm --dir frontend format:check
pnpm --dir frontend build
docker compose -f deploy/compose.yaml config --quiet
```

PostgreSQL/Redis 集成测试需要 `CPR_TEST_DATABASE_URL`、`CPR_TEST_REDIS_URL`。发布使用
`release/publish <version>`；不要把本地 commit、GitHub Release 和在线实例 revision 混为同一状态。
