# 调度与连接复用审计

本文档记录 Rust 版 `codex-proxy-rs` 与 TS 版 `/home/zyy/Codes/codex-proxy` 的调度审计过程。每个结论必须能回到源码、日志或数据库状态，不把猜测写成事实。

## 当前问题

- 用户在前端选择的是 `least_used` / 智能分配。
- 真实日志中，同一批连续自然文本请求在两个 active 账号之间来回切换：
  - `acct_81aaba2a4e084162924b17d6f55e8a10`
  - `acct_fa9f0172d2084eaf86ddbd0e5d0c1c16`
- 两个账号各自的 WebSocket 连接后续都能 `reuse`，但账号切换会让同一轮对话无法稳定复用同一个账号上的上游连接和缓存。

## TS 对照点

TS 调度入口：

- `/home/zyy/Codes/codex-proxy/src/auth/account-lifecycle.ts`
- `/home/zyy/Codes/codex-proxy/src/auth/rotation-strategy.ts`
- `/home/zyy/Codes/codex-proxy/src/proxy/ws-pool.ts`

TS `least_used` 比较顺序：

1. `cachedQuota.rate_limit.limit_reached` 的账号靠后。
2. 两个账号都有 `usage.window_reset_at` 时，更早 reset 的账号优先。
3. `usage.request_count` 更少的账号优先。
4. `usage.last_used` 更早的账号优先。
5. 只有完整比较结果相同的首组选项，才用 `roundRobinIndex` 轮转。

TS 前端文案是：

> 优先使用即将刷新额度的账号，最大化总使用量。

该文案依赖账号存在可比较的 `usage.window_reset_at`。如果候选账号都没有窗口重置时间，TS 会退化到 `request_count` + LRU，连续请求出现 A/B 交替是符合 TS 比较器的。

TS WebSocket pool 语义：

- pool key 是账号身份加 conversation identity。
- 注释写明 `${entryId}:${conversationId}`，实际目标是同账号同 conversation 复用同一条物理 WS。
- 因此智能分配切换账号时，`prompt_cache_key` 不会丢，但上游连接和缓存命中会跟着账号切换被打断。

## Rust 当前观察

Rust 调度入口：

- `src/upstream/accounts/pool.rs`
- `src/upstream/transport/client.rs`
- `src/proxy/dispatch/responses.rs`

Rust 当前 `least_used` 比较器与 TS 主要顺序一致：

- `quota_limit_reached`
- `window_reset_at`
- `request_count`
- `last_used_at`

Rust WebSocket pool key：

- `src/upstream/transport/client.rs`
- `websocket_pool_key()` 使用 `base_url + account_id + conversation_id`。
- `conversation_id` 来源优先级：`prompt_cache_key` -> `client_conversation_id` -> `previous_response_id`。

Rust 与 TS 一个高风险差异：

- TS 在 `AccountLifecycle.release()` 时调用 `registry.recordUsage()`，请求完成后才更新 `request_count` / `last_used`。
- Rust 当前在 `AccountPool::acquire_with_status_refresh()` 内调用 `mark_usage()`，获取账号时立刻更新 `request_count` / `last_used_at` / `window_request_count`，并且 `RuntimeAccountPoolService::acquire_with()` 立即持久化 `record_request()`。
- 这个差异会影响并发、失败请求、长流式请求中的排序时机。是否是用户看到 A/B 交替的主因还要继续用测试和 TS 行为确认。

## 2026-06-29 日志证据

日志：`.runtime/logs/codex-proxy-rs.2026-06-29.log`

22:11 后连续 `/v1/responses` 流式请求的账号序列：

- `req_5c9267ad...` -> `acct_81a...`，WS `new`
- `req_98cf5322...` -> `acct_fa9...`，WS `new`
- `req_c9babaa5...` -> `acct_81a...`，WS `new`
- `req_133b4d89...` -> `acct_fa9...`，WS `new`
- `req_5171359a...` -> `acct_81a...`，WS `reuse`
- `req_e62aae2e...` -> `acct_fa9...`，WS `reuse`
- `req_0936b7aa...` -> `acct_81a...`，WS `reuse`
- `req_a778e05a...` -> `acct_fa9...`，WS `reuse`
- `req_8c9647cc...` -> `acct_81a...`，WS `reuse`
- `req_960b167e...` -> `acct_81a...`，WS `reuse`

运行时 DB 当前状态：

- `runtime_settings.rotation_strategy = least_used`
- 两个 active 账号的 `window_reset_at` 都是 `null`。
- 两个 active 账号的 `request_count` 都是 `14`。
- 两个 active 账号的 `quota_limit_reached = 0`。

由此可解释：在没有 `window_reset_at` 可比较时，`least_used` 会退化到 `request_count` 和 LRU。连续请求容易在两个账号之间交替；这对缓存稳定不友好，但需要确认是否就是 TS 的预期语义。

## 待确认

- `websocket pool decision` 的 pool key hash 已补；最终流日志是否还需要重复写入 pool key hash，待真实链路日志再判断。

## 已确认偏差

### active 账号的 quota reset 没有同步到 window_reset_at

当前 DB 里两个 active 账号的 `quota_json.monthly_limit.reset_at` 都有值：

- `acct_81a...`：`monthly_limit.reset_at = 1785327099`，`window_minutes = 43200`
- `acct_fa9...`：`monthly_limit.reset_at = 1785325645`，`window_minutes = 43200`

但 `account_usage.window_reset_at` 都是 `null`，导致 `least_used` 无法执行“更早 reset 优先”的第二比较条件，只能退化到 `request_count` + LRU。

源码原因：

- `RuntimeAccountPoolService::apply_quota_snapshot()` 只有在 `quota_snapshot_reset_at()` 返回值时才调用 `sync_rate_limit_window()`。
- `src/upstream/accounts/quota/mod.rs` 当前 `quota_snapshot_reset_at()` 只有在 `monthly_limit_reached()` / `spend_control_limit_reached()` 时才读取 `/monthly_limit/reset_at`；非触顶 active 账号不会把 monthly/core reset 暴露给调度窗口。
- TS `src/routes/shared/proxy-rate-limit.ts` 对被动 rate-limit 同步是只要 `rateLimits.primary.reset_at != null` 就调用 `syncRateLimitWindow()`，不要求触顶。

修复目标：

- 非触顶 core rate-limit 窗口也要返回 reset/window，用于同步 `account_usage.window_reset_at` 和 `limit_window_seconds`。
- 当触顶来源是 monthly/spend control 时，冷却时间仍优先使用 monthly reset。

## 2026-06-29 修复记录

### quota reset 同步

已修改 `src/upstream/accounts/quota/mod.rs`：

- 非触顶 core bucket 只要包含 `reset_at`，`quota_snapshot_reset_at()` 就返回该窗口。
- 非触顶 core bucket 只要包含 `window_minutes`，`quota_snapshot_limit_window_seconds()` 就返回窗口秒数。
- 当 core 窗口不存在时，再回退到 `monthly_limit.reset_at` / `monthly_limit.window_minutes`。
- 当 monthly/spend-control 触顶时，仍优先使用 monthly reset 作为冷却时间。

已修改 `src/upstream/accounts/store.rs`：

- `list_pool_accounts()` / `get_pool_account()` 查询加入 `accounts.quota_json`。
- 恢复运行时账号池时，如果 `account_usage.window_reset_at` 或 `limit_window_seconds` 为空，会从 `quota_json` 推导窗口信息。
- 这不是兼容旧路径，而是把持久化 quota 快照纳入运行时调度输入，避免重启后智能分配退化。

已补测试：

- `quota_snapshot_reset_at_should_use_non_blocking_core_window`
- `quota_snapshot_reset_at_should_prefer_core_window_over_preserved_monthly_limit`
- `account_repository_should_restore_window_from_quota_json_when_usage_window_is_missing`

本轮已通过：

- `cargo test --test main quota_snapshot_reset_at -- --nocapture`
- `cargo test --test main account_pool -- --nocapture`
- `cargo test --test main account_repository -- --nocapture`
- `cargo check`

### 调度排查日志

已修改 `src/upstream/accounts/pool.rs`，账号选择时输出：

- `model`
- `rotation_strategy`
- `selection_source`
- `account_id`
- `candidate_count`
- `request_count`
- `window_request_count`
- `window_reset_at`
- `last_used_at`
- `quota_limit_reached`
- `quota_cooldown_until`
- `previous_slot_at`
- `rotation_cursor`

已修改 `src/upstream/transport/client.rs` / `src/upstream/transport/websocket_pool.rs`：

- `websocket pool decision` 增加 `conversation_id_hash`。
- `websocket pool decision` 增加 `ws_pool_key_hash`，用于判断同账号同 conversation 是否命中同一个 pool key。
- WebSocket fallback warn 已包含 `request_id` / `account_id` / `transport` / `fallback_transport` / `fallback_reason` / `error`。

### release 计数时机

已修改 `src/upstream/accounts/pool.rs`：

- `acquire` 只占用在途槽位，不再更新 `request_count` / `last_used_at` / `window_request_count`。
- 在途槽位记录 `created_at + model`，保留 TS 的 acquire lock 语义，同时让 release 能恢复对应 model。
- `release` 阶段更新 `request_count` / `last_used_at` / `window_request_count`。
- 如果长流式请求的槽位已被 stale cleanup 释放容量，`release` 仍按 TS 的 `recordUsage(entryId)` 语义更新账号级 request/LRU；此时只跳过无法恢复 model 的模型维度 request 计数。

已修改 `RuntimeAccountPoolService`：

- 去掉 `acquire_with()` 阶段的 `record_request()` 和 model request 计数。
- `release()` 阶段持久化账号 request 计数；存在 active slot model 时同步持久化 model request 计数。

这对齐 TS：

- TS `AccountLifecycle.acquire()` 只 `pushSlot()`。
- TS `AccountLifecycle.release()` 才 `popSlot()` 并调用 `registry.recordUsage()`。

本轮已通过：

- `cargo test --test main account_pool -- --nocapture`
- `cargo test --test main responses_should_record_request_count_when_5xx_retries_are_exhausted -- --nocapture`
- `cargo test --test main usage_logging -- --nocapture`
- `cargo test --test main account_repository -- --nocapture`
- `cargo check`

补充审计：

- TS `AccountLifecycle.release()` 先 `popSlot()`，随后无条件调用 `registry.recordUsage(entryId)`；`popSlot()` 找不到槽位不会阻止账号级 request/LRU 计数。
- Rust 已补齐该边界：slot 缺失只影响 model request 计数，不影响账号级 `request_count` / `last_used_at`。
- 已补测试：`release_should_mark_usage_after_stale_slot_cleanup`。

## 当前结论

`least_used` / 智能分配已经按 TS 对齐：

1. quota exhausted / quota limited 账号靠后或按配置跳过。
2. 两个账号都有 `window_reset_at` 时，优先选择更早 reset 的账号。
3. reset 无法比较时，选择更低 `request_count`。
4. request count 相同后，按更早 `last_used_at`。
5. 只有完整比较结果相同的首组选项才轮转。
6. request 计数和 LRU 更新时间已从 acquire 改为 release。

旧日志总是跳账号的原因：

- 当时两个 active 账号的 `account_usage.window_reset_at` 都是 `null`。
- 虽然 `accounts.quota_json.monthly_limit.reset_at` 有值，但旧 Rust 没把非触顶 reset 暴露给调度，也没在恢复账号池时从 `quota_json` 回填。
- 因此智能分配无法执行 TS 文案里的“优先使用即将刷新额度的账号”，只能退化到 `request_count + LRU`。
- 两个账号 `request_count` 又相同，连续请求自然容易 A/B/A/B。
- Rust 旧实现还在 acquire 时立即增加 request/LRU，进一步放大连续请求中的交替。

## 真实链路验证目标

- 修改后重启服务，运行同一组连续自然文本请求。
- 观察 `account selected for upstream request` 的 `window_reset_at` 是否不再为空。
- 在 `least_used` 下，如果两个账号都有不同 reset，应该优先选更早 reset 的账号，而不是仅靠 request_count/LRU 来回跳。
- 观察 `request_count` / `last_used_at` 是否只在 release 后影响下一次选择。

## 2026-06-29 真实链路验证

运行环境：

- 新二进制临时实例端口 `18080`，不影响旧 `8080` 进程。
- 审计目录：`.runtime/real-chain-scheduler-20260629_225737-least-used-release/`
- 请求：两条连续 `/v1/responses` stream，自然文本，`rotation_strategy=least_used`。

结果：

- `least_used_case=1`：HTTP 200，选中 `acct_fa9f0172d2084eaf86ddbd0e5d0c1c16`。
- `least_used_case=2`：HTTP 200，继续选中 `acct_fa9f0172d2084eaf86ddbd0e5d0c1c16`。
- 两次 `account selected for upstream request` 均为：
  - `rotation_strategy=least_used`
  - `selection_source=least_used`
  - `candidate_count=2`
  - `window_reset_at=Some(2026-07-29T11:47:25Z)`
  - `quota_limit_reached=false`
- 第二次选择时，`request_count` 从第一次的 `19` 变为 `20`，`last_used_at` 变成第一次流结束后的时间 `2026-06-29T14:58:36.611782973+00:00`，验证 request/LRU 计数已经发生在 release 后。
- WebSocket pool 第一次 `new`，第二次 `reuse`，`conversation_id_hash=f1128ab74b73`、`ws_pool_key_hash=8899d9457e43` 一致。

结论：

- 修复后 `least_used` 没有再在两个账号之间 A/B 跳转。
- 当前选择 `acct_fa9...` 是符合 TS 文案和比较器的：它的 reset 早于 `acct_81a...`，应优先使用即将刷新额度的账号。

### release 边界补充验证

运行环境：

- 新二进制临时实例端口 `18080`，从既有 run config 启动，不修改 `8080` 配置。
- 使用管理登录创建临时 client API key，测试结束后已删除。
- 审计目录：`.runtime/real-chain-scheduler-20260629_225737-least-used-release/after-release-edge/`
- WebSocket 审计目录：`.runtime/real-chain-scheduler-20260629_225737-least-used-release/ws-audit-after-release-edge/`
- 请求：两条连续 `/v1/responses` stream，自然文本，同一个 `prompt_cache_key`，`rotation_strategy=least_used`。

结果：

- `least-used-after-1`：HTTP 200，选中 `acct_fa9f0172d2084eaf86ddbd0e5d0c1c16`。
  - `request_id=req_aeecdf27-7319-43dd-9abc-2cdf14ea5e5a`
  - `request_count=21`
  - `window_request_count=21`
  - `window_reset_at=Some(2026-07-29T11:47:25Z)`
  - `websocket_pool_kind=Some("new")`
  - `conversation_id_hash=31a3f63f6dd8`
  - `ws_pool_key_hash=5e0f49abf9bb`
- `least-used-after-2`：HTTP 200，继续选中 `acct_fa9f0172d2084eaf86ddbd0e5d0c1c16`。
  - `request_id=req_d60506fb-9553-4cd0-9fca-1f2ceef0d7ff`
  - `request_count=22`
  - `window_request_count=22`
  - `last_used_at=2026-06-29T15:10:33.720793920+00:00`
  - `window_reset_at=Some(2026-07-29T11:47:25Z)`
  - `websocket_pool_kind=Some("reuse")`
  - `conversation_id_hash=31a3f63f6dd8`
  - `ws_pool_key_hash=5e0f49abf9bb`

补充结论：

- 第一条完成后，第二条选择前已看到 `request_count` 从 21 增至 22，说明 release 阶段计数生效。
- 两条连续请求没有 A/B 跳账号；在两个候选都有 reset 且 `acct_fa9...` reset 更早时，智能分配持续选它，符合 TS 文案。
- 两条请求的 `conversation_id_hash` 与 `ws_pool_key_hash` 一致，第二条 WS 复用成功。

## 2026-06-29 二次审计：候选集对齐

继续对照 TS `AccountLifecycle` 后，发现排序器本身已对齐，但候选集还有额外偏差：

- TS 模型 plan 过滤在 `getModelPlanTypes(model)` 有结果时，允许两类账号进入候选集：
  - `planType` 命中该模型的 preferred plans。
  - `planType` 尚未抓取过模型列表，即 `!isPlanFetched(planType)`。
- Rust 旧实现只有 `model -> plans` allowlist，缺少 `isPlanFetched(planType)` 等价状态，导致某个 plan 尚未刷新模型列表时会被提前排除。
- TS 的 `quota.skip_exhausted` 是配置项，默认 true；Rust 旧实现配置层没有该字段，账号池参数固定为 true。
- TS `getDistinctPlanAccounts()` 用账号池当前策略为每个 plan 选择模型刷新账号，并过滤 active、slot、Cloudflare cooldown、quota exhausted；Rust 旧模型刷新直接从 store 里按 plan 取第一个 active 账号，绕过了账号池策略。
- TS `getCapacitySummary()` 不把 Cloudflare cooldown 从容量统计中剔除；Rust 旧容量摘要会剔除，前端容量条和 TS 不一致。

本轮修复目标：

- 账号池保存 `fetched_model_plan_types`，模型 allowlist 命中时只排除“已抓取且不支持该模型”的 plan，未抓取 plan 继续参与调度。
- `quota.skip_exhausted` 加入配置文件和 `QuotaConfig`，默认 true，并映射到账号池 `skip_quota_limited`。
- 模型刷新通过账号池 `distinct_plan_accounts()` 获取每个 plan 的代表账号，使用当前 rotation strategy，并在刷新后 release。
- 模型刷新槽位不绑定具体模型；release 只计账号请求，不写入模型维度 request 计数。
- 容量摘要只按 active、quota、slot 统计，不再排除 Cloudflare cooldown 账号。

新增/调整测试：

- `account_pool_should_filter_by_model_plan_allowlist`
- `account_pool_should_keep_unfetched_plan_when_model_allowlist_exists`
- `account_pool_distinct_plan_accounts_should_filter_like_model_refresh`
- `capacity_summary_should_not_exclude_cloudflare_cooldown_accounts`
- `account_pool_options_should_use_quota_skip_exhausted`
- `model_service_should_refresh_plan_accounts_and_build_routing`

### 二次审计真实链路验证

运行环境：

- 新二进制临时实例端口 `18081`，不影响既有 `8080` 进程。
- 临时配置目录：`.runtime/real-chain-scheduler-20260629_233941-candidate-alignment/`
- 复用现有 SQLite 数据库；临时 client key 通过管理员登录创建，测试结束后已删除。
- 请求：两条连续 `/v1/responses` stream，自然中文文本，同一个 `prompt_cache_key`，`rotation_strategy=least_used`。

结果：

- 模型刷新先通过账号池选择 `plan_type=free` 的代表账号：
  - `account_id=acct_fa9f0172d2084eaf86ddbd0e5d0c1c16`
  - `rotation_strategy=least_used`
  - `candidate_count=2`
  - `window_reset_at=Some(2026-07-29T11:47:25Z)`
- `candidate-alignment-1`：HTTP 200，选中 `acct_fa9f0172d2084eaf86ddbd0e5d0c1c16`。
  - `request_id=req_60d7f1f9-03e7-4297-9e7d-fa5a4588f986`
  - `request_count=25`
  - `window_request_count=25`
  - `window_reset_at=Some(2026-07-29T11:47:25Z)`
  - `websocket_pool_kind=Some("new")`
  - `conversation_id_hash=3bec01931326`
  - `ws_pool_key_hash=900a71d29b76`
- `candidate-alignment-2`：HTTP 200，继续选中 `acct_fa9f0172d2084eaf86ddbd0e5d0c1c16`。
  - `request_id=req_20710963-668b-4c98-9a3c-19e8ef676da8`
  - `request_count=26`
  - `window_request_count=26`
  - `last_used_at=2026-06-29T15:41:04.860145306+00:00`
  - `window_reset_at=Some(2026-07-29T11:47:25Z)`
  - `websocket_pool_kind=Some("reuse")`
  - `conversation_id_hash=3bec01931326`
  - `ws_pool_key_hash=900a71d29b76`

结论：

- 模型刷新账号选择已走账号池策略和日志，不再绕过调度逻辑。
- 两条真实请求没有 A/B 跳账号。
- 同一个 `prompt_cache_key` 下第二条请求复用了同一个 `ws_pool_key_hash`，连接复用符合预期。
