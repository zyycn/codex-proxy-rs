# 测试覆盖审查(backend)

> 审查于 `feat/postgres-redis-migration` 工作树。对象:`backend/tests`(32,961 行、612 个测试函数,单入口 `tests/main.rs`)对照 `backend/src`(42,775 行)。数字均为实测:`wc -l`、`rg -c '#\[(tokio::)?test'`、`rg '#\[cfg\(test\)\]' src/ | wc -l`。

先说结论:这是一套**质量高于平均水平**的纯集成测试。灾难恢复(401/402/403/404/429/5xx 降级链)、WS 断连/超时/复用、token refresh 失败矩阵、self-update 安全属性——这些最容易偷懒的路径反而是全仓最密的,断言是真行为断言而非"没 panic"。真正的盲区集中在三处:**存储故障注入(PG/Redis 运行中不可用)全仓零覆盖**、**真并发争用测试为零**(89 处锁原语全靠顺序测试)、telemetry 的直接测试极薄(0.06 行比)。测试基建(每测试独立 PG 库 + Redis 前缀隔离)是并行安全的,值得保持。

**覆盖热力图**(直属 = `tests/<同名目录>`;多数子系统另有经 `tests/api` 的间接覆盖):

| 子系统 | src 行 | 直属测试行 | 测试数 | 行比 | 间接覆盖 / 备注 |
|---|---|---|---|---|---|
| fleet | 9,946 | 5,003 | 123 | 0.50 | + `api/admin/accounts_routes` 50 个(manage/import/probe/oauth 走 API) |
| upstream | 7,873 | 6,727 | 148 | 0.85 | protocol/transport 双层都测 |
| dispatch | 6,140 | 9,349 | 120 | **1.52** | 全仓最密,恢复/WS/SSE/用量四大块 |
| api | 5,691 | 8,860 | 157 | 1.56 | 含挂在此处的 system/dashboard 等跨子系统测试 |
| telemetry | 4,784 | 278 | 6 | **0.06** | + dashboard 19、usage_routes 4、dispatch/usage_logging 16 |
| bootstrap | 3,209 | 1,408 | 26 | 0.44 | tasks 半数无测试(见 P1-4) |
| update | 1,767 | 0 | 0 | — | `api/admin/system_routes` 23 个端到端,覆盖充分(见误报) |
| infra | 1,008 | 408 | 18 | 0.40 | identity/json/redis 无直测(小件) |
| models | 853 | 340 | 8 | 0.40 | + client/models 4、model_refresh 任务测试 |
| keys | 522 | 127 | 4 | 0.24 | + keys_routes 5、client auth 6 |
| settings | 476 | 0 | 0 | — | api/settings_routes 14 个,够用 |
| auth | 376 | 64 | 2 | 0.17 | + auth_routes 7;logout/过期未测(见 P1-5) |

**测试类型分布与取舍**:src 内 `#[cfg(test)]` 为 **0**——没有任何单元测试,612 个测试全部经 `cargo test --test main` 单二进制跑(417 个 `tokio::test`,其中 3 个 `start_paused`;195 个同步 `#[test]`,集中在 protocol 解析与 AccountPool 纯逻辑,实为"单元风格"测试放在集成树里)。这个取舍对本项目是**净收益**:测的是公开契约,所以 PG/Redis 大迁移(正是本分支)测试大面积存活;代价是私有纯函数(affinity 的 conversation key/variant hash 派生、`api/client/errors.rs` 分类)只能被间接锁定、故障注入受 trait 边界限制、改任何测试都全量重链接。不建议翻转架构,仅按 P2-9 对少数纯函数补直测。

## P0 — 灾难路径盲区

### 1. 存储故障注入零覆盖:PG/Redis 运行中不可用的"降级契约"从未验证
- 现状:全仓测试都假设 PG/Redis 健康(每测试新建库 + 真连接);`rg` 全 tests/ 找不到一处模拟 Redis 断连或 PG 写失败。
- src 侧的设计是 warn-and-continue:affinity 记录失败只 `warn!`(`dispatch/affinity/resolve.rs:211`)、usage/ops 落库失败只 `warn!`(`dispatch/recording.rs:181,248,540`)——即"遥测/亲和失败不拖垮请求"。**这个契约没有任何测试锁定**:未来有人把 `if let Err` 改成 `?`,Redis 一抖动就是全线 500,现有套件全绿。
- 同类未验证路径:refresh lease(`fleet/refresh/lease.rs`)Redis 失败时 token refresh 是跳过还是重复刷新;admin session store 失败时管理台是 401 还是 500;dispatch 中途 PG 断开时流是否照常完成。
- 可测性:`AccountStore`/`AccountUsageStore`/`ModelSnapshotStore` 已是 trait(`fleet/store/mod.rs:231` 等),注入失败实现零成本;`SessionAffinityService` 直接持有 Redis 具体类型(`dispatch/service.rs:91`),需要断连式模拟(连已关闭端口/中途 kill 连接)或补一层 trait。
- 建议:先做 trait 注入版——"PG usage 写失败,响应仍 200 且有 warn 日志";再做 Redis 断连版锁 affinity/lease/session 三条路径。

### 2. 真并发争用测试为零:89 处锁原语全靠顺序测试背书
- 现状:全 tests/ 无一处 `join_all`/`tokio::join!`/`FuturesUnordered`;`fleet/pool/selection.rs` 的 32 个测试全部是**同步顺序** acquire/release;`smart_should_spread_concurrent_requests_by_slot_pressure` 名字带 concurrent,实际是顺序模拟。唯一的真双请求 in-flight 测试是 `responses_should_stagger_same_account_requests_before_sending_upstream`(oneshot 编排,范本级,但只此一个)。
- 风险面:AccountPool slot 记账(泄漏 = 账号永久卡死)、websocket_pool 的 serial-per-conn/bypass 语义在并发 borrow 下的正确性、scheduler feedback EMA 交错写、`reasoning_replay` 的 `Arc<Mutex>`(`dispatch/service.rs:92`)。这些是代理的核心价值路径,顺序测试原理上测不出 race。
- 建议:N 任务 `join_all` 压 acquire/release,断言不变量(全释放后 slot 归零、同账号并发不超 cap、无重复借出);WS pool 并发 borrow 同 key,断言 serial-per-conn 与 bypass 计数。不必上 loom,多线程 runtime + 不变量断言即可。

## P1 — 覆盖薄弱区

### 3. telemetry:4,784 行 src 只有 278 行直测(行比 0.06)
- `account_usage/store.rs`(798 行)**零直接 store 测试**,只经 dashboard/quota 测试间接写读;`usage/query.rs`(725 行)+ `api/admin/usage_routes.rs`(671 行)只有 4 个路由测试——过滤器组合(failureClass/search/时间窗)、游标翻页边界、非法参数基本没测。`recorder.rs`(421 行)经 usage_logging 间接覆盖,但 `record_error` 自身失败分支无测试(与 P0-1 重叠)。
- 已有的间接覆盖不差:dashboard 19 个测试把 billing 计价断言到美元字符串(priority tier、cache read、long context),`usage_logging` 16 个测试锁了 dispatch→落库链路。薄的是**查询/过滤面**——这是管理台排障时最依赖的面,查询 bug = 排障时看到错误数据。
- 建议:补 `PgAccountUsageStore` 直测 + usage_routes 过滤矩阵(每维度一正一负)。

### 4. bootstrap 后台任务半数无测试
- 无直测:`tasks/periodic.rs`(105 行)、`tasks/retention_trim.rs`(70 行,**定时删数据**——store 层 trim 有 `usage_store_trims_by_runtime_retention` 兜底,但任务层触发/参数拼装无测试)、`tasks/cookie_cleanup.rs`(61 行)、`shutdown.rs`(92 行)。
- `coordinator.rs` 仅 2 个测试,其一 `task_coordinator_should_shutdown_without_panicking` 是全仓罕见的"只断言没 panic"式测试。优雅停机不排水在飞请求、任务 panic 后 coordinator 是否重启/放弃,均无覆盖。
- 已覆盖得好的:token_refresh(14 个,见"做得好的")、model_refresh、fingerprint_update、cleanup。

### 5. auth/keys 生命周期缺口
- `logout`(`api/admin/auth_routes.rs:105` → `delete_session`)**零测试**;session 过期后请求被拒未测(TTL 只在 store 层断言了 `1..=300` 秒区间);password 路由仅 1 个测试。已测得好的:登录 throttle、client key 不能当管理密码、登录不写 usage record。
- keys:`flush_pending_last_used`(last_used 批量回写)仅 store 级测试,dispatch 高频路径下 pending 累积/flush 时机无行为测试。

## P2 — 基建健壮性,视精力而定

### 6. flaky 隐患:websocket_pool 测试用真时钟贴边跑
- `tests/upstream/openai/transport/websocket_pool.rs` 6 处真实 `sleep` 配毫秒级时序参数:`ping_interval 1ms + liveness 20ms` 后 `sleep(60ms)`(795-816 行)、`max_age 5ms` 后 `sleep(15ms)`(868-889 行)等。CI 负载高时页边距很紧。
- 修法现成:同目录 `websocket.rs` 已用 `start_paused` 虚拟时钟(3 处);`fleet/quota.rs:724` 的 `wait_for_usage_requests`(deadline 轮询)也是好模板。推广其一即可。

### 7. Redis 测试键不清理
- `TestDatabaseGuard` drop 时 force-drop PG 库(`tests/support/storage.rs`),但 `create_test_redis` 的 `cpr:test:<label>:<uuid>` 前缀键**从不删除**。CI 的 ephemeral 服务无所谓;本地长期开发的 Redis 会键膨胀。Drop 时按前缀 SCAN+DEL,或文档标注定期 FLUSHDB。

### 8. 纯函数直测缺失(单入口的结构性代价)
- `dispatch/affinity/resolve.rs`(460 行)的 conversation key 派生、instructions/variant hash 决定缓存路由正确性,只被 WS 集成测试间接锁定——hash 输入序列化顺序变化这类回归,集成测试报错时定位成本高。`api/client/errors.rs`(373 行)同理。可对这几个函数开 `pub(crate)` 后在 tests/ 直测,或破例加 `#[cfg(test)]`。
- 小件:`infra/identity.rs`、`infra/json.rs`、`settings/{service,store}.rs` 无直测(settings 有 14 个路由测试兜底,可接受)。

## 做得好的(记录一下,免得反复怀疑)

- **恢复路径是全仓最密而非最薄**(审计前的怀疑不成立):`responses_recovery.rs` 37 个测试覆盖 401(过期/封禁/deactivated 文案)、402(quota)、403(Cloudflare 冷却 + 封禁)、404(path block 清 cookie + 三次禁用)、429、5xx 同账号重试,每条都有 stream/非 stream 双变体 + "fallback 耗尽后的终态错误"断言;还锁了重试边界语义(`structural_events_should_not_commit_before_history_failure`、`not_retry_history_failure_after_real_output`)。
- **WS 灾难语义覆盖完整**:terminal 前断连合成 `response.failed`、复用连接死亡后换新连接重试、first-token 超时三变体(fresh/reused/bypass)重试、上游静默 idle 超时、活跃流 stall 超时、账号状态变更驱逐池连接、implicit resume/reasoning replay/affinity 剥离与跨 window 阻断。SSE 侧同样有 abrupt close、无 completed 关闭、客户端断开反向关上游、late disconnect 仍记 usage。
- **测试隔离设计可并行、无共享状态**:每测试独立 PG 库(`cpr_test_<label>_<uuid>`,migrate 后用,Drop 里专线程 force drop)、Redis 每测试 uuid 前缀、库创建限流 semaphore(`storage.rs`)、CI 用真 `postgres:18-alpine` + `redis:8-alpine` services——不是 mock 存储,SQL/事务行为是真验证(`usage_record_and_bucket_are_one_transaction` 这类测试因此有含金量)。
- **mock 上游基建扎实**:wiremock 之外手写 raw TCP 的 SSE/WS 上游(可断言收到的 authorization/header、capture payload、abrupt/clean close 变体、chunked 分帧);64 个数据 fixture(53 个 .sse + 10 个 WS .json + `sqlite_v3.sql` golden)把协议样本从代码里拆了出去。
- **token refresh 失败矩阵**:6 个失败测试(transport 失败重试、重试耗尽延迟恢复、不复用 stale refresh token、invalid_grant 二次确认才置过期)+ 8 个调度测试(lease 持有跳过、双飞去重、定时器边界),正是最容易翻车的生命周期。
- **self-update 经 API 端到端覆盖且含安全属性**:untrusted api base/host、unsafe archive path、checksum 缺失/不匹配、insecure env flag 拒绝、二进制备份失败回滚 web assets、跨文件系统替换、stale lock 清除。
- **断言质量高**:抽查 `responses_recovery`/`selection`/`dashboard` 均为行为断言——精确状态码 + body 字段 + DB 侧副作用 + mock 层 header 匹配;dashboard 计费断言到 `"$8.75"`;schema 测试断言索引/约束存在;import_sqlite 断言 rollback 与 discard 报告。全仓仅个别 no-panic 式测试(P1-4)。
- **stagger 测试的 oneshot 编排**(`responses_http.rs:148`)是无 sleep 的确定性并发编排范本,扩并发测试(P0-2)时照抄即可。

## 核实过的误报

- **"update 子系统零测试"**——`tests/update` 目录确实不存在,但 `api/admin/system_routes` 23 个测试经 HTTP API 端到端覆盖 update/restart/状态/SSE 事件流,深度足够,无需另建目录。
- **"scheduler 只有 score+feedback 11 个测试,调度策略缺覆盖"**——`fleet/pool/selection.rs` 32 个测试覆盖全部 4 种策略(round_robin/sticky/smart/quota_reset priority)含 tie-break 矩阵、slot 上限、tier 优先、plan 过滤。调度**正确性**覆盖实际充分;缺的只是并发争用(P0-2)。
- **"恢复路径薄"**——见"做得好的"第一条,实际是全仓最密。
- **"settings 无测试"**——无直属目录,但 `api/admin/settings_routes` 14 个测试覆盖读写行为。
- **fixtures/ 目录测试数为 0 不是异常**——它是 64 个数据 fixture,非代码。

## 建议动手顺序

1. **P0-1 存储故障注入**:先用现成 trait 注入"PG 写失败仍 200"三件套(usage/ops/affinity 由集成断言锁 warn-and-continue),再补 Redis 断连模拟(lease/session/affinity)。这是唯一贯穿性的灾难盲区。
2. **P0-2 并发不变量**:AccountPool 与 websocket_pool 各一组 `join_all` 压力测试,断言 slot 守恒与 serial-per-conn;编排照抄 stagger 测试的 oneshot 模式。
3. **P1-3 telemetry**:`PgAccountUsageStore` 直测 + usage_routes 过滤矩阵,一次 commit 可完成。
4. **P1-4/5**:retention_trim 任务层、logout/session 过期、coordinator 行为断言,均为小补丁。
5. **P2-6 顺手做**:websocket_pool 时序测试改 `start_paused` 或 wait_for 轮询,防 CI 偶发红;其余 P2 视精力。
