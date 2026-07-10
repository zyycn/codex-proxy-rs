# Database —— 唯一权威文档

本文档是 Codex Proxy RS 存储层的**唯一**权威文档:终态形态为 **PostgreSQL(持久层)+ Redis(运行态/缓存)**。全量定义 PG Schema、Redis 键契约、全局纪律、统计口径、SQLite → PG 的一次性搬迁方案与代码改造清单。

- 取代 2026-07-08 版(SQLite 单进程形态 + 0004 迁移草案)。旧版描述的 v4 终态 Schema 语义**全部保留**,宿主引擎由 SQLite 换为 PG + Redis,0004 迁移方案改由导入命令吸收。
- 基线:生产 SQLite 库 schema v3(读侧,只读打开);PG 库从 0001 终态基线全新建立(写侧)。
- 策略:**一次破坏性切换直达终态,不做兼容层**。没有双写期、没有 SQLite 回退路径、没有查询层兼容过滤;不满足终态契约的历史数据按 §6.3 规则导入或丢弃,丢弃项全部显式列出。

---

## 1. 运行形态

**单实例部署;PostgreSQL 承载一切持久数据,Redis 承载一切运行态与缓存。** 选择外置存储是运维形态决策,不以多实例为目标(**不考虑多实例部署**):

- PG:真正的数据库服务器——在线备份、成熟工具链、管理端重查询与热路径写入不再争抢单写者;容器内不再有本地数据文件。
- Redis:运行态的原生 TTL 语义——会话/租约/亲和的过期由存储自身表达,三个清理任务与"重启恢复"步骤整体消失。

| 存储 | 承载 | 丢失语义 |
| --- | --- | --- |
| PostgreSQL | 元数据、配置、接入凭据、账号、事实、派生聚合 | 不可丢失,常规备份 |
| Redis | 管理会话、刷新租约、会话亲和、模型清单缓存 | 可丢失:会话重登、租约自愈、亲和退化为重新调度、缓存重拉 |

连接参数(`infra/database.rs` / `infra/redis.rs`,全部显式):

| 参数 | 值 | 理由 |
| --- | --- | --- |
| PG 池 `max_connections` | 10 | 自托管代理的写并发远低于此 |
| PG 池 `acquire_timeout` | 5s | 排队上限,接替 SQLite `busy_timeout` 的角色 |
| Redis 连接 | 单条多路复用连接(ConnectionManager,自动重连) | 命令全部为 O(1)/O(logN) 小操作,无需连接池 |
| Redis 键前缀 | `cpr:` | 与共用实例的其他应用隔离;测试用随机前缀隔离 |

**明确不做**:PG 侧不用 LISTEN/NOTIFY、不用 advisory lock(互斥一律走 Redis 租约);Redis 侧不开键空间通知、不承载任何"不可丢失"数据。

---

## 2. 全局纪律

### 2.1 命名

| 对象 | 规则 | 示例 |
| --- | --- | --- |
| 表名 | snake_case;实体集合复数;子表 `<父实体单数>_<内容>` | `accounts`、`account_cookies` |
| 表名后缀 | 事实 `_records`/`_logs`;聚合 `_buckets`;审计 `_history` | `usage_records` |
| 列名 | snake_case,全库同义同名 | `account_id` 处处一致 |
| 时间戳 | `*_at`,**timestamptz** | `created_at` |
| 时长 | `*_seconds` / `*_ms`,bigint | `latency_ms` |
| 计数 | `*_count`,bigint,`check >= 0` | `request_count` |
| JSON | `*_json`,**jsonb** | `metadata_json` |
| 布尔 | 裸形容词/过去式,**boolean** | `enabled` |
| 外键/逻辑引用列 | `<被引用实体单数>_id` | `client_api_key_id` |
| 凭据哈希 | `*_hash`,用于不可逆凭据(§2.6) | `password_hash`、`token_hash` |
| 主键 | `id`,除非自然复合键或哈希键 | — |
| 普通/唯一索引 | `idx_<表>_<列摘要>` / `ux_<表>_<列摘要>` | `ux_accounts_chatgpt_identity` |
| 双端状态码 | 视角前缀 | `status_code` / `client_status_code` / `upstream_status_code` |
| 模型归因 | 来源前缀 | `model` / `requested_model` / `upstream_model` |
| Redis 键 | `cpr:<域>:<实体>:<键值>`,冒号分层 | `cpr:lease:refresh:<account_id>` |

### 2.2 类型

| 用途 | PG 类型 | 约定 |
| --- | --- | --- |
| ID、名称、枚举、哈希 | text | 枚举加 `check in (...)` |
| 时间戳 | timestamptz | 库内一律 UTC 时刻 |
| 计数、时长 | bigint | 计数 `>= 0` |
| 状态码 | integer | `between` check |
| 布尔 | boolean | — |
| 快照/模板/调试 JSON | jsonb | 见 §2.7 |

**枚举用 text + check,不用 PG 原生 enum**(有意修订旧文档 §8 的一句话设想):原生 enum 增删值要 `alter type` 且值永远无法删除;check 约束换一条 `drop/add constraint` 即完成枚举演进(SQLite 0002 改名先例证明枚举确实会变)。**不使用 real/double 存业务数据,不存储金额**。成本一律查询期由 token × 单价计算(`telemetry/billing.rs`):单价会变、需追溯重算,落库的浮点成本既漂移又过期。

### 2.3 时间戳

- 库内一律 timestamptz;代码层统一 `chrono::DateTime<Utc>` 直接绑定,**不再经 RFC3339 文本中转**。
- RFC3339 文本只存在于两处边界:API 响应序列化、keyset 分页游标(游标文本解码回 `DateTime<Utc>` 后再进 SQL,比较发生在 timestamptz 域)。
- 时区换算(中国时区展示、15 分钟槽对齐)只在代码层(`infra/time.rs` 的 `china_*` 系列),数据库不感知时区。+08:00 是整小时偏移,15 分钟槽在 UTC 与中国时区下对齐结果相同,SQL 侧重算桶可直接 `to_timestamp(floor(extract(epoch from t) / 900) * 900)`。

### 2.4 ID

主键一律 text 随机 ID(`infra/identity.rs` 生成):无中心分配、写入前可生成、跨表无碰撞。例外:`schema_migrations.version`(天然有序整数)、`runtime_settings.id`(单例锚点)。**不用 serial/identity/uuid 列类型**——ID 是业务字符串,不是数据库生成物。

### 2.5 NULL 与 sentinel

- "无值 / 未观测"一律 NULL;**禁止** 0、空字符串做哨兵。NULL 与 0 语义不同(`input_tokens` NULL = 上游未报告,0 = 确认为零)。
- 聚合维度列不可空时,未知值用 `__unknown__`;预聚合汇总行(预留)用 `__all__`。
- 唯一例外:`ux_accounts_chatgpt_identity` 的索引表达式 `coalesce(chatgpt_user_id, '')`——空串只存在于索引表达式,不是存储值,不构成先例。

### 2.6 Secret 纪律与可取回边界

凡是"出示即获得权限"的值都是凭据。默认只保存不可逆摘要；但客户端 API key 与上游凭据有明确的可取回功能需求，必须可逆保存。所有可逆凭据均不得进入日志、遥测或非管理端响应，数据库备份与访问权限必须按 secret 级别保护。

| 凭据 | 存储 | 哈希类型 | 理由 |
| --- | --- | --- | --- |
| 管理员密码 | PG `admin_users.password_hash` | 慢哈希(argon2) | 低熵人类输入,必须抗字典攻击 |
| 客户端 API key | PG `client_api_keys.key` 明文列,标记 secret | —(功能上必须可逆) | 管理端需要长期复制并导入 CCSwitch；鉴权走 `key` unique 索引点查，列表只对已认证管理员返回完整值 |
| 管理端 session token | Redis `cpr:admin:session:<token_hash>` | SHA-256 | 键即哈希。Redis RDB/AOF 或内存转储泄露不再等于会话劫持 |
| 管理 API key | PG `runtime_settings.admin_api_key_hash` | SHA-256 | 校验 = 哈希后 `ct_eq`;哈希不可脱敏展示,状态 API 只回答"是否已启用" |
| 上游 access/refresh token、Cookie value | PG 明文列,标记 secret | —(功能上必须可逆) | 不进日志、不进列表 API;静态加密的触发条件见 §8 |

### 2.7 JSON 边界

`*_json` 列(jsonb)只允许三类内容:① 上游原样快照(`quota_json`、`manifest_json`);② 结构化模板(`default_headers_json`、`header_order_json`);③ 调试细节(`metadata_json`)。**凡进入 WHERE / GROUP BY / 告警的字段必须是一等列**;列一旦提升即为唯一可查询真相,写入时不再往 metadata 双写已提升字段,查询路径禁止出现 `->>` 聚合。

### 2.8 索引

1. 每个索引对应一条真实查询;unique 约束已覆盖的等值查找不再建索引。
2. 列表分页统一 keyset 模式,配 `(排序时间 desc, id desc)` 复合索引。
3. 可空列上的筛选索引一律部分索引 `where <col> is not null`。
4. FK 子表必须有以 FK 列开头的索引(PG 级联删除同样会在子表无索引时全扫)。免除条件必须注明。
5. 高频写入表的索引总数进 code review,新增需说明查询来源。

### 2.9 Redis 纪律

1. **每个键有确定的过期路径**:TTL(会话、租约、亲和)或"最新态覆盖"(缓存 HASH)。禁止只增不减的键空间。
2. **可丢弃性是准入条件**:FLUSHALL 之后系统必须无数据损失地继续运行(会话重登、租约重竞争、亲和退化为普通调度、缓存重拉)。不满足即不属于 Redis。
3. **互斥必须原子**:获取用 `SET NX PX`,续约/释放用 Lua 比较 owner 后操作,禁止 GET-判断-SET 三段式。
4. 二级索引(亲和的 conversation ZSET / account SET)由写入方维护,读取方容忍成员悬垂(主键已过期),遇悬垂惰性清理。
5. 单个业务对象统一编码为完整 JSON 文本,不把对象属性拆成 Redis Hash 字段。集合缓存可使用 HASH（如 `cpr:models:plan_snapshots`），但每个 field 的 value 仍是一个完整 JSON 对象，整读整写。

### 2.10 保留与可重建性(诚实边界)

| 数据 | 保留 | 配置/机制 |
| --- | --- | --- |
| `usage_records` | 30 天(默认) | `runtime_settings.usage_retention_days`,周期 trim 任务 |
| `ops_error_logs` | 30 天(默认) | `runtime_settings.ops_error_retention_days`,同上 |
| `request_time_buckets` | 90 天(默认) | `runtime_settings.bucket_retention_days`,同上 |
| `account_cookies` | 按 `expires_at` 清理 | 周期任务(PG) |
| Redis 会话/租约/亲和 | TTL 自然过期 | 无清理任务 |
| 其余 PG 表 | 常量级增长 | — |

- trim 是**周期后台任务**(每轮读 `runtime_settings` 三列),不再在每次写入后内联执行——v3 的"每写一次 delete 一次"是已知的写放大缺陷,终态修复。
- `request_time_buckets` 在事实保留期内可由两张事实表重算(`rebuild-buckets` 命令,§7);超出事实保留期的桶是**因事实过期而升格的一级数据**,写入纪律 + 同事务写入(§5.2)是它们的唯一保障。
- `account_usage` / `account_model_usage` 的累计列**不可重建**(事实早已过期),同样只受写入纪律保护。本文档不声称"一切派生皆可重算"。
- `window_*` 列是运行态(窗口边界来自上游),不可重建,可丢弃。

---

## 3. 架构分层与关系

```text
┌─ PostgreSQL ──────────────────────────────────────────┐
│ ┌─ 元数据 ────────────────────────────────────────┐   │
│ │  schema_migrations                              │   │
│ ├─ 配置 ──────────────────────────────────────────┤   │
│ │  runtime_settings(单例热配置)                    │   │
│ ├─ 接入 ──────────────────────────────────────────┤   │
│ │  admin_users · client_api_keys                  │   │
│ ├─ 账号 ──────────────────────────────────────────┤   │
│ │  accounts · account_cookies · fingerprints      │   │
│ │  fingerprint_update_history                     │   │
│ ├─ 事实(不可替代的业务事件)──────────────────────┤   │
│ │  usage_records(成功) · ops_error_logs(失败)    │   │
│ └─ 派生(口径见 §2.10)──────────────────────────┘   │
│    request_time_buckets · account_usage             │
│    account_model_usage                              │
└─────────────────────────────────────────────────────┘
┌─ Redis(运行态/缓存,可丢弃)─────────────────────────┐
│  cpr:admin:session:*       管理端会话(TTL)          │
│  cpr:lease:refresh:*       token 刷新互斥租约(TTL)  │
│  cpr:affinity:*            会话亲和 + 二级索引(TTL) │
│  cpr:models:plan_snapshots 套餐→模型清单缓存(HASH)  │
└─────────────────────────────────────────────────────┘
```

```text
accounts     1──1 account_usage                  FK cascade(PK=FK)
accounts     1──< account_model_usage            FK cascade(复合 PK 前缀)
accounts     1──< account_cookies                FK cascade(account_domain 前缀)
fingerprints 1──< fingerprint_update_history     FK cascade(idx_…_fingerprint)

usage_records.{account_id, client_api_key_id}    → 逻辑引用,无 FK
ops_error_logs.{account_id, client_api_key_id}   → 逻辑引用,无 FK
request_time_buckets.account_id                  → 逻辑维度,无 FK

Redis 亲和.account_id → 账号删除时经 cpr:affinity:account:<id> 索引显式清理(§4B.3)
Redis 租约.account_id → 不清理,TTL(分钟级)自然过期
```

**事实表/聚合表不建 FK 是有意为之**:审计事实的生命周期必须独立于账号与 key——删除账号或 key 不得抹掉历史用量和错误记录。运行态相反:账号没了亲和/租约就失去意义,v3 的 FK 级联在 Redis 中由"显式清理(亲和)+ TTL 自愈(租约)"等价实现。

`admin_users` 与 Redis 会话之间不存在级联:单管理员模型下删除用户无入口;未来多管理员需要"删用户踢会话"时,增加 `cpr:admin:user_sessions:<user_id>` SET 索引(§8)。

---

## 4. PostgreSQL 表设计(终态 DDL)

以下 DDL 即 `infra/migrations/0001_initial.sql`(PG 基线)。PG 库从此基线全新建立;历史数据经导入命令进入(§6),不存在 SQLite → PG 的迁移链。

### 4.1 schema_migrations

迁移版本的唯一事实来源。由 `database.rs` 建库时创建,不走迁移文件。只增不改;已发布迁移永不修改;`CURRENT_SCHEMA_VERSION` 与迁移列表同步提交。PG 谱系从 1 重新编号,与 SQLite 谱系(止于 3)无延续关系。

```sql
create table schema_migrations (
  version bigint not null check (version > 0),
  name text not null,
  applied_at timestamptz not null default now(),
  primary key (version)
);
```

### 4.2 admin_users

单管理员模型的登录凭据。不预建 `username`/`email`/`role` 死列;多管理员的扩展路径见 §8。会话不在 PG(§4B.1)。

```sql
create table admin_users (
  id text primary key,
  password_hash text not null,
  created_at timestamptz not null,
  updated_at timestamptz not null
);
```

### 4.3 client_api_keys

客户端调用 `/v1/*` 的凭据。key 与上游账号解耦,客户端不感知账号池。

```sql
create table client_api_keys (
  id text primary key,
  name text not null,
  prefix text not null,
  key text not null unique,
  label text,
  enabled boolean not null default true,
  created_at timestamptz not null,
  last_used_at timestamptz
);

create index idx_client_api_keys_created_id on client_api_keys(created_at desc, id desc);
```

| 字段 | 说明 |
| --- | --- |
| `id` | 稳定标识;管理操作与事实归因(`usage_records.client_api_key_id`)都引用 id,key 轮换不影响引用 |
| `name` / `label` | `name` 必填主展示,`label` 可选注记 |
| `prefix` | 明文前几位(`sk_abc1…`),用于列表快速定位与遮罩展示 |
| `key` | 完整客户端凭据,unique 索引即鉴权查找路径。只在已认证管理端列表/创建响应返回,供长期复制与导入 CCSwitch；严禁进入日志和遥测 |
| `enabled` | 软开关;刻意不建索引(低基数布尔) |
| `last_used_at` | 鉴权成功后延迟批量更新(1s 去抖),不在请求关键路径强一致 |

**鉴权路径**:`where key = $1 and enabled` unique 索引单点查询,**删除进程内明文鉴权表**——单一代码路径、禁用即刻生效、无缓存同步逻辑。管理端取回与客户端鉴权读取同一事实列,不存在 hash/明文双写。每 key 配额/限流不进本表(§8)。

### 4.4 runtime_settings

热更新全局配置,单行表,`check (id = 1)` 约束层面保证单例。选宽表不选 KV:列级类型与 check 是 KV 给不了的;加配置要加列,恰好强迫走迁移评审。只放小型标量配置,禁塞大 JSON 状态。

```sql
create table runtime_settings (
  id bigint primary key check (id = 1),
  model_aliases_json jsonb not null default '{}',
  refresh_margin_seconds bigint not null check (refresh_margin_seconds > 0),
  refresh_concurrency bigint not null check (refresh_concurrency > 0),
  max_concurrent_per_account bigint not null check (max_concurrent_per_account > 0),
  request_interval_ms bigint not null check (request_interval_ms >= 0),
  rotation_strategy text not null check (rotation_strategy in ('smart', 'quota_reset_priority', 'round_robin', 'sticky')),
  admin_api_key_hash text,
  usage_retention_days bigint not null default 30 check (usage_retention_days > 0),
  ops_error_retention_days bigint not null default 30 check (ops_error_retention_days > 0),
  bucket_retention_days bigint not null default 90 check (bucket_retention_days > 0),
  updated_at timestamptz not null
);
```

| 字段 | 说明 |
| --- | --- |
| `model_aliases_json` | 客户端模型名 → 上游模型名映射。JSON 合规:整体读写、不按别名查询 |
| `refresh_*` / `max_concurrent_per_account` / `request_interval_ms` | 刷新与调度参数 |
| `rotation_strategy` | 枚举 check;枚举值变更必须走迁移(SQLite 0002 先例),库内无死值 |
| `admin_api_key_hash` | 管理 API 机器凭据的 SHA-256。NULL = 未启用。校验 = 对来键哈希后 `ct_eq`。哈希不可脱敏,状态 API 只回答存在性 |
| `*_retention_days` 三列 | 三张增长表各自独立的保留天数,trim 任务每轮读取。bucket 默认更长:趋势比明细有更长查询价值 |

### 4.5 accounts

上游账号的身份、凭据、调度状态与 quota 快照。**保持单表是当前正确决策**:调度器每次决策同时读身份+状态+冷却,拆表只增加 join 与事务面;拆分触发条件明确(§8)。

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
  access_token_expires_at timestamptz,
  next_refresh_at timestamptz,
  status text not null check (status in ('active', 'expired', 'quota_exhausted', 'disabled', 'banned')),
  quota_json jsonb,
  quota_fetched_at timestamptz,
  quota_limit_reached boolean not null default false,
  quota_verify_required boolean not null default false,
  quota_cooldown_until timestamptz,
  cloudflare_cooldown_until timestamptz,
  added_at timestamptz not null,
  updated_at timestamptz not null
);

create index idx_accounts_status on accounts(status);
create index idx_accounts_added_id on accounts(added_at desc, id desc);
create unique index ux_accounts_chatgpt_identity
  on accounts(chatgpt_account_id, coalesce(chatgpt_user_id, ''))
  where chatgpt_account_id is not null;
```

要点:

- **身份**:`id` 与上游 ID 解耦(导入半成品账号仍可运转);`chatgpt_account_id` + `chatgpt_user_id` 复合唯一防重复导入,身份未知的账号不参与去重(部分索引)。`plan_type` 不加枚举 check:取值由上游定义。
- **凭据(secret)**:`access_token` not null 是账号存在的最低要求;`refresh_token` NULL = 不可续期,到期即 `expired`。不进日志、不进列表 API。
- **状态**:单列业务状态机 + 布尔/冷却补充列,避免状态爆炸。`refreshing` 不落 `accounts.status`,由 Redis 刷新租约 / 进程内 in-flight 派生为运行时展示态。冷却用时间而非布尔:自然过期无需回写。quota 与 CF 冷却分列:成因、时长、解除策略都不同。
- **快照**:`quota_json` 上游原样快照(JSON 合规);`added_at` 语义化命名——账号是"被添加进池子"的。
- 池加载顺序 = `added_at asc, id asc`(替代 SQLite 隐式 rowid 序,语义相同且跨引擎稳定)。

### 4.6 account_usage

账号维度两组计数:① 生命周期累计(列表展示,**不可重建**,§2.10);② 当前 quota 窗口计数(调度限流输入,运行态)。1:1 派生表,不与 accounts 合并:更新频率差多个数量级,合表会让高频计数写污染低频实体行。

```sql
create table account_usage (
  account_id text primary key references accounts(id) on delete cascade,
  request_count bigint not null default 0 check (request_count >= 0),
  empty_response_count bigint not null default 0 check (empty_response_count >= 0),
  input_tokens bigint not null default 0 check (input_tokens >= 0),
  output_tokens bigint not null default 0 check (output_tokens >= 0),
  cached_tokens bigint not null default 0 check (cached_tokens >= 0),
  reasoning_tokens bigint not null default 0 check (reasoning_tokens >= 0),
  total_tokens bigint not null default 0 check (total_tokens >= 0),
  image_input_tokens bigint not null default 0 check (image_input_tokens >= 0),
  image_output_tokens bigint not null default 0 check (image_output_tokens >= 0),
  image_request_count bigint not null default 0 check (image_request_count >= 0),
  image_request_failed_count bigint not null default 0 check (image_request_failed_count >= 0),
  window_request_count bigint not null default 0 check (window_request_count >= 0),
  window_input_tokens bigint not null default 0 check (window_input_tokens >= 0),
  window_output_tokens bigint not null default 0 check (window_output_tokens >= 0),
  window_cached_tokens bigint not null default 0 check (window_cached_tokens >= 0),
  window_image_input_tokens bigint not null default 0 check (window_image_input_tokens >= 0),
  window_image_output_tokens bigint not null default 0 check (window_image_output_tokens >= 0),
  window_image_request_count bigint not null default 0 check (window_image_request_count >= 0),
  window_image_request_failed_count bigint not null default 0 check (window_image_request_failed_count >= 0),
  window_started_at timestamptz,
  window_reset_at timestamptz,
  limit_window_seconds bigint check (limit_window_seconds is null or limit_window_seconds > 0),
  last_used_at timestamptz
);

create index idx_account_usage_last_used_account
  on account_usage(last_used_at desc, account_id desc);
```

| 组 | 说明 |
| --- | --- |
| 累计 | 口径 = usage_records 成功事实;失败率不得由本表推断。`total_tokens` 是已知的"有偿冗余"(列表排序高频),与分量列同一 upsert 内更新 |
| 窗口 | 当前 quota 窗口内计数,窗口重置清零。**窗口组刻意不含 `reasoning_tokens`/`total_tokens`**:窗口列只镜像上游 quota 核算维度,累计组是展示口径,两组不对称是设计而非遗漏 |
| 边界 | `window_started_at` / `window_reset_at` / `limit_window_seconds` 来自上游 rate-limit 响应,NULL = 未知 |

宽表(20 计数列)而非 EAV:计数种类是代码常量,宽表一次 upsert 全更新、check 逐列可写;EAV 只换来动态 SQL 和类型丢失。

### 4.7 account_model_usage

账号 × 模型分布,服务账号详情页与调度辅助。自然复合键,不引入代理 id。

```sql
create table account_model_usage (
  account_id text not null references accounts(id) on delete cascade,
  model text not null,
  request_count bigint not null default 0 check (request_count >= 0),
  error_count bigint not null default 0 check (error_count >= 0),
  input_tokens bigint not null default 0 check (input_tokens >= 0),
  output_tokens bigint not null default 0 check (output_tokens >= 0),
  cached_tokens bigint not null default 0 check (cached_tokens >= 0),
  last_used_at timestamptz,
  primary key (account_id, model)
);

create index idx_account_model_usage_last_used
  on account_model_usage(last_used_at desc, account_id, model);
```

`error_count` 是**收窄口径**:仅统计已归属到账号+模型之后的失败,用作调度回避信号(频繁失败 → 降权)。这是全库唯一允许的口径重叠(与 ops_error_logs),因为用途不同(调度局部信号 vs 全局错误事实);Dashboard 错误率禁止取自本列。

### 4.8 usage_records ★

**每个成功的客户端请求恰好一行**。用量、token、成本、账号与调用方归因的唯一事实来源。这是系统的账本;"只收成功"的边界由 DB 约束兜底,不再只靠 service 层。

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
  attempt_index bigint check (attempt_index is null or attempt_index >= 0),
  response_id text,
  upstream_request_id text,
  latency_ms bigint check (latency_ms is null or latency_ms >= 0),
  first_token_ms bigint check (first_token_ms is null or first_token_ms >= 0),
  input_tokens bigint check (input_tokens is null or input_tokens >= 0),
  output_tokens bigint check (output_tokens is null or output_tokens >= 0),
  cached_tokens bigint check (cached_tokens is null or cached_tokens >= 0),
  reasoning_tokens bigint check (reasoning_tokens is null or reasoning_tokens >= 0),
  message text not null,
  metadata_json jsonb not null,
  created_at timestamptz not null
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
| `request_id` | 请求链路 ID,与 ops 表同域——跨两表串起一次请求的完整故事(成功前的失败尝试在 ops 表) |
| `client_api_key_id` | **调用方归因**(逻辑引用 client_api_keys.id,无 FK)。可空:管理端代发/内部探测请求无 key。归因列是事实字段,错过不可回补——按 key 用量、未来按 key 配额核算都依赖它 |
| `kind` / `route` | kind 是稳定事件族(`v1.response`),route 是原始 HTTP 事实;route 变体收敛到同一 kind,历史统计不断裂。映射只允许存在于 `event_kind()` 一处 |
| `provider` | **上游 provider 归属**(`'openai'`,规划中 `'cloudflare'`)。not null:成功请求必知上游。事实必须自包含——账号可删除、模型别名可重映射,provider 不能靠 join 推导。开放取值不加 check(新 provider 只应是新增取值)。低基数列刻意不建索引 |
| `account_id` | **not null**:成功请求必有归属。无 FK(§3) |
| `model` / `requested_model` / `upstream_model` | 展示归因(计费默认)/ 客户端原始请求(别名映射前)/ 上游实际执行。三列语义互不混用 |
| `service_tier` | 计费单价第二维度 |
| `status_code` | **`between 200 and 399`**:终态响应码 + 只收成功,数据库层兜底。1xx 非终态,4xx/5xx 属 ops 表 |
| `transport` / `attempt_index` | 传输方式与成功发生在第几次尝试(>0 意味着此前有失败,记录在 ops 表) |
| `latency_ms` / `first_token_ms` | 端到端完成延迟 / 首 token 延迟。NULL = 未测得,区别于 0 |
| `input/output/cached/reasoning_tokens` | 一等列(§2.7)。NULL = 上游未报告,与 0 语义不同——因此不用 `not null default 0` |
| `message` | 一行人类可读摘要,必须脱敏 |
| `metadata_json` | 仅调试细节;已提升为列的字段不再写入 metadata |

8 个索引全部对应真实查询:keyset 分页 / 链路排查 / 事件族过滤 / 账号维度 / 模型维度 / 调用方维度 / response 续写排查 / 上游报障对账。每行 8 索引是全库最高写放大,是明细可查性的直接代价;量级失控的出路是缩短保留期(配置已就位),不是删索引。

**搜索边界**:列表 API 的 `search` 参数只匹配 `message ilike` 与 request_id/response_id 精确等值;**禁止**对 `metadata_json` 做任何 contains 扫描——metadata 是调试细节,不是查询面。

**禁止**:写入失败事件;token 写回 metadata 当真相;加任何兼容过滤查询历史数据。

### 4.9 ops_error_logs ★

**每个失败请求/运维错误恰好一行**。与 usage_records 互斥:一次请求终态只进一张表。失败与成功的查询维度完全不同(阶段/分类/归责 vs token/成本),拆表让两边 schema 各自朝自己的查询模型演进。

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
  attempt_index bigint check (attempt_index is null or attempt_index >= 0),
  failure_class text,
  response_id text,
  upstream_request_id text,
  latency_ms bigint check (latency_ms is null or latency_ms >= 0),
  message text not null,
  metadata_json jsonb not null,
  created_at timestamptz not null
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
| `client_api_key_id` | 调用方归因,可空(鉴权前失败、后台任务无 key) |
| `provider` | 上游 provider 归属,可空:模型解析前失败(鉴权失败、路由失败)时上游未定。与 `account_id` 同为"失败可以不知道,成功必须知道"的口径差异 |
| `account_id` | **可空是本表的关键差异**:调度前失败(无可用账号、模型不可用)天然无归属——这正是这类事件进不了 usage_records 的原因 |
| 三视角状态码 | `status_code`(事件主视角)/ `client_status_code`(用户看到什么)/ `upstream_status_code`(上游返回什么)。错误排查的第一个问题永远是"哪一层出的错" |
| `failure_class` | 开放取值不加 check:分类法仍在演进 |
| `metadata_json` | **禁止**存长请求/响应体(capture body 开启也必须截断脱敏);**禁止**存计费 token——失败不是计费事实,将来统计失败中观测到的 token 必须用 `observed_` 前缀新列 |

错误明细的管理端查询面(`/api/admin/ops/errors`)直接读本表;成功事件列表不再混排错误行。11 个索引全部对应错误排查的真实切片;可空维度全部部分索引;错误表写入频率远低于成功表,写放大可承受。

**预留演进列(有 UI/告警需求时增列,刻意不预建,本清单为唯一权威版本)**:`surface`、`error_phase`、`error_type`、`severity`、`requested_model` / `upstream_model` / `service_tier`、`client_ip` / `user_agent`、`error_source` / `error_owner`、`is_retryable` / `retry_count`。原则:字段跟着真实查询需求走,不照搬外部项目全集。

### 4.10 request_time_buckets ★

15 分钟 × provider × 账号 × 模型 × service_tier 预聚合,Dashboard 趋势与流量卡片的唯一数据源。**单表同时服务两种口径**:traffic(success + error)与 usage(token,仅成功)。曾有双表方案,已否决:每请求两次 upsert 的写放大不值得;分列 + 写入纪律已同时提供两种口径,rebuild 命令兜底(保留期内,§2.10)。

```sql
create table request_time_buckets (
  bucket_start timestamptz not null,
  provider text not null default '__unknown__',
  account_id text not null default '__unknown__',
  model text not null default '__unknown__',
  service_tier text not null default '__unknown__',
  success_count bigint not null default 0 check (success_count >= 0),
  error_count bigint not null default 0 check (error_count >= 0),
  input_tokens bigint not null default 0 check (input_tokens >= 0),
  output_tokens bigint not null default 0 check (output_tokens >= 0),
  cached_tokens bigint not null default 0 check (cached_tokens >= 0),
  first_token_latency_sum bigint not null default 0 check (first_token_latency_sum >= 0),
  first_token_latency_count bigint not null default 0 check (first_token_latency_count >= 0),
  latency_sum bigint not null default 0 check (latency_sum >= 0),
  latency_count bigint not null default 0 check (latency_count >= 0),
  max_latency_ms bigint not null default 0 check (max_latency_ms >= 0),
  min_latency_ms bigint check (min_latency_ms is null or min_latency_ms >= 0),
  updated_at timestamptz not null,
  primary key (bucket_start, provider, account_id, model, service_tier)
);

create index idx_request_time_buckets_model on request_time_buckets(model, bucket_start);
```

| 字段 | 说明 |
| --- | --- |
| `bucket_start` | 15 分钟槽起点(UTC 槽与中国时区槽对齐结果相同,§2.3) |
| 维度四列 | 不可空 + `__unknown__` sentinel(§2.5)。provider 由两条写入路径显式给值(成功/失败路径都知道自己在打哪家),`__unknown__` 仅兜底 |
| `success_count` / `error_count` | 分别由成功/失败事务 +1。**`request_count` 不落库**,恒等于两者之和,查询期推导——存储恒等式必然漂移 |
| token 三列 | **仅成功路径写入**;错误路径只 +1 `error_count`,不得触碰 token 与延迟列 |
| latency sum/count 对 | 平均值不可再聚合,sum/count 才能跨桶合并、跨维度上卷。**口径 = 仅成功**(失败延迟混入会让"变慢"和"在报错"互相污染) |
| `max_latency_ms` / `min_latency_ms` | 可单调合并的极值(`greatest()`/`least()`)。min 可空:NULL = 无样本,`min()` 天然忽略 NULL。**check 是 `>= 0`**——0ms 是合法样本(缓存命中/本地短路),`> 0` 会让 0ms 样本在同事务写入中炸掉整笔成功事实 |

**写入事务纪律**:事实 insert 与桶 upsert 必须同事务(§5.2)。

**禁止**:错误路径写 token/延迟列;落库 `request_count`;空字符串维度。

### 4.11 account_cookies

账号维度上游 Cookie(部分上游流程需要浏览器态凭据)。语义直接采用 RFC 6265 模型;unique 约束 = RFC 替换语义,upsert 依据。

```sql
create table account_cookies (
  id text primary key,
  account_id text not null references accounts(id) on delete cascade,
  domain text not null,
  name text not null,
  value text not null,
  path text not null default '/',
  expires_at timestamptz,
  updated_at timestamptz not null,
  unique(account_id, domain, name, path)
);

create index idx_account_cookies_account_domain on account_cookies(account_id, domain);
create index idx_account_cookies_expires on account_cookies(expires_at) where expires_at is not null;
```

`value` 是 secret(§2.6)。`expires_at` NULL = 会话 Cookie,不参与清理。**过期时间在捕获时解析为 timestamptz**(RFC2822/RFC3339 均接受,解析失败按会话 Cookie 落 NULL)——v3 存原始字符串再做字典序比较是已知缺陷,终态修复。

### 4.12 fingerprints

设备指纹模板(UA、header 集合与顺序——顺序本身是指纹的一部分)。行数常量级、按 PK 取用,**无二级索引**。版本三元组用 text:版本号不是数字(`1.2.3-beta`)。

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
  default_headers_json jsonb not null,
  header_order_json jsonb not null,
  source text not null,
  created_at timestamptz not null,
  updated_at timestamptz not null
);
```

### 4.13 fingerprint_update_history

指纹更新审计。版本快照列固化"当时更新到了什么"——不能只存 FK,指纹行会被后续更新覆写。级联删除可接受:指纹本体删除后其历史无独立价值。

```sql
create table fingerprint_update_history (
  id text primary key,
  current_fingerprint_id text not null references fingerprints(id) on delete cascade,
  app_version text not null,
  build_number text not null,
  chromium_version text,
  source text not null,
  manifest_json jsonb,
  created_at timestamptz not null
);

create index idx_fingerprint_update_history_created_id
  on fingerprint_update_history(created_at desc, id desc);
create index idx_fingerprint_update_history_fingerprint
  on fingerprint_update_history(current_fingerprint_id);
```

---

## 4B. Redis 键契约(终态)

运行态与缓存的唯一权威定义。所有键带 `cpr:` 前缀(测试环境随机前缀隔离)。值为 JSON 文本(§2.9)。

### 4B.1 管理端会话

```text
键     cpr:admin:session:<token_hash>          token_hash = SHA-256(session token) hex
值     {"userId":"...","createdAt":"..."}
TTL    session_ttl_minutes(创建时一次性设定)
```

- 客户端持有明文 token(`sess_` + 256bit 随机),鉴权时哈希后 GET,存在即有效——过期由 TTL 表达,不再有 `expires_at` 比较,**清理任务消失**。
- 登出 = DEL;"重启不掉线"由 Redis 持久化(RDB/AOF)提供;Redis 冷启丢失 = 管理员重登,可接受(§2.9)。
- IP/UA 审计不进会话值;需要时新建 PG `admin_login_events`(§8)。

### 4B.2 token 刷新互斥租约

```text
键     cpr:lease:refresh:<account_id>
值     owner(实例/任务标识,不透明字符串)
TTL    租约时长(acquire 时以 PX 设定)
```

- 获取:`SET key owner NX PX ttl`;失败后允许**同 owner 重入续约**(Lua:GET == owner 则 `SET PX` 刷新)。
- 释放:Lua 比较 owner 后 DEL,防误释放他人租约。
- 活跃租约批查(运行时展示 refreshing):`MGET` 一批键,非空即持有。
- 持有者崩溃 → TTL 自然失效,无需清理任务。历史不承载(需要时另建 PG `account_refresh_events`)。

### 4B.3 会话亲和

```text
主键   cpr:affinity:resp:<response_id>
值     {"accountId","conversationId","turnState","instructionsHash",
        "inputTokens","functionCallIds":[…],"variantHash","createdAt"}
TTL    亲和 TTL(默认 4h),EXPIREAT createdAt+ttl

索引   cpr:affinity:conv:<conversation_id>     ZSET member=response_id score=createdAt(epoch ms)
索引   cpr:affinity:account:<account_id>       SET  member=response_id
```

- 写入(记录一次成功响应):MULTI 内 `SET resp EXAT` + `ZADD conv` + `EXPIRE conv ttl` + `SADD account` + `EXPIRE account ttl`,并顺手 `ZREMRANGEBYSCORE conv -inf now-ttl` 惰性剪枝。
- 按 response_id 查:GET 主键。按 conversation 查最新:`ZREVRANGE conv` 取候选 → GET 主键过滤 variant_hash / max_age → 第一个存活者;悬垂成员(主键已过期)当场 ZREM。
- forget(response_id):GET 后 DEL 主键 + ZREM conv + SREM account。
- **账号删除的级联**:SMEMBERS `cpr:affinity:account:<id>` → 逐个 forget。等价于 v3 的 FK cascade + `idx_session_affinities_account`。
- **进程内不再维护亲和映射副本**:Redis 本身就是快存,进程内再留一份只换来双写同步与重启恢复逻辑;亲和查找相对上游请求(百 ms 级)是 <1ms 的往返,可忽略。v3 的"重启恢复"步骤随之消失。
- **禁止**承载会话历史/审计;全部键被清空只影响路由优化,不损失事实。

### 4B.4 模型清单缓存

```text
键     cpr:models:plan_snapshots               HASH field=<plan_type>
值     {"models":[…],"fetchedAt":"..."}
TTL    无(最新态覆盖,HSET 即替换)
```

缓存的正确形态,可随时清空重建(丢失 = 下次按需重拉)。**禁止**当模型目录事实表用;需要 diff/审计时新建 PG 历史表。

---

## 5. 统计口径(全系统唯一定义)

### 5.1 口径恒等式

任何 UI 卡片不得自创口径:

```text
traffic:   request_count = success_count + error_count     ← request_time_buckets(查询期推导)
usage:     token / 成本                                     ← usage_records 列(明细)、bucket token 列(趋势,仅成功)
errors:    错误分布 / 排查                                   ← ops_error_logs
按 key:    调用方用量 / 失败                                 ← usage_records / ops_error_logs 的 client_api_key_id
调度负载:   account_usage / account_model_usage              ← 派生累计,不可重建(§2.10)
```

### 5.2 写入路径与事务

```text
请求成功 ─┬─ usage_records insert
          └─ request_time_buckets upsert(success_count+1,token、延迟列)
          同一个 PG 事务(pool.begin() … commit)

请求失败 ─┬─ ops_error_logs insert
          └─ request_time_buckets upsert(error_count+1,仅此一列)
          同一个 PG 事务
```

`account_usage` / `account_model_usage` 的 upsert **不在**上述事务内:它们由调度器在槽位释放 / 用量观测时点独立写入(与事实写入不在同一代码时刻),每笔 upsert 自身原子。这是对旧文档"四表同事务"的**显式修订**:强行合并需要把调度器的持久化时点搬进 dispatch 事务,复杂度换不来对账收益——两边口径本就允许亚秒级漂移,对账以事实表为准。

### 5.3 Dashboard 口径决策(钉死)

- 周期 traffic 卡片(今日请求、区间请求、错误率、QPS 趋势)**一律来自 request_time_buckets**,语义为"bucket 保留期内"；生命周期累计卡片不从 bucket 推导。
- 生命周期累计仅有一处合法来源:`account_usage` 累计列(成功口径)。这是后端数据语义，不要求改动既有前端文案。
- 同一张卡片禁止混排两种口径的数字(v3 的 `todayRequests` 来自 bucket、`totalRequests` 来自 usage_records 全表扫描就是这个病)。
- 错误明细与错误分布来自 ops_error_logs;成功事件列表不再混排错误行。
- 真正的长期趋势需求 → 日粒度归档表 `request_day_buckets`(§8),不是延长 15 分钟桶保留期。

---

## 6. SQLite v3 → PG 一次性搬迁

引擎切换,不存在就地迁移:PG 库由 0001 基线全新建立,历史数据经**导入命令**搬入。旧文档的 0004 迁移草案(数据决策 1–4 与管理 API key 哈希回填)全部由导入命令吸收,SQLite 侧不再演进(v3 即其终版)。

### 6.1 导入命令

```text
codex-proxy-rs import-sqlite <旧库路径.sqlite>
```

- 前置:目标 PG 为空库(仅 0001 基线,无业务行),否则拒绝执行——导入是一次性动作,不做增量合并。
- 源库校验:`schema_migrations` 最高版本必须为 3,否则拒绝(未知谱系不猜)。
- 全程单个 PG 事务:要么全部导入,要么全部回滚。源库只读打开。
- Redis 不参与导入:v3 的运行态数据按 §6.3 显式丢弃。
- 结束打印逐表行数、规范化计数与丢弃计数报告。

### 6.2 逐表规则

| v3 源表 | 终态去向 | 变换 |
| --- | --- | --- |
| `admin_users` | PG 原样 | 时间戳 text→timestamptz |
| `client_api_keys` | PG | `key`、`prefix` 原值搬迁；完整 key 继续作为管理端长期复制与 CCSwitch 导入的唯一事实 |
| `runtime_settings` | PG | `admin_api_key` 明文 → 哈希落 `admin_api_key_hash`;三个 retention 列取默认值 |
| `accounts` | PG | 布尔 0/1→boolean,时间戳→timestamptz；旧版曾落库的瞬态 `refreshing` → `expired` 并清空 `next_refresh_at`，防止迁移后自动消费 RT；其余业务状态原样 |
| `account_usage` / `account_model_usage` | PG 原样 | 同上 |
| `account_cookies` | PG | `expires_at` 字符串解析为 timestamptz(RFC2822/RFC3339);解析失败 → NULL(降级为会话 Cookie)并计数报告 |
| `fingerprints` / `fingerprint_update_history` | PG 原样 | JSON text→jsonb |
| `usage_records`(v3 混合表) | **拆分**,见 §6.3 | — |
| `usage_time_buckets` | `request_time_buckets` | `provider='openai'`;空串维度→`__unknown__`;`success_count=max(request_count-error_count,0)`;`min_latency_ms` 0→NULL |
| `ops_error_logs`(v3) | PG | 增列 `client_api_key_id=NULL`、`provider='openai'` |
| `admin_sessions` / `session_affinities` / `account_refresh_leases` / `model_plan_snapshots` | **不导入** | 运行态/缓存,§6.3 损失清单 |

### 6.3 v3 usage_records 的拆分决策(原 0004 数据决策)

1. **成功契约行迁入 usage_records**:`level <> 'error'` 且 `account_id`/`model` 非空 且 `status_code` 200–399。token、`requested_model`/`upstream_model`/`service_tier`/`first_token_ms` 从 metadata 提升为列(`usage.*` 优先、顶层回退),此后不再回读 metadata;`provider='openai'`;`client_api_key_id=NULL`(v3 未归因,错过不可回补——这正是归因列必须尽早存在的证明)。
2. **`level = 'error'` 行迁入 ops_error_logs**(列同构映射,`client_status_code=NULL`)。
3. **既非成功契约也非 error 的行**(无账号/无模型/无终态状态码的历史噪音)**不迁移**,计数后随源库废弃。

**显式数据损失清单**(接受即执行,不做挽留方案):

- 全部管理端会话(重新登录)、会话亲和(退化为重新调度)、刷新租约(自愈)、模型清单快照(按需重拉)——运行态按 §2.9 本就可丢弃;
- v3 usage_records 中的噪音行(决策 3);
- 旧版误落库的 `accounts.status='refreshing'` 不作为业务状态保留，按 §6.2 规范化为 `expired`；
- 历史桶中错误路径污染的 token 按旧口径保留展示、不追溯清洗;事实保留期内的窗口随后由 `rebuild-buckets` 用新口径重算覆盖。

### 6.4 迁移后动作与验收

| # | 动作 | 验收 |
| --- | --- | --- |
| 1 | 服务指向 PG+Redis 启动 | `/healthz` 204(PG `select 1` + Redis `PING` 双活) |
| 2 | 管理员重新登录 | 登录成功;Redis 出现 `cpr:admin:session:*`,TTL ≈ session_ttl |
| 3 | 抽查 `select count(*) from client_api_keys where key is null or key = ''` = 0 | 旧 key 值不变；鉴权、管理端复制与 CCSwitch 导入均可用 |
| 3a | 抽查 `select count(*) from accounts where status = 'refreshing'` = 0 | 旧瞬态状态已规范化为 `expired`，且 `next_refresh_at is null` |
| 4 | 执行 `rebuild-buckets`(§7),用新口径重算事实保留期内的桶 | 保留期内桶 token 列 = usage_records 聚合值 |
| 5 | 核对 Dashboard 周期卡片走 bucket、生命周期累计走 account_usage，前端既有文案不变 | 同卡片无混合口径数字 |

---

## 7. 代码改造清单(本次交付)

与导入命令同一 PR 交付,按依赖顺序。**2026-07-10 已逐项核验完成**，对应自动化验收由后端集成测试、前端构建、架构检查和 Compose 配置检查共同覆盖：

1. **连接层**(`infra/database.rs` + `infra/redis.rs`):PgPool(参数见 §1)+ 迁移框架(PG 谱系 0001 起);Redis ConnectionManager + 键前缀 + `PING` 健康检查。`Cargo.toml`:sqlx 加 `postgres`(保留 `sqlite` 仅供导入命令读源库),新增 `redis` crate。
2. **配置**(`bootstrap/config.rs`、`deploy/config.example.yaml`):`database.url` 改 `postgres://…`,新增 `redis.url`。
3. **鉴权归因**(`keys/{store,service,manage}.rs`、`api/client/auth.rs`):key 创建落唯一 `key` 列；鉴权按完整 key 做 PG unique 点查(删除进程内鉴权缓存);鉴权返回 key `id`,调用链把 `client_api_key_id` 装进请求上下文直至事件写入。管理端列表持续返回完整 key，前端长期保留复制与 CCSwitch 导入入口。
4. **事件结构**(`telemetry/{usage,ops}`、`telemetry/recorder.rs`、`dispatch/recording.rs`):成功/失败事件携带 `client_api_key_id` 与 `provider`;token、requested/upstream_model、service_tier、first_token_ms 从 metadata 移到一等字段;metadata 不再写已提升字段;UsageRecord 删除 `level`(成功事实无等级)。
5. **写入路径**(`telemetry/usage/store.rs`、`telemetry/ops/store.rs`、`telemetry/buckets/store.rs`):§5.2 的两条 PG 事务;错误路径对桶只 `error_count + 1`;`__unknown__` sentinel;min/max 延迟使用 nullable-safe 的 `least()`/`greatest()`;内联 trim 移除。
6. **查询路径**(`telemetry/{usage,ops,account_usage}`、`api/admin/{usage,ops,dashboard,accounts}_routes*`):summary/分布/趋势全部走列,删除所有 `->>` 聚合与 metadata LIKE;新增 `/api/admin/ops/errors` 错误明细查询面;Dashboard 按 §5.3 只改取数来源，不改既有文案。
7. **运行态改 Redis**(`auth/store.rs` 会话、`accounts/refresh/lease.rs`、`dispatch/affinity/store.rs`、`models/store.rs`):按 §4B 契约实现;删除 admin 会话/亲和清理任务与亲和重启恢复;账号删除路径挂 Redis 亲和级联(§4B.3)。
8. **维护命令**(`main.rs` CLI):`import-sqlite <path>`(§6);`rebuild-buckets`——删除事实保留期内的桶并从两张事实表重算(15 分钟槽对齐在 SQL 侧 `floor(epoch/900)*900`,§2.3);保留期外只读不动。范围明确**不含** `account_usage` / `account_model_usage`(§2.10,不可重建)。
9. **清理任务**(`bootstrap/tasks/`):三张增长表的周期 trim(读 `runtime_settings` 三列);cookie 清理保留;admin 会话与亲和清理任务删除(TTL 接管)。
10. **健康检查**(`api/router.rs`):PG `select 1` + Redis `PING`,任一失败即 503。
11. **测试**(`backend/tests/`):support 换 PG(每测试独立数据库,`CPR_TEST_DATABASE_URL` 指定服务端)+ Redis(每测试随机键前缀,`CPR_TEST_REDIS_URL`);storage 测试断言终态 DDL、导入拆分规则(成功行迁入 / error 行转 ops / 噪音行丢弃 / 旧 `refreshing` 规范化)、两条写入事务的原子性、0ms 延迟样本可入桶、租约原子性。
12. **部署**(`deploy/docker-compose.yml`):新增 postgres、redis 服务与健康检查依赖。

---

## 8. 扩展路径与明确不做

每个可预见的增长方向都有"只加不改"的路径:

| 未来需求 | 扩展方式 |
| --- | --- |
| 多管理员 | `admin_users` 加 `username`/`role` + PG `admin_login_events` + Redis `cpr:admin:user_sessions:<user_id>` SET(踢会话索引) |
| 按 key 限流/配额 | 新表 `client_api_key_policies (key_id FK, …)`;归因数据已在事实表就位;运行态计数走 Redis |
| 错误运维大盘 | `ops_error_logs` 按 §4.9 预留清单增列 |
| 长期趋势/年度账务 | 新表 `request_day_buckets`(由 15 分钟桶降采样归档),Dashboard 才允许出现"历史总量" |
| 刷新历史分析 | 新表 `account_refresh_events` |
| 模型别名审计 | `model_aliases_json` 拆 `model_aliases` 表 |
| 凭据静态加密 / 合规 | `accounts` 拆 `account_credentials`(1:1,PK=FK),集中一个破坏窗口 |
| 接入第二上游 provider(已规划:Cloudflare Workers AI) | 事实表与桶的 `provider` 维度已就位(§4.8–4.10),CF 首条请求落库前无需再动事实表。届时一个迁移完成:`accounts` 加 `provider` + `chatgpt_account_id/user_id` 更名 `upstream_account_id/upstream_user_id` + 唯一索引加 provider 维度;模型缓存 HASH field 改 `<provider>:<plan_type>`;billing 单价查找加 provider 参数。回填全部确定为 `'openai'` |

Cloudflare 账号对现有 schema 的适配(无需新列):静态 API key = `access_token`,`refresh_token`/`access_token_expires_at` NULL(永不过期,语义已覆盖);无套餐配额窗口 → `account_usage.window_*` 恒为初始值、窗口边界 NULL;`account_cookies` 对 CF 账号空置。**硬规则**:任何新 provider 的第一条请求落库之前,其事实行必须已携带 provider 归因——终态之后这条自动满足。

**明确不做**:**不考虑多实例部署**——调度器、登录限速、`last_used_at` 去抖、CF 风控跟踪均为单进程语义,PG/Redis 的选择不以横向扩容为目标;不保留 SQLite 运行路径(源库只被导入命令只读打开);不为历史数据加任何兼容过滤;不预建"可能有用"的列——每一列都必须有当前的真实读者;不照搬外部项目的字段全集;Redis 不承载任何不可丢失数据。
