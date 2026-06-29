# 主请求链路延迟与阻塞审计

日期：2026-06-30

本文档记录 `codex-proxy-rs` 从客户端请求进入，到调度账号、请求上游、回流给客户端、最后完成统计写入这条主链路的延迟/阻塞风险。目标是先把审计点落地，后续实现只围绕这些已确认点推进，不把日志、兼容分支或非热路径逻辑混进主链路。

参考实现：

- TS 版：`/home/zyy/Codes/codex-proxy`
- sub2api：`/home/zyy/Codes/sub2api`

## 范围

主链路按下面顺序审计：

1. 客户端请求进入代理。
2. request id / trace / API Key 鉴权。
3. OpenAI 请求解析与模型校验。
4. 调度账号与可选 quota preflight。
5. 发起上游 HTTP SSE 或 WebSocket 请求。
6. 代理把上游流式数据回传给客户端。
7. 流结束后写 usage、账号额度、session affinity、事件记录。

不在本轮重点里的内容：

- `least_used` / 智能分配排序器本身，已由 `docs/scheduler-ts-alignment-audit.md` 记录。
- WebSocket terminal 语义和连接复用细节，已由 `docs/ws-ts-alignment.md` 记录。
- 缓存命中率优化。缓存会受账号调度影响，但本轮重点是主链路是否有不必要堵塞。

## 当前结论

正常日志不是主请求链路的主要堵塞风险：

- 文件日志使用 `tracing_appender::non_blocking`，常规 tracing 写日志不会同步写磁盘。
- HTTP trace 只记录 request / response / failure；没有对 SSE 每个 chunk 做 trace 日志。
- 已补的结构化链路日志只在关键节点写字段，不是逐 token 写日志。

当前最明确的可优化热路径风险是请求上游前的 DB I/O：

- 客户端 API Key 鉴权每次请求都先读 SQLite，再写 `last_used_at`。
- 模型校验每次请求都通过 `ModelService::catalog()` 读取模型快照存储。
- `quota_verify_required=true` 时，请求业务上游前会额外请求 `/codex/usage`，这是有意的 quota preflight，但要在日志里能看出这段耗时。

流式路径不是全量缓冲后再返回客户端，但仍有两个需要明确接受的成本：

- Responses SSE 会预取第一个完整 SSE frame，用来判断首帧错误、历史恢复、额度耗尽等分支。
- 回传时会累计完整 SSE body，流结束后用于 usage 提取、response 解析、事件记录和 session affinity。

## 主链路审计表

| 阶段 | 当前行为 | 风险等级 | 结论 |
| --- | --- | --- | --- |
| tracing / 文件日志 | `src/infra/logging.rs` 使用 non-blocking appender；`src/http/middleware/trace.rs` 不记录 body chunk | 低 | 不是主要延迟来源，继续避免逐 chunk 日志 |
| API Key 鉴权 | `src/proxy/auth.rs` 调 `ClientKeyService::verify()`；`src/admin/keys/service.rs` 里 `verify_and_touch()` 每次 `select` 后立即 `update last_used_at` | 高 | 请求上游前有一次读库和一次写库，写库会带来 SQLite 写锁风险 |
| 模型校验 | `src/proxy/openai/responses.rs` / `src/proxy/openai/chat.rs` 进入 dispatch 前读取 catalog；`src/upstream/models/mod.rs` 当前会 `list_plan_snapshots().await` | 中 | 每次请求前可能读模型快照，适合改为内存 catalog |
| session affinity 查找 | 当前主要走内存结构，命中后决定 preferred account | 低 | 不应成为主要堵塞点，DB 记录发生在流结束后 |
| 账号池 acquire | `src/upstream/accounts/pool.rs` 内存锁内选择账号，只 push in-flight slot，不跨网络 await | 低 | 当前设计可以保留 |
| quota preflight | `quota_verify_required=true` 时在业务请求前调用上游 `/codex/usage` | 中 | 这是正确性优先的有意延迟，需要可观测字段，不应隐藏成普通调度耗时 |
| request interval | 默认 `request_interval_ms=50`，同账号连续请求可能 sleep | 低到中 | 是配置型节流，不是阻塞 bug；需要在排查日志里可见 |
| WebSocket audit artifact | 设置 `CODEX_PROXY_WS_AUDIT_DIR` 时，请求上游 WS 前写 audit 文件 | 中 | 只用于诊断；生产和基准测试不要开启 |
| 首帧预取 | `prefetch_first_sse_chunk()` 最多读到一个完整 SSE frame，受 `MAX_STREAM_PREFETCH_BYTES` 限制 | 中 | 有意等待首帧，用来分类错误；会影响首 token 前延迟 |
| 流式回传 | 上游 chunk 通过 bounded mpsc 回传，channel size 当前为 8 | 低 | 不是全量缓冲后返回；慢客户端会形成背压，这是合理行为 |
| 完整 body 累计 | `send_live_response_stream_chunks()` 每个 chunk 同时写入 `body_bytes` | 中 | 长输出会增加内存和流结束后的解析成本 |
| usage / quota / affinity 写入 | `record_token_usage()`、usage record、session affinity 在流结束后执行 | 中 | 通常不影响首 token，但会影响流任务收尾和下一轮统计可见性 |

## TS 版可借鉴点

### API Key lastUsedAt 不阻塞每次请求持久化

TS `src/auth/api-key-pool.ts` 的行为：

- `ApiKeyPool` 把 API key 条目保存在内存数组。
- `acquireByModelAndCapability()` 选中条目后调用 `markUsed()`。
- `markUsed()` 只更新内存里的 `lastUsedAt`，注释明确写着 `Defer persist - lastUsedAt is non-critical`。
- add / remove / setStatus / setLabel 等管理端变更才立即 `persist()`。

可借鉴结论：

- `last_used_at` 是观测字段，不是鉴权正确性的必要字段。
- 热路径不应该为了它每次同步写 SQLite。
- 鉴权正确性应该依赖内存态 active key 集合；管理端变更负责同步内存态。

### ModelStore 常驻内存，刷新后重建

TS `src/models/model-store.ts` 的行为：

- 启动时从 `data/models-cache.yaml` 加载 plan snapshots 到内存。
- `applyBackendModelsForPlan()` 更新内存 plan snapshot 后调用 `rebuildCatalogFromPlanSnapshots()`。
- `getModelCatalog()` 直接返回 `this.catalog` 的浅拷贝。
- cache 落盘通过 `syncCache()` 异步写文件。

可借鉴结论：

- 模型目录应该是运行时内存视图。
- 刷新任务负责把上游快照写入存储并重建内存 catalog。
- 请求热路径只读内存 catalog，不应该每次请求读快照存储。

## sub2api 可借鉴点

### last_used 延迟写和批量写

sub2api 有两种相关模式：

- `backend/internal/service/deferred_service.go`：`ScheduleLastUsedUpdate()` 先写入内存 `sync.Map`，定时 `BatchUpdateLastUsed()`，失败时把更新放回内存等待下次 flush。
- `backend/internal/service/api_key_service.go`：`TouchLastUsed()` 对 API Key last used 使用 L1 debounce 和 `singleflight`，注释明确说明这是尽力而为，不应阻塞主请求链路。

可借鉴结论：

- last-used 这类观测字段可以延迟、合并、失败重试。
- 即使需要保留 DB 字段，也不需要每次请求同步写库。

### 能力探测和调度快照

sub2api `backend/internal/pkg/openai_compat/upstream_capability.go` 的设计是：

- OpenAI APIKey 上游是否支持 Responses，在创建/修改账号时一次性探测并落到账号 extra。
- 调度快照只带上热路径需要的字段，例如 `openai_responses_supported`、WS 开关、quota 百分比、reset 时间等。

可借鉴结论：

- 上游能力、模型列表、调度输入应尽量提前探测并固化成运行时快照。
- 请求热路径只消费快照，不在每次业务请求里做能力探测或昂贵读取。

## 建议实现边界

### 1. ClientKeyService 改成内存鉴权

目标：

- 启动时加载 enabled client API keys 到内存 map。
- 每次请求只在内存里验证 key 是否存在且启用。
- `last_used_at` 在内存中更新，并通过后台 flush 合并写 SQLite。
- 管理端 create / enable / disable / delete / label update 后同步更新 DB 和内存态。

约束：

- 不保留“请求时 DB verify”兼容分支。
- 不支持外部直接改 SQLite 后自动生效，除非后续明确加 reload 操作。
- `last_used_at` 允许短时间滞后；鉴权启停不能滞后于本进程管理端操作。

建议日志：

- 鉴权失败继续只记录必要字段，不能打印明文 key。
- flush 失败只 warn 并保留待写入项，不能阻塞请求。

### 2. ModelService 改成内存 catalog

目标：

- `ModelService` 持有 `RwLock<ModelCatalog>` 或等价内存快照。
- 启动时从配置和 snapshot store 构建一次 catalog。
- `catalog()` 只 clone 内存快照，不访问 SQLite。
- 模型刷新成功后写 snapshot store，再重建内存 catalog。
- 配置 alias / custom model 更新后同步重建内存 catalog。
- 账号“测试模型”列表也从同一份内存 catalog 读取，避免单独静态配置或额外 DB 读取。

约束：

- 不做请求热路径的兜底 DB 读取。
- snapshot store 失败时，刷新任务报错或保留旧内存 catalog；请求路径不因此阻塞。

### 3. quota preflight 保留，但补观测

目标：

- 保留 `quota_verify_required` 的业务语义。
- 在调度日志或单独 debug/info 日志中记录：
  - `request_id`
  - `account_id`
  - `quota_verify_required`
  - `quota_verify_result`
  - `retry_with_another_account`

约束：

- 不为了减少延迟跳过 quota verify。
- 不把 quota verify 失败简单写成普通 upstream request 失败。

### 4. WS audit 明确诊断开关

目标：

- 保持 `CODEX_PROXY_WS_AUDIT_DIR` 默认关闭。
- 文档和真实链路测试脚本里明确：开启后会在 WS 上游请求前写 audit artifact，不适合性能基准。

约束：

- 不在生产路径默认写 audit 文件。

### 5. 流式 body 累计后续再评估

当前完整 body 累计服务于 usage、final record、session affinity 和 response 解析。短期先不拆。

后续如果要降内存占用，应单独设计：

- 一边 streaming parse usage / terminal event。
- 一边只保留必要摘要或截断后的 body。
- 保证 usage record、session affinity、错误分类不退化。

这不是本轮第一优先级。

## 临时耗时字段验证

本轮曾临时增加下面这些阶段耗时字段用于真实链路验证：

- `auth_ms`
- `model_catalog_ms`
- `account_acquire_ms`
- `request_interval_ms`
- `quota_verify_ms`
- `upstream_connect_ms`
- `first_frame_ms`
- `first_token_ms`
- `stream_finalize_ms`

验证结论：

- 区分 DB 热路径、调度等待、quota preflight、上游连接和模型推理。
- 已确认 API key 鉴权、模型目录读取、账号 acquire 在当前链路里不是主要堵塞点。
- 这些临时 debug 打点已从常驻代码移除，避免默认运行时日志冗余。

约束：

- 后续如需重新排查阶段耗时，应临时打开或引入明确诊断开关。
- 不记录请求正文、API key、access token、cookie 等敏感内容。
- 不对每个 SSE chunk 记录日志。

## 后续测试项

### 单元/集成测试

- API key verify 在初始化后不访问 store。
- 管理端 create / enable / disable / delete key 后，运行时鉴权内存态立即生效。
- `last_used_at` flush 合并多次 touch，失败后保留待重试。
- `ModelService::catalog()` 多次调用不访问 snapshot store。
- 模型刷新成功后重建内存 catalog，刷新失败时保留旧 catalog。
- 账号测试模型列表来自同一份 `ModelService` catalog。
- quota preflight 的成功、耗尽、失败分支都有可观测字段。

### 真实链路测试

- 连续自然文本 `/v1/responses` stream，确认首 token 前没有 API key 写库和模型快照读库。
- HTTP SSE 和 WebSocket 各跑连续请求，确认首 token 前没有 API key 写库和模型快照读库。
- 人为设置账号 `quota_verify_required=true`，确认只该账号触发 `/codex/usage` preflight，并能从日志看到耗时。
- 关闭 `CODEX_PROXY_WS_AUDIT_DIR` 后跑 WebSocket 基准；开启时只用于抓 artifact，不拿来评估性能。
- 管理端禁用 client key 后立刻用旧 key 请求，应被拒绝。
- 管理端刷新模型后，测试链接里的模型下拉和真实请求模型校验看到同一份 catalog。

## 当前优先级

1. 先改 API key 鉴权热路径，移除每请求 DB write。
2. 再改 ModelService catalog 热路径，移除每请求 snapshot store 读取。
3. 补 quota preflight 必要状态日志，并用临时耗时字段验证关键阶段。
4. 真实链路验证 HTTP SSE / WebSocket 两条路径。
5. 最后再评估流式 body 累计是否需要拆成 streaming parse。

## 2026-06-30 实施记录

已完成：

- API Key 鉴权热路径改为运行时内存表。启动时加载 enabled client API keys；管理端 create / enable / disable / delete 后同步更新内存表；成功鉴权只入队 `last_used_at`，由后台 flush 合并写 SQLite。
- `SqliteClientKeyStore` 不再作为请求鉴权 store 使用，只保留 CRUD、加载 enabled keys、更新 `last_used_at` 等持久化能力。
- ModelService 增加内存 catalog。启动/显式 reload 从 snapshot store 加载；`catalog()` 和 `model_plan_routing()` 只读内存；模型刷新成功后重载内存 catalog。
- 启动流程在对外接请求前调用 `Services::initialize_hot_path_state()`，加载 client key cache 和 model catalog。
- 账号测试模型列表已经走同一份 ModelService catalog；测试中如果先构造服务再手工插入 snapshot，需要显式 reload，符合运行时语义。
- 临时补充过阶段耗时 debug 日志用于真实链路验证；验证后已清理，只保留必要错误/状态日志。

已验证：

- `cargo check`
- `cargo test --no-run`
- `cargo test --test main client_key -- --nocapture`
- `cargo test --test main model_service -- --nocapture`
- `cargo test --test main models_route -- --nocapture`
- `cargo test --test main responses_routes -- --nocapture`
- `cargo test --test main responses_should_passively_cache_rate_limit_headers -- --nocapture`
- `cargo test --test main model_refresh_task -- --nocapture`
- `cargo test --test main responses_websocket_should_route_previous_response_id_to_recorded_account -- --nocapture`
- `cargo test --test main responses_should_stagger_same_account_requests_before_sending_upstream -- --nocapture`
- `cargo test --test main`

## 2026-06-30 真实链路验证

运行方式：

- 使用当前真实 SQLite：`.runtime/data/codex-proxy-rs.sqlite`。
- 保持已有 `8080` 旧进程不动，临时启动当前构建产物到 `127.0.0.1:18090`。
- 本次日志目录：`.runtime/real-chain-samedb-20260630_005228/logs`。
- 未开启 `CODEX_PROXY_WS_AUDIT_DIR`。
- 通过管理员 API Key 创建临时 client key；验证结束后已删除，匹配 `real-chain-hotpath-%` 的测试 key 数量为 0。
- 验证实例已停止，验证后只剩原 `8080` 进程监听。

验证结果：

- 新建 client key 后立刻请求 `/v1/models` 成功，说明管理端 create 后运行时鉴权内存态立即生效，不需要重启。
- 新 key 初始 `last_used_at=null`；请求 `/v1/models` 后后台 flush 写回 `last_used_at`，证明 last-used 已从同步热路径写库改成异步落盘。
- `/v1/models` 返回模型：`codex-auto-review`、`gpt-5.4-mini`、`gpt-5.5`。
- 连续 4 次真实 `/v1/responses` stream 均返回 `response.completed` 和 `[DONE]`，usage record、account usage 均写入同一个真实库。
- 4 次请求均走 WebSocket，选中账号均为 `acct_fa9f0172d2084eaf86ddbd0e5d0c1c16`；两个 `quota_exhausted` 账号没有被调度。
- 当前真实库 `rotation_strategy=least_used`，不是 `sticky`。
- 本次未触发 quota preflight；两个 active 账号的 `quota_verify_required=0`，没有为了验证而修改真实账号状态。

请求摘要：

| 场景 | request_id | response_id | 耗时 | transport | WS 复用 | input | cached | output | first_token_ms |
| --- | --- | --- | --- | --- | --- | ---: | ---: | ---: | ---: |
| 短输入 1 | `req_07ae7ef0-6b94-49f7-ab65-f135801fcc56` | `resp_0020ce295f4d7ab0016a42a386ad988193a1aabd3ae37ec8c8` | 79.226s | websocket | new | 111 | 0 | 1918 | 10084 |
| 短输入 2 | `req_adb730f5-a4bd-44d7-83c0-a0dcc76cf716` | `resp_0921c12f99f4ba2f016a42a42faf388193a53bd68020d36bbd` | 73.032s | websocket | retry_after_stale_reuse | 111 | 0 | 1842 | 6544 |
| 长输入缓存 1 | `req_b9d465e9-cd5f-4622-ae97-49be116cb8e0` | `resp_07c61104cedca07d016a42a4bd01b88195984914845ff9d13a` | 42.096s | websocket | new | 2582 | 0 | 993 | 6307 |
| 长输入缓存 2 | `req_86d0a685-a683-45fe-94b1-cc53195f1c55` | `resp_0a7a60b075937c57016a42a501007081948daa26ea9e521dfa` | 38.995s | websocket | reuse | 2582 | 2304 | 1042 | 1759 |

缓存结论：

- 短输入两连的 `cached_tokens=0`，输入只有 111 tokens，不足以证明缓存异常。
- 长输入两连使用同一个上游 `prompt_cache_key=cp_b94d4b2e28dfb90fae377a33beba0a3a`、同一个账号，第二次 `cached_tokens=2304`，缓存生效。
- 第二次长输入的 WS pool 为 `reuse`，首 token 从 6307ms 降到 1759ms。

日志观察：

- 默认 `info` 日志可直接看到：
  - HTTP request/response 和 `latency_ms`
  - `account selected for upstream request`
  - `websocket pool decision`
  - `live response stream finalized`
  - `first_token_ms`
  - `transport`
  - `websocket_pool_kind`
  - `response_id`
- 阶段耗时 debug 字段已经用于本次验证并在验证后从常驻代码移除。需要再次排查时，应临时打开诊断打点或增加显式诊断开关。
