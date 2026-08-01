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
├── crates/gateway-admin/       管理领域、用例、抽象端口与备份 Worker 策略
├── crates/gateway-store/       PostgreSQL、Redis、S3/R2 与 pg_dump adapter
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
- `gateway-store` 实现 Admin/Core 端口与备份基础设施（S3/R2、`pg_dump`、受控暂存），但不拥有业务策略。
- 只有 `apps/gateway` 组合具体实现。

依赖 DAG 主要由上述约定与评审维护，机器校验只覆盖两处：`gateway-api` 的 architecture 测试冻结自身源码树与测试树，并禁止 manifest 出现 store/Provider/基础设施依赖（禁止清单不含 `gateway-host`）；`apps/gateway` 的 architecture 测试冻结应用源码树、禁止生产测试挂载并约束 bootstrap 只做装配。其余 crate 没有 architecture 测试。

## 3. 请求执行链

```text
API decode/auth
  -> freeze RuntimeSnapshot
  -> compile RoutePlan
  -> admission
  -> enqueue model_request observation
  -> select Provider/account
  -> enqueue attempt/send observations
  -> cross upstream send barrier
  -> canonical stream
  -> cross in-memory downstream delivery barrier and enqueue observation
  -> enqueue terminal observation and accounting
```

核心不变量：

1. 每个客户端请求只有一条 `model_requests`；attempt 明细保存在其 JSON 事实中，不另建 attempt 表。
2. 任何可能到达上游的调用都必须先按序提交 request/attempt 观测到有界队列；PostgreSQL 投影是可恢复
   观测，不是数据面发送的同步前置条件。3. `not_sent`、`sent`、`ambiguous` 是不同的上游发送边界；发送状态是请求级单调水位（`sent` > `ambiguous` > `not_sent`），跨 attempt 只升不降，终态写回不得低于水位；`ambiguous` 不自动重放。
4. 内存中的 downstream commit 表示网关已作出不可撤回的交付承诺，从该时刻禁止 retry/fallback；
   它在实际首字节写出前越过并按序入队。`downstream_committed_at` 是该事实的 PostgreSQL 观测投影，
   不宣称字节已经到达客户端，也不反向决定协议交付。
5. Provider 每次 `execute` 只能选择一个 credential 并准备一个 cold stream，不得隐藏换号或业务 retry。
6. 下游 commit 前，Core 可按冻结策略处理同一 Provider 内的账号 fallback；禁止跨 Provider fallback。
7. 本架构不通过隐式连接复用承载业务身份。HTTP client 可安全复用 transport 连接，但账号、credential revision、cookie/session binding 必须显式绑定到本次调用。

管理端 Usage 详情中的 attempt 列表是 best-effort 观测（见 adr-002）：中间失败来自 `ops_events`，最终尝试由 `model_requests` 合成，二者都不承诺完整（队列丢弃、写入失败、非失败观测不落库会造成缺口）。API 通过 `attemptsComplete: false` 显式标注这一语义，不再声称「全部尝试」。

## 4. 路由、fallback 与错误处理

RuntimeSnapshot 冻结 Provider 集合、模型能力、运行时策略和全局 `config_revision`。一次请求始终使用同一快照，不在执行中拼接新旧配置。

Fallback 只允许在同一 Provider 内更换可用账号，不跨 Provider。

明确的上游认证、封禁、额度或 cooldown 错误会更新对应账号状态，使下一次选择排除该账号；满足重放安全条件且尚未 downstream commit 时，可以换号。传输结果不明确时不得假定请求未到达上游。

重试矩阵属于独立策略，OpenAI 保持原正式行为、xAI 保持参考行为，不在 Provider transport 内增加额外重试。

## 5. Continuation

系统在 `model_requests.client_response_id` 与 `upstream_response_id` 中记录响应生命周期确认的 ID，
两者都按 opaque UTF-8 bytes 保存，不做格式、长度或唯一性推断；不新增 conversation、transcript、
continuation 或 claim 表。

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

- OpenAI 是逐字节透明代理：请求 body 的未知字段和字段顺序保持不变（受控模型映射除外），上游 wire 是客户端可见的事实来源，HTTP SSE 帧以原始字节原样下发，WebSocket 上游 JSON 文本原样下发；response ID 以 opaque bytes 存储，不假设 UUID 或固定长度。canonical facts 只从同一帧旁路解析，用于观测、亲和与计费。仅网关内部帧（rate limits、metadata）被就地消费，不向客户端转发。
- xAI 是翻译层：把 Grok wire 解码后重新合成 Responses wire 事件，不承诺字节透明；上游错误按结构化 message/code/type 透出，message 中的 UUID 账号指纹替换为占位符。

credential 更新使用 `credential_revision` CAS。认证永久失败、封禁、额度耗尽和带截止时间的 cooldown 是账号运行时事实。cooldown 到期后，Core 的有效调度谓词会自然允许账号重新参与选择；Redis cooldown 只是热缓存。成功调用或成功额度观测才可把账号状态重新观测为 `ready`。自动 refresh 的瞬态失败（限流、5xx、超时、畸形响应）保留现有凭据并按指数退避推迟下一次刷新，不把账号标记为失效；只有上游明确的永久错误（`invalid_grant`、封禁）才写入终态失效。

`refresh_token_expires_at` 不是公共 SQL 列或 Core 权威状态。xAI 可在 `provider_credentials_json` 内保存它作为 Provider 私有提示；真正失效以 refresh endpoint 返回的永久错误为准。

OpenAI 导入、首次 OAuth 和重新授权都先验证签名 token 与稳定账号身份。重新授权的目标绑定只保存
目标账号 ID，不接受或冻结客户端提供的 credential revision；complete 阶段重新读取目标账号及其当前
revision，并以当前 revision 做最终 CAS。上游账号或用户身份发生换绑时拒绝提交；Free token 缺少
账号 claim 时，用目标账号 header 下的 usage 身份做交叉确认。认证主体投影可以随同一上游账号的
合法重新授权更新，但目标 upstream account/user 不得改变。

credential 与 quota 是两套状态机。主动 quota refresh 会拒绝过期 access token；quota 探测的
401/403 不具备判定 refresh token 永久失效的证据。成功主动观测和正常推理响应中的 rate-limit header
观测都 revision-fenced 写入同一 quota，并转换 readiness、`QuotaExhausted` 或 `Cooldown` 等额度事实；
只有明确的 deactivated workspace 才能由额度链路写入 `Banned`，且额度观测不能清除 `Invalid`、
`Expired`、`Banned`。Free、K12 等套餐复用该逻辑；`plan_type` 在此不参与额度分支，只作为 Provider
事实并用于隔离不同套餐的 catalog cache。

## 7. 控制面与内部 revision

Admin API 不要求客户端提交配置 revision，也不向客户端暴露配置 revision。会改变调度快照或安全配置的写入，必须在同一 PostgreSQL 事务中：

- 执行业务 mutation；
- 推进 `runtime_settings.config_revision`；
- 写入脱敏 `admin_audit_events`。

推进全局 revision 的 mutation 包括 runtime settings、客户端 Key、账号导入/创建/删除、管理员显式启停和管理员 credential rotation。

不推进全局 revision 的运行时观测包括 quota、cooldown、catalog generation、请求统计以及自动 credential refresh；自动 refresh 只推进 `credential_revision`。提交后 Redis 通知只负责缩短其他副本的收敛延迟，周期性 PostgreSQL 对账才是正确性基础。

## 8. PostgreSQL 终态

`backend/migrations/0001_initial.sql` 创建且只创建七张业务表；`0003_s3_backup.sql` 增加备份配置与备份记录两张表：

| 表 | 权威事实 |
| --- | --- |
| `admin_users` | 管理员身份与密码摘要 |
| `admin_audit_events` | 管理 mutation 审计 |
| `client_api_keys` | 客户端鉴权、限额与授权范围 |
| `runtime_settings` | 全局配置与 `config_revision` |
| `provider_accounts` | 账号资料、Provider-owned 明文 credential JSON、revision、quota、cooldown |
| `model_requests` | 请求、attempt、计费、交付与恢复事实 |
| `ops_events` | 脱敏运行事件 |
| `backup_settings` | S3/R2 存储、Cron 计划与保留策略单例配置 |
| `backup_records` | 备份任务状态与归档事实（`queued/dumping/uploading/completed/failed/deleting`） |

备份设计边界见 `docs/s3-backup-design-audit.md`：单副本部署下备份由单个可取消
`DaemonTask` 承担调度、执行、删除收敛与保留清理，不需要 leader lease、fencing token 或
heartbeat；计划时间点冲突只记录日志/指标并推进游标，不产生 skipped 记录；删除成功后
记录硬删除，操作历史进入 `admin_audit_events`。管理员身份事实不重复写入
`backup_records`。存储身份在存在记录时锁定：endpoint/region/bucket/path-style 不允许
变化，只允许轮换凭据与修改 prefix。每份备份可设置创建时确定的手动过期时间
`expires_at`（手动与计划备份均可，`expiresInDays` 天数生成），到期由 Worker 自动清理。

设计规则：

- 一个事实只存一次；可表达关系使用真实 FK 与支持索引。
- Provider 差异只进入受 schema/version 校验的 JSONB 边界。
- credential JSON 以 Provider 自己的 schema 明文保存在 `provider_credentials_json`；日志、Debug、API 和 audit
  不输出其中的 secret，Provider 只在需要发往上游时把值包装为运行时 secret。
- stale recovery 只把超时 `running` 请求收敛为 `incomplete`，不重放业务请求。
- 数据面 execution observation 通过有界、带总字节预算的进程内队列交给 PostgreSQL OpsFlush worker；
  入队不等待数据库，队列满、worker 不可用或写入失败时记录累计统计并丢弃该可恢复观测，不能替换
  客户端可见协议结果。队列不是第二份业务权威，也不做无幂等键的盲目重试。
- retention 只删除已满足保留规则的历史事实，不改变运行中请求。

业务 schema 由 `backend/migrations/0001_initial.sql` 直接定义当前大版本的完整新库基线，其中
response ID 使用无格式假设的 UTF-8 bytes，`model_requests.service_tier` 保存响应观测到的服务档位，
且额度耗尽账号可以携带冷却截止时间。同一大版本内已合入的迁移永久冻结——
`.frozen-sha256` 是冻结清单，CI 校验清单相对 PR base 只增不改，启动时 sqlx 重校验已应用
迁移的 checksum，不一致直接拒绝启动。规则见 `backend/migrations/README.md`。

## 9. Redis 终态

Redis 保存的协调状态恰好是：客户端 admission 热状态、credential lease/fencing 与调度信号、账号 cooldown 与 availability 热缓存、catalog 缓存、Provider circuit、会话亲和与会话级账号排除、continuation pin、OAuth pending flow、worker leader lease 以及 runtime change 通知。Redis 丢失后必须从 PostgreSQL 或 Provider 事实恢复；恢复完成前需要保护的 acquire 路径 fail closed。

多副本部署时，跨副本协调面只有上述 Redis 状态与 PostgreSQL 事实；WebSocket 连接池、请求画像与 RuntimeSnapshot 副本都是进程内状态，其中 RuntimeSnapshot 依靠周期对账收敛，请求画像依靠各自的官方版本检查收敛。

OAuth pending key：

```text
codex-proxy-rs:oauth-pending:v1:{provider_kind}:{flow_fingerprint}
flow_fingerprint = SHA-256(provider_kind || 0x00 || raw_flow_binding)
```

创建时的基础 Hash 字段为：

- `owner_fingerprint`
- `expires_at_epoch_seconds`
- `provider_payload`

complete 先通过 Lua 原子 claim，并临时增加 `claim_fingerprint` 与
`claim_expires_at_epoch_millis`。owner mismatch 不删除记录；有效 claim 阻止并发 complete，失败时只有
claim owner 可以释放，账号事务提交后才原子删除整个 pending key。OpenAI TTL 为 10 分钟，xAI TTL
为 30 分钟。

## 10. 后台任务与恢复

HTTP serve/drain 与 worker 运行时都在 `gateway-host`：Host 负责注册校验、leader lease、失败退避、健康暴露与关闭；`apps/gateway` 的 bootstrap 只装配。WorkerContribution 由 Store、Core、Admin 与两个 Provider bundle 显式贡献，host bundle 自身不贡献任何 worker：

- Store：stale execution recovery、retention，以及 PostgreSQL execution observation、Redis client
  admission release 和 Provider circuit feedback 三个 OpsFlush daemon。native continuation 直接写 Redis，
  单次 Lua 记录只清理有界数量的过期/超限索引项，避免无界阻塞。
- Core：RuntimeSnapshot revision 周期对账（无 lease，逐副本运行）与 Redis change 长驻订阅（daemon，逐副本运行）。
- Admin：备份 daemon（`WorkerKind::Backup`）。单副本边界下它是一个可取消 `DaemonTask`，内部循环推进
  Cron 游标、恢复中间状态、完成删除收敛、领取并执行一个 queued 任务、执行一小批保留清理；长时间
  `pg_dump` 与 multipart 上传由 daemon 自身持有，Host 只负责 panic 后重启、健康与关闭。它不使用
  Redis leader lease（单副本无竞争），只依赖 PostgreSQL 部分唯一约束防御并发 API/调度创建。
- Provider：credential refresh、quota/catalog 健康，以及模型 etag 与官方版本 release 检查（OpenAI CLI、OpenAI Desktop、xAI CLI）。

带 lease 的周期任务在每个周期开始向 Redis 申请一次 leader lease，周期内续租，周期结束即释放。这只是单周期互斥，不是跨周期 leader 选举：各副本独立计时，N 副本部署下同一任务的实际执行频率最坏可达单副本的 N 倍。

自更新、回滚与进程重启不是 worker：它们是 Admin API 触发的 Host 系统操作，只替换并重启处理该请求的那个副本。

关闭时序：收到取消信号后，HTTP drain（axum 优雅关闭与游离连接等待）共享同一个从信号时刻起算的绝对截止点，逾期放弃等待存量连接；随后 worker 在独立的 shutdown 超时内 join。有界观测队列会在该阶段按各自预算 drain，逾期的可恢复观测被计数并丢弃。

worker 不得通过导入 Admin use case 绕过边界，也不得维护第二份业务状态。

## 11. 测试与验收

- 生产 `src` 禁止 `#[cfg(test)]`、`#[path]` 和 `include!` 测试挂载。
- 测试位于各 package 的 `tests/`，目录镜像生产模块。
- Core 规则使用确定性测试；Provider 使用 contract/fixture 测试。
- FK、CAS、revision、recovery 使用真实 PostgreSQL 测试。
- admission、lease、cooldown、OAuth pending 使用真实 Redis 测试。
- 自动化测试不得发送真实对话或轮换生产 refresh token；仓库不提供破坏性 live fixture。
- OpenAI identity 仅保留可选的外部 `CODEX_REAL_ACCOUNT_FILE` 官方 JWKS 签名检查，环境变量未设置时
  直接跳过；它不经过 selector/transport，也不是数据面验收。

终态门禁：

```bash
cd backend
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --test main --locked
```
