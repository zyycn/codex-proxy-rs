# 数据库设计审计

审计日期：2026-06-27
执行状态：P0/P1/P2/P3/P4/P5 已落地，当前源码 schema 已移除空壳迁移表，新增运行设置表、模型用量统计表、时间桶聚合表与模型账号路由表，统一本地敏感字段策略，并补齐关键查询与诊断筛选索引。

本文档审计当前 SQLite schema、真实查询路径、系统设置持久化方式与测试覆盖。结论以“新工程、无历史包袱”为前提：不保留半套迁移、不保留误导性表、不为了兼容旧数据维持不合理结构。

## 总体结论

当前数据库已经覆盖了轻量模型网关的核心运行域：管理员、客户端 API Key、账号池、账号用量、时间桶趋势聚合、账号 Cookie、设备指纹、事件日志、模型快照、会话亲和。表边界整体可理解，外键、布尔检查、状态检查、非负计数约束也有基础质量。

当前已完成第一轮数据库边界收敛：

1. 已删除源码 schema 中的 `schema_migrations` 空壳表。
2. 已新增 `runtime_settings`，后台系统设置改为数据库持久化。
3. 已补齐账号、用量、API Key、指纹历史的关键分页索引。
4. 已删除设置写回 `config.yaml` 的代码路径。

当前数据库审计文档中的结构性收敛项已经落地。趋势统计不再依赖事件日志窗口，而是写入独立聚合事实表；事件日志继续只承担诊断窗口。

## 当前事实

### Schema 初始化

当前初始化入口在 `src/infra/database.rs`：

- 启动时创建 SQLite 父目录。
- 使用 WAL。
- 开启 foreign keys。
- 设置 busy timeout。
- 执行整段 `src/infra/schema.sql`。

当前选择是“当前态 schema 初始化”，不是版本化迁移。`schema.sql` 使用 `create table if not exists` 只负责创建新库；已有开发库如果曾经创建过旧结构，直接重建数据库。项目不保留迁移动作代码，也不保留兼容旧字段的分支。

### 当前表

`src/infra/schema.sql` 当前定义：

- `admin_users`
- `admin_sessions`
- `client_api_keys`
- `runtime_settings`
- `accounts`
- `account_refresh_leases`
- `account_usage`
- `account_model_usage`
- `usage_time_buckets`
- `model_account_routes`
- `account_cookies`
- `fingerprints`
- `fingerprint_update_history`
- `event_logs`
- `model_plan_snapshots`
- `session_affinities`

源码 schema 不再包含 `schema_migrations`。已有开发库如果曾经创建过旧表，需要直接重建数据库；项目不保留迁移或兼容清理代码。

### 系统设置现状

后台系统设置当前已经进入数据库：

- `runtime_settings` 保存后台可改的运行配置。
- 启动时如果缺少设置行，会用启动配置初始化缺省行。
- 启动时如果已有设置行，会用数据库运行设置覆盖运行时配置。
- `POST /api/admin/settings` 只写 `runtime_settings`，不再写回 `config.yaml`。

`config.yaml` 只保留启动级配置，不再承担后台设置持久化。

## 高优先级结论

### 1. 空壳迁移表必须处理

状态：已处理。

`schema_migrations` 曾经存在，但没有任何代码读取、写入、校验版本。它会制造错误预期：看起来有迁移体系，实际只是 `create table if not exists`。

当前已经按新工程策略处理：直接删除空壳迁移表。schema 以当前文件为唯一事实；已有旧开发库直接重建，不提供迁移动作代码。

### 2. 系统设置应该入库

状态：已处理。

页面可改的运行配置不应该写回整份 YAML。`config.yaml` 应保留为启动级配置，例如：

- 服务监听地址和端口。
- 数据库 URL。
- 上游基础地址。
- 日志目录。
- 首次启动管理员默认值。

后台设置页改动的是需要人工决策的运行态配置，更适合入库，例如：

- 模型别名。
- 提前刷新秒数。
- 刷新并发数。
- 单账号并发数。
- 请求间隔。
- 账号调度策略。
- 模型到账户的路由或绑定。

当前使用单行强类型表，而不是散乱 key-value：

```sql
create table runtime_settings (
  id integer primary key check (id = 1),
  model_aliases_json text not null default '{}',
  refresh_margin_seconds integer not null check (refresh_margin_seconds > 0),
  refresh_concurrency integer not null check (refresh_concurrency > 0),
  max_concurrent_per_account integer not null check (max_concurrent_per_account > 0),
  request_interval_ms integer not null check (request_interval_ms >= 0),
  rotation_strategy text not null check (rotation_strategy in ('least_used', 'round_robin', 'sticky')),
  updated_at text not null
);
```

模型映射绑定账号不塞进 `model_aliases_json`。这属于调度关系，不是配置字符串，当前使用独立关系表：

```sql
create table model_account_routes (
  model text not null,
  account_id text not null references accounts(id) on delete cascade,
  priority integer not null default 0,
  enabled integer not null default 1 check (enabled in (0, 1)),
  created_at text not null,
  updated_at text not null,
  primary key (model, account_id)
);

create index idx_model_account_routes_account on model_account_routes(account_id);
create index idx_model_account_routes_enabled_model on model_account_routes(enabled, model, priority asc, account_id);
```

这样页面配置、调度逻辑、账号删除行为都有明确数据库约束。

设置接口通过 `modelAccountRoutes` 读写该表，运行时账号池按请求模型过滤显式账号列表；账号删除时由外键级联清理相关路由。

### 3. 关键查询缺复合索引

状态：核心索引已处理。

已补齐：

```sql
create index idx_accounts_added_id on accounts(added_at desc, id desc);
create index idx_account_usage_last_used_account on account_usage(last_used_at desc, account_id desc);
create index idx_client_api_keys_created_id on client_api_keys(created_at desc, id desc);
create index idx_fingerprint_update_history_created_id on fingerprint_update_history(created_at desc, id desc);
```

运行清理和诊断筛选索引已补齐：

```sql
create index idx_account_cookies_expires on account_cookies(expires_at) where expires_at is not null;
create index idx_session_affinities_active_order on session_affinities(expires_at, created_at, response_id);
create index idx_event_logs_level_created on event_logs(level, created_at desc);
create index idx_event_logs_route_created on event_logs(route, created_at desc) where route is not null;
create index idx_event_logs_model_created on event_logs(model, created_at desc) where model is not null;
create index idx_event_logs_status_created on event_logs(status_code, created_at desc) where status_code is not null;
create index idx_event_logs_upstream_status_created on event_logs(upstream_status_code, created_at desc) where upstream_status_code is not null;
```

事件日志继续保持诊断窗口定位，不引入 FTS。`message like '%...%'` 和 `metadata_json like '%...%'` 不适合普通 BTree；当前不把全文搜索定义为核心能力。

### 4. 长期统计不应该依赖事件日志

状态：已处理。

账号模型用量已经从事件日志分页回放中剥离。请求调度成功会记录模型请求数，请求完成会记录模型维度 token 用量，空响应会记录模型维度错误数。账号列表的模型统计现在读取 `account_model_usage`，事件日志容量裁剪不会影响该统计。

概览卡片、24 小时趋势与 7 天健康时间线不再从 `event_logs` 反推。事件写入时会同步写入 `usage_time_buckets`，Dashboard 读取聚合事实表；清空或裁剪事件日志不会影响趋势统计。

当前使用独立统计表：

```sql
create table account_model_usage (
  account_id text not null references accounts(id) on delete cascade,
  model text not null,
  request_count integer not null default 0 check (request_count >= 0),
  error_count integer not null default 0 check (error_count >= 0),
  input_tokens integer not null default 0 check (input_tokens >= 0),
  output_tokens integer not null default 0 check (output_tokens >= 0),
  cached_tokens integer not null default 0 check (cached_tokens >= 0),
  last_used_at text,
  primary key (account_id, model)
);

create index idx_account_model_usage_last_used on account_model_usage(last_used_at desc, account_id, model);
```

当前使用 15 分钟时间桶，兼顾 24 小时趋势、7 天健康时间线和按模型成本计算：

```sql
create table usage_time_buckets (
  bucket_start text not null,
  account_id text not null default '',
  model text not null default '',
  service_tier text not null default '',
  request_count integer not null default 0 check (request_count >= 0),
  error_count integer not null default 0 check (error_count >= 0),
  input_tokens integer not null default 0 check (input_tokens >= 0),
  output_tokens integer not null default 0 check (output_tokens >= 0),
  cached_tokens integer not null default 0 check (cached_tokens >= 0),
  first_token_latency_sum integer not null default 0 check (first_token_latency_sum >= 0),
  first_token_latency_count integer not null default 0 check (first_token_latency_count >= 0),
  latency_sum integer not null default 0 check (latency_sum >= 0),
  latency_count integer not null default 0 check (latency_count >= 0),
  max_latency_ms integer not null default 0 check (max_latency_ms >= 0),
  min_latency_ms integer not null default 0 check (min_latency_ms >= 0),
  updated_at text not null,
  primary key (bucket_start, account_id, model, service_tier)
);
```

## 表级审计

### `admin_users`

设计合理。当前规模极小，按 `created_at asc, id asc` 找首个管理员不需要额外优化。若未来支持多管理员列表页，再补 `created_at` 索引。

### `admin_sessions`

设计合理。`user_id` 外键级联删除、`expires_at` 索引用于会话校验和清理。保持。

### `client_api_keys`

整体合理。`key` 明文字段用于 `/v1` 认证查找，方向和当前产品策略一致；`prefix` 只作为管理端展示字段，不承担认证。

已补 `created_at desc, id desc` 列表索引，并使用启用状态下的 `key` 索引支撑认证查询。当前不再保存额外哈希字段，也不再需要额外本地密钥文件。

### `accounts`

核心字段基本合理，已经没有旧的 token 加密字段。`chatgpt_account_id` 与 `chatgpt_user_id` 的唯一表达式索引是正确方向。

已补 `accounts(added_at desc, id desc)`，用于账号列表和账号池恢复排序。如果状态筛选加排序成为常用查询，再补 `(status, added_at desc, id desc)`，当前不提前扩展。

`quota_json` 当前可以接受，因为上游 quota 形态不稳定；但如果页面要按余额、额度、冷却时间筛选排序，应把关键字段提升为列，不要用 JSON 做业务查询。

### `account_refresh_leases`

设计合理。以 `account_id` 为主键，`expires_at` 索引用于过期释放。保持。

### `account_usage`

作为账号级累计聚合表是合理的。非负 check 覆盖较完整。

它承担账号列表用量展示和用量分页，当前已经补齐 `last_used_at desc, account_id desc` 索引。边界是它只保存账号总量，不保存模型维度和时间趋势。

### `account_model_usage`

用于账号 + 模型维度的长期聚合。它补齐了 `account_usage` 缺少的模型维度，也避免从事件日志回放业务统计。

当前记录：

- 请求数。
- 错误数。
- input tokens。
- output tokens。
- cached tokens。
- 最近使用时间。

这张表是长期事实表，事件日志是诊断窗口，两者不再混用。趋势图不扩展这张表为时序明细，而是由 `usage_time_buckets` 承担。

### `usage_time_buckets`

用于保存 15 分钟粒度的请求趋势聚合。它不是事件日志副本，不保存请求内容、响应内容或长尾元数据，只保存 Dashboard 和长期统计需要的数值：

- 请求数与错误数。
- input/output/cached tokens。
- 首 token 延迟与完成延迟的 sum/count/max/min。
- model 与 service tier，用于成本计算。

写入路径在事件日志落库时同步更新聚合表。读取路径在 Dashboard 使用该表生成卡片、趋势和健康时间线；`event_logs` 清空或容量裁剪不影响这些趋势数据。

### `model_account_routes`

用于保存模型到账号 ID 的显式调度关系。它是运行配置的一部分，但不能压进 `runtime_settings.model_aliases_json`：

- `model_aliases_json` 只表达客户端模型名到真实上游模型名的别名。
- `model_account_routes` 表达真实调度约束，受账号生命周期影响。
- `account_id` 外键级联删除，避免账号删除后留下无效路由。
- `priority` 保存账号选择顺序，`enabled` 用于保留可控开关。

运行时账号池会先按模型账号路由过滤候选账号，再执行计划 allowlist、套餐优先级、并发槽位与轮换策略。

### `account_cookies`

按账号、域名、名称、路径唯一是合理的。当前已经统一为本地明文可运维策略，字段为 `value`。

已删除 `value_cipher`、Cookie 加解密路径、`master_key_file` 配置和不再使用的加密模块。Cookie、token、API Key 的本地持久化策略不再混用。`expires_at` 清理索引已补齐。

### `fingerprints` 与 `fingerprint_update_history`

当前把默认 headers 和 header order 存 JSON 是合理的，因为它们是配置形态，不适合拆成多张表。

已补 `fingerprint_update_history(created_at desc, id desc)`，匹配读取最新历史记录的排序。

### `event_logs`

这是当前 schema 里设计最完整的一块：热字段结构化，长尾信息放 `metadata_json`，基础索引覆盖最新列表、kind、request、account、transport、failure、response、upstream request。

边界要明确：事件日志是诊断窗口，不是长期统计账本。容量裁剪后，任何从日志反推的统计都只是窗口统计。

事件日志页面的结构化筛选索引已补齐。全文搜索暂不引入 FTS，避免把日志系统做成半个分析数据库。Dashboard 不再依赖事件日志计算长期趋势。

### `model_plan_snapshots`

按 plan 存模型列表 JSON 是合理的。模型目录来自上游，结构变化概率高，当前不需要拆表。

按模型控制账号路由已经由 `model_account_routes` 承担，不改造这个快照表承担调度职责。

### `session_affinities`

设计基本合理。它是运行时恢复和会话亲和状态，不是长期数据。`conversation_id` 和 `expires_at` 索引覆盖主要查询。

已补 `expires_at, created_at, response_id` 复合索引，匹配活跃亲和恢复和过期清理的排序路径。

## 目标数据库边界

建议把数据分成三类。

启动配置留在 `config.yaml`：

- server。
- database。
- API base URL。
- TLS。
- 日志目录。
- 首次管理员默认值。

认证刷新、额度刷新和调度节奏不再作为 YAML 输入项；可人工调整的部分进入运行配置表，固定策略使用代码默认值。

运行配置进入数据库：

- model aliases。
- model-account routes。
- refresh margin。
- refresh concurrency。
- per-account concurrency。
- request interval。
- rotation strategy。
后台默认策略不进入运行配置表，避免把不需要人工决策的开关暴露成持久配置。

运行事实进入数据库：

- accounts。
- account usage。
- account model usage。
- usage time buckets。
- sessions。
- API keys。
- event logs。
- cookies。
- fingerprints。
- model snapshots。
- session affinities。

这个边界更符合网关项目：配置文件负责启动，数据库负责后台可管理状态。

## 推荐落地顺序

### P0：数据库基础收敛

1. 已删除空壳 `schema_migrations`。
2. 已新增 `runtime_settings` 表，把后台系统设置从 YAML 写回改为数据库持久化。
3. 已让 `config.yaml` 只保留启动级配置。
4. 已将后台设置接口改为读写数据库，启动时从 DB 合成运行时配置。
5. 已补齐账号、用量、API Key、指纹历史的复合索引。
6. 已新增 `model_account_routes`，模型账号绑定进入关系表并参与调度。
7. 已更新 `tests/infra/storage_schema`，覆盖新表、索引、约束。

### P1：统计模型收敛

状态：已完成。

1. 已新增 `account_model_usage`。
2. 已在账号调度成功时记录模型请求数。
3. 已在 Responses、Chat、Compact 路径完成时记录模型 token 用量。
4. 已在空响应路径记录模型错误数。
5. 账号列表模型统计已改为读取 `account_model_usage`。
6. 已明确事件日志只作为诊断窗口。
7. 已新增 `usage_time_buckets`，Dashboard 趋势不再依赖事件日志容量。

### P2：日志与搜索能力决策

状态：已完成。

1. 已保留轻量事件日志表，不引入 FTS。
2. 已补事件日志结构化筛选索引。
3. 已补 Cookie 过期清理索引。
4. 已补活跃 session affinity 排序索引。

### P3：敏感字段策略统一

状态：已完成。

1. 已明确本地数据库不单独加密 Cookie。
2. 已删除 `value_cipher` 和 Cookie 加解密路径。
3. 已删除 `master_key_file` 启动配置与不再使用的加密模块。
4. 已补 schema 与账号 Cookie 行为测试，固定 Cookie 明文持久化策略。

### P4：模型账号路由收敛

状态：已完成。

1. 已新增 `model_account_routes`。
2. 已让后台设置接口读写 `modelAccountRoutes`。
3. 已让运行时账号池按模型账号路由过滤候选账号。
4. 已补账号删除级联测试，账号删除后模型路由同步清理。

### P5：趋势聚合收敛

状态：已完成。

1. 已新增 `usage_time_buckets`。
2. 已在事件日志写入时同步更新时间桶聚合。
3. 已将 Dashboard 卡片、趋势和健康时间线切换到聚合表。
4. 已补测试验证清空事件日志后趋势统计仍然保留。

## 测试覆盖

本轮数据库收敛对应的测试覆盖：

1. schema 创建测试：所有目标表存在，空壳表不存在。
2. 索引测试：关键列表查询需要的索引存在。
3. 约束测试：布尔、状态、非负计数、单行设置表 id 约束。
4. 设置持久化测试：后台设置写入 DB，重启服务后仍生效。
5. YAML 边界测试：后台设置更新不再重写整份 `config.yaml`。
6. 已补统计测试：模型用量来自 `account_model_usage`，不依赖事件日志容量。
7. 已补删除级联测试：账号删除后相关 usage、model usage、cookies、leases、session affinities 正确清理。事件日志按诊断历史保留，不参与账号外键级联。
8. 已补模型账号路由测试：schema、索引、约束、设置持久化、重启恢复、运行时调度过滤和账号删除级联。
9. 已补时间桶趋势测试：schema、索引、约束、日志写入同步聚合，以及清空事件日志后 Dashboard 仍读取聚合趋势。

## 最终建议

当前数据库设计已经完成第一轮“无历史包袱”收敛。当前边界已经更清晰：启动配置归配置文件，后台运行状态归数据库，长期统计归统计表，事件日志归诊断窗口。
