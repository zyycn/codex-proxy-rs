# Codex Proxy RS 当前架构

本文只描述仓库当前生效的终态。设计依据是
[多 Provider 目标架构](multi-provider-architecture.md) 与
[终态数据模型](multi-provider-database.md)；旧账号池、旧 dispatch、旧 telemetry 和迁移期兼容路径不属于当前系统。

## 系统边界

Codex Proxy RS 是单进程、多 Provider 的透明 AI 网关：

- 客户端面提供 `POST /v1/responses`、`GET /v1/models` 和模型详情。
- 控制面提供 `/api/admin/*` 与 Vue 管理端静态资源。
- 当前注册 Provider 为 Codex 和 xAI；两者都使用 OAuth credential，xAI 不接受 API Key 模式。
- PostgreSQL 保存配置、请求、Attempt、价格、延续和审计等权威事实。
- Redis 只保存可恢复的 admission、lease、cooldown、OAuth pending flow 与配置通知。
- `.runtime/` 保存本地密钥材料、日志和部署数据，不把秘密写入源码或普通配置 JSON。

客户端协议、Gateway Engine 与 Provider adapter 相互独立。OpenAI Responses handler 不识别 Codex/xAI；Provider 不拥有调用方预算和跨平台路由策略。

## Workspace 与依赖方向

```text
backend/
├── apps/gateway/                 composition root、HTTP、任务
├── crates/gateway-core/          operation、routing、engine、policy、accounting
├── crates/gateway-api/           OpenAI Responses 边界适配
├── crates/gateway-protocol/      可共享 wire contract
├── crates/gateway-store/         PostgreSQL/Redis adapter
├── crates/providers/codex/       Codex Provider 与 credential owner
├── crates/providers/xai/         xAI/Grok OAuth session Provider
├── migrations/0001_initial.sql   唯一初始迁移
└── Cargo.toml
```

依赖方向固定为：

```text
gateway binary
├── gateway-api -------> gateway-core
├── gateway-store -----> gateway-core
├── provider-codex ----> gateway-core
└── provider-xai ------> gateway-core
```

`gateway-core` 不依赖 Axum、SQLx、Redis、reqwest 或具体 Provider。Provider crate 之间禁止互相依赖；只有 bootstrap 可以把具体 adapter 绑定到抽象边界。

## 数据面

一次请求只有一个 `gateway_requests`，所有可能到达上游的调用分别对应有序 `request_attempts`：

```text
API decode
  -> authenticate client key + freeze RuntimeSnapshot
  -> resolve native/portable continuation
  -> compile RoutePlan + admission reservation
  -> persist logical request
  -> Provider selects credential and returns cold stream
  -> persist Attempt
  -> mark ambiguous immediately before first poll/send
  -> canonical events
  -> downstream commit barrier
  -> terminal CAS + usage/cost/bucket/continuation transaction
```

不变量：

1. Logical request 与 Attempt 必须先提交 PostgreSQL，才允许发送可能计费的 payload。
2. `upstream_send_state` 与 `downstream_committed` 是独立边界。
3. `not_sent` 和已经收到明确失败响应的 `sent` 可在下游 commit 前按策略 retry；`ambiguous` 禁止自动重放。
4. 同一 request 最多一个 committed Attempt，commit 后不能新建 Attempt。
5. Provider transport 不隐藏 credential retry、payload retry 或跨 origin redirect。
6. 请求级 RoutePlan、credential revision、service tier 与价格时间线均来自同一冻结 Snapshot。

### 路由与价格

Router 先按 operation、capability、健康、地区和调用方 allowlist 过滤，再按 route policy 排序。Target 只引用 Provider instance 与上游模型，不固定 credential。

每个 Attempt 按自己的 `started_at` 从冻结价格时间线选择 `effective_from <= started_at` 的最新版本。`not_sent` 阶段不写价格；跨越 send barrier 时同时冻结 price version ID，终态使用十进制定点计算 known/partial/unknown 成本。零价必须由显式零费率表达。

Strict budget 只保留同币种且可计算完整保守上界的 target，按输入估算、最大输出和最大 attempts 预留。终结时使用全部 Attempts 的 known 成本结算；任何 partial/unknown 都不会按零释放。Redis epoch 丢失时先从持久事实重建，再恢复 admission。

### Continuation

- Native continuation 固定 Provider、instance 和必要 credential scope；single-use claim/consume/ambiguous 由 PostgreSQL CAS 保证。
- Portable continuation 使用调用方隔离的加密 transcript snapshot 链，可跨 Provider 路由；输入、可表示的成功输出和 binding 与 terminal request/Attempt 在同一事务提交，失败与取消不生成正文。
- Redis 只协助短期串行化，flush 不会让已经可能消费的 native handle 再次可用。
- 终态 continuation、最终 Attempt 与 request 在同一事务完成后，API 才能发送 terminal event。

## Provider 边界

`Provider` 接收 canonical `Operation + PlannedTarget + AttemptContext`，返回带冻结 metadata 的 cold canonical stream。Gateway Engine 只通过 Registry 查找 Provider，不写平台分支。

Credential 由对应 Provider 独占管理：

- Codex：OAuth token、ChatGPT 可查询字段、Cookie policy、credential selector 与 Codex transport。
- xAI：Device/Authorization Code OAuth、OIDC 校验、加密 session token、Grok inference transport。

Provider selector 同时冻结 `credential_revision` 与 `state_revision`。明确的认证失败、额度耗尽和 429 分别写入 `invalid`、`exhausted` 或带截止时间的 `cooldown`；更新使用 credential/state 双 revision CAS 与 `observed_at`，因此旧请求不能污染已经轮换的新 secret。普通成功 Attempt 不回写 credential state。

Secret envelope、Cookie、PKCE verifier、device code 和 native state 使用用途/owner/行身份绑定的 AEAD；日志、Debug、Admin API 与 audit details 不输出秘密或完整身份。

## 控制面

控制面管理 client key、Provider instance/model、model route/target、不可变 price version，以及 Codex/xAI OAuth credential。

所有 Admin HTTP 契约遵守：

- 只使用 GET 与 POST。
- ID 放在 query params 或 JSON body，不放动态 URL path。
- 结构变更携带 `expectedConfigRevision`。
- 业务写入、单调 `config_revision` 和脱敏 audit event 位于同一 PostgreSQL 事务。
- OAuth token rotation 只推进 `credential_revision`，高频 availability/cooldown 只推进 `state_revision`。

Provider instance 的 create/update/enable 在 PostgreSQL mutation 前先由当前已注册 adapter 校验 endpoint 和公开 options；未知 Provider 或非法配置不会推进 revision。提交成功后，控制面通过带 namespace 的 Redis Pub/Sub v1 通知广播 config/credential/state revision；credential ID 在进入 payload 前只保留 SHA-256 指纹，通知不携带 secret 或资源原文。通知只唤醒一次完整 PostgreSQL reload，解析失败、断线、乱序或发布失败都不会拼接局部状态，也不会改变已经提交的 PostgreSQL 结果。

RuntimeSnapshot 周期对账始终每 5 秒比较全局 `config_revision` 和 credential `(credential_revision, state_revision)` 向量，Redis 通知仅缩短收敛延迟。对账允许 config revision 跨多个版本并直接重载最新一致快照；任何 revision 回退、credential 集合无结构版本变化或候选快照无效都会保持 fail closed。

Price version 发布后不可更新或删除修正；更正必须发布新版本。

## 持久化 owner

| 事实 | 唯一 owner |
| --- | --- |
| client key、runtime settings | control-plane settings |
| Provider instance/model/route/price | config publisher |
| upstream credential secret/config | 对应 Provider credential manager |
| credential availability/cooldown | Provider state owner |
| request/attempt | Gateway execution lifecycle |
| transcript/binding | history/continuation owner |
| ops event | ops recorder |
| request/attempt metric bucket | bucket aggregator |

`backend/migrations/0001_initial.sql` 直接创建终态 schema。应用只接受空业务 schema，不包含旧表 backfill、dual-write、旧字段读取或兼容 migration。可表达的关系全部使用真实 FK，并为 FK 子列提供支持索引；外部协议 ID、删除后保留的 `_ref` 与多态 audit ref 是明确例外。

## Redis epoch 与后台任务

Redis 空状态不等于零使用量：

- client admission epoch 从 running/近期 request 与 attempt 成本事实重建。
- Provider/credential lease epoch 从 running Attempt 和存活 worker 的 live guard 重建。
- epoch 为 missing/rebuilding 时，受限 admission 与 lease acquire 均 fail closed。
- Lua 使用 Redis server time、hash tag 和 fencing/epoch 检查完成原子操作。

后台任务还负责 RuntimeSnapshot revision 对账、stale execution recovery、closed bucket rebuild 与 retention。Retention 固定先 recovery，再清 binding/transcript，最后清 request/attempt 和过期 bucket。

## 前端与测试

前端复用旧项目的克制视觉语言，但信息架构只面向终态控制面。外部响应在 API/composable 边界收窄；局部状态与返回值以推导为主，不维护与后端 DTO 镜像的显式类型层。

测试全部位于对应 package 的 `tests/`，目录结构镜像 `src/`。生产 `src` 不包含 `#[cfg(test)]`、测试模块挂载或 test-only 分支；架构测试会强制检查该约束：

- core 规则使用快速单元测试。
- Provider 使用 contract suite 覆盖流、错误、取消、secret redaction。
- FK、CAS、revision、continuation、price、retention 和 rebuild 使用真实 PostgreSQL。
- admission、lease、OAuth pending flow 与 epoch recovery 使用真实 Redis，必要时联合真实 PostgreSQL。

架构验收以两个权威文档为准；若新增普通 Provider 仍需要修改 Gateway Engine、OpenAI handler 或通用 schema，说明边界已经回退。
