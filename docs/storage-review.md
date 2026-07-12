# 存储层审查(backend)

> 审查于 `feat/postgres-redis-migration` 工作树(fleet 改名之后)。覆盖 `infra/{database,migrations,redis}.rs` 连接与迁移、`fleet/store`、`telemetry/{usage,ops,buckets,account_usage}`、`keys/store`、`settings/store`、`fleet/cookies`、`upstream/openai/fingerprint/store`、四个 Redis 存储(session/lease/affinity/models)、`bootstrap/import_sqlite`、保留期 trim 任务与 `deploy/docker-compose.yml`。对照基准是 `docs/database.md`(唯一权威文档)。

先说结论:实现与 database.md 的契约**高度一致**——终态 DDL 逐字落地、事实+桶同事务、Redis 互斥走 Lua、keyset 分页配套索引、测试断言 schema。担心的"迁移演进不存在"是误判:自研迁移框架有版本表、幂等、单事务、防降级,加列 = 往 `MIGRATIONS` 数组追加一项。真正的问题集中在三类:**第一次 schema 演进时的流程脆弱点**(P0,常量漂移会造成确定性启动失败)、**多步写缺事务**(P1,账号导入)、**写而不读的表和索引**(P1/P2,白付写放大)。

## Redis 键全景(盘点)

前缀统一 `cpr:`(`bootstrap/services.rs:469` 传入,`infra/redis.rs:49-54` 规范化;测试用随机前缀)。与 database.md §4B 逐键比对无缺漏、无文档外的键:

| 键 | 结构 | TTL | 位置 |
| --- | --- | --- | --- |
| `cpr:admin:session:<sha256>` | String(JSON) | `SET EX`,默认 1440 分钟(`config.rs:300`),登出 DEL,不滑动续期 | `auth/store.rs:94-136` |
| `cpr:lease:refresh:<account_id>` | String(owner) | `SET NX PX` + Lua 同 owner 续约/比较释放 | `fleet/refresh/lease.rs:49-87` |
| `cpr:affinity:resp:<response_id>` | String(JSON) | `EXAT created_at+4h` | `dispatch/affinity/store.rs:56-61` |
| `cpr:affinity:conv:<conversation_id>` | ZSET | `EXPIRE 4h` 每写刷新 + `ZREMRANGEBYSCORE` 惰性剪枝 | `store.rs:63-84` |
| `cpr:affinity:account:<account_id>` | SET | `EXPIRE 4h` 每写刷新,**成员无剪枝**(见 P2-7) | `store.rs:72-79` |
| `cpr:models:plan_snapshots` | HASH(field=plan_type) | 无 TTL,HSET 最新态覆盖(field 只增不删,见 P2-8) | `models/store.rs:62-99` |

无 TTL 的只有模型缓存 HASH(文档许可的"最新态覆盖"形态);不存在游离的永久 String 键。命名全部符合 `cpr:<域>:<实体>:<键值>` 规范。

## P0 — 正确性风险(第一次迁移演进就会踩)

### 1. `CURRENT_SCHEMA_VERSION` 与 `MIGRATIONS` 手工同步——漏改的后果是"上线成功、重启失败"
- 现状:`infra/database.rs:7` 的 `const CURRENT_SCHEMA_VERSION: i64 = 1` 与 `:8-12` 的 `MIGRATIONS` 数组是两个独立的手工维护点;`validate_applied_migrations`(`database.rs:82-99`)在**每次启动、应用迁移之前**检查 `applied.last() > CURRENT_SCHEMA_VERSION` 即报错。database.md §4.1 只用一句"与迁移列表同步提交"约束这个同步。
- 问题:推演"上线后加一列"的标准动作——追加 `Migration { version: 2, ... }` 但忘了把常量改成 2:**第一次启动完全正常**(校验时 applied=[1] 通过,随后应用 v2 并落表);**第二次启动必然失败**(applied=[1,2],last=2 > 1 → `unsupported PostgreSQL schema version 2`),从此 boot loop,直到改代码重新发版。故障错开一个重启周期出现,极难在发布验证中发现;Docker `restart: unless-stopped` 会把它放大成持续崩溃重启。没有任何测试守护这个同步(`tests/infra/database.rs` 只断言 0001 的结果)。
- 建议:删掉手工常量,派生之——`MIGRATIONS.last().map(|m| m.version)`;或至少加 `const _: () = assert!(...)` / 单元测试断言两者一致。一行改动消灭整类事故。

## P1 — 正确性风险

### 2. 已应用迁移无 checksum,"已发布迁移永不修改"只靠自觉
- 现状:`schema_migrations` 只记 `(version, name, applied_at)`(`database.rs:121-134`),不存 SQL 摘要;`migrate()` 对已应用版本直接 skip(`database.rs:47-50`)。sqlx 自带的 `migrate!` 宏有 checksum 校验,本项目自研框架没有。
- 问题:有人事后编辑 `0001_initial.sql`(哪怕只是"顺手修个注释里的列名"),存量库与新装库从此走上两条 schema,且**永远不会报错**。这类漂移只有在某条查询炸掉时才暴露。
- 建议:`Migration` 增加 `sql` 的 SHA-256,应用时落库、启动时对已应用版本比对,不匹配即拒绝启动。顺手把"迁移演进测试模板"补上:一个从 v1 库跑 `migrate()` 到 v2 并断言列存在的测试,给未来第一条真实迁移打样。

### 3. 账号导入的多步写不在事务里
- 现状:`fleet/manage/import.rs:190-211` 新账号路径是三条独立语句:`store.insert(account)` → `store.set_next_refresh_at(...)` → `store.apply_imported_quota_state(...)`;`:170-189` 更新路径同样是 `update_from_import` → `set_next_refresh_at` 两步。每步失败都映射成 `AccountManageError::Import` 返回,但已执行的写不回滚。全仓只有 5 处 `begin()`(usage append、ops append、bucket rebuild、migrate、import_sqlite),账号域一处都没有。
- 问题:中途失败留下半初始化账号——最典型是 insert 成功、quota 状态未落:`quota_verify_required` 丢失意味着下一轮调度不会强制校验配额,导入进来的 quota_exhausted 账号可能被当健康账号用。`next_refresh_at` 丢失可由 token 刷新调度器自愈(`refresh/service.rs:234` 会 persist),quota 标志则要等周期 quota refresh 兜底,窗口内行为不对。
- 建议:给 `PgAccountStore` 加一个接受 `&mut Transaction` 的导入写路径(或直接把三条 SQL 合成一条带 CTE 的语句);import 单账号 = 单事务。同类小问题:`account_usage/store.rs:513-554` 的 `update_rate_limit_window` 是"SELECT 判断 → 二选一 UPDATE"的 read-then-write,两个并发窗口同步可能双双判为不重置;单进程 + 每账号串行调度下概率极低,顺手改成单条 SQL(把 `should_reset_usage_window` 判定内联进 `case when`)即可闭环。

### 4. `account_model_usage` 全库零读者——每请求白付一次 upsert
- 现状:写路径活跃——每次槽位释放/用量观测各一次 `RECORD_MODEL_USAGE_SQL` upsert(`telemetry/account_usage/store.rs:49-60`,由 `fleet/pool/mod.rs:218-236,389-409` 调用);但**全仓没有任何 SELECT 读它**(唯一出现处是 import_sqlite 的搬迁写入)。账号详情页的"当前窗口模型分布"实际走的是 `request_time_buckets` 的 `model_usage_by_windows`(`api/admin/accounts_routes/query.rs:311-316` → `telemetry/buckets/query.rs:203-236`)。database.md §4.7 宣称的两个读者(详情页、调度辅助 `error_count` 降权)在代码里都不存在。
- 问题:热路径上每请求多一次 upsert + `idx_account_model_usage_last_used` 索引维护,收益为零。更麻烦的是它按 §2.10 属"不可重建"数据——现在不决定,数据继续积累,以后退役的沉没成本只会更高。
- 建议:二选一并同步改文档——(a)把详情页"生命周期模型分布"接到这张表上(buckets 只有 90 天,两者口径本就不同);(b)确认没有该需求,删表删写路径。这是产品口径决策,不是纯技术项,但**保持现状是最差选项**。

## P2 — 性能与纪律

### 5. 四枚无读者索引,违反 §2.8"每个索引对应一条真实查询"
- `idx_accounts_status`(`0001_initial.sql:63`):没有任何查询按 status 过滤 accounts——池加载是全表 `LIST_POOL_ACCOUNTS_SQL`(`fleet/store/queries.rs:9-16`),状态过滤全在内存(`dashboard_routes.rs:315-335`)。
- `idx_request_time_buckets_model`(`:237-238`):三条 bucket 查询(`buckets/query.rs:27,161,221`)的谓词分别是 bucket_start 范围、account_id IN + 范围、`model != '__unknown__'`(不等式),没有一条能把它当驱动索引。**这是全库最热写入表**,每请求 upsert 都在维护它。
- `idx_account_usage_last_used_account`(`:100-101`):唯一按 last_used_at 排序的查询用的是 `coalesce(au.last_used_at, 'epoch')` 表达式(`account_usage/store.rs:570-580`),普通列索引匹配不上;表本身每账号一行,量级无所谓。
- `fingerprint_update_history` 两枚(`:284-287`):该表也是只写不读(仅 `fingerprint/store.rs:147-172` insert)——审计表可以接受无代码读者,但 `created_id` 这种 keyset 分页索引显然为不存在的列表 API 预建,与"不预建可能有用"的纪律冲突。
- 建议:删 `idx_accounts_status` 与 `idx_request_time_buckets_model`(后者有真实写收益);`idx_account_usage_last_used_account` 要么删、要么改成 `coalesce(...)` 表达式索引与查询对齐;fingerprint 两枚随表的去留一起定。

### 6. 错误桶的 service_tier 维度:live 写入与 rebuild 口径不一致
- 现状:`upsert_error` 把内存里的 `event.service_tier` 当维度写桶(`buckets/store.rs:116`,recorder 会从 metadata 提升它,`recorder.rs:250-253`),但 `ops_error_logs` **没有 service_tier 列**(读取时硬编码 `service_tier: None`,`ops/store.rs:314`);`rebuild_reconstructible_range` 重算时错误行只能归 `'__unknown__'`(`buckets/store.rs:207`)。
- 问题:同一批错误,live 写入落在 `(model, 'priority')` 桶,rebuild 后并进 `(model, '__unknown__')` 桶——总量守恒但维度分布漂移,`rebuild-buckets` 的"验收 = 聚合值一致"对 error_count 分维度不成立。
- 建议:要么 `upsert_error` 也统一传 `None`(错误事实本就不承载计费维度,最小改动);要么给 ops_error_logs 增列(§4.9 预留清单路径)。前者更符合现有文档口径。

### 7. `cpr:affinity:account:<id>` SET 活跃期内只增不减
- 现状:每个成功响应 `SADD` 一个 response_id 且 `EXPIRE` 刷新 4h(`dispatch/affinity/store.rs:72-79`);conv ZSET 每写有 `ZREMRANGEBYSCORE` 剪枝(`:80-84`),account SET 没有等价物;`record_response_affinity` 记录新响应时也不 forget 前链(`dispatch/affinity/resolve.rs:170-222`,forget 只在 implicit-resume 恢复路径出现)。
- 问题:持续活跃(写间隔 < 4h)的账号,SET 成员数 ≈ 自上次 4h 空闲以来的全部成功响应数。自托管场景每天总有空闲窗清零,内存上界 ≈ 单账号单日响应量(几 MB 级),不致命;但删除账号时 `forget_account` 是 SMEMBERS 后逐成员 GET+pipe(`store.rs:171-181`),几万成员就是几万次往返。
- 建议:低成本版——upsert 时对 account 键也做惰性剪枝(SET 换 ZSET,score=createdAt,复用 ZREMRANGEBYSCORE);或维持现状但在文档 §4B.3 注明"活跃期上界=空闲窗内响应数"这个诚实边界。

### 8. 小件
- **`cpr:models:plan_snapshots` field 只增不删**:plan_type 从池中消失后其快照 field 永久残留(`models/store.rs:62-81` 只 HSET),`list_plan_snapshots` 会继续返回。基数=历史上出现过的套餐种类,无害,但严格说不满足"最新态覆盖"的完整语义;账号池側有 plan 过滤兜底。
- **页码分页 = OFFSET + count(\*)**:`list_page` 系列(`usage/store.rs:172-199`、`ops/store.rs:114-141`、`fleet/store/mod.rs:487-517`)每次视图先全量 `count(*)` 再 OFFSET。30 天保留期封顶 + 仅管理端,现阶段无感;量级上来的出路是文档已定的"缩短保留期",不必预优化。
- **PG 池参数硬编码**:`max_connections(10)`/`acquire_timeout(5s)`(`infra/database.rs:22-24`)与 database.md §1 一致,但 idle_timeout/max_lifetime 吃 sqlx 默认(10min/30min),且全部不可配置。单实例自托管可接受;若未来遇到网络中间件掐空闲连接,记得这里。Redis 侧 ConnectionManager 默认重连参数(`infra/redis.rs:14-21`),与文档"单条多路复用连接"一致。
- **`to_page` 静默吞行**:`fleet/store/rows.rs:231-239` 与 `account_usage/store.rs:773-798` 对 map 失败的行 `if let Ok` 跳过,坏行只会让页短一截而无日志;status 有 check 约束兜底,概率极低,加个 warn 即可。

### 9. import_sqlite 退役与 feature 裁剪(评估结论:可以动手)
- 现状:sqlite 代码只有 4 个文件触达——`main.rs:11-23` CLI 入口 + `bootstrap/import_sqlite/{mod,core,telemetry}.rs`;`Cargo.toml:34` 的 sqlx 开着 `sqlite` 与 `uuid` 两个 feature,后者**全仓零使用**(UUID 一律以字符串 bind,`fleet/cookies/store.rs:141`)。
- 退役条件已天然满足:导入要求目标库零业务行(`import_sqlite/mod.rs:149-173`),而 `serve` 首次启动就会写入 runtime_settings/admin_users——**任何已运行过的部署都永久过了导入窗口**,这条命令对存量实例已不可用,只服务"还没搬迁的旧 SQLite 用户"。
- 建议:分两步。现在就删 `uuid` feature(零成本);`sqlite` feature + import 模块用 cargo feature(如 `import-sqlite`)门控,默认不编,发布说明标注"需要搬迁请用 vX.Y";一两个版本后整体删除,历史留在 git。收益:少编译一整个 SQLite 静态库,砍掉 sqlx 双驱动的编译面。

## 做得好的(记录一下,免得反复怀疑)

- **事实+桶同事务,契约兜底**:成功路径 `usage/store.rs:84-128`、失败路径 `ops/store.rs:48-87` 都是 `begin() → insert 事实 → upsert 桶 → commit`,与 §5.2 逐字一致;0ms 延迟样本合法(check `>= 0`)、min/max 用 nullable-safe 的 case/`greatest()`(`buckets/store.rs:69-74`)。
- **迁移框架的防御做对了**:整批未应用迁移跑在单事务里(PG DDL 事务性,全成或全无,`database.rs:37-56`);拒绝无版本存量库(`:101-119`);拒绝库版本高于二进制(防旧版回滚踩新库);`schema_migrations` 由建库路径而非迁移文件创建。加列的机制是存在且幂等的——P0 只是常量同步这一个点。
- **import_sqlite 是教科书式一次性搬迁**:源库只读打开、schema v3 强校验、目标非空拒绝、全程单 PG 事务、逐表导入/规范化/丢弃三本账(`import_sqlite/mod.rs:112-137`)。
- **Redis 互斥纪律**:lease 获取 `SET NX PX` + 同 owner Lua 续约、释放 Lua 比较 owner(`lease.rs:49-87`),批查用 MGET(`:104-107`);没有 GET-判断-SET 三段式。亲和写入 MULTI pipe + conv 惰性剪枝,与 §4B.3 一致。
- **索引与热查询整体匹配**:usage_records 8 枚索引与 `UsageRecordFilter` 字段一一对应(`usage/store.rs:284-346`),可空维度全部部分索引;keyset 分页统一 `(created_at desc, id desc)` 配复合索引;鉴权是 `key` unique 点查(`keys/store.rs:50-60`)。热查询路径没有任何 `->>` JSON 聚合(quota_json 只在单行读取时解析)。
- **last_used_at 去抖**:1s 批量聚合后单条 `unnest` UPDATE(`keys/service.rs:18-104`、`keys/store.rs:192-213`),失败回灌重试,不在请求关键路径。
- **池恢复无 N+1**:`restore_from_store` 恰好两条查询——账号全表 + `snapshots(ids)` 批查(`fleet/pool/mod.rs:133-160`);email 映射、bucket 窗口聚合也都是 IN/unnest 批量(`usage/query.rs:371-401`、`buckets/query.rs:130-201`)。
- **trim 是周期任务**:每小时读 runtime_settings 三列后各删一次(`retention_trim.rs:16,49-69`),v3 的内联写放大确认已移除;`rebuild-buckets` 用 `lock table ... share row exclusive` 保证重算窗口一致性(`buckets/store.rs:148-153`)。
- **健康检查双活**:`/healthz` = PG `select 1` + Redis `PING`(`bootstrap/state.rs:31-41`);compose 里 postgres:18-alpine / redis:8-alpine(AOF 开启)带 healthcheck 且应用 `service_healthy` 依赖。
- **测试基建**:每测试独立数据库 + `migrate()` 真跑 + 随机 Redis 前缀(`tests/support/storage.rs`);`tests/infra/database.rs` 断言终态表集、约束、关键索引、凭据存储形态。

## 核实过的误报

- **"account_usage 不与事实同事务是漏洞"**——不是。§5.2 显式修订过:调度器持久化时点独立、每笔 upsert 自身原子、允许亚秒漂移,对账以事实表为准。代码(`fleet/pool/mod.rs:189-237`)与文档一致。
- **"窗口计数双写冲突"**——`RECORD_USAGE_SQL` 增量与 `SYNC_RUNTIME_WINDOW_SQL` 覆盖确实都写 window_* 列,但内存池是权威源,同步带 reset_at 单调守卫(`account_usage/store.rs:81-83`),漂移在文档允许范围内。
- **"live 桶用代码槽对齐、rebuild 用 SQL 槽对齐,两者会漂"**——`china_quarter_hour_start`(`infra/time.rs:117-130`)按中国时区截断到 15 分钟,+08:00 是整小时偏移,与 `floor(epoch/900)*900` 数学等价,§2.3 的论证成立。
- **"`AccountStore::get_pool_account` 默认实现全表加载"**——trait 默认实现(`fleet/store/mod.rs:236-241`)确实是 list 后 find,但 `PgAccountStore` 覆写为点查(`:321-325`);默认实现只有测试 fake 在用。
- **"quota/token 刷新每轮全表拉账号是 N+1"**——是一条全表查询 + 内存过滤(`fleet/quota/runtime.rs:114-134`、`fleet/refresh/service.rs:197-256`),账号表常量级,合理。
- **"forget_account 循环是 N+1"**——SMEMBERS 后逐条 forget 是 §4B.3 设计原文;真正的问题在 SET 增长(P2-7),不在循环本身。
- **"迁移并发竞争"**——两进程同时 migrate 时,后者会在 `record_migration` 的 PK 冲突上回滚整个事务而失败退出,不会写坏库;单实例部署 + compose 依赖下可接受,无需 advisory lock。

## 建议动手顺序

1. **P0 第 1 条**(派生 `CURRENT_SCHEMA_VERSION` + 一致性断言)先做——一行改动,消灭"第一次加列就翻车"的确定性事故;顺手把 **P1 第 2 条**的 checksum 和迁移测试模板一起交付,凑成一个"迁移框架加固" commit。
2. **P1 第 3 条**(导入事务化 + rate-limit 窗口单语句化)——正确性,改动局部在 `fleet/store` 与 `account_usage/store`。
3. **P1 第 4 条**(account_model_usage 定去留)——先开口径讨论再动代码;结论落进 database.md。
4. **P2 第 5、6 条**一起做(删两枚无读者索引 + 错误桶 service_tier 统一)——都要动 0001 基线之外的第一条真实迁移,正好用上第 1 步打好的框架。
5. P2 第 9 条(feature 裁剪)随下个发布窗口;其余 P2 视精力。
