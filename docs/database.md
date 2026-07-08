# Database —— 唯一权威文档

本文档是 Codex Proxy RS 数据库的**唯一**权威文档：终态 Schema 全量定义、全局纪律、统计口径、一次到位的迁移方案（v3 → v4）与代码改造清单。

- 取代并合并旧的三份文档（database-design.md / database-audit.md / database-schema.md），旧文档一律作废删除。
- 基线：2026-07-08，生产库 schema v3（`backend/src/infra/database.rs`）。
- 策略：**一次破坏性迁移直达终态，不做兼容层**。没有双写期、没有回填过渡期、没有查询层兼容过滤；不满足终态契约的历史数据按 §6.3 的规则迁移或丢弃，丢弃项全部显式列出。

---

## 1. 运行形态

单进程 SQLite。这是刻意选择：自托管代理、单实例部署，SQLite 免去外部依赖且性能远超需求。多实例是明确的未来触发条件而非当前需求（§8）。

连接参数（`database.rs` 连接期设置，全部显式）：

| Pragma | 值 | 理由 |
| --- | --- | --- |
| `journal_mode` | WAL | 读写不互斥，单写多读的正确形态 |
| `synchronous` | NORMAL | WAL 下的标准取舍：掉电最多丢最后一次 checkpoint 后的事务，不损坏库。FULL 的额外 fsync 对本系统无意义 |
| `foreign_keys` | ON | 级联删除是业务语义的一部分 |
| `busy_timeout` | 5s | 单写者下的排队上限 |
| `auto_vacuum` | INCREMENTAL | 三张增长表按保留期批量删除，必须有空间回收路径；trim 任务后执行 `pragma incremental_vacuum`（§7.7）。对已存在的库，该设置在 0004 迁移后的一次 `vacuum` 时生效 |

---

## 2. 全局纪律

### 2.1 命名

| 对象 | 规则 | 示例 |
| --- | --- | --- |
| 表名 | snake_case；实体集合复数；子表 `<父实体单数>_<内容>` | `accounts`、`account_cookies` |
| 表名后缀 | 事实 `_records`/`_logs`；聚合 `_buckets`；缓存 `_snapshots`；运行态 `_leases`/`_affinities`；审计 `_history` | `usage_records` |
| 列名 | snake_case，全库同义同名 | `account_id` 处处一致 |
| 时间戳 | `*_at`，TEXT，RFC3339 UTC | `created_at` |
| 时长 | `*_seconds` / `*_ms`，INTEGER | `latency_ms` |
| 计数 | `*_count`，INTEGER，`check >= 0` | `request_count` |
| JSON | `*_json`，TEXT | `metadata_json` |
| 布尔 | 裸形容词/过去式，INTEGER，`check in (0,1)` | `enabled` |
| 外键/逻辑引用列 | `<被引用实体单数>_id` | `client_api_key_id` |
| 凭据哈希 | `*_hash`，明文永不落库（§2.6） | `key_hash`、`token_hash` |
| 主键 | `id`，除非自然复合键或哈希键 | — |
| 普通/唯一索引 | `idx_<表>_<列摘要>` / `ux_<表>_<列摘要>` | `ux_accounts_chatgpt_identity` |
| 双端状态码 | 视角前缀 | `status_code` / `client_status_code` / `upstream_status_code` |
| 模型归因 | 来源前缀 | `model` / `requested_model` / `upstream_model` |

### 2.2 类型

只用两种存储类型表达业务数据：

| 用途 | 类型 | 约定 |
| --- | --- | --- |
| ID、名称、枚举、时间戳、JSON、哈希 | TEXT | 枚举加 `check in (...)` |
| 计数、时长、布尔、状态码 | INTEGER | 计数 `>= 0`；布尔 `in (0,1)` |

**不使用 REAL，不存储金额**。成本一律查询期由 token × 单价计算（`admin/monitoring/billing.rs`）：单价会变、需追溯重算，落库的浮点成本既漂移又过期。

### 2.3 时间戳

- 库内一律 UTC，统一经 `chrono::DateTime::to_rfc3339()` 写入（`+00:00` 后缀）。RFC3339 UTC 文本的字典序 = 时间序，TEXT 时间戳因此可直接做范围查询与排序索引。
- 时区换算（中国时区展示、15 分钟槽对齐）只在代码层（`infra/time.rs` 的 `china_*` 系列），数据库不感知时区。
- **禁止**在同一张做范围比较的表里混用 `strftime('…Z')` 与 `to_rfc3339()`。唯一例外：`schema_migrations.applied_at` 用 `strftime`（不做范围比较）。

### 2.4 ID

主键一律 TEXT 随机 ID（`infra/identity.rs` 生成）：无中心分配、写入前可生成、跨表无碰撞。例外：`schema_migrations.version`（天然有序整数）、`runtime_settings.id`（单例锚点）、凭据表以哈希为键（§2.6）。

### 2.5 NULL 与 sentinel

- "无值 / 未观测"一律 NULL；**禁止** 0、空字符串做哨兵。NULL 与 0 语义不同（`input_tokens` NULL = 上游未报告，0 = 确认为零）。
- 聚合维度列不可空时，未知值用 `__unknown__`；预聚合汇总行（预留）用 `__all__`。
- 唯一例外：`ux_accounts_chatgpt_identity` 的索引表达式 `coalesce(chatgpt_user_id, '')`——空串只存在于索引表达式，不是存储值，不构成先例。

### 2.6 Secret 纪律（无例外）

凡是"出示即获得权限"的值都是凭据，**明文一律不落库**：

| 凭据 | 存储 | 哈希类型 | 理由 |
| --- | --- | --- | --- |
| 管理员密码 | `admin_users.password_hash` | 慢哈希（argon2/bcrypt） | 低熵人类输入，必须抗字典攻击 |
| 客户端 API key | `client_api_keys.key_hash` | SHA-256 | 高熵随机串无字典攻击面，快哈希保住 unique 索引 O(log n) 查找；明文只在创建响应出现一次，`prefix` 供事后定位 |
| 管理端 session token | `admin_sessions.token_hash` | SHA-256 | 同上。库文件/备份泄露不再等于会话劫持 |
| 管理 API key | `runtime_settings.admin_api_key_hash` | SHA-256 | 同上；校验 = 哈希后 `ct_eq` |
| 上游 access/refresh token、Cookie value | 明文列，标记 secret | —（功能上必须可逆） | 不进日志、不进列表 API；静态加密的触发条件见 §8 |

### 2.7 JSON 边界

`*_json` 列只允许三类内容：① 上游原样快照（`quota_json`、`models_json`、`manifest_json`）；② 结构化模板（`default_headers_json`、`header_order_json`）；③ 调试细节（`metadata_json`）。**凡进入 WHERE / GROUP BY / 告警的字段必须是一等列**；列一旦提升即为唯一可查询真相，JSON 内同名字段仅供人工排查，写入时不再往 metadata 双写已提升字段。

### 2.8 索引

1. 每个索引对应一条真实查询；unique 约束已覆盖的等值查找不再建索引。
2. 列表分页统一 keyset 模式，配 `(排序时间 desc, id desc)` 复合索引。
3. 可空列上的筛选索引一律部分索引 `where <col> is not null`。
4. FK 子表必须有以 FK 列开头的索引（SQLite 级联删除在子表无索引时对父表每行全扫）。免除条件必须注明（如 `admin_sessions`：行数个位数）。
5. 高频写入表的索引总数进 code review，新增需说明查询来源。

### 2.9 保留与可重建性（诚实边界）

| 表 | 保留 | 配置 |
| --- | --- | --- |
| `usage_records` | 30 天（默认） | `runtime_settings.usage_retention_days` |
| `ops_error_logs` | 30 天（默认） | `runtime_settings.ops_error_retention_days` |
| `request_time_buckets` | 90 天（默认） | `runtime_settings.bucket_retention_days` |
| `admin_sessions` / `account_cookies` / `session_affinities` | 按 `expires_at` 清理 | 定时任务 |
| 其余 | 常量级增长 | — |

**可重建性以事实保留期为界，不夸大**：

- `request_time_buckets` 在事实保留期内可由两张事实表重算（`rebuild-derived` 命令，§7.6）；超出事实保留期的桶是**因事实过期而升格的一级数据**，写入纪律 + 同事务写入（§5.2）是它们的唯一保障。
- `account_usage` / `account_model_usage` 的累计列**不可重建**（事实早已过期），同样只受事务保护。文档不再声称"一切派生皆可重算"。
- `window_*` 列是运行态（窗口边界来自上游），不可重建，可丢弃。

---

## 3. 架构分层与关系

```text
┌─ 元数据 ──────────────────────────────────────────────┐
│  schema_migrations                                    │
├─ 配置 ────────────────────────────────────────────────┤
│  runtime_settings（单例热配置）                         │
├─ 接入 ────────────────────────────────────────────────┤
│  admin_users · admin_sessions · client_api_keys       │
├─ 账号 ────────────────────────────────────────────────┤
│  accounts · account_cookies · fingerprints            │
│  fingerprint_update_history · model_plan_snapshots    │
├─ 事实（不可替代的业务事件）────────────────────────────┤
│  usage_records（成功） · ops_error_logs（失败/运维）    │
├─ 派生（口径见 §2.9）──────────────────────────────────┤
│  request_time_buckets · account_usage                 │
│  account_model_usage                                  │
└─ 运行态（临时协调，可丢弃）───────────────────────────┤
   session_affinities · account_refresh_leases          │
```

```text
admin_users  1──< admin_sessions                 FK cascade（索引免除：行数个位数）
accounts     1──1 account_usage                  FK cascade（PK=FK）
accounts     1──1 account_refresh_leases         FK cascade（PK=FK）
accounts     1──< account_model_usage            FK cascade（复合 PK 前缀）
accounts     1──< account_cookies                FK cascade（account_domain 前缀）
accounts     1──< session_affinities             FK cascade（idx_…_account）
fingerprints 1──< fingerprint_update_history     FK cascade（idx_…_fingerprint）

usage_records.{account_id, client_api_key_id}    → 逻辑引用，无 FK
ops_error_logs.{account_id, client_api_key_id}   → 逻辑引用，无 FK
request_time_buckets.account_id                  → 逻辑维度，无 FK
```

**事实表/聚合表不建 FK 是有意为之**：审计事实的生命周期必须独立于账号与 key——删除账号或 key 不得抹掉历史用量和错误记录。运行态表相反：账号没了亲和/租约就失去意义，级联删除是正确语义。

---

## 4. 表设计（终态 DDL）

### 4.1 schema_migrations

迁移版本的唯一事实来源。由 `database.rs` 建库时创建，不走迁移文件。只增不改；已发布迁移永不修改；`CURRENT_SCHEMA_VERSION` 与迁移列表同步提交。

```sql
create table schema_migrations (
  version integer not null check (version > 0),
  name text not null,
  applied_at text not null,
  primary key (version)
);
```

### 4.2 admin_users

单管理员模型的登录凭据。不预建 `username`/`email`/`role` 死列；多管理员的扩展路径见 §8。

```sql
create table admin_users (
  id text primary key,
  password_hash text not null,
  created_at text not null,
  updated_at text not null
);
```

### 4.3 admin_sessions

管理端会话的服务端状态。session 存库（而非 JWT）使"登出即失效、改密码踢会话、重启不掉线"都是一条 SQL。

```sql
create table admin_sessions (
  token_hash text primary key,
  user_id text not null references admin_users(id) on delete cascade,
  expires_at text not null,
  created_at text not null
);

create index idx_admin_sessions_expires on admin_sessions(expires_at);
```

| 字段 | 说明 |
| --- | --- |
| `token_hash` | `SHA-256(session token)`。客户端持有明文 token，鉴权时哈希后按 PK 等值查找。**明文 token 不落库**（§2.6） |
| `user_id` | FK cascade，删用户即踢会话 |
| `expires_at` | 鉴权比较 + 清理任务扫描（`idx_admin_sessions_expires`） |

`user_id` 不建索引是注明的免除：单管理员下行数个位数。IP/UA 审计不进本表；需要时新建 `admin_login_events`。

### 4.4 client_api_keys

客户端调用 `/v1/*` 的凭据。key 与上游账号解耦，客户端不感知账号池。

```sql
create table client_api_keys (
  id text primary key,
  name text not null,
  prefix text not null,
  key_hash text not null unique,
  label text,
  enabled integer not null default 1 check (enabled in (0, 1)),
  created_at text not null,
  last_used_at text
);

create index idx_client_api_keys_created_id on client_api_keys(created_at desc, id desc);
```

| 字段 | 说明 |
| --- | --- |
| `id` | 稳定标识；管理操作与事实归因（`usage_records.client_api_key_id`）都引用 id，key 轮换不影响引用 |
| `name` / `label` | `name` 必填主展示，`label` 可选注记 |
| `prefix` | 明文前几位（`sk-abc1…`），明文销毁后定位 key 的唯一手段 |
| `key_hash` | `SHA-256(key)`，unique 索引即鉴权查找路径。明文只在创建响应出现一次，**管理端列表 API 永不回显完整 key** |
| `enabled` | 软开关；刻意不建索引（低基数布尔） |
| `last_used_at` | 鉴权成功后异步更新，不在请求关键路径强一致 |

每 key 配额/限流不进本表——未来 `client_api_key_policies` 附属表（§8），归因数据已由事实表的 `client_api_key_id` 备好。

### 4.5 runtime_settings

热更新全局配置，单行表，`check (id = 1)` 约束层面保证单例。选宽表不选 KV：列级类型与 check 是 KV 给不了的；加配置要加列，恰好强迫走迁移评审。只放小型标量配置，禁塞大 JSON 状态。

```sql
create table runtime_settings (
  id integer primary key check (id = 1),
  model_aliases_json text not null default '{}',
  refresh_margin_seconds integer not null check (refresh_margin_seconds > 0),
  refresh_concurrency integer not null check (refresh_concurrency > 0),
  max_concurrent_per_account integer not null check (max_concurrent_per_account > 0),
  request_interval_ms integer not null check (request_interval_ms >= 0),
  rotation_strategy text not null check (rotation_strategy in ('smart', 'quota_reset_priority', 'round_robin', 'sticky')),
  admin_api_key_hash text,
  usage_retention_days integer not null default 30 check (usage_retention_days > 0),
  ops_error_retention_days integer not null default 30 check (ops_error_retention_days > 0),
  bucket_retention_days integer not null default 90 check (bucket_retention_days > 0),
  updated_at text not null
);
```

| 字段 | 说明 |
| --- | --- |
| `model_aliases_json` | 客户端模型名 → 上游模型名映射。JSON 合规：整体读写、不按别名查询 |
| `refresh_*` / `max_concurrent_per_account` / `request_interval_ms` | 刷新与调度参数 |
| `rotation_strategy` | 枚举 check；枚举值变更必须走迁移（0002 先例），库内无死值 |
| `admin_api_key_hash` | 管理 API 机器凭据的 SHA-256。NULL = 未启用。校验 = 对来键哈希后 `ct_eq` 比较 |
| `*_retention_days` 三列 | 三张增长表各自独立的保留天数。bucket 默认更长：趋势比明细有更长查询价值 |

### 4.6 accounts

上游账号的身份、凭据、调度状态与 quota 快照。**保持单表是当前正确决策**：调度器每次决策同时读身份+状态+冷却，拆表只增加 join 与事务面；拆分触发条件明确（§8）。

```sql
create table accounts (
  id text primary key,
  email text,
  chatgpt_account_id text,
  chatgpt_user_id text,
  label text,
  plan_type text,
  access_token text not null,
  refresh_token text,
  access_token_expires_at text,
  next_refresh_at text,
  status text not null check (status in ('active', 'expired', 'quota_exhausted', 'refreshing', 'disabled', 'banned')),
  quota_json text,
  quota_fetched_at text,
  quota_limit_reached integer not null default 0 check (quota_limit_reached in (0, 1)),
  quota_verify_required integer not null default 0 check (quota_verify_required in (0, 1)),
  quota_cooldown_until text,
  cloudflare_cooldown_until text,
  added_at text not null,
  updated_at text not null
);

create index idx_accounts_status on accounts(status);
create index idx_accounts_added_id on accounts(added_at desc, id desc);
create unique index ux_accounts_chatgpt_identity
  on accounts(chatgpt_account_id, coalesce(chatgpt_user_id, ''))
  where chatgpt_account_id is not null;
```

要点：

- **身份**：`id` 与上游 ID 解耦（导入半成品账号仍可运转）；`chatgpt_account_id` + `chatgpt_user_id` 复合唯一防重复导入，身份未知的账号不参与去重（部分索引）。`plan_type` 不加枚举 check：取值由上游定义。
- **凭据（secret）**：`access_token` not null 是账号存在的最低要求；`refresh_token` NULL = 不可续期，到期即 `expired`。不进日志、不进列表 API。
- **状态**：单列状态机 + 布尔/冷却补充列，避免状态爆炸。冷却用时间而非布尔：自然过期无需回写。quota 与 CF 冷却分列：成因、时长、解除策略都不同。
- **快照**：`quota_json` 上游原样快照（JSON 合规）；`added_at` 语义化命名——账号是"被添加进池子"的。

### 4.7 account_refresh_leases

token 刷新互斥租约。租约（owner + 到期）是最小充分的互斥原语：持有者崩溃后自然失效，无需显式释放；天然支持未来多实例竞争。运行态表，不承载历史（需要时另建 `account_refresh_events`）。

```sql
create table account_refresh_leases (
  account_id text primary key references accounts(id) on delete cascade,
  owner text not null,
  expires_at text not null,
  updated_at text not null
);

create index idx_account_refresh_leases_expires on account_refresh_leases(expires_at);
```

PK=FK 的 1:1 建模把"每账号至多一个持有者"变成主键约束。`owner` 在续约/释放时校验，防误释放他人租约。

### 4.8 account_usage

账号维度两组计数：① 生命周期累计（列表展示，**不可重建**，§2.9）；② 当前 quota 窗口计数（调度限流输入，运行态）。1:1 派生表，不与 accounts 合并：更新频率差多个数量级，合表会让高频计数写污染低频实体行。

```sql
create table account_usage (
  account_id text primary key references accounts(id) on delete cascade,
  request_count integer not null default 0 check (request_count >= 0),
  empty_response_count integer not null default 0 check (empty_response_count >= 0),
  input_tokens integer not null default 0 check (input_tokens >= 0),
  output_tokens integer not null default 0 check (output_tokens >= 0),
  cached_tokens integer not null default 0 check (cached_tokens >= 0),
  reasoning_tokens integer not null default 0 check (reasoning_tokens >= 0),
  total_tokens integer not null default 0 check (total_tokens >= 0),
  image_input_tokens integer not null default 0 check (image_input_tokens >= 0),
  image_output_tokens integer not null default 0 check (image_output_tokens >= 0),
  image_request_count integer not null default 0 check (image_request_count >= 0),
  image_request_failed_count integer not null default 0 check (image_request_failed_count >= 0),
  window_request_count integer not null default 0 check (window_request_count >= 0),
  window_input_tokens integer not null default 0 check (window_input_tokens >= 0),
  window_output_tokens integer not null default 0 check (window_output_tokens >= 0),
  window_cached_tokens integer not null default 0 check (window_cached_tokens >= 0),
  window_image_input_tokens integer not null default 0 check (window_image_input_tokens >= 0),
  window_image_output_tokens integer not null default 0 check (window_image_output_tokens >= 0),
  window_image_request_count integer not null default 0 check (window_image_request_count >= 0),
  window_image_request_failed_count integer not null default 0 check (window_image_request_failed_count >= 0),
  window_started_at text,
  window_reset_at text,
  limit_window_seconds integer check (limit_window_seconds is null or limit_window_seconds > 0),
  last_used_at text
);

create index idx_account_usage_last_used_account
  on account_usage(last_used_at desc, account_id desc);
```

| 组 | 说明 |
| --- | --- |
| 累计 | 口径 = usage_records 成功事实；失败率不得由本表推断。`total_tokens` 是已知的"有偿冗余"（列表排序高频），与分量列同一 upsert 内更新，事务保证一致 |
| 窗口 | 当前 quota 窗口内计数，窗口重置清零。**窗口组刻意不含 `reasoning_tokens`/`total_tokens`**：窗口列只镜像上游 quota 核算维度，reasoning 不参与上游窗口计数；累计组是展示口径，两组不对称是设计而非遗漏 |
| 边界 | `window_started_at` / `window_reset_at` / `limit_window_seconds` 来自上游 rate-limit 响应，NULL = 未知 |

宽表（20 计数列）而非 EAV `(account_id, metric, value)`：计数种类是代码常量，宽表一次 upsert 全更新、check 逐列可写；EAV 只换来动态 SQL 和类型丢失。

### 4.9 account_model_usage

账号 × 模型分布，服务账号详情页与调度辅助。自然复合键，不引入代理 id。

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

create index idx_account_model_usage_last_used
  on account_model_usage(last_used_at desc, account_id, model);
```

`error_count` 是**收窄口径**：仅统计已归属到账号+模型之后的失败，用作调度回避信号（频繁失败 → 降权）。这是全库唯一允许的口径重叠（与 ops_error_logs），因为用途不同（调度局部信号 vs 全局错误事实）；Dashboard 错误率禁止取自本列。

### 4.10 usage_records ★

**每个成功的客户端请求恰好一行**。用量、token、成本、账号与调用方归因的唯一事实来源。这是系统的账本；"只收成功"的边界由 DB 约束兜底，不再只靠 service 层。

```sql
create table usage_records (
  id text primary key,
  request_id text,
  client_api_key_id text,
  kind text not null,
  route text,
  provider text not null,
  account_id text not null,
  model text not null,
  requested_model text,
  upstream_model text,
  service_tier text,
  status_code integer not null check (status_code between 200 and 399),
  transport text,
  attempt_index integer check (attempt_index is null or attempt_index >= 0),
  response_id text,
  upstream_request_id text,
  latency_ms integer check (latency_ms is null or latency_ms >= 0),
  first_token_ms integer check (first_token_ms is null or first_token_ms >= 0),
  input_tokens integer check (input_tokens is null or input_tokens >= 0),
  output_tokens integer check (output_tokens is null or output_tokens >= 0),
  cached_tokens integer check (cached_tokens is null or cached_tokens >= 0),
  reasoning_tokens integer check (reasoning_tokens is null or reasoning_tokens >= 0),
  message text not null,
  metadata_json text not null,
  created_at text not null
);

create index idx_usage_records_created_id on usage_records(created_at desc, id desc);
create index idx_usage_records_request_id on usage_records(request_id) where request_id is not null;
create index idx_usage_records_kind_created on usage_records(kind, created_at desc);
create index idx_usage_records_account_created on usage_records(account_id, created_at desc);
create index idx_usage_records_model_created on usage_records(model, created_at desc);
create index idx_usage_records_key_created on usage_records(client_api_key_id, created_at desc) where client_api_key_id is not null;
create index idx_usage_records_response_id on usage_records(response_id) where response_id is not null;
create index idx_usage_records_upstream_request_id on usage_records(upstream_request_id) where upstream_request_id is not null;
```

| 字段 | 说明 |
| --- | --- |
| `request_id` | 请求链路 ID，与 ops 表同域——跨两表串起一次请求的完整故事（成功前的失败尝试在 ops 表） |
| `client_api_key_id` | **调用方归因**（逻辑引用 client_api_keys.id，无 FK）。可空：管理端代发/内部探测请求无 key。归因列是事实字段，错过不可回补——按 key 用量、未来按 key 配额核算都依赖它 |
| `kind` / `route` | kind 是稳定事件族（`v1.response`），route 是原始 HTTP 事实；route 变体收敛到同一 kind，历史统计不断裂。映射只允许存在于 `event_kind()` 一处 |
| `provider` | **上游 provider 归属**（`'openai'`，规划中 `'cloudflare'`）。not null：成功请求必知上游。事实必须自包含——账号可删除、模型别名可重映射，provider 不能靠 join 推导。开放取值不加 check（SQLite 改 check 要重建大表，新 provider 只应是新增取值）。低基数列刻意不建索引 |
| `account_id` | **not null**：成功请求必有归属。无 FK（§3） |
| `model` / `requested_model` / `upstream_model` | 展示归因（计费默认）/ 客户端原始请求（别名映射前）/ 上游实际执行。三列语义互不混用 |
| `service_tier` | 计费单价第二维度 |
| `status_code` | **`between 200 and 399`**：终态响应码 + 只收成功，数据库层兜底。1xx 非终态，4xx/5xx 属 ops 表 |
| `transport` / `attempt_index` | 传输方式与成功发生在第几次尝试（>0 意味着此前有失败，记录在 ops 表） |
| `latency_ms` / `first_token_ms` | 端到端完成延迟 / 首 token 延迟。NULL = 未测得，区别于 0 |
| `input/output/cached/reasoning_tokens` | 一等列（§2.7）。NULL = 上游未报告，与 0 语义不同——因此不用 `not null default 0` |
| `message` | 一行人类可读摘要，必须脱敏 |
| `metadata_json` | 仅调试细节；已提升为列的字段不再写入 metadata |

8 个索引全部对应真实查询：keyset 分页 / 链路排查 / 事件族过滤 / 账号维度 / 模型维度 / 调用方维度 / response 续写排查 / 上游报障对账。每行 8 索引是全库最高写放大，是明细可查性的直接代价；量级失控的出路是缩短保留期（配置已就位），不是删索引。

**禁止**：写入失败事件；token 写回 metadata 当真相；加任何兼容过滤查询历史数据。

### 4.11 ops_error_logs ★

**每个失败请求/运维错误恰好一行**。与 usage_records 互斥：一次请求终态只进一张表。失败与成功的查询维度完全不同（阶段/分类/归责 vs token/成本），拆表让两边 schema 各自朝自己的查询模型演进。

```sql
create table ops_error_logs (
  id text primary key,
  request_id text,
  client_api_key_id text,
  kind text not null,
  provider text,
  account_id text,
  route text,
  model text,
  status_code integer check (status_code is null or (status_code between 100 and 599)),
  client_status_code integer check (client_status_code is null or (client_status_code between 100 and 599)),
  upstream_status_code integer check (upstream_status_code is null or (upstream_status_code between 100 and 599)),
  transport text,
  attempt_index integer check (attempt_index is null or attempt_index >= 0),
  failure_class text,
  response_id text,
  upstream_request_id text,
  latency_ms integer check (latency_ms is null or latency_ms >= 0),
  message text not null,
  metadata_json text not null,
  created_at text not null
);

create index idx_ops_error_logs_created_id on ops_error_logs(created_at desc, id desc);
create index idx_ops_error_logs_request_id on ops_error_logs(request_id) where request_id is not null;
create index idx_ops_error_logs_key_created on ops_error_logs(client_api_key_id, created_at desc) where client_api_key_id is not null;
create index idx_ops_error_logs_account on ops_error_logs(account_id, created_at desc) where account_id is not null;
create index idx_ops_error_logs_route_created on ops_error_logs(route, created_at desc) where route is not null;
create index idx_ops_error_logs_model_created on ops_error_logs(model, created_at desc) where model is not null;
create index idx_ops_error_logs_status_created on ops_error_logs(status_code, created_at desc) where status_code is not null;
create index idx_ops_error_logs_transport_created on ops_error_logs(transport, created_at desc) where transport is not null;
create index idx_ops_error_logs_failure_class on ops_error_logs(failure_class, created_at desc) where failure_class is not null;
create index idx_ops_error_logs_response_id on ops_error_logs(response_id) where response_id is not null;
create index idx_ops_error_logs_upstream_request_id on ops_error_logs(upstream_request_id) where upstream_request_id is not null;
```

| 字段 | 说明 |
| --- | --- |
| `client_api_key_id` | 调用方归因，可空（鉴权前失败、后台任务无 key） |
| `provider` | 上游 provider 归属，可空：模型解析前失败（鉴权失败、路由失败）时上游未定。与 `account_id` 同为"失败可以不知道，成功必须知道"的口径差异 |
| `account_id` | **可空是本表的关键差异**：调度前失败（无可用账号、模型不可用）天然无归属——这正是这类事件进不了 usage_records 的原因 |
| 三视角状态码 | `status_code`（事件主视角）/ `client_status_code`（用户看到什么）/ `upstream_status_code`（上游返回什么）。错误排查的第一个问题永远是"哪一层出的错" |
| `failure_class` | 开放取值不加 check：分类法仍在演进 |
| `metadata_json` | **禁止**存长请求/响应体（capture body 开启也必须截断脱敏）；**禁止**存计费 token——失败不是计费事实，将来统计失败中观测到的 token 必须用 `observed_` 前缀新列 |

11 个索引全部对应错误排查的真实切片；可空维度全部部分索引；错误表写入频率远低于成功表，写放大可承受。

**预留演进列（有 UI/告警需求时增列，刻意不预建，本清单为唯一权威版本）**：`surface`（client_api/admin/system）、`error_phase`（auth/dispatch/upstream/stream/parse/internal）、`error_type`（稳定错误类型）、`severity`（P1–P3）、`requested_model` / `upstream_model` / `service_tier`、`client_ip` / `user_agent`、`error_source` / `error_owner`、`is_retryable` / `retry_count`。原则：字段跟着真实查询需求走，不照搬外部项目全集。

### 4.12 request_time_buckets ★

15 分钟 × provider × 账号 × 模型 × service_tier 预聚合，Dashboard 趋势与流量卡片的唯一数据源。**单表同时服务两种口径**：traffic（success + error）与 usage（token，仅成功）。曾有双表方案，已否决：每请求两次 upsert 的写放大不值得；分列 + 写入纪律已同时提供两种口径，rebuild 命令兜底（保留期内，§2.9）。

```sql
create table request_time_buckets (
  bucket_start text not null,
  provider text not null default '__unknown__',
  account_id text not null default '__unknown__',
  model text not null default '__unknown__',
  service_tier text not null default '__unknown__',
  success_count integer not null default 0 check (success_count >= 0),
  error_count integer not null default 0 check (error_count >= 0),
  input_tokens integer not null default 0 check (input_tokens >= 0),
  output_tokens integer not null default 0 check (output_tokens >= 0),
  cached_tokens integer not null default 0 check (cached_tokens >= 0),
  first_token_latency_sum integer not null default 0 check (first_token_latency_sum >= 0),
  first_token_latency_count integer not null default 0 check (first_token_latency_count >= 0),
  latency_sum integer not null default 0 check (latency_sum >= 0),
  latency_count integer not null default 0 check (latency_count >= 0),
  max_latency_ms integer not null default 0 check (max_latency_ms >= 0),
  min_latency_ms integer check (min_latency_ms is null or min_latency_ms >= 0),
  updated_at text not null,
  primary key (bucket_start, provider, account_id, model, service_tier)
);

create index idx_request_time_buckets_model on request_time_buckets(model, bucket_start);
```

| 字段 | 说明 |
| --- | --- |
| `bucket_start` | UTC 时刻，按中国时区 15 分钟槽对齐（`china_quarter_hour_start`）。对齐规则只活在代码层 |
| 维度四列（provider / account_id / model / service_tier） | 不可空 + `__unknown__` sentinel（§2.5）。provider 由两条写入路径显式给值（成功/失败路径都知道自己在打哪家），`__unknown__` 仅兜底 |
| `success_count` / `error_count` | 分别由 usage / ops 写入路径 +1。**`request_count` 不落库**，恒等于两者之和，查询期推导——存储恒等式必然漂移 |
| token 三列 | **仅成功路径写入**；错误路径只 +1 `error_count`，不得触碰 token 与延迟列 |
| latency sum/count 对 | 平均值不可再聚合，sum/count 才能跨桶合并、跨维度上卷。**口径 = 仅成功**（失败延迟混入会让"变慢"和"在报错"互相污染） |
| `max_latency_ms` / `min_latency_ms` | 可单调合并的极值。min 可空：NULL = 无样本，`min()` 天然忽略 NULL。**check 是 `>= 0`**——0ms 是合法样本（缓存命中/本地短路），与 `latency_ms >= 0` 的合法域一致；`> 0` 会让 0ms 样本在同事务写入中炸掉整笔成功事实 |

**写入事务纪律**：事实 insert 与桶 upsert 必须同事务（§5.2）。

**禁止**：错误路径写 token/延迟列；落库 `request_count`；空字符串维度。

### 4.13 account_cookies

账号维度上游 Cookie（部分上游流程需要浏览器态凭据）。语义直接采用 RFC 6265 模型；unique 约束 = RFC 替换语义，upsert 依据。

```sql
create table account_cookies (
  id text primary key,
  account_id text not null references accounts(id) on delete cascade,
  domain text not null,
  name text not null,
  value text not null,
  path text not null default '/',
  expires_at text,
  updated_at text not null,
  unique(account_id, domain, name, path)
);

create index idx_account_cookies_account_domain on account_cookies(account_id, domain);
create index idx_account_cookies_expires on account_cookies(expires_at) where expires_at is not null;
```

`value` 是 secret（§2.6）。`expires_at` NULL = 会话 Cookie，不参与清理。v3 单独的 `idx_account_cookies_account` 被 `account_domain` 前缀覆盖，终态不保留。

### 4.14 fingerprints

设备指纹模板（UA、header 集合与顺序——顺序本身是指纹的一部分）。行数常量级、按 PK 取用，**无二级索引**。版本三元组用 TEXT：版本号不是数字（`1.2.3-beta`）。

```sql
create table fingerprints (
  id text primary key,
  originator text not null,
  app_version text not null,
  build_number text not null,
  platform text not null,
  arch text not null,
  chromium_version text not null,
  user_agent_template text not null,
  default_headers_json text not null,
  header_order_json text not null,
  source text not null,
  created_at text not null,
  updated_at text not null
);
```

### 4.15 fingerprint_update_history

指纹更新审计。版本快照列固化"当时更新到了什么"——不能只存 FK，指纹行会被后续更新覆写。级联删除可接受：指纹本体删除后其历史无独立价值。

```sql
create table fingerprint_update_history (
  id text primary key,
  current_fingerprint_id text not null references fingerprints(id) on delete cascade,
  app_version text not null,
  build_number text not null,
  chromium_version text,
  source text not null,
  manifest_json text,
  created_at text not null
);

create index idx_fingerprint_update_history_created_id
  on fingerprint_update_history(created_at desc, id desc);
create index idx_fingerprint_update_history_fingerprint
  on fingerprint_update_history(current_fingerprint_id);
```

### 4.16 model_plan_snapshots

套餐 → 可用模型清单缓存。刻意"最新态覆盖"而非追加历史——缓存表的正确形态，可随时清空重建。**禁止**当模型目录事实表用；需要 diff/审计时新建历史表。

```sql
create table model_plan_snapshots (
  plan_type text primary key,
  models_json text not null,
  fetched_at text not null
);
```

### 4.17 session_affinities

`response_id` → 账号/会话上下文映射，保证续写请求路由回同一账号（上游 response 链只在原账号内有效）。纯运行态：全表清空只影响路由优化，不损失事实。**禁止**承载会话历史/审计。

```sql
create table session_affinities (
  response_id text primary key,
  account_id text not null references accounts(id) on delete cascade,
  conversation_id text not null,
  turn_state text,
  instructions_hash text,
  input_tokens integer check (input_tokens is null or input_tokens >= 0),
  function_call_ids_json text not null default '[]',
  variant_hash text,
  expires_at text not null,
  created_at text not null
);

create index idx_session_affinities_conversation on session_affinities(conversation_id, created_at desc);
create index idx_session_affinities_active_order on session_affinities(expires_at, created_at, response_id);
create index idx_session_affinities_account on session_affinities(account_id);
```

`instructions_hash` / `variant_hash` 共同约束隐式续写安全（指令或参数变了就不能隐式续写）；存哈希不存原文。`active_order` 同时服务过期清理（前缀）与活跃排序；v3 单独的 `expires` 索引是其前缀，终态删除。`account` 索引是 FK 子表索引（级联删除路径）。

---

## 5. 统计口径（全系统唯一定义）

### 5.1 口径恒等式

任何 UI 卡片不得自创口径：

```text
traffic:   request_count = success_count + error_count     ← request_time_buckets（查询期推导）
usage:     token / 成本                                     ← usage_records 列（明细）、bucket token 列（趋势，仅成功）
errors:    错误分布 / 排查                                   ← ops_error_logs
按 key:    调用方用量 / 失败                                 ← usage_records / ops_error_logs 的 client_api_key_id
调度负载:   account_usage / account_model_usage              ← 派生累计，事务保护，不可重建（§2.9）
```

### 5.2 写入路径与事务

```text
请求成功 ─┬─ usage_records insert
          ├─ request_time_buckets upsert（success_count+1，token、延迟列）
          ├─ account_usage upsert（累计 + 窗口）
          └─ account_model_usage upsert
          全部在同一个事务（pool.begin() … commit）

请求失败 ─┬─ ops_error_logs insert
          └─ request_time_buckets upsert（error_count+1，仅此一列）
          同一个事务
```

### 5.3 Dashboard 口径决策（钉死）

- 所有 traffic 卡片（今日请求、区间请求、错误率、QPS 趋势）**一律来自 request_time_buckets**，语义为"bucket 保留期内"。卡片文案如实标注区间，**不提供也不暗示"历史总量"**——bucket 有 90 天保留期，任何"total"字样都是口径谎言。
- 生命周期累计仅有一处合法来源：`account_usage` 累计列（成功口径），使用时必须标注"成功累计"。
- 同一张卡片禁止混排两种口径的数字（v3 的 `todayRequests` 来自 bucket、`totalRequests` 来自 usage summary 就是这个病）。
- 真正的长期趋势需求 → 日粒度归档表 `request_day_buckets`（§8），不是延长 15 分钟桶保留期。

---

## 6. 迁移 0004：一次到位（v3 → v4）

### 6.1 前置：迁移框架支持 Rust 步骤

SQL 无法计算 SHA-256，0004 需要一个事务内的 Rust 钩子。改造 `database.rs`：

```rust
struct Migration {
    version: i64,
    name: &'static str,
    sql: &'static str,
    /// SQL 之后、同一事务内执行的 Rust 步骤（哈希回填等 SQL 做不到的事）。
    post: Option<fn(&mut Transaction<'_, Sqlite>) -> BoxFuture<'_, Result<(), sqlx::Error>>>,
}
```

应用顺序不变：`raw_sql(migration.sql)` → `post`（若有）→ `record_migration`，全程同一事务。`CURRENT_SCHEMA_VERSION = 4`。

### 6.2 0004_final_schema.sql（SQL 部分）

```sql
-- ===== admin_sessions：清空重建（哈希键） =====
-- 数据决策：现存会话全部作废，管理员重新登录。会话是运行态，无保留价值。
drop table admin_sessions;
create table admin_sessions ( … §4.3 DDL … );

-- ===== client_api_keys：改名暂存，Rust 步骤哈希回填 =====
alter table client_api_keys rename to client_api_keys_v3;
drop index if exists idx_client_api_keys_key_enabled;
drop index if exists idx_client_api_keys_created_id;
create table client_api_keys ( … §4.4 DDL … );
create index idx_client_api_keys_created_id on client_api_keys(created_at desc, id desc);

-- ===== runtime_settings：重建（admin_api_key → 暂存，Rust 哈希） =====
create table _migration_admin_key as select admin_api_key as value from runtime_settings;
alter table runtime_settings rename to runtime_settings_v3;
create table runtime_settings ( … §4.5 DDL … );
insert into runtime_settings (id, model_aliases_json, refresh_margin_seconds, refresh_concurrency,
  max_concurrent_per_account, request_interval_ms, rotation_strategy, admin_api_key_hash, updated_at)
select id, model_aliases_json, refresh_margin_seconds, refresh_concurrency,
  max_concurrent_per_account, request_interval_ms, rotation_strategy, null, updated_at
from runtime_settings_v3;
drop table runtime_settings_v3;

-- ===== ops_error_logs：先增列（下方 usage_records 迁移要写入这些列） =====
alter table ops_error_logs add column client_api_key_id text;
alter table ops_error_logs add column provider text;
update ops_error_logs set provider = 'openai';
create index idx_ops_error_logs_key_created on ops_error_logs(client_api_key_id, created_at desc) where client_api_key_id is not null;

-- ===== usage_records：重建为成功事实表 =====
alter table usage_records rename to usage_records_v3;
create table usage_records ( … §4.10 DDL 与全部索引 … );

-- 数据决策 1：满足成功契约的行迁入，token 等列从 metadata 提升（此后不再回读 metadata）。
insert into usage_records (
  id, request_id, client_api_key_id, kind, route, provider, account_id, model,
  requested_model, upstream_model, service_tier, status_code, transport, attempt_index,
  response_id, upstream_request_id, latency_ms, first_token_ms,
  input_tokens, output_tokens, cached_tokens, reasoning_tokens,
  message, metadata_json, created_at)
select
  id, request_id, null, kind, route, 'openai', account_id, model,
  nullif(trim(json_extract(metadata_json, '$.requestedModel')), ''),
  nullif(trim(json_extract(metadata_json, '$.upstreamModel')), ''),
  nullif(trim(json_extract(metadata_json, '$.serviceTier')), ''),
  status_code, transport, attempt_index,
  response_id, upstream_request_id, latency_ms,
  coalesce(cast(json_extract(metadata_json, '$.firstTokenMs') as integer),
           cast(json_extract(metadata_json, '$.usage.firstTokenMs') as integer)),
  coalesce(cast(json_extract(metadata_json, '$.usage.inputTokens') as integer),
           cast(json_extract(metadata_json, '$.inputTokens') as integer)),
  coalesce(cast(json_extract(metadata_json, '$.usage.outputTokens') as integer),
           cast(json_extract(metadata_json, '$.outputTokens') as integer)),
  coalesce(cast(json_extract(metadata_json, '$.usage.cachedTokens') as integer),
           cast(json_extract(metadata_json, '$.cachedTokens') as integer)),
  coalesce(cast(json_extract(metadata_json, '$.usage.reasoningTokens') as integer),
           cast(json_extract(metadata_json, '$.reasoningTokens') as integer)),
  message, metadata_json, created_at
from usage_records_v3
where level <> 'error'
  and account_id is not null and trim(account_id) <> ''
  and model is not null and trim(model) <> ''
  and status_code between 200 and 399;

-- 数据决策 2：历史错误行迁入 ops_error_logs（列同构）。
insert into ops_error_logs (
  id, request_id, client_api_key_id, kind, provider, account_id, route, model,
  status_code, client_status_code, upstream_status_code, transport, attempt_index,
  failure_class, response_id, upstream_request_id, latency_ms, message, metadata_json, created_at)
select
  id, request_id, null, kind, 'openai', account_id, route, model,
  status_code, null, upstream_status_code, transport, attempt_index,
  failure_class, response_id, upstream_request_id, latency_ms, message, metadata_json, created_at
from usage_records_v3
where level = 'error';

-- 数据决策 3：既非成功契约也非 error 的行（无账号/无模型/无终态状态码的历史噪音）不迁移，随 v3 表一起删除。
drop table usage_records_v3;

-- ===== usage_time_buckets → request_time_buckets =====
create table request_time_buckets ( … §4.12 DDL … );
create index idx_request_time_buckets_model on request_time_buckets(model, bucket_start);

-- 数据决策 4：历史桶迁入（sentinel 归一、success 推导、0-sentinel 转 NULL）。
-- 历史桶的 token 列含 v3 错误路径污染，按旧口径保留展示；事实保留期内的窗口
-- 随后由 rebuild-derived 用新口径重算覆盖（§6.4 验收第 4 步）。
insert into request_time_buckets (
  bucket_start, provider, account_id, model, service_tier,
  success_count, error_count, input_tokens, output_tokens, cached_tokens,
  first_token_latency_sum, first_token_latency_count,
  latency_sum, latency_count, max_latency_ms, min_latency_ms, updated_at)
select
  bucket_start,
  'openai',
  case when account_id = '' then '__unknown__' else account_id end,
  case when model = '' then '__unknown__' else model end,
  case when service_tier = '' then '__unknown__' else service_tier end,
  max(request_count - error_count, 0), error_count,
  input_tokens, output_tokens, cached_tokens,
  first_token_latency_sum, first_token_latency_count,
  latency_sum, latency_count, max_latency_ms,
  nullif(min_latency_ms, 0), updated_at
from usage_time_buckets;

drop table usage_time_buckets;

-- ===== 索引修正（其余表） =====
drop index if exists idx_session_affinities_expires;
create index idx_session_affinities_account on session_affinities(account_id);
create index idx_fingerprint_update_history_fingerprint on fingerprint_update_history(current_fingerprint_id);
drop index if exists idx_account_cookies_account;
```

### 6.3 0004 的 Rust 步骤（`post` 钩子，同一事务）

1. **client key 哈希搬迁**：读 `client_api_keys_v3` 全部行 → 每行计算 `SHA-256(key)` → insert 进新表（`key_hash` = 哈希，其余列原样）→ `drop table client_api_keys_v3`。明文自此不存在于库中。
2. **admin API key 哈希**：读 `_migration_admin_key.value`，非空则 `update runtime_settings set admin_api_key_hash = <sha256>`；`drop table _migration_admin_key`。

### 6.4 迁移后动作与验收

| # | 动作 | 验收 |
| --- | --- | --- |
| 1 | 事务提交后执行一次 `vacuum`（使 `auto_vacuum = incremental` 对旧库生效；vacuum 不能在事务内） | `pragma auto_vacuum` 返回 2 |
| 2 | 管理员重新登录（会话已清空） | 登录成功，新会话 `token_hash` 长度 64 |
| 3 | 抽查 `select count(*) from client_api_keys where length(key_hash) <> 64` = 0 | 明文 key 消失，鉴权仍通过 |
| 4 | 执行 `rebuild-derived`（§7.6），用新口径重算事实保留期内的桶 | 保留期内桶 token 列 = usage_records 聚合值 |
| 5 | 核对 Dashboard 卡片全部走 bucket 口径 | 同卡片无混合口径数字 |

**显式数据损失清单**（接受即执行，不做挽留方案）：全部管理端会话；v3 usage_records 中既非成功契约也非 error 的噪音行；历史桶中错误路径污染的 token 按旧口径保留、不追溯清洗（保留期内被 rebuild 覆盖为新口径）。

---

## 7. 代码改造清单

与 0004 同一 PR 交付，按依赖顺序：

1. **迁移框架**（`infra/database.rs`）：`Migration.post` 钩子；连接参数补 `synchronous(Normal)`、`auto_vacuum(Incremental)`；`CURRENT_SCHEMA_VERSION = 4`。
2. **鉴权归因**（`proxy/auth.rs`）：`authorize_client_api_key_result` 改为返回 key 的 `id`（现在验完即弃）；SHA-256 后查 `key_hash`。调用链把 `client_api_key_id` 装进请求上下文。
3. **事件结构**（`proxy/dispatch/usage_events.rs`、`proxy/dispatch/responses/event_recording.rs`）：成功/失败事件携带 `client_api_key_id`；token、requested/upstream_model、service_tier、first_token_ms 从 metadata 移到一等字段；metadata 不再写已提升字段。
4. **写入路径**（`admin/monitoring/usage_record_store.rs`、ops error store）：§5.2 的两条事务；错误路径对桶只 `error_count + 1`；`__unknown__` sentinel；min/max 延迟直接 `min()`/`max()`（无 0-sentinel 分支）。
5. **查询路径**（`usage_record_service.rs`、`dashboard.rs`、`account_usage_service.rs`）：summary/分布/趋势全部走列，删除所有 `json_extract` 聚合与 `like '%…%'` 搜索；Dashboard 按 §5.3 口径改卡片来源与文案。
6. **维护命令**：管理端 `rebuild-derived`——删除事实保留期内的桶并从两张事实表重算；保留期外只读不动。范围明确**不含** `account_usage` / `account_model_usage`（§2.9，不可重建）。
7. **清理任务**：三个 trim 任务改读 `runtime_settings` 三列；新增 bucket trim；每轮 trim 后 `pragma incremental_vacuum`。
8. **管理端 API**（`admin/auth/service.rs`、`config/settings.rs`、keys 路由）：session 创建/校验走 `token_hash`；`verify_admin_api_key` 对来键哈希后 `ct_eq`；key 列表响应删除完整 key 字段（只余 `prefix`）——**前端同步移除完整 key 展示**。
9. **测试**（`backend/tests/infra/storage_schema/`）：断言终态 DDL、迁移数据规则（成功行迁入 / error 行转 ops / 噪音行丢弃）、两条写入事务的原子性、0ms 延迟样本可入桶。

---

## 8. 扩展路径与明确不做

每个可预见的增长方向都有"只加不改"的路径：

| 未来需求 | 扩展方式 |
| --- | --- |
| 多管理员 | `admin_users` 加 `username`/`role` + 新表 `admin_login_events` |
| 按 key 限流/配额 | 新表 `client_api_key_policies (key_id FK, …)`；归因数据已在事实表就位 |
| 错误运维大盘 | `ops_error_logs` 按 §4.11 预留清单增列 |
| 长期趋势/年度账务 | 新表 `request_day_buckets`（由 15 分钟桶降采样归档），Dashboard 才允许出现"历史总量" |
| 刷新历史分析 | 新表 `account_refresh_events` |
| 模型别名审计 | `model_aliases_json` 拆 `model_aliases` 表 |
| 凭据静态加密 / 合规 | `accounts` 拆 `account_credentials`（1:1，PK=FK），集中一个破坏窗口 |
| 接入第二上游 provider（已规划：Cloudflare Workers AI） | 事实表与桶的 `provider` 维度已在 v4 就位（§4.10–4.12），CF 首条请求落库前无需再动事实表。届时一个迁移完成：`accounts` 加 `provider` + `chatgpt_account_id/user_id` 更名 `upstream_account_id/upstream_user_id` + 唯一索引加 provider 维度；`model_plan_snapshots` PK 改 `(provider, plan_type)`；billing 单价查找加 provider 参数。回填全部确定为 `'openai'` |
| 多实例部署 | SQLite → Postgres：TEXT 时间戳 → timestamptz、`check in` → enum、upsert 语法兼容；租约表天然支持竞争 |

Cloudflare 账号对现有 schema 的适配（无需新列）：静态 API key = `access_token`，`refresh_token`/`access_token_expires_at` NULL（永不过期，语义已覆盖）；无套餐配额窗口 → `account_usage.window_*` 恒为初始值、窗口边界 NULL；`account_cookies` 对 CF 账号空置。**硬规则**：任何新 provider 的第一条请求落库之前，其事实行必须已携带 provider 归因——v4 之后这条自动满足。

**明确不做**：无多实例需求前不引入外部数据库/缓存；不为历史数据加任何兼容过滤；不预建"可能有用"的列——每一列都必须有当前的真实读者；不照搬外部项目的字段全集。
