# 代码质量审计与改造计划

日期：2026-06-14

范围：当前仓库 `src/`、`tests/`，不包含 `designs/`。

目标：确认 `/tmp/code_quality_audit.md` 的结论，并基于当前代码给出后续改造顺序。项目处于早期阶段，不需要保留低质量兼容层；改造应以清晰边界、可测试、可维护为目标。

## 审计原则

1. 不为了“拆小文件”而拆文件。文件大小本身不是问题，问题是职责混杂、变更理由混在一起、审查和定位成本过高。
2. 不用“兼容历史”掩盖坏设计。内部 API 可以在测试保护下重构，外部行为必须由测试证明未回退。
3. Rust 代码遵循本项目的 `$rust-best-practices`：明确错误类型、少 clone、少分配、少隐式 panic，但性能优化要有证据。
4. 日志输出使用中文，保留必要英文专业术语，例如 WebSocket、SSE、OAuth、quota、token。
5. 每个改造切片先写或补齐测试，再改生产代码，最后跑目标测试、`cargo fmt --check`、`cargo clippy --all-targets --all-features --locked -- -D warnings`。

## 审计时量化事实

审计开始时 `src/` + `tests/` 约 37,055 行。生产代码里最需要关注的文件：

| 文件 | 行数 | 判断 |
|---|---:|---|
| `src/codex/serving/dispatch/mod.rs` | 1474 | 已有子模块，但主文件仍承担请求编排、上游调用、stream audit、fallback、限流处理等多种职责 |
| `src/codex/gateway/transport/websocket.rs` | 1175 | WebSocket pool、连接建立、请求编码、SSE 转发、错误分类、rate limit 捕获混在同一文件 |
| `src/codex/accounts/repository.rs` | 1103 | 账号 CRUD、token 更新、quota、usage、lease、pool read model 混在同一存储文件 |
| `src/codex/gateway/transport/client.rs` | 783 | 上游 transport 编排较重，但当前还可接受 |
| `src/codex/accounts/pool.rs` | 627 | 调度策略集中，复杂度主要来自业务策略，不是首要拆分对象 |

## Claude 审计确认

### 1. `update_tokens` / `update_from_claims` 重复

结论：成立。

当前 `AccountRepository::update_tokens` 和 `AccountRepository::update_from_claims` 都重复处理：

- `Utc::now().to_rfc3339()`
- access token 加密
- refresh token 可选加密
- expires_at 格式化
- “没有新 refresh_token 时保留旧值”的分支

需要修复，但不建议照搬报告里的 `QueryBuilder` 方案。当前只有几个固定 SQL 形态，动态构建 SQL 会把绑定顺序变成新的维护风险。更合适的第一步是：

- 补测试：`update_from_claims` 省略 refresh_token 时必须保留旧 refresh_token。
- 提取私有 token 写入准备 helper。
- 将 UPDATE SQL 改成命名多行常量。
- 保持 `disabled` / `banned` 不被 `update_tokens` 重新激活的现有语义。

### 2. SQL 字符串地狱

结论：部分成立。

报告里的状态已经有些过期：`LIST_POOL_ACCOUNTS_SQL`、`RECORD_USAGE_SQL`、`GET_USAGE_SQL` 等已是多行常量。但问题仍存在：

- `insert/get/list/list_metadata/update_tokens/update_from_claims/quota/lease/AccountUsageRepository::list/summary` 仍有较多长单行 SQL。
- 一部分 SQL 的业务语义不容易在 review 时看清，例如 token 更新时的状态保护、quota cooldown 的最大值保留、refresh lease 的抢占条件。

建议先使用命名多行常量。`sqlx::query_file!` 不是当前第一选择，因为项目当前使用运行时 `sqlx::query`，切换到宏式编译期校验会引入额外数据库元数据/离线缓存约束，改造面比可读性收益更大。

### 3. `AccountRepository` God Class

结论：部分成立。

`AccountUsageRepository` 已经存在，说明项目已经开始拆 usage 读模型；但 `AccountRepository` 里仍混有：

- 账号 CRUD
- token 更新
- pool read model
- quota JSON 与 cooldown
- usage record/get/window sync
- refresh lease

这确实不是理想边界。后续可以拆，但不应机械拆成很多小文件。推荐按变更理由拆：

- account 基础读写
- token 写入
- quota 状态
- usage 统计写入与窗口同步
- refresh lease
- pool read model

是否暴露多个 repository 类型，需要看调用侧是否真的收益明显。可以先做内部模块拆分，再决定是否调整服务层依赖。

### 4. 超大文件

结论：成立，但“拆文件”不是目标。

三个文件已经大到影响定位和审查：

- `dispatch/mod.rs`
- `websocket.rs`
- `repository.rs`

但文件拆分必须跟职责边界一致：

- `websocket.rs` 适合按 pool、connection、request codec、SSE forwarding、error classification 拆。
- `dispatch/mod.rs` 适合把 stream audit、fallback/rate-limit 应用、上游请求执行拆到更明确的模块。
- `repository.rs` 适合先把 SQL 与行映射收拢，再拆 token/quota/usage/lease。

不建议为了把每个文件压到某个行数而过度碎片化。

### 5. `Account` clone 性能开销

结论：部分成立，不是第一优先级。

`AccountPool` 确实会 clone `Account`，而 `Account` 包含 token 字符串。这个成本存在，但当前设计也有合理性：`AccountPool` 在 `Mutex` 后面，返回 owned `Account` 可以避免把锁生命周期带入 async 上游调用。

直接改成 `Arc<Account>` 不是无成本优化，会牵涉：

- pool 内账号状态更新如何替换 snapshot
- 调度策略如何避免共享可变状态
- token 刷新和 cooldown 更新如何保持一致

建议先完成结构性重构。性能优化应结合 profile 或压测数据，再考虑 immutable snapshot + `Arc<Account>`。

### 6. 字符串分配过度

结论：审计过泛，暂不作为直接改造项。

`.to_string()` / `.clone()` 数量本身不能证明问题。当前很多转换来自：

- JSON/SSE 协议需要 owned `String`
- secret 解密后写入运行时 pool
- header、metadata、测试数据构造
- async 任务跨 await / spawn 的所有权要求

后续可以用 Clippy、profile 和热点路径 review 定点优化，不能按数量批量替换成 `Cow` 或引用。

## 我的补充审计

### P0：token 更新重复和测试缺口

`update_tokens` 已有测试覆盖“没有新 refresh_token 时保留旧值”，但 `update_from_claims` 缺同类测试。由于导入已有账号时会走 `update_from_claims`，这是第一批代码改造前必须补齐的行为测试。

改造目标：

- 增加 `update_from_claims` 保留 refresh_token 测试。
- 提取 token 写入准备逻辑。
- 将对应 UPDATE SQL 改为命名多行常量。
- 保持现有行为，不顺手重构调用层。

### P0：Repository SQL 可读性

`repository.rs` 的 SQL 可读性是当前最明显的维护问题。建议第一阶段只处理账号 repository 内的长 SQL，不扩散到全项目。

改造目标：

- 账号表常用 SELECT 语句提取为常量。
- token/quota/lease/usage summary 的长 SQL 改成多行常量。
- SQL 常量命名表达业务语义，例如 `UPDATE_TOKENS_PRESERVING_DISABLED_STATUS_SQL`。

### P1：Repository 边界

存储层已经有一些 DDD 边界，但 `AccountRepository` 仍承载太多变更理由。推荐在 SQL 可读性改善后再拆，避免一边搬文件一边改 SQL。

推荐顺序：

1. 先把 SQL 常量和 row mapper 收拢。
2. 再拆 token/quota/lease/usage 内部模块。
3. 最后评估是否需要多个 public repository。

### P1：WebSocket transport 边界

`websocket.rs` 当前实现已经覆盖原版核心链路，但职责过密：

- pool lifecycle
- idle probe / GC
- handshake request 构造
- request body 转换
- WS -> SSE 转发
- rate limit 内部事件捕获
- WS 错误分类

建议后续按“协议转换”和“连接池”先拆，不改变行为：

- `websocket/pool.rs`
- `websocket/connection.rs`
- `websocket/codec.rs`
- `websocket/stream.rs`
- `websocket/error.rs`

这个拆分是为了边界清楚，不是为了追求小文件。

### P1：Dispatch 编排边界

`dispatch/mod.rs` 已有 `affinity/fallback/refresh/routing/stream/usage` 子模块，但主文件仍过重。建议把以下逻辑迁出：

- 上游 HTTP SSE / WebSocket stream audit
- quota/rate-limit header 应用
- fallback account retry 决策
- 上游请求执行 wrapper

目标是让 `mod.rs` 更像编排入口，而不是承载所有细节。

### P1：日志一致性

当前日志大部分已经是中文，且不少已使用结构化字段。但仍需继续统一：

- 非中文日志消息需要改成中文，例如 UpdateChecker 相关日志。
- `error = %e`、`account_id = %account_id` 这类结构化字段应保持。
- 不把 token、cookie、refresh_token 放进日志。
- HTTP trace 日志已存在中文入口，应继续检查是否有 request_id/span 传播缺口。

### P2：生产代码里的 `unwrap()`

大多数 `unwrap()` 位于测试模块。生产路径中发现 `usage_snapshots.rs` 的时间桶对齐使用 `and_hms_opt(...).unwrap()`。逻辑上这些时间值可证明有效，但风格上仍建议改成有语义的 helper 或 `expect` 文案，避免以后被复制到不可证明场景。

### P2：测试文件过大

`tests/codex_gateway/websocket.rs` 约 1436 行，已经接近生产大文件的认知成本。测试拆分应按场景拆，而不是一个 endpoint 一个文件：

- WebSocket pool 复用/淘汰
- WebSocket fallback
- WS -> SSE 转换
- rate limit internal event
- error classification

### P2：注释质量

项目里已有中文注释，但后续应保持“解释为什么”，避免解释显而易见的代码行为。适合保留的注释包括：

- token/refresh_token 保留策略
- SSE 响应头已发出后不能回滚 HTTP 状态
- 原版协议兼容的非显然参数
- 安全链路中的指纹/安装 ID/turn state 传递原因

## 推荐改造顺序

### Phase 1：账号 repository P0

目标：先处理最确定、最小风险的问题。

1. 补 `update_from_claims` refresh_token 保留测试。
2. 提取 token 更新准备 helper。
3. UPDATE token/claims SQL 改命名多行常量。
4. 账号 repository 的明显长 SQL 改命名常量。
5. 跑 `cargo test codex_accounts::repository` 或对应过滤测试，再跑 fmt/clippy。

验收条件：

- `update_tokens` 和 `update_from_claims` 不再各自重复 token 加密、expires_at 格式化和 updated_at 生成逻辑。
- `update_from_claims` 省略 refresh_token 时保留旧 refresh_token，有测试覆盖。
- `update_tokens` 仍保持现有保护：`disabled` / `banned` 账号不会因为 token 更新被重新激活。
- token/claims/quota/lease/usage summary 的长 UPDATE/SELECT SQL 不再以内联单行字符串形式散落在方法体里。
- 不引入 `QueryBuilder`，除非实际需要动态字段集合。当前固定 SQL 形态优先使用命名多行常量，降低绑定顺序风险。
- 不调整调用层、不拆 public repository API、不改数据库 schema。
- 验证命令至少包括：
  - `cargo test --test codex_accounts repository --locked`
  - `cargo fmt --check`
  - `cargo clippy --all-targets --all-features --locked -- -D warnings`

Phase 1 不包含：

- `AccountRepository` public API 拆分。
- WebSocket/Dispatch 文件拆分。
- `Account` 改 `Arc<Account>`。
- 全局 `.clone()` / `.to_string()` 批量替换。
- 日志系统重构。

Phase 1 状态：已完成。

已完成内容：

- 增加 `update_from_claims` 省略 `refresh_token` 时保留旧 refresh token 的测试。
- 提取 token 写入准备逻辑，统一 access token 加密、可选 refresh token 加密、expires_at 格式化和 updated_at 生成。
- 将 token/claims 更新 SQL 改为命名多行常量，并保留 `disabled` / `banned` 不被 token 更新重新激活的语义。
- 将账号 repository 内 usage、quota、refresh lease 的长 SQL 和实现迁出到内部模块，public API 保持不变。

验证结果：

- `cargo test --test codex_accounts repository --locked`：13 passed。
- `cargo test prepare_token_write_should_encrypt_tokens_and_format_timestamps --locked`：1 passed。
- `cargo fmt --check`：通过。
- `cargo clippy --all-targets --all-features --locked -- -D warnings`：通过。

### Phase 2：WebSocket transport 边界

目标：在不改变 WebSocket 行为的前提下，按职责拆分 `src/codex/gateway/transport/websocket.rs`。

建议顺序：

1. 先拆错误类型和协议编解码 helper。
2. 再拆连接建立、握手请求构造、消息发送。
3. 最后拆连接池生命周期和 stream 转发。

验收条件：

- WebSocket pool 的 55 分钟 max age、25 秒 ping、串行复用语义保持不变。
- WS -> SSE 输出格式保持不变。
- rate limit internal event、turn state、set-cookie、rate-limit header 捕获保持不变。
- HTTP SSE fallback 行为保持不变。
- 不引入 IP proxy/VPN 相关能力。

Phase 2 状态：已完成。

已完成内容：

- 新增 `websocket/pool.rs`，集中 WebSocket pool key、pool config、55 分钟 max age、keepalive、GC、shutdown、account cap、busy key bypass 等池化生命周期逻辑。
- 新增 `websocket/codec.rs`，集中 upstream request body 转换、WS frame 文本提取、WS event 分类、WS error frame 分类、retry-after/header 提取。
- `websocket.rs` 保留对外入口、连接建立、active connection 归还/丢弃、WS -> SSE stream 编排。
- 没有引入 IP proxy/VPN 能力。

验证结果：

- `cargo test --test codex_gateway websocket --locked`：20 passed。
- `cargo fmt --check`：通过。
- `cargo clippy --all-targets --all-features --locked -- -D warnings`：通过。

### Phase 3：Dispatch 编排边界

目标：让 `src/codex/serving/dispatch/mod.rs` 更像请求编排入口，把可独立变化的细节迁到已有子模块或新的内部模块。

建议顺序：

1. 先确认已有 `affinity/fallback/refresh/routing/stream/usage` 子模块职责。
2. 优先迁出 stream audit 和 upstream response 转换逻辑。
3. 再评估 fallback/retry 决策、quota/rate-limit 应用是否需要继续迁出。

验收条件：

- 外部 API、SSE 输出、fallback 语义、quota cooldown、session affinity 保持不变。
- `dispatch/mod.rs` 不再同时承载所有 stream 细节。
- 每个迁移切片都有目标测试覆盖。

Phase 3 状态：已完成。

已完成内容：

- 新增 `dispatch/audit.rs`，集中 HTTP SSE / WebSocket stream 的审计、usage 记录、上游失败识别和账户 slot 释放。
- 扩展 `dispatch/fallback.rs`，将 fallback retry 的副作用迁入同一模块，包括 quota cooldown、Cloudflare cooldown、ban/quota 状态更新和备用账户获取。
- 新增 `dispatch/limits.rs`，集中 rate-limit header 解析、quota 缓存同步、window 同步和被动 cooldown 应用。
- `dispatch/mod.rs` 保留服务入口、请求编排和上游 client request 构造。上游 request 构造暂不硬拆，避免为了降行数引入复杂 lifetime/owned context 设计。

验证结果：

- `cargo test --test codex_serving --locked`：57 passed。
- `cargo fmt --check`：通过。
- `cargo clippy --all-targets --all-features --locked -- -D warnings`：通过。

### Phase 4：日志、panic 点和测试组织收尾

目标：处理不会改变业务行为但影响长期维护质量的问题。

检查项：

1. 日志消息使用中文，保留 WebSocket/SSE/OAuth/quota/token 等专业术语。
2. 生产代码中避免无语义的 `unwrap()` / `expect()`。
3. 测试文件按场景组织，不为了拆小而过度拆分。
4. 继续避免 token、cookie、refresh token 等敏感信息进入日志。

Phase 4 状态：已完成。

已完成内容：

- 将 fingerprint 更新检查器日志中的 `UpdateChecker` 英文前缀改为中文表达，保留 fingerprint 专业术语。
- 清理 `usage_snapshots.rs` 生产路径中的时间桶对齐 `unwrap()`，改为无 panic 的 helper；测试代码中的 unwrap 保留。
- 将 `tests/codex_gateway/websocket.rs` 中 WebSocket pool 场景迁到 `tests/codex_gateway/websocket/pool.rs`，保留共享 helper 和非 pool 场景测试在父文件。
- 不继续拆更细的测试文件，避免把一个协议场景拆成过多小模块。

验证结果：

- `cargo test usage_snapshots --locked`：4 passed。
- `cargo test --test codex_gateway websocket --locked`：20 passed。
- `cargo fmt --check`：通过。
- `cargo clippy --all-targets --all-features --locked -- -D warnings`：通过。

## 改造后边界

本轮改造没有追求小文件数量，而是按变更理由拆分。当前关键文件规模如下：

| 文件 | 行数 | 当前职责 |
|---|---:|---|
| `src/codex/accounts/repository.rs` | 705 | 账号基础读写、row mapper、pool read model |
| `src/codex/accounts/repository/token.rs` | 203 | token/claims 写入和 refresh_token 保留策略 |
| `src/codex/accounts/repository/quota.rs` | 125 | quota JSON、quota cooldown、Cloudflare cooldown |
| `src/codex/accounts/repository/usage.rs` | 403 | usage 累计、window 同步、usage 列表和汇总 |
| `src/codex/accounts/repository/lease.rs` | 51 | refresh lease 抢占和释放 |
| `src/codex/gateway/transport/websocket.rs` | 506 | WebSocket 对外入口、连接建立、WS -> SSE 编排 |
| `src/codex/gateway/transport/websocket/pool.rs` | 492 | WebSocket pool 生命周期、keepalive、GC、shutdown |
| `src/codex/gateway/transport/websocket/codec.rs` | 206 | upstream body 转换、frame/event/error/header 解析 |
| `src/codex/serving/dispatch/mod.rs` | 1013 | 服务入口、请求编排、上游 request 构造 |
| `src/codex/serving/dispatch/audit.rs` | 329 | HTTP SSE/WebSocket stream 审计和账户 slot 释放 |
| `src/codex/serving/dispatch/fallback.rs` | 255 | fallback retry 分类和副作用应用 |
| `src/codex/serving/dispatch/limits.rs` | 80 | rate-limit header、quota 缓存、window/cooldown 同步 |
| `tests/codex_gateway/websocket.rs` | 880 | WebSocket 非 pool 协议场景和共享测试 helper |
| `tests/codex_gateway/websocket/pool.rs` | 559 | WebSocket pool 复用、淘汰、cap、keepalive、shutdown 场景 |

`dispatch/mod.rs` 仍然偏大，但当前保留的是请求主流程和上游 request 构造。继续硬拆需要引入额外 context 类型或更复杂的生命周期设计，收益暂时低于风险。

## 后续观察项

### 性能优化

目标：只优化有证据的热点。

1. 跑压测或 profile，确认 clone/string 分配是否是瓶颈。
2. 如果 `Account` clone 是热点，再设计 immutable account snapshot 或 `Arc<Account>`。
3. 不做无证据的全局 `Cow`/引用替换。

### 后续日志治理

本轮只处理审计中确认的日志消息一致性问题，没有重构日志系统。后续如果继续做日志最佳实践，应单独落地 HTTP/span/request_id 字段治理，避免和结构重构混在一起。
