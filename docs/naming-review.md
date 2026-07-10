# 命名审查(backend/src)

> 复核于 `5f2839db`(两个 `refactor: ...architecture` 提交之后)。上一版审查写于旧结构,本次已按当前代码逐条核对——多条被重构消化,状态标注在每条下。

对照 Rust API Guidelines(C-CASE / C-WORD-ORDER)与社区惯例,对 `backend/src` 全量模块与公开类型做走查。整体质量不错——蛇形/驼峰规范到位、函数名没有 `mgr`/`calc`/`do_` 一类缩写、没有 `utils`/`helper`/`misc` 垃圾桶模块、trait 抽象层与 `Pg*`/`Redis*` 具体实现分离得当。

## ✅ 已被重构解决(本轮复核确认)

- **`AccountStore as AccountStoreTrait` 别名** —— 已消失,全仓 grep 不到 `AccountStoreTrait`。上一版的 P0 关闭。
- **旧 `accounts/service.rs` / `AccountService`** —— 已删除;`quota_available_at` / `cloudflare_available_at` 下沉到 `fleet/scheduler/candidates.rs` 成为模块内自由函数,不再挂在一个空壳 service 上。
- **`telemetry/usage/store.rs` + `store/` 双层** —— 已拍平,现在只有 `telemetry/usage/store.rs` 单文件,`store/query.rs` 那层没了。上一版 P1-3 关闭。
- **`dispatch/` 整体重排** —— `responses/` 子树拆成 `dispatch/recovery/`(auth/cloudflare/exhaustion/implicit_resume/reasoning_replay)与 `dispatch/stream/`(lifecycle/live/prefetch/sse_failure/trace),二者**都是纯 `mod.rs` 风格**,顶层 `service.rs`/`upstream_call.rs`/`recording.rs` 是无同名目录的纯文件。新结构没有引入任何 `foo.rs + foo/`,等于给下面第 1 条的 `mod.rs` 方向做了示范。
- **`refresh/runtime.rs + runtime/`、`protocol/websocket.rs + websocket/`、`transport/client.rs + client/`、`transport/websocket.rs + websocket/`、`admin/dashboard_routes.rs + dashboard_routes/`** —— 这几处 `foo.rs + foo/` 都在重构里被拍平(改用 `policy.rs`/`websocket_errors.rs`/`client_sse.rs`/`websocket_frames.rs` 这类平铺兄弟文件,或直接并入单文件)。

## ✅ P1 — 结构性不一致（本轮已完成）

### 1. `mod.rs` 与 `foo.rs + foo/` 两种模块风格混用
- 结果:全仓已统一到 `mod.rs` 风格,原有 5 处入口已迁移为:
  - `fleet/pool/mod.rs`
  - `fleet/scheduler/mod.rs`
  - `fleet/store/mod.rs`
  - `api/admin/accounts_routes/mod.rs`
  - `bootstrap/import_sqlite/mod.rs`
- 决定:**目录模块使用 `foo/mod.rs`,叶子模块使用 `foo.rs`**,并由架构门禁禁止 `foo.rs + foo/` 并存。理由:
  - 无重名——不会出现 `scheduler.rs` 和 `scheduler/` 一文件一目录并存。
  - VSCode/资源管理器里文件夹排在上、文件排在下,`foo.rs + foo/` 会把同一模块的入口和主体分到两个区,视觉割裂;`mod.rs` 风格下"一个目录 = 一个模块",树形一眼到底。
  - 标准库(rust-lang/rust 本体)至今以 `mod.rs` 为主;`cargo fmt` 对两种风格都不强制,纯团队口味。
  - 代价(多个标签页都叫 `mod.rs`)可接受——现代编辑器标签页会显示父目录名消歧。
- 做法:入口文件仅做路径迁移,内容与 `mod` 声明不变;对应测试入口同步使用相同风格。

## ✅ P2 — 语义命名（本轮已完成）

### 2. `RuntimeXxxService` 前缀家族
- 结果:领域服务统一为 `AccountPoolService`、`SessionAffinityService`、`TokenRefreshService`、`QuotaRefreshService`、`SettingsService`;对应错误类型同步收敛为 `AccountPoolError`、`SessionAffinityError`。
- `SettingsService` 现在是唯一设置服务:原无状态类型的 `apply_patch` 合并进持有 PG store、watch sender 与更新锁的服务,不保留同义类型或兼容别名。
- `RuntimeConfig`、`RuntimeFingerprint`、`RuntimeRefreshPolicy`、`RuntimeAccountStateSnapshot`、`RuntimeHealthProbe`、`RuntimeProcessControl` 等保留:这些名称确实表达配置转换后的运行时值、快照、策略或进程设施,不是领域服务的冗余前缀。

### 3. WebSocket 连接池设置与运行时配置命名（本轮已完成）
- `bootstrap/config.rs` 使用 `WebSocketPoolSettings`:表示配置文件反序列化得到的启动设置,保留 `max_age_ms: u64` 等外部表示。
- `upstream/openai/transport/websocket_pool.rs` 使用 `CodexWebSocketPoolConfig`:表示已转换为 `Duration` 等运行时类型的上游连接池配置。
- 结果:名称直接体现“外部设置 → 运行时配置”的边界,不再由两个近似的 `*Config` 名称承载不同阶段。

### 4. `accounts` 领域模块名（本轮已完成）
- 决定:领域目录与公开模块统一改为 `fleet`,表达“被池化、调度、轮转和刷新的上游账号机队”,并避免与内部 `pool` 重名。
- `backend/src/fleet` 与 `backend/tests/fleet` 已同步迁移,全仓领域引用使用 `crate::fleet::` / `codex_proxy_rs::fleet::`,不保留 `accounts` 兼容模块或 re-export。
- `Account` 实体、`accounts` 数据库表、`/api/admin/accounts` 路由、`accounts_routes` HTTP 模块与 `support::accounts` 测试构造器仍准确表达“账号集合”,不随领域边界名改动。

## 无需改动(记录一下,免得反复纠结)

- `Pg*` / `Redis*` 前缀:这是**具体实现**(`PgAccountStore` 实现 trait `AccountStore`),技术前缀标明后端合理,不算泄漏。
- `Codex*` 前缀:上游协议类型统一带 `Codex`,和领域内类型区分清楚,保留。
- `infra`:内含 database/redis/json/time/paths/logging/identity/format,是名副其实的基础设施层,不是垃圾桶,保留。
- `RequestId(String)` / `ClientIp(String)`:newtype 命名规范,位置(`api/middleware`)合理。
- 函数命名:全量抽查未见缩写/非惯用后缀,`init_tracing` 等符合惯例。

## 本轮执行范围

1. **P1 第 1 条**:已完成。
2. **P2 第 3 条**:已完成。
3. **P2 第 2 / 4 条**:已完成。
