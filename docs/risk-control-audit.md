# 风控链路审计

审计日期：2026-06-20

## 范围

本次只审计 OpenAI / Codex 上游真实请求链路相关的风控实现，不纳入通用 proxy、VPN 或其它 provider。

参照对象：

- TS 仓库：`/home/zyy/桌面/Codes/codex-proxy`
- 重点版本：`v2.0.78-beta.15c49d6`
- 后续风控增强参照：当前 TS HEAD `v2.0.80`
- Rust 仓库：当前工作区 `/home/zyy/Codes/codex-proxy-rs`

## 对齐原则

本轮严格以 TS 实现和 OpenAI 原版实现为依据，不再从单次抓包或经验规则直接推导代码行为。

- TS 当前 HEAD 是 `v2.0.80`，已经包含 `v2.0.78-beta.15c49d6` 之后的 Cloudflare cooldown、上游 session identity 隔离、reasoning replay 等风控增强；没有特别说明时，优先对齐当前 TS HEAD。
- `docs/openai-res.txt` 是真实链路验收证据，不是直接改代码的唯一来源。抓包和 TS 行为冲突时，先记录差异，再用 TS / OpenAI 原版 / 新真实链路三方确认。
- TS 没有实现的增强，不写成“已对齐”。例如 active quota refresher 的 30 分钟节流在 TS 中是进程内 `Map`，不是持久化字段；如果后续要做跨重启持久化节流，应标为 Rust 侧增强，而不是 TS 对齐项。
- 请求头、payload、账号状态流转都先查 TS 源码和调用点；缺少明确来源的字段不直接加入真实请求链路。

相关历史：

- `981f94c fix(auth): add cascading ban defense on session affinity degradation`
- `c559141 fix(auth): add active quota refresher and drift defense verification`
- `3178d21 fix(auth): address PR #624 review findings`
- `d19ed47 fix(auth): resolve TypeScript null-narrowing in drift-defense loop`
- `abc5b98 fix(auth): only strip session on ban, not quota rotation`
- `d4ec5a7 Merge pull request #624 from icebear0828/fix/cascading-ban-defense`
- 后续增强：`b079af1` Cloudflare challenge cooldown、`bc4b13b` 上游 session identity 隔离、`9eb8612` reasoning replay cache。

## 结论

当前 Rust 已经覆盖了不少后续 TS 增强，尤其是上游 identity 隔离、reasoning replay、WebSocket required/fallback 安全、Cloudflare path-block 处理、禁用/封禁状态持久化、`next_refresh_at` 持久化。

本次已补齐 TS PR #624 中与请求链路相关的 P0 缺口，并补齐后续 TS 风控增强里的 refresh token 安全重试与 Cloudflare challenge cooldown：

1. `quota_verify_required` 已接入请求发送前 `/usage` 漂移验证，chat / responses / stream / compact 都会在真实业务请求前验证 dirty 账号。
2. 后台 `QuotaRefreshTask` 已同时扫描 `quota_limit_reached` 和 `quota_verify_required`。
3. session affinity 降级时，只有 preferred 账号为 `Banned` / `Disabled` 才会在发送前剥离历史；quota rotation 不预剥离。
4. refresh token transport error 已拆成可安全重试的 pre-flight 失败和不可安全重试的 ambiguous 失败；每次真正刷新前都会重读 DB refresh token，避免复用 stale RT。
5. 401/token invalid 与 refresh `invalid_grant` 已拆成两阶段：请求链路普通 token invalid 先落 `Expired`，允许 RT 自救；一旦 refresh token 本身确认 `invalid_grant` / invalid / expired，则落 `Disabled` 并停止后续调度。TS 当前把 refresh `invalid_grant` 也落 `Expired`，但 Rust 有持久化周期调度，照搬会反复刷新坏 RT。
6. Cloudflare challenge cooldown 已按 10/30/90/120 秒递增，1 小时无 challenge 后重置，并继续持久化 `cloudflare_cooldown_until`。

仍需继续做真实链路层面的验证：HTTP SSE header、TLS 指纹、真实上游是否持续返回可复用的 `cf_clearance`，以及 OpenAI 原版客户端后续风控差异。cookie 自动捕获范围和捕获时机已先按 TS 对齐。

## PR #624 四个修复逐项对照

这组修复都在 TS `v2.0.78-beta.15c49d6` 之前合入，属于当前必须对齐的风控基线。

| TS 修复 | TS 行为 | Rust 当前状态 | 判断 |
| --- | --- | --- | --- |
| `fix(auth): add active quota refresher and drift defense verification` | `ActiveQuotaRefresher` 每 15 分钟扫描 active 账号，命中 `limit_reached` 或 `quotaVerifyRequired` 都主动拉 `/usage`；请求链路遇 dirty 账号先做 `/usage` 校验，仍限流则排除并重新 acquire，最多 5 次 | Rust 已在 `QuotaRefreshTask` 扫描 locked/dirty；`verify_acquired_quota_if_required` 已接入 chat / responses / stream / compact；成功验证会通过 `apply_quota_snapshot` 同步 DB 和内存池 | 已对齐 |
| `fix(auth): add cascading ban defense on session affinity degradation` | 当 `previous_response_id` 指向的 preferred 账号已经 `banned/disabled`，但本次 acquire 降级到其它账号时，发送前剥离 `previous_response_id` 和 `turnState`，避免封禁关联扩散 | Rust 已在 Responses complete/stream 发送前检查 preferred 账号状态；仅 `Banned` / `Disabled` 且换号时剥离 `previous_response_id` / `turn_state` 并忘记 affinity；不额外清理 `turn_metadata` / `prompt_cache_key` | 已对齐 |
| `fix(auth): only strip session on ban, not quota rotation` | 只在 `banned/disabled` 这种 ban-risk 状态下预剥离；quota rotation 不预剥离，交给 `previous_response_not_found` 的普通 retry | Rust 的预剥离 guard 只匹配 `AccountStatus::Banned | AccountStatus::Disabled`；dirty quota / rate-limit / quota exhausted 只走排除重调度或原有 history recovery | 已对齐 |
| `fix(auth): resolve TypeScript null-narrowing in drift-defense loop` | 这是 TS 类型收窄修复，保证 drift-defense loop 中 acquire 变量在重新赋值后类型安全 | Rust 不存在 TS null-narrowing 问题；业务语义已由 `QuotaVerificationDecision` 显式区分 ready、retry、max attempts | 类型问题不适用，业务已覆盖 |

## 覆盖矩阵

| 能力 | TS 参照 | Rust 当前状态 | 判断 |
| --- | --- | --- | --- |
| Active quota refresher | `src/auth/active-quota-refresher.ts:63-99` 同时扫 `limit_reached` 和 `quotaVerifyRequired` | `crates/runtime/src/tasks/quota_refresh.rs` 同时扫 `quota_limit_reached || quota_verify_required`，成功后应用完整 quota state | 覆盖 |
| 请求前 quota 漂移验证 | `src/routes/shared/proxy-handler.ts:92-139`，最多 5 次 `/usage` 校验并排除仍限流账号 | `crates/runtime/src/services.rs` 的 chat / responses / stream / compact acquire 后先调用 `verify_acquired_quota_if_required`；仍限流则 release + exclude + reacquire | 覆盖 |
| quota 冷却后 dirty 标记 | `account-registry.ts` 离线窗口 rollover 后 dirty | `crates/core/src/accounts/pool.rs:670-689` 冷却或窗口过期后置 `quota_verify_required=true` | 部分覆盖 |
| ban-risk session 剥离 | `src/routes/shared/proxy-retry-recovery.ts:110-152` 仅 preferred 为 `banned/disabled` 且换号时剥离 | `ResponseDispatchService::apply_cascading_ban_defense` 仅在 preferred 为 `Banned/Disabled` 且换号时剥离；quota rotation 不剥离 | 覆盖 |
| same-account history recovery | TS 对 `previous_response_not_found` / unanswered function call strip 后同账号重试 | `crates/runtime/src/services.rs:2040-2048`、`2580-2624`、`3293-3300` 可错误后 strip/retry | 覆盖 |
| 上游 session identity 隔离 | `src/proxy/codex-api.ts:108-139`、`328-358`、`384-413` | `crates/core/src/gateway/conversation.rs:27-46`、`145-168`；`crates/runtime/src/services.rs:2863-2885`、`2901-2923`；`crates/adapters/src/codex/client.rs:1064-1110` | 覆盖 |
| reasoning replay cache | TS `9eb8612` | `crates/core/src/serving/reasoning_replay.rs:8-10`、`44-108`、`173-238`；记录/驱逐在 `crates/runtime/src/services.rs:3812-3895` | 覆盖 |
| WebSocket previous_response 安全 | TS `PreviousResponseWebSocketError` 禁止降级 | `crates/core/src/serving/responses.rs:21-39` + `crates/adapters/src/codex/client.rs:390-448`，`previous_response_id` 为 WebSocketRequired | 覆盖 |
| WS 请求头和 deflate | TS 使用 Node `ws` 完整握手；项目只传业务 headers，`ws@8.19.0` 客户端默认 `perMessageDeflate=true` | Rust 使用 OpenAI fork tungstenite；`crates/adapters/src/codex/websocket/connect.rs:377-399` 构造 opening 审计快照，`497-503` 启用 permessage-deflate | 覆盖，仍需真实抓包持续比对 |
| Cloudflare challenge cooldown | TS `src/auth/cf-challenge-cooldown.ts:1-56` 递增 10/30/90/120 秒 | `CloudflareChallengeCooldownTracker` 按 10/30/90/120 秒递增并 1 小时 stale reset；runtime 持久化 `cloudflare_cooldown_until`，成功响应后清理 cooldown state | 覆盖 |
| Cloudflare path-block | TS 空 body 404 清 cookie，累计 3 次 disable | `crates/runtime/src/services.rs:3095-3100` 识别空 404；`878-899` 清 cookie / 计数 / disable；`crates/core/src/accounts/cloudflare.rs:8-66` 3 次阈值、1 小时 stale | 覆盖 |
| cookie 自动捕获 | TS `cookie-jar.ts` 只允许自动捕获 `cf_clearance`；`codex-api.ts` 在 HTTP/compact transport response 后、状态码判断前 capture，WS 升级响应 header 也会回传 | Rust `SqliteCookieStore::capture_set_cookie` 仍只持久化 `cf_clearance`；HTTP/compact/stream/WS 的成功响应和 upstream error 都会在 runtime 消费 `set_cookie_headers` | 已对齐 |
| refresh disabled/banned skip | TS refresh scheduler 跳过 disabled/banned | `crates/runtime/src/tasks/token_refresh.rs:220-229` 跳过 disabled/banned | 覆盖 |
| `next_refresh_at` 持久化 | TS 持久化下次刷新日期 | `crates/runtime/src/tasks/token_refresh.rs:315-319`、`388-397`、`513-541` | 覆盖 |
| 永久刷新失败确认 | TS 永久失败 2 次确认 | `crates/runtime/src/tasks/token_refresh.rs:44`、`589-600` | 覆盖 |
| token invalid 状态映射 | TS 请求链路 401 非 deactivated 落 `expired`；401 deactivated 落 `banned` 但返回仍是 401；refresh `invalid_grant` 连续确认后落 `expired` | Rust 请求链路仍将 `token_revoked` / `token_invalid` 落 `Expired`；refresh token 确认 `InvalidGrant` 后落 `Disabled`，避免持久化周期调度反复刷新坏 RT；`account_deactivated` / `banned` 仍为 `Banned`，并保留上游触发状态码返回客户端 | 有证据偏离 TS |
| refresh token 安全重试 | TS `src/tls/direct-fallback.ts:52-75` 只重试安全 pre-flight 错误 | `RefreshFailure::RetryableTransport` 才重试；`RefreshFailure::Transport` 直接进入 recovery delay，不复用 RT 重试 | 覆盖 |
| stale RT / 跨进程刷新防护 | TS refresh 前比对磁盘 RT，发现不同则同步不消费旧 RT | `TokenRefreshTask::prepare_refresh_attempt` 每次真正刷新前重读 DB；refresh token 与扫描快照不一致时跳过，不消费旧 RT | 覆盖 |
| stream 失败可观测 | TS `recordStreamCloseEvent` 会把 stream 异常携带 requestId、path、model、account 写入排查日志 | Rust 不迁移 TS 旧日志体系；Responses stream 启动阶段终态失败写入现有 `event_logs`，包含 `stream=true`、`transport`、`failureClass`、`exhaustedCount`、`upstreamError`；live stream completed/failed 继续走已有事件 | 覆盖 |

## 关键发现

### 1. quota 漂移防御已补齐

TS 在请求链路里对 `quotaVerifyRequired` 做前置 `/usage` 校验：

- 最多 `MAX_VERIFY_ATTEMPTS = 5`，避免单请求放大 `/usage`。
- 校验后仍 `limit_reached` 的账号会 release，加入 `verifiedExcludeIds`，重新 acquire。
- 校验失败不会清 dirty flag，交给下一次请求或后台 refresher。

Rust 当前具备状态字段和发送前验证：

- `accounts.quota_verify_required` 已在 schema 中持久化：`crates/platform/src/storage/schema.sql:59`
- 账号模型有字段：`crates/core/src/accounts/model.rs:51`
- SQLite 可读写该字段：`crates/adapters/src/sqlite/accounts.rs:162`、`517`、`536`、`547`
- quota 冷却过期后会设置 dirty：`crates/core/src/accounts/pool.rs:685-689`

- `crates/core/src/serving/quota.rs` 提供主 rate-limit 的触顶、reset_at、窗口解析。
- `crates/adapters/src/sqlite/accounts.rs` 新增 `apply_quota_snapshot`，成功验证会清 `quota_verify_required`，并按 `/usage` 结果同步 `quota_limit_reached` / `quota_cooldown_until`。
- `crates/core/src/accounts/pool.rs` 新增 `apply_quota_state`，请求前验证会同步内存账号池。
- `crates/runtime/src/services.rs` 的 `verify_acquired_quota_if_required` 在业务请求发送前处理 dirty 账号，最多 5 次；仍限流则 release、exclude、reacquire；`/usage` 拉取失败会保留 dirty 并继续原链路。
- `crates/runtime/src/tasks/quota_refresh.rs` 后台候选已扩展为 `quota_limit_reached || quota_verify_required`。

TS 复核点：

- TS `ActiveQuotaRefresher` 每 15 分钟 jitter 调度，per-account 最小 30 分钟刷新间隔保存在进程内 `lastRefreshedAt: Map<string, number>`。
- TS 没有把 active quota refresher 的最小刷新间隔持久化到账号数据。
- TS 的 token refresh scheduler 另有持久化 `next_refresh_at` 语义；它约束 access token 自动刷新，不等同于 active quota refresher 的 `/usage` 主动校验节流。

注意：后台 `QuotaRefreshTask` 当前仍只持有 SQLite store，不直接刷新运行时内存池。真实发送链路已经通过请求前验证同步 DB 和内存；如果需要后台刷新后立即让内存池解锁，需要后续把 `RuntimeAccountPoolService` 注入 task。

### 2. cascading-ban defense 已补齐

TS 的语义很明确：

- 如果 preferred account 因 session affinity 指向旧账号，但 acquire 实际换到新账号；
- 且 preferred account 状态是 `banned` 或 `disabled`；
- 才剥离 `previous_response_id` / `turnState`，避免把封禁风险关联到新账号；
- quota rotation 不剥离，由普通 `previous_response_not_found` retry 处理。

Rust 目前已有并已补齐：

- `previous_response_id` 查 preferred account：`crates/runtime/src/services.rs:1849-1858`
- 错误后 history recovery：`crates/runtime/src/services.rs:1860-1875`
- `previous_response_not_found` / `invalid_encrypted_content` 后 recovery：`crates/runtime/src/services.rs:2040-2048`、`2580-2624`
- 实际剥离函数：`crates/runtime/src/services.rs` 的 `strip_request_history`
- 新增 `ResponseDispatchService::apply_cascading_ban_defense`，在 Responses complete/stream acquire 后、发送前执行。
- 仅当 preferred 账号状态为 `Banned` 或 `Disabled`，且 acquired 账号不同，才剥离 `previous_response_id` / `turn_state` 并忘记 affinity；`turn_metadata` 和 `prompt_cache_key` 保留，和 TS `applyCascadingBanDefense` / `applyProxyRetryRecoveryDecision` 一致。
- `QuotaExhausted` / rate-limit / dirty quota rotation 不做预剥离，继续保留现有错误后 history recovery。
- 普通 history recovery 的隐式续链回滚也按 TS `restoreImplicitResumeRequestState` + `applyProxyRetryRecoveryDecision` 对齐：恢复原请求后只清 `previous_response_id` / `turn_state`，不清 `turn_metadata`。

### 3. refresh token 重试策略已收窄

Rust 当前安全点：

- disabled/banned 不参与调度：`crates/runtime/src/tasks/token_refresh.rs:220-229`
- `next_refresh_at` 未来时间会跳过：`crates/runtime/src/tasks/token_refresh.rs:315-319`
- token 更新后会持久化下一次刷新时间：`crates/runtime/src/tasks/token_refresh.rs:581-586`
- 永久失败需达到阈值 2：`crates/runtime/src/tasks/token_refresh.rs:44`、`589-600`
- 请求链路 `token_revoked` / `token_invalid` 先归类为 `expired`；refresh token 自身确认 `invalid_grant` / `invalid_token` / `access_denied` / `refresh_token_expired` 后归类为 `disabled`；`refresh_token_reused` / `account has been deactivated` / `banned` 归类为 `banned`。
- refresh lease 防止同一时刻多进程刷新同一账号：`crates/runtime/src/tasks/token_refresh.rs`
- `RefreshFailure::RetryableTransport` 表示 DNS、连接拒绝、网络不可达、TLS handshake 等 pre-flight 错误，允许同 RT 重试。
- `RefreshFailure::Transport` 表示读响应失败、body parse 失败、未知 HTTP transport 等 ambiguous 状态，不再复用同 RT 重试，直接进入 recovery delay。
- `OpenAiOAuthClient::refresh` 只在 `.send()` 的明确 pre-flight 错误上返回 `RetryableTransport`；读 body 和解析 token 失败都返回普通 `Transport`。
- `TokenRefreshTask::prepare_refresh_attempt` 每次真正调用 refresher 前重读 DB refresh token；如果 DB token 与本次扫描快照不同，直接跳过，避免消费 stale RT。

### 4. Cloudflare challenge cooldown 已覆盖

Rust 已有并已补齐：

- 403 且包含 challenge 信号时识别为 Cloudflare challenge：`crates/runtime/src/services.rs:3087-3093`
- challenge 后删除账号 cookie 并设置 cooldown：`CloudflareRecovery::apply_challenge`
- 账号池 acquire 会跳过 cooldown 中账号：`crates/core/src/accounts/pool.rs:535-549`
- cooldown 持久化：`crates/adapters/src/sqlite/accounts.rs:1375-1389`
- `CloudflareChallengeCooldownTracker` 记录 per-account count 和 updated_at。
- backoff 与 TS 对齐：10 / 30 / 90 / 120 秒，超过 120 秒后封顶。
- 1 小时无 challenge 后重置计数。
- 成功响应后通过 `reset_account_recovery` 清理 challenge cooldown 和 path-block state。

### 5. Cloudflare path-block 基本覆盖

Rust 当前实现与 TS 后续增强基本对齐：

- 空 body 404 识别为 path-block：`crates/runtime/src/services.rs:3095-3100`
- path-block 后清 cookie、计数、三次后 disabled：`crates/runtime/src/services.rs:878-899`
- 阈值 3、1 小时 stale：`crates/core/src/accounts/cloudflare.rs:8-66`
- 成功请求后 reset path-block：`crates/runtime/src/services.rs:2225`、`2369`、`3713-3714`
- cookie 只捕获 `cf_clearance`，请求时按 domain/path 匹配：`crates/adapters/src/sqlite/cookies.rs:11-13`、`95-147`
- HTTP/SSE、compact、stream 和 WebSocket 升级响应带回的 `Set-Cookie` 已在 runtime 层统一 capture；HTTP/compact 非成功状态也会通过 `CodexClientError::Upstream.set_cookie_headers` 传回后 capture，时机与 TS “收到 transport response 先 capture，再分类状态码”一致。

这个方向可以保留。后续真实链路只观察 OpenAI 是否稳定返回可复用的 `cf_clearance`；不因一次响应出现其它 cookie 就直接扩大自动捕获范围。

### 6. 上游身份隔离已覆盖

TS 后续 `bc4b13b` 的核心是：客户端传来的 `prompt_cache_key` / window id 不能跨账号原样复用，必须加 account scope hash。

Rust 当前已实现同等机制：

- `build_conversation_identity` 将 conversation/window 变成 `cp_` / `cw_` hash：`crates/core/src/gateway/conversation.rs:27-46`、`145-168`
- Responses 非流式和流式请求都构造 account-scoped identity：`crates/runtime/src/services.rs:2863-2885`、`2901-2923`
- body 中 `prompt_cache_key` 会替换成 account-scoped `session_id`：`crates/adapters/src/codex/client.rs:1064-1073`
- `client_metadata[x-codex-window-id]` 会写 account-scoped window id：`crates/adapters/src/codex/client.rs:1076-1110`
- OpenAI Responses 翻译层已按 TS `firstRequestString` 从 `client_metadata[x-codex-window-id]` 兜底读取客户端 window id，因此 metadata-only 请求也会进入 account-scoped identity。
- WS header 当前按 TS 的统一 headers 投影：过滤 HTTP/SSE 专用 `content-type`、`accept`，保留 `session_id`、fingerprint 默认头、cookie、`x-openai-internal-codex-residency`、`x-codex-installation-id`、`x-codex-turn-state`、`x-codex-turn-metadata`、`x-codex-beta-features`、`x-responsesapi-include-timing-metrics`、`version`、`x-codex-parent-thread-id`、`x-openai-subagent` 等条件头，见 `crates/adapters/src/codex/client.rs:1192-1228`

这个能力当前应保留，不要回退到原始客户端 ID。

### 7. reasoning replay 已覆盖

Rust 已有：

- TTL 55 分钟、最多 512 条、单条 256KB、总 4MB：`crates/core/src/serving/reasoning_replay.rs:8-10`
- lookup 按 `account_id + conversation_id + variant_hash + response_id` 隔离：`crates/core/src/serving/reasoning_replay.rs:88-108`
- 只保存 reasoning encrypted_content 和 function_call replay 项：`crates/core/src/serving/reasoning_replay.rs:173-238`
- completed 后记录：`crates/runtime/src/services.rs:3812-3873`
- `invalid_encrypted_content` 后驱逐：`crates/runtime/src/services.rs:3876-3905`

这个与 TS `9eb8612` 的方向一致。

### 8. WebSocket 和 fallback 安全已基本覆盖

Rust 当前：

- 使用 OpenAI fork 的 `tokio-tungstenite` / `tungstenite-rs`：`Cargo.toml:52-53`
- WS opening header 顺序明确，包含 `permessage-deflate; client_max_window_bits`：`crates/adapters/src/codex/websocket/connect.rs:377-399`
- fork tungstenite 配置 deflate：`crates/adapters/src/codex/websocket/connect.rs:497-503`
- receive idle timeout 20 秒：`crates/adapters/src/codex/websocket/connect.rs:44`、`769-775`
- WS pool ping 间隔 25 秒、timeout 5 秒：`crates/adapters/src/codex/websocket/pool.rs:16-17`
- `previous_response_id` 强制 WebSocket，不允许 HTTP SSE fallback：`crates/core/src/serving/responses.rs:21-39`

TS 对照：

- `src/proxy/ws-transport.ts` 的 `buildWsConstructorOpts` 只设置 `{ headers }` 和可选 agent，没有手写 `Sec-WebSocket-Extensions`。
- `ws@8.19.0` 客户端默认 `perMessageDeflate=true`，真实 client offer 由库生成。
- `ws@8.19.0` 的 `WebSocketServer` 默认 `perMessageDeflate=false`，旧 TS 测试 helper 默认值不能作为真实客户端不开 deflate 的证据。

仍需注意：

- HTTP SSE 仍是 `reqwest` + `rustls`，不是 TS 的 curl/libcurl/native transport 指纹。
- TLS 指纹不能仅靠 header 判断，必须用真实抓包或服务端反馈继续验证。
- 不应在文档或日志中暴露 token、账号邮箱、refresh token。

### 9. 请求头现状

WS 链路当前先与 TS 发送语义保持一致：

- opening 固定头：`Host`、`Connection`、`Upgrade`、`Sec-WebSocket-Version`、`Sec-WebSocket-Key`
- 业务/指纹头：来自统一 Codex header map，过滤 `content-type`、`accept` 后保留；包含默认 fingerprint headers、`authorization`、`chatgpt-account-id`、`originator`、`openai-beta`、`x-openai-internal-codex-residency`、`x-client-request-id`、`x-codex-installation-id`、`session_id`、Codex context 条件头
- 扩展头：`sec-websocket-extensions: permessage-deflate; client_max_window_bits`

HTTP SSE 链路当前会带：

- base headers：`authorization`、`chatgpt-account-id`、`originator`、`user-agent`、`sec-ch-ua` 和 fingerprint 默认头。
- response headers：`content-type`、`accept: text/event-stream`、`openai-beta`、`x-openai-internal-codex-residency`、`x-client-request-id`、`x-codex-installation-id`、`session_id`、`x-codex-window-id`、`x-codex-turn-state`、`x-codex-turn-metadata` 等，见 `crates/adapters/src/codex/client.rs:788-835`。

注意：TS WebSocket/HTTP SSE 当前都使用 `session_id`；真实 WS 抓包和 OpenAI 原版 Rust 使用 `session-id` / `thread-id`。本轮先保持 TS，后续用真实链路决定是否需要有依据地偏离 TS。

TS `config/fingerprint.yaml` 是 HTTP header 指纹基线，不是 TLS 指纹；Rust 的默认 `Fingerprint` 已覆盖同一组默认 headers、header order，并以 TS `config/default.yaml` 的 `darwin/arm64` UA 为默认基线。本轮复核确认 `auth_domains` / `auth_domain_exclusions` 只在 TS config schema/loading 中出现，未找到发送链路使用点，暂不作为 Rust 必须新增的运行时逻辑。

TS 的 Codex context 字段读取规则是“请求体直接字段优先，`client_metadata` 兜底”。Rust 已补齐该规则，覆盖 `x-codex-turn-metadata`、`x-codex-beta-features`、`x-responsesapi-include-timing-metrics`、`x-codex-window-id`、`x-codex-parent-thread-id`；`turnState` 和 `version` 仍保持直接字段/header 来源。WS header 投影已回到从统一 header map 派生，避免手写白名单漏掉 `x-codex-turn-state` 等 TS 条件头。

WS payload 当前也按 TS `createResponseViaWebSocket` 对齐：`instructions` 即使为空也发送；`tool_choice` / `parallel_tool_calls` 使用 TS 默认值；`reasoning`、`tools`、`include` 仅在 TS 会赋值时发送，不再保留旧 Rust 快照里的 `reasoning:null`、`tools:[]`、`include:[]`。

## 后续推进顺序

建议按这个顺序修：

1. P1：继续用真实链路比 HTTP SSE header、TLS 指纹，以及真实上游 cookie 返回情况。
2. P1：持续跟踪 OpenAI 原版客户端和 TS 后续风控增强，发现差异先记录再修复。

## 不建议做的事

- 不要为了“测试覆盖”先新增大量迁移兼容测试。优先用真实链路验证关键风控行为，再补最小固定测试。
- 不要把 quota rotation 也纳入预剥离。TS 修复明确要求“仅封禁/禁用风险才剥离”，quota rotation 由普通 history recovery 处理。
- 不要把旧项目所有行为都默认照搬。遇到差异先记录事实，再用真实链路和原版请求对比判断。
- 不要扩大到非 OpenAI provider 的兼容层。
