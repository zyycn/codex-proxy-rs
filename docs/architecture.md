# Codex Proxy RS 架构

本文是仓库唯一架构文档，只描述当前架构。

## 1. 系统边界

Codex Proxy RS 是单进程、单副本部署的多 Provider AI 网关：

- 客户端面提供 OpenAI Responses 兼容的 JSON、SSE、WebSocket 与模型目录协议。
- 管理面提供 `/api/admin/*` 和 Vue 静态管理端。
- 当前 Provider 为 OpenAI 与 xAI；账号 credential 均由所属 Provider 独占解释。
- OpenAI 行为以重构前正式实现为语义基准；xAI/Grok 行为以 `grok2api` 验证结果为基准。
- 两个 Provider 的请求画像都由启动配置提供基线，并通过进程级共享状态向每次新请求发布一致快照。OpenAI CLI、OpenAI Desktop 与 xAI CLI 的官方版本检查会分别原子更新自己负责的版本字段。
- OpenAI CLI 使用官方 npm 包 `@openai/codex`，OpenAI Desktop 使用官方 appcast，xAI CLI 使用官方 npm 包 `@xai-official/grok`。检查失败保留上一份成功画像；检查成功后 OAuth、catalog、quota/billing、inference 与 Dashboard 自动采用最新版本。
- PostgreSQL 是业务与配置事实的唯一权威存储。
- Redis 只保存可丢失、可重建或有自然过期时间的协调状态。

部署假定：单副本。带 lease 的周期任务只做单周期互斥而非跨周期 leader 选举，N 副本会让同一任务最坏以 N 倍频率执行（见第 10 节）；自更新只替换并重启处理该请求的那个副本，其余副本停留在旧版本。不要以多副本方式运行本网关。

Gateway Engine 不识别具体 Provider 协议；Provider 不拥有客户端 admission、跨 Provider 路由或业务重试预算。

## 2. Workspace 与依赖方向

```text
backend/
├── apps/gateway/               composition root（Bundle 装配）
├── crates/gateway-core/        operation、routing、engine、policy、accounting
├── crates/gateway-protocol/    可共享 wire contract 与 canonical event
├── crates/gateway-admin/       管理领域、用例与抽象端口
├── crates/gateway-store/       PostgreSQL、Redis adapter
├── crates/gateway-api/         OpenAI Responses 与 Admin HTTP adapter
├── crates/gateway-host/        配置与日志、HTTP serve/drain、worker 运行时、自更新
├── crates/providers/openai/    OpenAI credential、catalog、transport
├── crates/providers/xai/       xAI/Grok credential、catalog、transport
└── migrations/                 编号迁移与冻结清单 .frozen-sha256
```

依赖规则：

- `gateway-core` 不依赖 HTTP、数据库、Redis 或具体 Provider。
- `gateway-protocol` 不依赖其他 workspace crate。
- Provider crate 之间禁止互相依赖。
- `gateway-api` 只面向 Admin/Core/Protocol 抽象，不导入具体 Provider。
- `gateway-store` 实现 Admin/Core 端口，但不拥有业务策略。
- 只有 `apps/gateway` 组合具体实现。

依赖 DAG 主要由上述约定与评审维护，机器校验只覆盖两处：`gateway-api` 的 architecture 测试冻结自身源码树与测试树，并禁止 manifest 出现 store/Provider/基础设施依赖（禁止清单不含 `gateway-host`）；`apps/gateway` 的 architecture 测试冻结应用源码树、禁止生产测试挂载并约束 bootstrap 只做装配。其余 crate 没有 architecture 测试。

## 3. 请求执行链

```text
API decode/auth
  -> freeze RuntimeSnapshot
  -> compile RoutePlan
  -> admission
  -> persist model_request
  -> select Provider/account
  -> persist attempt facts in model_request
  -> cross upstream send barrier
  -> canonical stream
  -> persist downstream delivery barrier
  -> terminal observation and accounting
```

核心不变量：

1. 每个客户端请求只有一条 `model_requests`；attempt 明细保存在其 JSON 事实中，不另建 attempt 表。
2. 任何可能到达上游的调用都必须先有持久请求事实。
3. `not_sent`、`sent`、`ambiguous` 是不同的上游发送边界；发送状态是请求级单调水位（`sent` > `ambiguous` > `not_sent`），跨 attempt 只升不降，终态写回不得低于水位；`ambiguous` 不自动重放。
4. `downstream_committed_at` 表示网关已作出不可撤回的交付承诺，从该时刻禁止 retry/fallback；它在实际首字节写出前持久化，不宣称字节已经到达客户端。
5. Provider 每次 `execute` 只能选择一个 credential 并准备一个 cold stream，不得隐藏换号或业务 retry。
6. 下游 commit 前，Core 可按冻结策略处理同一 Provider 内的账号 fallback；禁止跨 Provider fallback。
7. 本架构不通过隐式连接复用承载业务身份。HTTP client 可安全复用 transport 连接，但账号、credential revision、cookie/session binding 必须显式绑定到本次调用。

## 4. 路由、fallback 与错误处理

RuntimeSnapshot 冻结 Provider 集合、模型能力、运行时策略和全局 `config_revision`。一次请求始终使用同一快照，不在执行中拼接新旧配置。

Fallback 只允许在同一 Provider 内更换可用账号，不跨 Provider。

明确的上游认证、封禁、额度或 cooldown 错误会更新对应账号状态，使下一次选择排除该账号；满足重放安全条件且尚未 downstream commit 时，可以换号。传输结果不明确时不得假定请求未到达上游。

重试矩阵属于独立策略，OpenAI 保持原正式行为、xAI 保持参考行为，不在 Provider transport 内增加额外重试。

## 5. Continuation

系统只保存已被上游确认的安全外部 ID：`model_requests.upstream_response_id`，不新增 conversation、transcript、continuation 或 claim 表。

- `store=true`：使用 Provider 持久化的 native handle。
- `store=false`：opaque replay state 仅存在于活连接内，不落 PostgreSQL。
- OpenAI continuation 顺序为 native、replay owner、replay any。
- xAI 使用客户端提交的完整历史作为已验证 continuation 路径。
- continuation 失败仍受 send barrier、downstream commit 和 Provider kind 边界约束。

## 6. Provider 与 credential owner

`Provider` 接收 canonical `Operation + ProviderCandidate + AttemptContext`，返回携带冻结 metadata 的 canonical cold stream。Registry 使用稳定 `ProviderKind` 查找实现。

Provider 独占以下职责：

- credential 文档的编码、解码与校验；
- OAuth 登录、导入、刷新与轮换；
- 账号选择所需的 Provider 私有事实；
- catalog 查询与能力编译；
- HTTP/WebSocket transport 和错误分类；
- quota 投影与 Provider 私有观测字段。

客户端可见正文的立场按 Provider 不同：

- OpenAI 是逐字节透明代理：上游 wire 是客户端可见的事实来源，HTTP SSE 帧以原始字节原样下发，WebSocket 上游 JSON 文本原样下发；canonical facts 只从同一帧旁路解析，用于观测、亲和与计费。仅网关内部帧（rate limits、metadata）被就地消费，不向客户端转发。
- xAI 是翻译层：把 Grok wire 解码后重新合成 Responses wire 事件，不承诺字节透明；上游错误按结构化 message/code/type 透出，message 中的 UUID 账号指纹替换为占位符。

credential 更新使用 `credential_revision` CAS。认证永久失败、封禁、额度耗尽和带截止时间的 cooldown 是账号运行时事实。cooldown 到期后，Core 的有效调度谓词会自然允许账号重新参与选择；Redis cooldown 只是热缓存。成功调用后才可把账号状态重新观测为 `ready`。自动 refresh 的瞬态失败（限流、5xx、超时、畸形响应）保留现有凭据并按指数退避推迟下一次刷新，不把账号标记为失效；只有上游明确的永久错误（`invalid_grant`、封禁）才写入终态失效。

`refresh_token_expires_at` 不是公共 SQL 列或 Core 权威状态。xAI 可在 `provider_credentials_json` 内保存它作为 Provider 私有提示；真正失效以 refresh endpoint 返回的永久错误为准。

## 7. 控制面与内部 revision

Admin API 不要求客户端提交配置 revision，也不向客户端暴露配置 revision。会改变调度快照或安全配置的写入，必须在同一 PostgreSQL 事务中：

- 执行业务 mutation；
- 推进 `runtime_settings.config_revision`；
- 写入脱敏 `admin_audit_events`。

推进全局 revision 的 mutation 包括 runtime settings、客户端 Key、账号导入/创建/删除、管理员显式启停和管理员 credential rotation。

不推进全局 revision 的运行时观测包括 quota、cooldown、catalog generation、请求统计以及自动 credential refresh；自动 refresh 只推进 `credential_revision`。提交后 Redis 通知只负责缩短其他副本的收敛延迟，周期性 PostgreSQL 对账才是正确性基础。

## 8. PostgreSQL 终态

`backend/migrations/0001_initial.sql` 创建且只创建七张业务表：

| 表 | 权威事实 |
| --- | --- |
| `admin_users` | 管理员身份与密码摘要 |
| `admin_audit_events` | 管理 mutation 审计 |
| `client_api_keys` | 客户端鉴权、限额与授权范围 |
| `runtime_settings` | 全局配置与 `config_revision` |
| `provider_accounts` | 账号资料、Provider-owned 明文 credential JSON、revision、quota、cooldown |
| `model_requests` | 请求、attempt、计费、交付与恢复事实 |
| `ops_events` | 脱敏运行事件 |

设计规则：

- 一个事实只存一次；可表达关系使用真实 FK 与支持索引。
- Provider 差异只进入受 schema/version 校验的 JSONB 边界。
- credential JSON 以 Provider 自己的 schema 明文保存在 `provider_credentials_json`；日志、Debug、API 和 audit
  不输出其中的 secret，Provider 只在需要发往上游时把值包装为运行时 secret。
- stale recovery 只把超时 `running` 请求收敛为 `incomplete`，不重放业务请求。
- `ops_events` 逐条同步持久化，`occurrence_count=1`；OpsFlush worker 以 Disabled 声明贡献（没有需要刷写的内存缓冲），不存在隐藏内存聚合权威。
- retention 只删除已满足保留规则的历史事实，不改变运行中请求。

业务 schema 由 `backend/migrations/` 下的编号迁移按序定义：`0001_initial.sql` 建表，后续迁移只做增量变更。已合入的迁移永久冻结——`.frozen-sha256` 是冻结清单，CI 校验清单相对 PR base 只增不改，启动时 sqlx 重校验已应用迁移的 checksum，不一致直接拒绝启动。规则见 `backend/migrations/README.md`。

## 9. Redis 终态

Redis 保存的协调状态恰好是：客户端 admission 热状态、credential lease/fencing 与调度信号、账号 cooldown 与 availability 热缓存、catalog 缓存、Provider circuit、会话亲和与会话级账号排除、continuation pin、OAuth pending flow、worker leader lease 以及 runtime change 通知。Redis 丢失后必须从 PostgreSQL 或 Provider 事实恢复；恢复完成前需要保护的 acquire 路径 fail closed。

多副本部署时，跨副本协调面只有上述 Redis 状态与 PostgreSQL 事实；WebSocket 连接池、请求画像与 RuntimeSnapshot 副本都是进程内状态，其中 RuntimeSnapshot 依靠周期对账收敛，请求画像依靠各自的官方版本检查收敛。

OAuth pending key：

```text
codex-proxy-rs:oauth-pending:v1:{provider_kind}:{flow_fingerprint}
flow_fingerprint = SHA-256(provider_kind || 0x00 || raw_flow_binding)
```

Hash 字段恰好为：

- `owner_fingerprint`
- `expires_at_epoch_seconds`
- `provider_payload`

create/take 使用 Lua 原子执行。owner mismatch 不删除记录；匹配 owner 的 take 返回 payload 并一次性删除。OpenAI TTL 为 10 分钟，xAI TTL 为 30 分钟。

## 10. 后台任务与恢复

HTTP serve/drain 与 worker 运行时都在 `gateway-host`：Host 负责注册校验、leader lease、失败退避、健康暴露与关闭；`apps/gateway` 的 bootstrap 只装配。WorkerContribution 由 Store、Core 与两个 Provider bundle 显式贡献，host bundle 自身不贡献任何 worker：

- Store：stale execution recovery 与 retention；OpsFlush 以 Disabled 声明贡献。
- Core：RuntimeSnapshot revision 周期对账（无 lease，逐副本运行）与 Redis change 长驻订阅（daemon，逐副本运行）。
- Provider：credential refresh、quota/catalog 健康，以及模型 etag 与官方版本 release 检查（OpenAI CLI、OpenAI Desktop、xAI CLI）。

带 lease 的周期任务在每个周期开始向 Redis 申请一次 leader lease，周期内续租，周期结束即释放。这只是单周期互斥，不是跨周期 leader 选举：各副本独立计时，N 副本部署下同一任务的实际执行频率最坏可达单副本的 N 倍。

自更新、回滚与进程重启不是 worker：它们是 Admin API 触发的 Host 系统操作，只替换并重启处理该请求的那个副本。

关闭时序：收到取消信号后，HTTP drain（axum 优雅关闭与游离连接等待）共享同一个从信号时刻起算的绝对截止点，逾期放弃等待存量连接；随后 worker 在独立的 shutdown 超时内 join。

worker 不得通过导入 Admin use case 绕过边界，也不得维护第二份业务状态。

## 11. 测试与验收

- 生产 `src` 禁止 `#[cfg(test)]`、`#[path]` 和 `include!` 测试挂载。
- 测试位于各 package 的 `tests/`，目录镜像生产模块。
- Core 规则使用确定性测试；Provider 使用 contract/fixture 测试。
- FK、CAS、revision、recovery 使用真实 PostgreSQL 测试。
- admission、lease、cooldown、OAuth pending 使用真实 Redis 测试。
- 真实账号测试只从仓库外部文件读取，不打印、复制或提交凭证。

真实对话测试是显式 ignored 的生产边界测试，必须依次穿过 Provider 导入/必要刷新、实时 catalog、账号 selector、生产 HTTP/SSE transport、canonical 解码、usage 与 completed 终态。xAI 用例允许 OAuth refresh token 轮换，因此必须额外显式授权：

```bash
CODEX_REAL_ACCOUNT_FIXTURE=/outside/cpr-accounts.json \
  cargo test -p provider-openai --test main \
  admin::real_openai_conversation_crosses_production_provider_boundaries \
  -- --ignored --exact

XAI_REAL_ACCOUNT_FIXTURE=/outside/grok-accounts.json \
XAI_ALLOW_DESTRUCTIVE_FIXTURE_REFRESH=1 \
  cargo test -p provider-xai --test main \
  admin::real_xai_conversation_crosses_production_provider_boundaries \
  -- --ignored --exact
```

终态门禁：

```bash
cd backend
cargo fmt --all -- --check
cargo test --workspace --all-targets
```
