# 多平台统一 AI 网关终态数据模型

本文定义 [多平台统一 AI 网关目标架构](multi-provider-architecture.md) 对应的终态 PostgreSQL 与 Redis 数据边界。它是目标设计，不表示当前迁移已经完成；当前生效表仍以 [architecture.md](architecture.md#数据存储) 和仓库迁移为准。

本文中的表名、字段归属、事实层级、删除规则和秘密存储规则是终态约束。实际迁移可以分阶段完成，但不能长期保留两套生产事实来源。

## 设计决策

1. PostgreSQL 是持久事实的唯一权威来源；Redis 只保存可过期、可重建的协调状态。
2. Provider 是编译进进程的代码，不创建 `providers` 表；数据库只保存可以动态配置的 Provider instance。
3. Router 选择 Provider instance 和 model target，Provider 自己选择 credential。
4. 调用方隔离和策略单元固定为现有 `client_api_key`，不提前引入 tenant、organization 或 project。
5. 下游 client API key 只保存不可逆摘要；上游 API Key、OAuth token、Cookie 等可恢复秘密保存加密 envelope 或外部 secret reference。
6. 稳定、需要查询的跨平台事实使用普通列；Provider 专属且不参与通用查询的内容使用经过 adapter 校验的 `jsonb`。
7. 客户端的一次请求记录为 logical request；每次真实上游调用记录为 attempt。成功、失败和取消不再分散到不同事实表。
8. logical request usage 表示客户端实际收到的用量；attempt usage 表示上游实际报告、可能产生费用的用量。
9. 默认不保存 prompt、response body 或原始 Provider 错误正文。Portable continuation 的内容使用独立加密表。
10. 聚合桶是派生数据，不参与调度、计费结算或请求正确性判断。

## 类型与命名约定

- ID 使用带语义前缀的应用生成 `text`，例如 `inst_`、`cred_`、`route_`、`req_`、`att_`。分页始终使用时间与 ID 组合游标，不依赖 ID 的自然顺序。
- 所有时间点使用 `timestamptz`，应用按 UTC 读写。
- token、次数和毫秒使用非负 `bigint`；状态码使用受范围约束的 `integer`。
- 金额使用 `numeric(20, 10)`，禁止浮点；货币使用通过 `^[A-Z]{3}$` 校验的大写三字符代码。
- `jsonb` 列只接受 object，并使用 `jsonb_typeof(...) = 'object'` check；空对象是否有效由字段语义决定，写入 RuntimeSnapshot 前再由 owner adapter 完整校验。
- 不使用 PostgreSQL enum。稳定生命周期状态使用 `text + check`；Provider、协议、operation、metric 等可扩展标识由应用注册表校验。
- 外键只表达真实所有权。配置实体优先 disable 而不是删除；历史事实不能随配置删除而消失。
- 不为 `jsonb` 默认创建 GIN index。只有出现稳定查询并有执行计划证据时才增加。
- `text` 不代表允许无限输入。名称、外部 ID、错误消息、URL 和 JSON 都必须由 owner 设置长度/字节上限；客户端或 Provider 提供的值在进入数据库前截断或拒绝，不能让遥测成为无界存储入口。
- 所有数据库生命周期时间由 PostgreSQL 时钟生成；延迟由进程单调时钟计算后写入，不能用两台机器的 wall clock 相减。Provider 提供的时间只能作为带来源的观察值。
- 所有会改变运行行为的 JSON（policy、config、capabilities、limits、options、state）必须包含正整数 `schema_version`，由唯一 owner 做版本迁移和完整校验。`metadata_json` 不是逃避 schema 演进的配置入口。
- `__...__` 前缀保留给聚合维度中的系统 sentinel；Provider、模型、tier、protocol、operation 等外部或注册标识不得使用该前缀。

### 主键、外键与快照字段命名

- 表名使用复数名词，独立实体的主键默认命名为 `id`；只有纯关联表或严格一对一 extension table 才可用复合键或父实体 FK 作主键。
- 外键字段使用被引用实体的完整单数名加 `_id`，例如 `provider_instance_id`、`upstream_credential_id`、`model_route_id` 和 `gateway_request_id`。
- 同一父表存在多个语义角色时增加角色前缀，例如 `actor_admin_user_id`。
- 复合外键字段与父键同名，例如 `(provider_instance_id, upstream_model_id)`。
- `_id` 也可用于明确限定来源的外部协议标识，例如 `upstream_request_id` 和 `client_response_id`；这类字段必须带 `upstream`、`client` 等来源前缀，不能伪装成本地外键。
- 配置删除后需要保留的历史维度使用 `_ref`，例如 `provider_instance_ref`、`model_route_ref` 和 `resource_ref`。`_ref` 永远不是外键。
- 不使用含义不清的 `route_id`、`request_id`、`credential_id`、`transcript_id` 或历史 `_key` 缩写。

约束和索引显式命名：

```text
pk_<table>
fk_<child_table>__<parent_table>
ux_<table>__<columns>
idx_<table>__<columns>
ck_<table>__<rule>
```

每个外键都必须有以该外键列开头的主键、唯一索引或普通索引，避免父行更新和删除时扫描子表。不能为了“可能查询”重复创建已经被主键或唯一约束覆盖的同列索引。

### 身份字段不可重绑定

一个 ID 一旦进入 request/attempt 历史，就不能被改造成另一项资源：

- Provider instance 的 `provider_kind`、endpoint authority、region/issuer 等身份字段不可原位替换；变更这些内容创建新 instance。展示名称和非身份参数可以更新。
- Credential 不能移动到另一 instance，`resource_ref` 不可修改；普通 secret 轮换只有在 adapter 确认仍是同一上游主体时才沿用原 credential，否则创建新行。
- Provider model 的复合主键不可更新；模型重命名视为新模型。
- Model route 的 `public_model_id` 不可改绑到另一个外部名称；需要改名时创建新 route 并显式迁移调用方策略。
- Model route target 的 route、instance 和 upstream model 关联不可更新；切换目标时创建新 target，只有 priority、weight、enabled 和 options 可以修改。
- 已发布 price version、request、attempt、audit 和 conversation item 都是 append/finalize 事实，不能通过 UPDATE 改写身份。

这些规则由 repository API 和契约测试保证；数据库 FK 只防悬空引用，不能自动判断“同一个 ID 的业务含义是否被偷换”。

## 关系图

```text
admin_users
  `-- admin_audit_events
client_api_keys
  |-- gateway_requests --< request_attempts -- model_price_versions
  |     `-- optional source --> continuation_bindings
  |
  `-- continuation_bindings -- source response --> gateway_requests
          |-- native_state_envelope
          `-- portable --> conversation_transcripts
                            |-- parent --> conversation_transcripts
                            `-- conversation_items

provider_instances
  |-- upstream_credentials
  |     |-- upstream_credential_states
  |     |-- codex_accounts
  |     `-- codex_account_cookies
  |-- provider_models
  |     `-- model_price_versions
  |           `-- model_price_rates
  `-- model_route_targets -- model_routes

ops_events
request_metric_buckets     [derived]
attempt_metric_buckets     [derived]
```

## Writer 所有权

| 数据 | 唯一 writer |
| --- | --- |
| `admin_users`、管理会话 | Auth owner |
| `client_api_keys`、`runtime_settings` | Control-plane settings owner |
| `admin_audit_events` | Audit owner；控制面修改必须在业务 transaction 内追加 |
| `provider_instances`、`provider_models`、routes、prices | Control-plane config publisher |
| `upstream_credentials` secret/config | Provider credential manager |
| `upstream_credential_states` | Provider state owner |
| `codex_accounts`、`codex_account_cookies` | Codex Provider owner |
| `gateway_requests`、`request_attempts` | Gateway request/attempt lifecycle |
| `conversation_*`、`continuation_bindings` | History/continuation owner |
| `ops_events` | Ops recorder |
| 两张 metric bucket | 唯一 bucket aggregator |

API handler、Dashboard query、Provider transport 和后台清理任务不能绕过 owner 直接拼 SQL 修改业务事实。Retention worker 只删除已经满足本文生命周期条件的数据。

## 身份与全局设置

### `admin_users`

沿用当前管理员表：

| 字段 | 类型与约束 | 语义 |
| --- | --- | --- |
| `id` | `text primary key` | 管理员 ID |
| `password_hash` | `text not null` | Argon2 密码摘要 |
| `created_at` | `timestamptz not null` | 创建时间 |
| `updated_at` | `timestamptz not null` | 修改时间 |

管理员登录会话继续保存在 Redis。`runtime_settings.admin_api_key_hash` 继续保存高熵管理 Key 的摘要，不新增另一套管理凭据表。

### `admin_audit_events`

控制面修改必须生成不可变、已脱敏的审计事实：

| 字段 | 类型与约束 | 语义 |
| --- | --- | --- |
| `id` | `text primary key` | Audit event ID |
| `actor_kind` | `text not null` | `admin_session/admin_api_key/system/anonymous` |
| `actor_admin_user_id` | `text references admin_users on delete set null` | 可选管理员 live join |
| `actor_ref` | `text not null` | Actor 历史伪名；不是 session/API Key 本身 |
| `admin_request_id` | `text` | 管理 HTTP request ID，不是外键 |
| `action` | `text not null` | create、update、disable、delete、rotate 等 |
| `entity_kind` | `text not null` | provider_instance、credential、route、price 等 |
| `entity_ref` | `text not null` | 被修改实体的历史引用，不是外键 |
| `config_revision` | `bigint` | 本次修改发布的结构配置版本 |
| `details_json` | `jsonb not null default '{}'` | 已脱敏字段变化或安全事件详情，不含 secret 值 |
| `created_at` | `timestamptz not null` | 发生时间 |

约束和索引：

```text
actor_kind in ('admin_session', 'admin_api_key', 'system', 'anonymous')
(created_at desc, id desc)
(entity_kind, entity_ref, created_at desc, id desc)
(actor_admin_user_id, created_at desc, id desc)
  where actor_admin_user_id is not null
(actor_ref, created_at desc, id desc)
```

Credential 创建、替换和轮换只记录“secret 已变化”、secret reference 类型和新 revision，绝不能记录明文、密文、fingerprint 或外部 secret 路径。管理员登录成功、失败、登出、密码/API Key 轮换和强制删除也进入本表；未认证事件使用 `actor_kind = 'anonymous'`、空 actor FK 和经过速率限制的安全详情，避免审计表本身成为攻击者制造的无界写入通道。

### `client_api_keys`

下游 Key 只允许在创建时返回一次完整值：

| 字段 | 类型与约束 | 语义 |
| --- | --- | --- |
| `id` | `text primary key` | Key ID |
| `name` | `text not null` | 名称 |
| `label` | `text` | 管理标签 |
| `prefix` | `text not null` | 管理端展示前缀 |
| `key_hash` | `text not null unique` | 完整 Key 的 SHA-256 摘要 |
| `enabled` | `boolean not null default true` | 管理员开关 |
| `policy_json` | `jsonb not null` | 带 schema version 的模型 allowlist、速率、并发和预算策略 |
| `created_at` | `timestamptz not null` | 创建时间 |
| `updated_at` | `timestamptz not null` | 修改时间 |

索引：

```text
unique(key_hash)
(created_at desc, id desc)
```

该方案只接受网关生成的至少 256 bit CSPRNG Key，因此快速摘要不会把低熵口令变成可离线猜测的凭据；认证比较使用完整摘要且不记录原 Key。若未来允许用户自定义低熵 token，必须改用带独立 pepper 的 keyed hash 或 password KDF，不能继续假设普通 SHA-256 足够。

`policy_json` 是当前自托管产品的调用方策略边界，由 client-policy owner 校验，至少允许表达：

```json
{
  "schema_version": 1,
  "allowed_model_ids": [],
  "max_concurrency": 0,
  "requests_per_minute": 0,
  "tokens_per_minute": 0,
  "budget": {
    "period": "month",
    "amount": "0.0000000000",
    "currency": "USD"
  }
}
```

缺省或零值的含义由版本化 validator 明确定义，不能由各 middleware 自行解释。若未来产品真的引入多租户、共享预算或组织权限，需要单独修订本文，而不是现在增加空的 tenant/project 表。

预算 period 默认按 UTC calendar window 计算，边界为 `[window_start, window_end)`；不能由各应用节点本地时区推导。若未来允许调用方时区，时区 ID 本身必须进入冻结的 policy/window 事实并处理规则变更，不能只保存一个模糊的 “month”。

### `runtime_settings`

只保留部署级设置：

| 字段 | 类型与约束 | 语义 |
| --- | --- | --- |
| `id` | `bigint primary key check (id = 1)` | 单例 |
| `config_revision` | `bigint not null default 1 check (> 0)` | 已发布 RuntimeSnapshot 的单调版本 |
| `admin_api_key_hash` | `text` | 管理 API Key 摘要 |
| `usage_retention_days` | `bigint not null check (> 0)` | request/attempt 原始事实保留期 |
| `ops_event_retention_days` | `bigint not null check (> 0)` | 运维事件保留期 |
| `audit_retention_days` | `bigint not null check (> 0)` | 管理审计保留期 |
| `bucket_retention_days` | `bigint not null check (> 0)` | 聚合桶保留期 |
| `updated_at` | `timestamptz not null` | 修改时间 |

Client policy、Provider instance、credential 结构配置、model catalog、route 或价格发生有效变化时，控制面必须在同一事务中使用 `config_revision = config_revision + 1 returning config_revision` 原子推进版本，并写入对应 audit event。数据面使用 repeatable-read 只读事务读取一次 revision 和全部配置，以该一致性快照构建相同 revision 的 RuntimeSnapshot；事务结束后再用一次独立读取与当前 revision 比较，若已经推进则立即重载。Redis 只广播版本；数据面发现版本跳变或丢失通知时同样从 PostgreSQL 重载。

Redis Pub/Sub 不是可靠消息队列。每个数据面实例还必须周期性轮询 `config_revision`，并轻量比较 `(upstream_credential_id, credential_revision, state_revision)` 向量；通知只用于缩短收敛时间。即使提交后进程在发布通知前崩溃，其他实例也必须在有界时间内发现变化。轮询发现版本倒退、缺口或无法解析的新 JSON schema 时保留最后一个有效 Snapshot、停止接收依赖新配置的请求并记录运维事件，不能部分套用配置。

所有管理写请求携带读页面时看到的 `expected_config_revision`。更新事务只在当前 revision 相等时提交，否则返回 conflict 并要求重新读取，避免两个管理员或后台同步任务发生静默 last-write-wins。后台 catalog 同步只能修改自己拥有的 `available/last_seen_at` 等字段，不能覆盖管理员拥有的 `enabled` 和手工 metadata。

Provider kind 或 JSON schema 的发布与滚动升级协调：先部署能理解新 schema/adapter 的二进制并确认旧实例已 drain，再允许控制面发布该配置；不能依赖“旧实例碰到未知 JSON 后大概会重载”来完成兼容。这里不为部署清单新增数据库表，兼容矩阵由发布系统和启动前 Registry 校验管理。

OAuth access token 刷新和 credential 运行状态变化不递增全局 `config_revision`，分别使用 `credential_revision` 和 `state_revision` 通知，避免高频刷新触发全量 Snapshot 重建。

`config_revision` 是一致性 token，不是假装存在的历史配置外键：配置表仍可原位更新，revision 本身不能还原一份已经过期的完整 Snapshot。终态只承诺通过 request/attempt 的 route、target、provider、model、price 和安全 decision metadata 复盘实际执行；若未来出现监管级“完整配置时点回放”要求，必须新增不可变配置归档及独立 retention，不能仅凭一个 revision 数字宣称可重现。

`schema_migrations` 继续沿用当前严格递增版本、名称和 SQL checksum 设计，不并入业务设置表。

以下当前字段不再属于全局设置：

- `model_aliases_json` 迁入 `model_routes` 和 `model_route_targets`。
- `rotation_strategy` 迁入 `model_routes.routing_strategy`。
- `max_concurrent_per_account`、`request_interval_ms` 迁入 credential 或 Provider instance 配置。
- `refresh_margin_seconds`、`refresh_concurrency` 迁入 Codex Provider 配置。

## Provider、凭据与模型

### `provider_instances`

一行表示一个具体的上游部署、区域或 endpoint：

| 字段 | 类型与约束 | 语义 |
| --- | --- | --- |
| `id` | `text primary key` | Instance ID |
| `provider_kind` | `text not null` | 已注册 adapter 名称 |
| `name` | `text not null` | 管理名称 |
| `base_url` | `text` | 可选 endpoint；Bedrock 等平台可为空 |
| `enabled` | `boolean not null default true` | 是否进入 RuntimeSnapshot |
| `max_concurrency` | `integer check (> 0)` | 可选 instance 总并发上限 |
| `request_interval_ms` | `bigint not null default 0 check (>= 0)` | Instance 请求间隔 |
| `config_json` | `jsonb not null` | 带 schema version 的 region、API version、非秘密 header 等配置 |
| `created_at` | `timestamptz not null` | 创建时间 |
| `updated_at` | `timestamptz not null` | 修改时间 |

约束与索引：

```text
unique(provider_kind, name)
(provider_kind, enabled)
```

`provider_kind` 不设置数据库枚举或固定 check。控制面必须确认名称存在于 Provider Registry，并由对应 adapter 校验 `base_url` 和 `config_json`。HTTP endpoint 必须 canonicalize，只允许明确支持的 scheme，拒绝 URL userinfo，并阻断云 metadata 地址；私网与 link-local endpoint 只有在本地模型等场景显式启用 trusted-network policy 后才允许。Transport 在实际连接和每次 redirect 时重新校验解析 IP，默认禁用跨 origin redirect，防止 DNS rebinding 绕过控制面校验。`config_json` 禁止保存任何秘密。

Instance 限制约束整个 endpoint/pool，credential 限制约束单个资源；两者同时存在时必须同时取得 permit 并遵守更晚的请求间隔。无 credential Provider 仍能使用 instance 限制。

### `upstream_credentials`

一行表示 Provider instance 可选择的一项认证资源：

| 字段 | 类型与约束 | 语义 |
| --- | --- | --- |
| `id` | `text primary key` | Credential ID |
| `provider_instance_id` | `text not null references provider_instances on delete cascade` | 所属 instance |
| `name` | `text not null` | 管理名称 |
| `credential_kind` | `text not null` | `api_key`、`oauth`、`aws`、`service_account` 等 |
| `secret_envelope` | `bytea` | AEAD 加密的 Provider 专属 secret JSON |
| `secret_key_id` | `text` | 加密密钥版本 |
| `secret_locator` | `text` | Vault、KMS 或环境变量中的外部 secret 定位符 |
| `secret_version` | `text` | 外部 secret manager 的不可逆版本/etag；envelope 模式为空 |
| `credential_fingerprint` | `text` | Adapter 从稳定 credential 身份生成的 HMAC，用于去重 |
| `resource_ref` | `text not null unique` | 遥测使用的稳定匿名资源 ID；不是外键，也不是 credential fingerprint |
| `credential_revision` | `bigint not null default 1 check (> 0)` | Secret 或 credential 配置版本 |
| `enabled` | `boolean not null default true` | 管理员开关 |
| `max_concurrency` | `integer check (> 0)` | 可选凭据级并发上限 |
| `request_interval_ms` | `bigint not null default 0 check (>= 0)` | 凭据请求间隔 |
| `metadata_json` | `jsonb not null default '{}'` | 可展示、不可含秘密的 Provider metadata |
| `created_at` | `timestamptz not null` | 创建时间 |
| `updated_at` | `timestamptz not null` | 修改时间 |

Secret 存储必须满足二选一：

```text
exactly one of secret_envelope and secret_locator is not null
secret_envelope is not null <-> secret_key_id is not null
secret_envelope is not null -> secret_version is null
secret_locator is not null <-> secret_version is not null
secret_locator is not null -> secret_key_id is null
```

没有认证要求的 instance 不创建 credential 行。一个 instance 就是一个 credential pool；如果同一 endpoint 需要互相隔离的资源池，创建两个 instance，不新增 credential group 抽象。

外部 `secret_locator` 不能指向一个会在数据库不知情时任意变更的“最新值”：secret manager integration 必须把可比较的版本/etag 写入 `secret_version`，发现变化后以 CAS 推进 `credential_revision` 并通知数据面；否则 in-memory lease、attempt snapshot 和审计无法知道哪一代凭据真正被使用。

`credential_fingerprint` 不能直接 HMAC 整个 secret envelope：OAuth access token 和 refresh token 可能轮换。Adapter 必须选择稳定身份，例如 API Key 本身或已经验证的 Provider subject；无法获得稳定身份时保持为空，并使用 Provider 专属唯一约束。

`resource_ref` 在 credential 创建时由 identity pseudonym key 和经过 adapter 规范化的稳定身份生成；没有稳定身份时以随机值生成并永久复制到历史事实。它不能等于 `credential_fingerprint`，也不能从邮箱、Key prefix 等可识别字段直接拼接。Secret 轮换不改变同一 credential 的 `resource_ref`；删除后重新导入是否继承历史身份只能由 adapter 在能够安全证明同一主体时决定。

稳定身份的 HMAC 输入必须包含 Provider/issuer namespace，避免两个无关 OpenAI-compatible endpoint 上碰巧相同的 Key 被视为同一资源。全局 `unique(resource_ref)` 同时阻止把同一上游资源复制到多个 instance 来绕过 quota/concurrency；若以后确有一个资源合法挂载多个 endpoint 的需求，应显式引入 resource 与 instance attachment 模型并按 resource 共用 lease，不能复制 credential 行。

索引：

```text
(provider_instance_id, enabled)
unique(provider_instance_id, name)
unique(provider_instance_id, credential_fingerprint)
  where credential_fingerprint is not null
unique(id, provider_instance_id)
(updated_at, id)
```

Secret 刷新、任何 Provider 专属认证材料（例如 Cookie）变更或 credential 配置更新都使用 `credential_revision` 做 compare-and-swap，并发布 credential 级变更通知。它不修改全局 `config_revision`，除非 `enabled`、并发限制或其他会改变 Route Plan 的结构配置同时变化。

### `upstream_credential_states`

Credential 配置和高频运行状态具有不同写入频率，必须分表：

| 字段 | 类型与约束 | 语义 |
| --- | --- | --- |
| `upstream_credential_id` | `text primary key references upstream_credentials on delete cascade` | Credential |
| `availability` | `text not null default 'unknown'` | `unknown/ready/cooldown/exhausted/invalid` |
| `availability_reason` | `text` | 已脱敏原因 |
| `cooldown_until` | `timestamptz` | 跨重启仍需遵守的冷却截止时间 |
| `state_json` | `jsonb not null` | 带 schema version 的 quota、reset window 等 Provider 状态 |
| `state_revision` | `bigint not null default 1 check (> 0)` | 状态 CAS 版本 |
| `observed_at` | `timestamptz not null` | 产生当前状态事实的时间 |
| `updated_at` | `timestamptz not null` | 持久化时间 |

约束和索引：

```text
availability in ('unknown', 'ready', 'cooldown', 'exhausted', 'invalid')
(availability = 'cooldown') = (cooldown_until is not null)
(availability, cooldown_until)
(updated_at, upstream_credential_id)
```

状态更新必须使用 `state_revision` CAS，并由 Provider owner 合并并发事实；旧 `observed_at` 的结果不能覆盖更新状态。只有 availability/cooldown/quota 等有效状态发生变化或专门的状态刷新任务取得新观察时才持久化，普通成功 attempt 不触碰本行。进程内 semaphore、在途 lease 和连接状态仍不写 PostgreSQL，Redis 只缓存这张表可恢复的热状态。

最后使用/成功/失败时间从 `request_attempts` 的 credential 索引或 metric bucket 查询，不回写本行；否则热门 credential 每个请求都更新同一行，会制造行锁热点、WAL 和 autovacuum 压力，也形成第二套可漂移事实。Client API key 的最后使用时间同理从 `gateway_requests` 查询。

创建 `upstream_credentials` 时必须在同一事务中创建唯一 state 行，不能用“缺少 state 行代表 unknown”形成第二套隐式状态。`credential_revision` 与 `state_revision` 各自单调递增，互不覆盖。

### `codex_accounts`

Codex 是当前唯一需要可查询专属字段的 Provider，一行与一个 OAuth credential 一对一：

| 字段 | 类型与约束 | 语义 |
| --- | --- | --- |
| `upstream_credential_id` | `text primary key references upstream_credentials on delete cascade` | 对应 credential |
| `email` | `text` | ChatGPT 邮箱 |
| `chatgpt_account_id` | `text` | ChatGPT account ID |
| `chatgpt_user_id` | `text` | ChatGPT user ID |
| `plan_type` | `text` | 订阅计划 |
| `access_token_expires_at` | `timestamptz` | access token 到期时间 |
| `next_refresh_at` | `timestamptz` | 下次计划刷新时间 |

唯一索引沿用当前身份约束：

```text
unique(chatgpt_account_id, coalesce(chatgpt_user_id, ''))
  where chatgpt_account_id is not null
```

Access token 和 refresh token 位于 `upstream_credentials.secret_envelope`，本表不得保存明文。Quota、Cloudflare cooldown 和 quota verification 等运行状态归入 `upstream_credential_states.state_json`，避免同一事实存在两个 writer。

Codex owner 在创建 account/cookie 的同一事务中验证 credential 所属 instance 的 `provider_kind = 'codex'` 且 credential kind 与认证流程匹配；普通 FK 只能证明 credential 存在，不能证明它属于正确 adapter。该跨表 invariant 必须进入数据库契约测试。

### `codex_account_cookies`

| 字段 | 类型与约束 | 语义 |
| --- | --- | --- |
| `id` | `text primary key` | Cookie ID |
| `upstream_credential_id` | `text not null references upstream_credentials on delete cascade` | Codex credential |
| `domain` | `text not null` | Domain |
| `name` | `text not null` | Cookie name |
| `value_envelope` | `bytea not null` | 加密 Cookie value |
| `secret_key_id` | `text not null` | 加密密钥版本 |
| `path` | `text not null default '/'` | Cookie path |
| `host_only` | `boolean not null` | 无 Domain attribute 时为 true，禁止错误发送给子域 |
| `secure` | `boolean not null` | 只允许通过安全 transport 发送 |
| `expires_at` | `timestamptz` | 过期时间 |
| `updated_at` | `timestamptz not null` | 修改时间 |

索引：

```text
unique(upstream_credential_id, domain, name, path)
(expires_at) where expires_at is not null
```

Cookie owner 只持久化 Provider allowlist 中确有业务需要的名称。捕获 `Set-Cookie` 时，Domain 必须 domain-match 实际响应 origin 且不能是 public suffix；无 Domain attribute 时保存 `host_only = true`。重放时同时检查 domain、host-only、path、secure 和 expiry，不能因为数据库里已有一行就向另一个 Provider endpoint 发送。

Cookie upsert 与父 credential 的 `credential_revision` 递增必须在同一事务内完成；否则 attempt 无法准确冻结它使用的是哪一代认证材料，旧 Cookie 的失败也可能污染新会话状态。

其他 Provider 只有在出现真实、需要查询的专属状态时才增加自己的扩展表。普通 OpenAI、Anthropic、Google 或 xAI API Key 不创建空扩展表。

### `provider_models`

一行表示一个 instance 上的真实模型：

| 字段 | 类型与约束 | 语义 |
| --- | --- | --- |
| `provider_instance_id` | `text not null references provider_instances on delete cascade` | 所属 instance |
| `upstream_model_id` | `text not null` | 上游真实模型 ID |
| `display_name` | `text` | 展示名称 |
| `catalog_source` | `text not null` | `discovered` 或 `manual` |
| `enabled` | `boolean not null default true` | 管理员开关 |
| `available` | `boolean not null default true` | 最近 catalog 是否仍存在 |
| `capabilities_json` | `jsonb not null` | 带 schema version 的 operation 和 feature 能力 |
| `limits_json` | `jsonb not null` | 带 schema version 的 context、输出等限制 |
| `metadata_json` | `jsonb not null default '{}'` | Provider 原始目录的安全子集 |
| `last_seen_at` | `timestamptz` | 最近同步出现时间 |
| `created_at` | `timestamptz not null` | 创建时间 |
| `updated_at` | `timestamptz not null` | 修改时间 |

主键：

```text
primary key(provider_instance_id, upstream_model_id)
catalog_source in ('discovered', 'manual')
```

Catalog 同步发现模型消失时设置 `available = false`，不能删除仍被 model route target 或历史价格引用的模型。只有 `enabled`、`available`、capability 或 limits 的有效值发生变化时才递增全局 `config_revision`；单纯更新 `last_seen_at` 不触发 Snapshot 重建。

`capabilities_json` 不是开放式能力布尔值集合，而是带来源的声明：对每项能力区分 `native/emulated/unsupported/unknown`，并记录由 adapter 代码、manual override 或 probe 得到。Router 对 unknown 默认不路由；probe 失败不会把未知能力当成支持，协议 adapter 的 emulation 也必须显式进入 Route Plan。

Catalog 拉取必须先得到一个完整、成功的远端 snapshot，再在单个数据库事务中 upsert 本轮模型并只对 `catalog_source = 'discovered'` 的缺失行设置 unavailable。分页中断、鉴权失败或部分响应不得把未见到的模型批量下线；手工模型也不受 discovery 缺失处理影响。

Router 从编译后的 capability snapshot 过滤 instance 级目标，不在请求热路径查询 JSON。某个 credential 因套餐、授权或区域无法使用模型时，由 Provider credential selector 根据专属 metadata/state 再过滤；不为此创建一张所有 Provider 都必须填充的空映射表。

### `model_routes`

一行表示一个对客户端公开的模型：

| 字段 | 类型与约束 | 语义 |
| --- | --- | --- |
| `id` | `text primary key` | Route ID |
| `public_model_id` | `text not null unique` | 客户端模型 ID |
| `description` | `text` | 管理说明 |
| `enabled` | `boolean not null default true` | 是否公开 |
| `routing_strategy` | `text not null` | `priority/weighted/latency/cost/balanced/sticky` |
| `policy_json` | `jsonb not null` | 带 schema version 的 fallback、最大 attempts、预算等策略 |
| `created_at` | `timestamptz not null` | 创建时间 |
| `updated_at` | `timestamptz not null` | 修改时间 |

约束：

```text
unique(id, public_model_id)
```

`routing_strategy` 由应用注册表校验，不设置数据库 check，以便增加组合策略时不修改数据库类型。控制面只有在 route 至少存在一个 enabled target 且所有 target/options 都通过校验后，才能发布新的 `config_revision`。

### `model_route_targets`

一行表示 route 的一个候选真实模型：

| 字段 | 类型与约束 | 语义 |
| --- | --- | --- |
| `id` | `text primary key` | Target ID |
| `model_route_id` | `text not null references model_routes on delete cascade` | 所属 route |
| `provider_instance_id` | `text not null` | 目标 instance |
| `upstream_model_id` | `text not null` | 真实模型 ID |
| `priority` | `integer not null default 100 check (>= 0)` | 数字越小优先级越高 |
| `weight` | `integer not null default 1 check (> 0)` | 加权策略权重 |
| `enabled` | `boolean not null default true` | 是否参与候选 |
| `options_json` | `jsonb not null` | 带 schema version 的 service tier 等目标级非秘密选项 |
| `created_at` | `timestamptz not null` | 创建时间 |
| `updated_at` | `timestamptz not null` | 修改时间 |

约束与索引：

```text
foreign key(provider_instance_id, upstream_model_id)
  references provider_models(provider_instance_id, upstream_model_id)
  on delete restrict

unique(model_route_id, provider_instance_id, upstream_model_id)
(model_route_id, enabled, priority, id)
(provider_instance_id, upstream_model_id)
```

Target 不引用 credential。Provider 必须在执行 attempt 时从 instance 的 credential pool 中选择资源。

### `model_price_versions`

一行表示一个模型和 service tier 从某个时间点开始生效的一整套价格。价格版本发布后不可修改：

| 字段 | 类型与约束 | 语义 |
| --- | --- | --- |
| `id` | `text primary key` | Price version ID |
| `provider_instance_id` | `text not null` | Instance |
| `upstream_model_id` | `text not null` | 真实模型 ID |
| `service_tier` | `text not null default '__default__'` | 规范化 tier；sentinel 表示 Provider 默认档位 |
| `currency` | `text not null` | 大写货币代码 |
| `effective_from` | `timestamptz not null` | 生效时间 |
| `metadata_json` | `jsonb not null default '{}'` | 价格来源和说明 |
| `created_at` | `timestamptz not null` | 创建时间 |

约束与索引：

```text
foreign key(provider_instance_id, upstream_model_id)
  references provider_models(provider_instance_id, upstream_model_id)
  on delete restrict

unique(
  provider_instance_id,
  upstream_model_id,
  service_tier,
  effective_from
)

unique(id, provider_instance_id, upstream_model_id, service_tier, currency)

currency ~ '^[A-Z]{3}$'

```

一次 attempt 在发送前从当时的 RuntimeSnapshot 选择 `effective_from <= started_at` 的最新版本，并把 version ID 写入 attempt 起始行；在途请求不受后来发布或回溯生效的版本影响。没有 `effective_to`，因此不存在重叠区间；价格修正通过新增版本完成，不能回写已经被 attempt 使用的历史版本。

### `model_price_rates`

价格版本中的每个计费 metric 一行：

| 字段 | 类型与约束 | 语义 |
| --- | --- | --- |
| `model_price_version_id` | `text not null references model_price_versions on delete cascade` | 所属价格版本 |
| `metric` | `text not null` | `input_token`、`image`、`audio_second` 等 |
| `unit_size` | `numeric(20, 6) not null check (> 0)` | 报价单位数量 |
| `amount` | `numeric(20, 10) not null check (>= 0)` | 每单位价格 |

主键：

```text
primary key(model_price_version_id, metric)
```

Version 与至少一条 rate 必须在一个事务中创建并完整验证，再递增 `config_revision` 发布；空版本不能发布。没有匹配版本表示 unknown；版本存在但缺少本次 usage 所需 metric 表示 partial。零价格必须由 `amount = 0` 的明确 rate 表达。不同货币永远分组展示和统计，系统没有汇率事实时禁止相加。

Partial 的 `estimated_cost_amount` 只表示已知 metric 的下界，不能作为完整成本参与 strict budget 释放；unknown 没有金额。只有 known（包括由明确零价 rate 算出的 known zero）可以作为完整估算展示。

`metric` 来自 Gateway 的版本化 normalized-usage registry，并与 request/attempt 的 `usage_metrics_json` 使用同一名称和单位。当前 price rate 只表达 `usage × unit price` 的线性定价；阶梯价、包月抵扣、承诺消费和 Provider invoice adjustment 不允许塞入 `metadata_json` 或临时公式。真正需要这些能力时应单独设计账务边界，在此之前标记 partial/unknown。

Price repository 对已发布版本只提供读取，不提供更新；更正必须新增 version。成本路由遇到 unknown/partial price 时不能把它当作零成本，严格预算只能选择与预算 currency 相同且费率完整的目标，否则必须按 policy 明确拒绝或降级。

计费计算全程使用十进制定点数，按 `usage × amount / unit_size` 计算每个 metric 后求和，只在持久化 `estimated_cost_amount` 时按 10 位小数统一舍入；Rust 业务代码禁止经过 `f32/f64`。

`cost_breakdown_json` 只保存 rate metric、price version、非负十进制字符串数量/金额和舍入说明；它不是 Provider 原始账单响应，也不允许 JSON 浮点。

这里的 `estimated_cost_amount` 是按当时价格目录计算的网关成本估算，不是 Provider 发票、付款或会计结算事实。预算可以基于该保守估算执行，但 UI 和 API 必须标注 estimated；未来若接入账单对账，应新增独立的 invoice/reconciliation 模型，不能覆盖已记录的请求价格快照来伪装成实际账单。

## 请求、Attempt 与用量

### `gateway_requests`

请求被接受后立即插入一行，所有终止路径只 finalize 一次：

| 字段 | 类型与约束 | 语义 |
| --- | --- | --- |
| `id` | `text primary key` | Logical request ID |
| `client_api_key_id` | `text references client_api_keys on delete set null` | 调用方 Key；内部任务可为空 |
| `client_api_key_ref` | `text` | 调用方 Key ID 历史快照；内部任务可为空 |
| `config_revision` | `bigint not null check (> 0)` | 本请求冻结 Route Plan 时的 RuntimeSnapshot 版本 |
| `protocol` | `text not null` | OpenAI Responses、Anthropic Messages 等 |
| `operation` | `text not null` | `generate/embed/rerank/...` |
| `client_transport` | `text not null` | `http_json/http_sse/websocket` 等入站 transport |
| `model_route_id` | `text` | 冻结时选中的 route；与 public model 组成复合 FK |
| `model_route_ref` | `text` | Route ID 历史快照 |
| `requested_model_id` | `text not null` | 客户端原始模型 ID 快照 |
| `public_model_id` | `text` | 成功解析后的对外模型 ID 快照；未解析时为空 |
| `continuation_binding_id` | `text` | 本请求使用的 source binding；与 client key 组成复合 FK |
| `continuation_binding_ref` | `text` | Source binding ID 历史快照 |
| `stream` | `boolean not null` | 客户端是否请求流式返回 |
| `outcome` | `text not null default 'running'` | `running/succeeded/failed/cancelled/incomplete` |
| `client_status_code` | `integer` | 最终客户端 HTTP/协议状态 |
| `client_response_id` | `text` | 网关暴露的 response ID |
| `error_kind` | `text` | 稳定 Gateway error taxonomy |
| `error_message` | `text` | 已脱敏的客户端安全消息 |
| `budget_reserved_amount` | `numeric(20, 10)` | 接入时持久化的保守预算预留 |
| `budget_limit_amount` | `numeric(20, 10)` | 本请求冻结的预算上限 |
| `budget_currency` | `text` | 预留货币 |
| `budget_window_start` | `timestamptz` | 本请求所属预算窗口起点 |
| `budget_window_end` | `timestamptz` | 本请求所属预算窗口终点（exclusive） |
| `input_tokens` | `bigint` | 客户端逻辑响应 usage |
| `output_tokens` | `bigint` | 客户端逻辑响应 usage |
| `cached_tokens` | `bigint` | 客户端逻辑响应 usage |
| `cache_write_tokens` | `bigint` | 客户端逻辑响应 usage |
| `reasoning_tokens` | `bigint` | 客户端逻辑响应 usage |
| `total_tokens` | `bigint` | Provider 报告或规范化总量 |
| `usage_metrics_json` | `jsonb not null default '{}'` | 非 token 的注册 normalized usage metric |
| `first_token_ms` | `bigint` | 从请求开始到首个客户端可见 token |
| `latency_ms` | `bigint` | 请求总耗时 |
| `metadata_json` | `jsonb not null default '{}'` | 不含正文和秘密的请求事实 |
| `started_at` | `timestamptz not null` | 开始时间 |
| `deadline_at` | `timestamptz not null` | 接入时冻结的绝对执行截止时间 |
| `completed_at` | `timestamptz` | 终止时间 |

约束：

```text
outcome in ('running', 'succeeded', 'failed', 'cancelled', 'incomplete')
client_status_code is null or client_status_code between 100 and 599
foreign key(model_route_id, public_model_id)
  references model_routes(id, public_model_id)
  on delete set null (model_route_id)

foreign key(continuation_binding_id, client_api_key_id)
  references continuation_bindings(id, client_api_key_id)
  on delete set null (continuation_binding_id)
所有 token、first_token_ms、latency_ms 为 null 或 >= 0
预算五字段 reserved/limit/currency/window_start/window_end 必须同时为 null 或同时非 null
budget_reserved_amount is null or (budget_reserved_amount >= 0 and budget_limit_amount >= budget_reserved_amount)
budget_currency is null or budget_currency ~ '^[A-Z]{3}$'
budget_window_start is null or (client_api_key_ref is not null and budget_window_start < budget_window_end)
client_api_key_id is null or (client_api_key_ref is not null and client_api_key_ref = client_api_key_id)
model_route_id is null or (model_route_ref is not null and model_route_ref = model_route_id)
model_route_id is null or public_model_id is not null
continuation_binding_id is null or (continuation_binding_ref is not null and continuation_binding_ref = continuation_binding_id)
continuation_binding_id is null or client_api_key_id is not null
started_at <= deadline_at
running -> completed_at is null
非 running -> completed_at is not null and completed_at >= started_at
```

索引：

```text
(started_at desc, id desc)
(client_api_key_id, started_at desc, id desc)
(client_api_key_ref, started_at desc, id desc) where client_api_key_ref is not null
(model_route_id, started_at desc, id desc) where model_route_id is not null
(public_model_id, started_at desc, id desc) where public_model_id is not null
(continuation_binding_id, client_api_key_id, started_at desc, id desc)
  where continuation_binding_id is not null
(continuation_binding_ref, started_at desc, id desc) where continuation_binding_ref is not null
(outcome, started_at desc, id desc)
(deadline_at, id) where outcome = 'running'
(client_api_key_ref, budget_window_start, budget_window_end, budget_currency)
  where budget_reserved_amount is not null
unique(client_response_id) where client_response_id is not null
unique(id, client_response_id, client_api_key_id)
```

`client_api_key_ref`、`model_route_ref` 和 `continuation_binding_ref` 在创建请求时与对应 live FK 使用同一个 ID；只要 live FK 非空，数据库 check 就要求对应 `_ref` 相等。配置删除后 FK 可以置空，历史维度仍然可查询。Continuation owner 只能通过当前 client key 解析 response ID 得到 `continuation_binding_id`，API handler 不能自行填入其他调用方的 binding。`requested_model_id` 是有严格长度上限、默认不建索引的诊断原值，`public_model_id` 是 Router 成功解析后的稳定业务维度，不能互相冒充。严格预算请求必须冻结完整预算窗口和上限，使 policy 在请求执行中途被修改、Key 被禁用或 Redis 丢失时，reservation 仍可从事实表确定性重建。Policy 更新以新窗口生效；若管理员要求当前窗口立即生效，控制面必须先在 PostgreSQL 中按新规则重算并成功重建 Redis，再发布新 revision。

`incomplete` 只表示 Gateway 无法确认完整终态（例如进程终止或持久化失败后的 recovery），不是 OpenAI `response.incomplete`、max-token finish reason 等合法 Provider 结果；后者若已按协议完整交付仍记为 succeeded，并在规范化 finish metadata 中表达。

Token 列以 null 表示 Provider/协议没有给出可信值，以 0 表示明确观测到零，不能互换。`usage_metrics_json` 只保存 registry 中带固定单位的 metric 到非负十进制字符串的映射，例如 `{"image.output.count":"2","audio.input.second":"12.345"}`，禁止 JSON 浮点和 Provider 原始 usage body；它与固定 token 列一起构成价格计算输入，并继续汇总到长期 bucket。

本表不保存 Provider、credential 或 upstream model，它们属于 attempt。本表 usage 只表示最终交付给调用方的逻辑结果，不能用于计算重试产生的上游目录价格估算。

成功 request 必须存在且只存在一个 downstream-committed attempt；在鉴权后但调用上游前失败的 request 可以没有 attempt。Request usage 来自客户端实际收到的 canonical 结果，不能简单复制最后 attempt 或把多次 attempt 相加，尤其是在协议转换、截断和部分流式输出场景。

### `request_attempts`

Provider 选定执行资源、准备发送上游请求时插入一行；未越过 commit 也必须记录：

| 字段 | 类型与约束 | 语义 |
| --- | --- | --- |
| `id` | `text primary key` | Attempt ID |
| `gateway_request_id` | `text not null references gateway_requests on delete cascade` | Logical request |
| `attempt_index` | `integer not null check (> 0)` | 请求内从 1 开始的序号 |
| `trigger` | `text not null` | `initial/credential_retry/target_retry/route_fallback` |
| `model_route_target_id` | `text references model_route_targets on delete set null` | 当前可关联 route target |
| `model_route_target_ref` | `text` | Target ID 历史快照 |
| `provider_instance_id` | `text references provider_instances on delete set null` | 当前可关联 instance |
| `provider_instance_ref` | `text not null` | Instance ID 历史快照 |
| `provider_kind` | `text not null` | Provider 名称快照 |
| `upstream_credential_id` | `text` | 当前可关联 credential；与 instance 组成复合 FK |
| `upstream_credential_revision` | `bigint` | 本 attempt 实际使用的 credential secret/config revision |
| `resource_ref` | `text not null` | 匿名资源快照；无 credential 时为 `__none__` |
| `upstream_model_id` | `text not null` | 实际上游模型 ID |
| `service_tier` | `text not null default '__default__'` | 规范化后的实际 tier |
| `upstream_transport` | `text not null` | HTTP/SSE/WebSocket 等上游 transport |
| `upstream_send_state` | `text not null default 'not_sent'` | `not_sent/sent/ambiguous` |
| `outcome` | `text not null default 'running'` | `running/succeeded/failed/cancelled/incomplete` |
| `downstream_committed` | `boolean not null default false` | 是否向客户端越过 commit 边界 |
| `upstream_status_code` | `integer` | 上游 HTTP/协议状态 |
| `provider_error_code` | `text` | Provider 原始稳定 code |
| `failure_kind` | `text` | 稳定 Provider failure taxonomy |
| `retry_after_ms` | `bigint` | 上游建议等待时间 |
| `upstream_request_id` | `text` | 上游 request ID |
| `upstream_response_id` | `text` | adapter 分类为非 bearer 的上游诊断 response ID |
| `input_tokens` | `bigint` | 本 attempt 上游 usage |
| `output_tokens` | `bigint` | 本 attempt 上游 usage |
| `cached_tokens` | `bigint` | 本 attempt 上游 usage |
| `cache_write_tokens` | `bigint` | 本 attempt 上游 usage |
| `reasoning_tokens` | `bigint` | 本 attempt 上游 usage |
| `total_tokens` | `bigint` | 本 attempt 上游 usage |
| `usage_metrics_json` | `jsonb not null default '{}'` | 非 token 的注册 normalized usage metric |
| `model_price_version_id` | `text references model_price_versions on delete restrict` | 使用的价格版本；与模型/tier/currency 另组成复合 FK |
| `cost_estimate_status` | `text not null default 'not_applicable'` | `not_applicable/known/partial/unknown` |
| `estimated_cost_amount` | `numeric(20, 10)` | 本 attempt 已知或部分已知的目录价格估算 |
| `estimated_cost_currency` | `text` | 估算货币 |
| `cost_breakdown_json` | `jsonb not null default '{}'` | 各 metric 的估算明细和舍入依据 |
| `headers_ms` | `bigint` | 上游 headers 延迟 |
| `first_event_ms` | `bigint` | 上游首事件延迟 |
| `first_token_ms` | `bigint` | 上游首 token 延迟 |
| `latency_ms` | `bigint` | Attempt 总耗时 |
| `metadata_json` | `jsonb not null default '{}'` | 已脱敏 transport 和诊断事实 |
| `started_at` | `timestamptz not null` | 开始时间 |
| `deadline_at` | `timestamptz not null` | 本 attempt 冻结的绝对截止时间 |
| `completed_at` | `timestamptz` | 终止时间 |

约束：

```text
unique(gateway_request_id, attempt_index)
unique(gateway_request_id) where downstream_committed

foreign key(upstream_credential_id, provider_instance_id)
  references upstream_credentials(id, provider_instance_id)
  on delete set null (upstream_credential_id)

foreign key(model_price_version_id, provider_instance_id, upstream_model_id, service_tier, estimated_cost_currency)
  references model_price_versions(id, provider_instance_id, upstream_model_id, service_tier, currency)
  on delete restrict

trigger in ('initial', 'credential_retry', 'target_retry', 'route_fallback')
upstream_send_state in ('not_sent', 'sent', 'ambiguous')
outcome in ('running', 'succeeded', 'failed', 'cancelled', 'incomplete')
upstream_status_code is null or upstream_status_code between 100 and 599
retry_after_ms、token 和延迟字段为 null 或 >= 0
cost_estimate_status in ('not_applicable', 'known', 'partial', 'unknown')
cost_estimate_status == 'not_applicable' -> upstream_send_state == 'not_sent' 且 estimated_cost_amount、estimated_cost_currency、model_price_version_id 均为 null
upstream_send_state == 'not_sent' -> cost_estimate_status == 'not_applicable'
upstream_send_state in ('sent', 'ambiguous') -> cost_estimate_status != 'not_applicable'
cost_estimate_status == 'unknown' -> estimated_cost_amount、estimated_cost_currency 均为 null；model_price_version_id 可为空或记录已经找到但 usage 不足以计算的版本
cost_estimate_status in ('known', 'partial') -> estimated_cost_amount、estimated_cost_currency、model_price_version_id 均非 null
estimated_cost_amount is null or estimated_cost_amount >= 0
estimated_cost_currency is null or estimated_cost_currency ~ '^[A-Z]{3}$'
model_route_target_id is null or (model_route_target_ref is not null and model_route_target_ref = model_route_target_id)
provider_instance_id is null or provider_instance_ref = provider_instance_id
upstream_credential_id is not null -> provider_instance_id is not null and resource_ref != '__none__'
upstream_credential_id is not null -> upstream_credential_revision is not null
upstream_credential_revision is null or upstream_credential_revision > 0
started_at <= deadline_at
running -> completed_at is null
非 running -> completed_at is not null and completed_at >= started_at
```

索引：

```text
(model_route_target_id, started_at desc, id desc) where model_route_target_id is not null
(model_route_target_ref, started_at desc, id desc) where model_route_target_ref is not null
(provider_instance_id, started_at desc, id desc) where provider_instance_id is not null
(provider_instance_ref, started_at desc, id desc)
(upstream_credential_id, started_at desc, id desc) where upstream_credential_id is not null
(resource_ref, started_at desc, id desc) where resource_ref is not null
(upstream_model_id, started_at desc, id desc)
(upstream_transport, started_at desc, id desc)
(model_price_version_id) where model_price_version_id is not null
(failure_kind, started_at desc, id desc) where failure_kind is not null
(provider_instance_ref, upstream_request_id) where upstream_request_id is not null
(provider_instance_ref, upstream_response_id) where upstream_response_id is not null
(deadline_at, id) where outcome = 'running'
```

`model_route_target_ref`、`provider_instance_ref`、`provider_kind`、`resource_ref` 和 `upstream_model_id` 是刻意保存的事实快照。配置实体被删除后，历史 attempt 仍可正确统计。Attempt 从 credential 行复制 `resource_ref`；它不能包含邮箱、API Key prefix 或其他身份信息。

`provider_kind`、instance ref、target ref 和实际 model 一律由 Attempt Coordinator 从已验证的 Route Plan/Provider result 派生，不能信任 API handler 或 Provider 错误对象中的任意字符串。插入时若 live instance/target 仍存在，owner 必须验证它们的 Provider kind 与目标模型一致；删除后的历史只依赖快照。

`deadline_at` 由 parent request 的冻结 deadline 与 target/Provider 更短 timeout 取最小值，不能晚于 parent request；这个跨表约束由 Attempt Coordinator 的同一行锁事务验证，防止一个 retry 悄悄越过客户端已经放弃的执行时间。

Attempt 还冻结所用的 `upstream_credential_revision`。Provider state owner 收到认证/配额结果时必须把该 revision 一并比较：旧 revision 的失败可以记录诊断，但不能把已经轮换的新 secret 标成 invalid 或覆盖其 cooldown/quota 状态。

`upstream_send_state` 与 `downstream_committed` 是两个独立边界：前者决定上游是否可能处理或计费，后者决定网关是否还能向客户端切换 attempt。一个 downstream-committed attempt 随后仍可 `failed` 或 `incomplete`；downstream commit 不等于成功。`ambiguous` 表示 payload 可能已送达但结果未知，默认禁止自动重放。

发送状态采用保守转换：attempt 初始为 `not_sent/not_applicable`；紧邻实际网络 send 之前，先 CAS 持久化为 `ambiguous/unknown`；transport 明确确认发送后改为 `sent`，只有能够证明业务 payload 零字节发送时才允许回到 `not_sent/not_applicable`。因此进程在 send 边界崩溃只会留下 ambiguous，不会把可能已处理的请求误判为可安全重放。

`failure_kind` 使用跨 Provider 稳定分类：`invalid_request`、`unsupported`、`unauthorized`、`permission_denied`、`rate_limited`、`quota_exhausted`、`timeout`、`transport`、`protocol`、`unavailable`、`cancelled` 和 `process_terminated`。Provider 原始错误 code 只用于诊断，不能代替该分类。

Attempt insert 是请求级上游 exchange 的持久化屏障：只有 `gateway_requests` 和本次 `request_attempts` 行都已提交，Provider 才能发起携带本次 credential 的请求级 handshake 或发送可能产生费用、副作用的 payload。Provider 内部不得隐藏 credential retry；每一次可能到达上游的业务调用都必须对应独立 attempt。与业务请求无关、尚未发送 payload 的后台 preconnect 只记录 transport/ops telemetry，不伪造 attempt。

HTTP/SDK/WebSocket transport 的自动 retry 和携带 payload 的自动 redirect 默认关闭；否则底层可能产生数据库看不见的第二次上游调用。确需 redirect 时由 Provider 校验目标并交回 Attempt Coordinator，以独立 attempt 记录；仅连接建立阶段且能够证明 payload 未发送的内部重试可以留在同一 attempt。

创建 attempt 的事务和把 attempt 改为 `downstream_committed = true` 的事务都必须锁定对应 `gateway_requests` 行。前者确认 request 仍为 running、尚无 committed attempt，并在锁内分配下一个 `attempt_index`；后者在同一锁内再次确认没有竞争者。这样不是只依赖 partial unique index“事后发现冲突”，也不会由两个并发 retry 从 `max(attempt_index) + 1` 得到相同序号，commit 后的新 attempt 也被串行拒绝。锁只覆盖同一个 logical request，不是全局热路径锁。

Request 和 attempt finalize 都使用 compare-and-swap：只允许把 `running` 更新为一个 terminal outcome，重复 finalizer 不得再次写 usage 或 bucket。Downstream-committed partial unique index 只保证最多一个已提交客户端的 attempt；“commit 后不能创建新 attempt”仍由 Attempt Coordinator 的状态机和契约测试保证，不使用数据库 trigger。

最终 attempt、logical request、continuation binding 与 portable transcript metadata 的终态写入使用同一 PostgreSQL 事务。只有 terminal CAS 实际更新一行时，事务内才调用唯一 bucket aggregator 应用对应 delta；CAS 未命中时不能再次累加。需要 continuation 的响应在该事务成功前不能发送最后 terminal event。

进程异常可能留下 `running` 行。启动和 recovery worker 必须依据持久化 `deadline_at`，把已经超时且不存在活动 lease 的 request/attempt 收敛为 `incomplete`，并记录稳定的 process-termination failure；不能依赖已经变化的 timeout 配置，也不能永久把它们算作在途请求。Retention worker 只在 recovery 完成后删除终态事实，不兼任状态推断。

## Continuation 与可移植历史

### `conversation_transcripts`

每一行是一个不可变的 portable continuation canonical transcript snapshot：

| 字段 | 类型与约束 | 语义 |
| --- | --- | --- |
| `id` | `text primary key` | Transcript ID |
| `client_api_key_id` | `text not null references client_api_keys on delete restrict` | 所属调用方 |
| `parent_conversation_transcript_id` | `text` | 上一个 snapshot；同一调用方内的自引用复合 FK |
| `canonical_version` | `integer not null check (> 0)` | Canonical item schema 版本 |
| `metadata_json` | `jsonb not null default '{}'` | 不含对话正文的摘要 |
| `created_at` | `timestamptz not null` | 创建时间 |
| `expires_at` | `timestamptz not null` | 自动清理时间 |

索引：

```text
(client_api_key_id, created_at desc, id desc)
unique(id, client_api_key_id)
(parent_conversation_transcript_id, client_api_key_id)
  where parent_conversation_transcript_id is not null
(expires_at)

foreign key(parent_conversation_transcript_id, client_api_key_id)
  references conversation_transcripts(id, client_api_key_id)
  on delete restrict

parent_conversation_transcript_id is null or parent_conversation_transcript_id != id
```

一个 portable continuation 从前一个 binding 的 snapshot 创建新的子 snapshot，只写入本轮 canonical input/output delta；parent 一经插入不可更新，旧 snapshot 和它的 items 永不 append 或改写。因此多个 response ID 从同一历史分支继续时，互不看到对方后续内容，也不会形成循环链。读取上下文按 parent 链从根到叶拼接，并设置受控最大深度；超过阈值时 history owner 创建新的 materialized root、解密后重新加密必要 items，绝不修改已经绑定给旧 response 的 snapshot。Materialized root 继承链上最早的 `expires_at`，不能借 compaction 延长用户正文保留期。

子 snapshot 的 `expires_at` 不得晚于 parent，且 owner 必须拒绝跨 client API key 建链。这个跨行时间规则与 parent owner FK 一起保证 retention 可以从叶子向根删除，而不会让较新的 continuation 指向已清理历史。

### `conversation_items`

对话内容以 snapshot delta 保存，不以明文 JSON 保存：

| 字段 | 类型与约束 | 语义 |
| --- | --- | --- |
| `conversation_transcript_id` | `text not null references conversation_transcripts on delete cascade` | Transcript |
| `sequence` | `bigint not null check (>= 0)` | 当前 snapshot delta 内的 Canonical 顺序 |
| `item_kind` | `text not null` | `client_input/assistant_output/tool_result/...` |
| `item_envelope` | `bytea not null` | 加密 canonical item |
| `secret_key_id` | `text not null` | 加密密钥版本 |
| `token_estimate` | `bigint check (token_estimate is null or token_estimate >= 0)` | 可选上下文预算估算 |
| `created_at` | `timestamptz not null` | 写入时间 |

主键：

```text
primary key(conversation_transcript_id, sequence)
```

图片、音频和文件只保存受控资源引用，禁止把 base64 大对象写进 `item_envelope`。

### `continuation_bindings`

一行表示客户端 response ID 的后续请求如何恢复：

| 字段 | 类型与约束 | 语义 |
| --- | --- | --- |
| `id` | `text primary key` | Binding ID |
| `client_response_id` | `text not null unique` | 网关暴露的 response ID |
| `gateway_request_id` | `text not null` | 来源 logical request；与 response/key 组成复合 FK |
| `client_api_key_id` | `text not null references client_api_keys on delete restrict` | 所属调用方 |
| `mode` | `text not null` | `native` 或 `portable` |
| `provider_instance_id` | `text references provider_instances on delete restrict` | Native owner |
| `upstream_credential_id` | `text` | 需要固定资源时填写；与 instance 组成复合 FK |
| `native_state_envelope` | `bytea` | 加密的 Provider continuation handle/state |
| `native_state_schema_version` | `integer` | Provider native state schema 版本 |
| `secret_key_id` | `text` | Native envelope 加密密钥版本 |
| `native_reuse_mode` | `text not null default 'not_applicable'` | `reusable/single_use/not_applicable` |
| `native_use_state` | `text not null default 'not_applicable'` | single-use binding 的持久化消费状态 |
| `native_consumer_gateway_request_id` | `text references gateway_requests on delete set null` | single-use binding 当前/最后 consumer request |
| `native_use_revision` | `bigint not null default 1 check (> 0)` | single-use state CAS 版本 |
| `conversation_transcript_id` | `text` | Portable transcript；与 client key 组成复合 FK |
| `metadata_json` | `jsonb not null default '{}'` | connection scope 等非秘密绑定信息 |
| `created_at` | `timestamptz not null` | 创建时间 |
| `updated_at` | `timestamptz not null` | 修改时间 |
| `expires_at` | `timestamptz not null` | 自动清理时间 |

模式约束：

```text
mode in ('native', 'portable')

foreign key(gateway_request_id, client_response_id, client_api_key_id)
  references gateway_requests(id, client_response_id, client_api_key_id)
  on delete restrict

foreign key(conversation_transcript_id, client_api_key_id)
  references conversation_transcripts(id, client_api_key_id)
  on delete restrict

foreign key(upstream_credential_id, provider_instance_id)
  references upstream_credentials(id, provider_instance_id)
  on delete restrict

native:
  provider_instance_id != null
  native_state_envelope、native_state_schema_version、secret_key_id 均非 null
  native_state_schema_version > 0
  conversation_transcript_id == null
  native_reuse_mode in ('reusable', 'single_use')
  reusable -> native_use_state = 'reusable' 且 native_consumer_gateway_request_id is null
  single_use -> native_use_state in ('available', 'claimed', 'consumed', 'ambiguous')

portable:
  conversation_transcript_id != null
  provider_instance_id == null
  upstream_credential_id == null
  native_state_envelope == null
  native_state_schema_version == null
  secret_key_id == null
  native_reuse_mode = 'not_applicable'
  native_use_state = 'not_applicable'
  native_consumer_gateway_request_id == null
```

索引：

```text
unique(gateway_request_id)
unique(id, client_api_key_id)
(client_api_key_id, expires_at)
(provider_instance_id, expires_at) where provider_instance_id is not null
(upstream_credential_id) where upstream_credential_id is not null
(native_consumer_gateway_request_id) where native_consumer_gateway_request_id is not null
(conversation_transcript_id) where conversation_transcript_id is not null
(expires_at)
```

Redis 可以缓存 binding 和 connection-local topology，但 PostgreSQL 是持久 binding 的事实源。Native 失败时不能自动改为 portable。

复合外键在数据库中保证 response/request、request owner、transcript owner 和 native credential/instance 一致，应用校验只是更早返回友好错误，不能作为唯一防线。API 按当前 Key 查询 binding，不能只凭可猜测的 response ID 读取其他调用方历史。删除或重新生成 client API key 不自动继承旧 Key 的 portable transcript。

Native binding 的 instance、可选 credential scope 和 continuation state 必须取自该 request 唯一 downstream-committed attempt 的终态结果，不能接受 API handler 传入的任意组合；portable binding 只能引用本次 finalization transaction 新建的不可变 canonical transcript snapshot。`native_state_envelope` 由对应 Provider adapter 生成和校验，可以容纳 response ID、conversation ID 或 connection scope，但通用 store 不解析其内部字段。

Portable binding 的 `expires_at` 不得晚于 transcript 的 `expires_at`；native binding 的有效期也不能超过 Provider 声明的上游 continuation 生命周期。Provider adapter 在写 binding 时声明 native state 是 reusable（必须能安全并发使用）还是 single-use，不能让 API 选择；“可重复但只能串行”不进入通用 binding 语义，Provider 必须提供自己的持久状态边界或禁用 native continuation。对 single-use binding，history owner 在创建 consumer request 时锁定 binding 行并 CAS `available → claimed`，同时写入 `native_consumer_gateway_request_id`；若 attempt 被证明 `not_sent` 则 CAS 释放为 available，若 sent 则变为 consumed，若 send 边界 ambiguous 则永久标为 ambiguous、禁止重用。Redis continuation lease 只缩短竞争窗口，PostgreSQL `native_use_state/native_use_revision` 才是 Redis 丢失后的正确性边界。新 response 产生新 binding，旧 binding 不原地变成新 continuation state。该跨行规则由 history owner 在同一事务中校验，并由 retention 契约测试覆盖。

Recovery worker 对超时 `claimed` binding 读取其 consumer request/attempt：没有 attempt 或全部已证明 not-sent 才释放，存在 sent/ambiguous attempt 则分别收敛为 consumed/ambiguous。它不能根据 Redis lease 是否过期擅自释放，否则进程崩溃会把已经到达上游的一次消费重新放行。

## 运维事件

### `ops_events`

只记录请求事实之外的后台错误和重要运维事件。请求错误已经存在于 `gateway_requests` 和 `request_attempts`，禁止再复制一份。

| 字段 | 类型与约束 | 语义 |
| --- | --- | --- |
| `id` | `text primary key` | Event ID |
| `level` | `text not null` | `warning` 或 `error` |
| `component` | `text not null` | models、oauth、quota、update 等 owner |
| `operation` | `text not null` | 具体后台操作 |
| `provider_instance_id` | `text references provider_instances on delete set null` | 可选 live join |
| `provider_instance_ref` | `text` | Instance 历史快照 |
| `provider_kind` | `text` | Provider 名称快照 |
| `upstream_credential_id` | `text` | 可选 live join；与 instance 组成复合 FK |
| `resource_ref` | `text` | 资源历史快照 |
| `failure_kind` | `text not null` | 稳定错误分类 |
| `status_code` | `integer` | 可选协议状态 |
| `message` | `text not null` | 已脱敏安全消息 |
| `metadata_json` | `jsonb not null default '{}'` | 已脱敏诊断 |
| `created_at` | `timestamptz not null` | 发生时间 |

约束：

```text
level in ('warning', 'error')
status_code is null or status_code between 100 and 599
foreign key(upstream_credential_id, provider_instance_id)
  references upstream_credentials(id, provider_instance_id)
  on delete set null (upstream_credential_id)
provider_instance_id is null or (provider_instance_ref is not null and provider_instance_ref = provider_instance_id)
upstream_credential_id is null or (provider_instance_id is not null and resource_ref is not null)
```

索引：

```text
(created_at desc, id desc)
(component, created_at desc, id desc)
(provider_instance_id, created_at desc, id desc)
  where provider_instance_id is not null
(provider_instance_ref, created_at desc, id desc)
  where provider_instance_ref is not null
(upstream_credential_id, created_at desc, id desc)
  where upstream_credential_id is not null
(resource_ref, created_at desc, id desc)
  where resource_ref is not null
(failure_kind, created_at desc, id desc)
```

Ops recorder 对相同故障做有界速率限制并周期性写入已脱敏的 suppressed count，不能在 Provider outage 的每次后台 retry 都无限插入一行。限流只抑制重复诊断噪声，不得吞掉首次故障或管理审计事实。

当 live instance/credential 存在时，Ops recorder 从其受控 snapshot 填充 `provider_instance_ref`、`provider_kind` 和 `resource_ref`；错误对象只能提供经过 allowlist 的 failure/status 信息，不能反向决定这些维度。

## 派生聚合

### `request_metric_buckets`

保存客户端视角的长期趋势，不包含 Provider 维度：

| 字段 | 类型与约束 |
| --- | --- |
| `bucket_start` | `timestamptz not null` |
| `client_api_key_ref` | `text not null` |
| `protocol` | `text not null` |
| `operation` | `text not null` |
| `client_transport` | `text not null` |
| `model_route_ref` | `text not null` |
| `public_model_id` | `text not null` |
| `request_count` | `bigint not null default 0 check (>= 0)` |
| `success_count` | `bigint not null default 0 check (>= 0)` |
| `failure_count` | `bigint not null default 0 check (>= 0)` |
| `cancelled_count` | `bigint not null default 0 check (>= 0)` |
| `incomplete_count` | `bigint not null default 0 check (>= 0)` |
| `caller_error_count` | `bigint not null default 0 check (>= 0)` |
| `input_tokens` | `bigint not null default 0 check (>= 0)` |
| `output_tokens` | `bigint not null default 0 check (>= 0)` |
| `cached_tokens` | `bigint not null default 0 check (>= 0)` |
| `cache_write_tokens` | `bigint not null default 0 check (>= 0)` |
| `reasoning_tokens` | `bigint not null default 0 check (>= 0)` |
| `total_tokens` | `bigint not null default 0 check (>= 0)` |
| `usage_metrics_json` | `jsonb not null default '{}'` |
| `first_token_latency_sum` | `bigint not null default 0 check (>= 0)` |
| `first_token_latency_count` | `bigint not null default 0 check (>= 0)` |
| `latency_sum` | `bigint not null default 0 check (>= 0)` |
| `latency_count` | `bigint not null default 0 check (>= 0)` |
| `max_latency_ms` | `bigint not null default 0 check (>= 0)` |
| `min_latency_ms` | `bigint check (min_latency_ms is null or min_latency_ms >= 0)` |
| `updated_at` | `timestamptz not null` |

主键：

```text
primary key(
  bucket_start,
  client_api_key_ref,
  protocol,
  operation,
  client_transport,
  model_route_ref,
  public_model_id
)
```

`client_api_key_ref` 和 `model_route_ref` 使用当时的 ID 快照；无对应实体时使用稳定的 `__none__`，未知值使用 `__unknown__`。无法解析的客户端模型统一聚合为 `public_model_id = '__unmatched__'`，绝不能把任意 `requested_model_id` 放进 bucket 主键制造无界高基数；原始有界值只留在 `gateway_requests` 保留期内。

### `attempt_metric_buckets`

保存上游视角的健康、资源用量和成本：

| 字段 | 类型与约束 |
| --- | --- |
| `bucket_start` | `timestamptz not null` |
| `provider_kind` | `text not null` |
| `provider_instance_ref` | `text not null` |
| `resource_ref` | `text not null` |
| `upstream_model_id` | `text not null` |
| `service_tier` | `text not null` |
| `upstream_transport` | `text not null` |
| `currency` | `text not null` |
| `attempt_count` | `bigint not null default 0 check (>= 0)` |
| `success_count` | `bigint not null default 0 check (>= 0)` |
| `failure_count` | `bigint not null default 0 check (>= 0)` |
| `cancelled_count` | `bigint not null default 0 check (>= 0)` |
| `incomplete_count` | `bigint not null default 0 check (>= 0)` |
| `rate_limited_count` | `bigint not null default 0 check (>= 0)` |
| `auth_failure_count` | `bigint not null default 0 check (>= 0)` |
| `provider_5xx_count` | `bigint not null default 0 check (>= 0)` |
| `not_billable_attempt_count` | `bigint not null default 0 check (>= 0)` |
| `input_tokens` | `bigint not null default 0 check (>= 0)` |
| `output_tokens` | `bigint not null default 0 check (>= 0)` |
| `cached_tokens` | `bigint not null default 0 check (>= 0)` |
| `cache_write_tokens` | `bigint not null default 0 check (>= 0)` |
| `reasoning_tokens` | `bigint not null default 0 check (>= 0)` |
| `total_tokens` | `bigint not null default 0 check (>= 0)` |
| `usage_metrics_json` | `jsonb not null default '{}'` |
| `first_token_latency_sum` | `bigint not null default 0 check (>= 0)` |
| `first_token_latency_count` | `bigint not null default 0 check (>= 0)` |
| `latency_sum` | `bigint not null default 0 check (>= 0)` |
| `latency_count` | `bigint not null default 0 check (>= 0)` |
| `max_latency_ms` | `bigint not null default 0 check (>= 0)` |
| `min_latency_ms` | `bigint check (min_latency_ms is null or min_latency_ms >= 0)` |
| `fully_priced_attempt_count` | `bigint not null default 0 check (>= 0)` |
| `partially_priced_attempt_count` | `bigint not null default 0 check (>= 0)` |
| `unpriced_attempt_count` | `bigint not null default 0 check (>= 0)` |
| `estimated_cost_amount` | `numeric(20, 10) not null default 0 check (>= 0)` |
| `updated_at` | `timestamptz not null` |

主键：

```text
primary key(
  bucket_start,
  provider_kind,
  provider_instance_ref,
  resource_ref,
  upstream_model_id,
  service_tier,
  upstream_transport,
  currency
)
```

无 credential 的 Provider 使用 `resource_ref = '__none__'`，默认 tier 使用 `service_tier = '__default__'`。`not_sent` attempt 使用 `currency = '__none__'` 并增加 `not_billable_attempt_count`；unknown estimate 使用 `currency = '__unknown__'` 并增加 `unpriced_attempt_count`；partial estimate 使用真实 currency 并增加 `partially_priced_attempt_count`。Dashboard 必须同时展示 not-applicable/known/partial/unknown 覆盖率，不能通过 `estimated_cost_amount = 0` 伪装成免费，也不能跨 currency 求和。

两张表都使用 UTC 对齐的固定 15 分钟 `bucket_start`，数据库 check 要求 epoch 秒数能被 900 整除；更粗粒度由查询层 roll up，不在同一表混存多种 bucket width。表中的 `request_count/attempt_count` 只在事实终态时增加，总数必须等于 success、failure、cancelled 与 incomplete 四类 terminal 计数之和；`caller_error_count` 是 failure 的子集，不参与第二次求和。Attempt 表还必须满足 `attempt_count = not_billable + fully_priced + partially_priced + unpriced`。

Attempt bucket 的 `currency` 只能是大写三字符代码、`__none__` 或 `__unknown__`。`__none__` 行只能累计 not-billable attempt，`__unknown__` 行只能累计 unpriced attempt，真实 currency 行只能累计 fully/partially priced attempt；这些关系使用 check constraint 固化，避免同一行把无价格和零价格混在一起。

`usage_metrics_json` 是注册 metric 到非负十进制字符串累计值的对象，由 bucket owner 使用十进制定点数逐 key 加法，用于在原始事实过期后仍保留 image、audio 等非 token 用量。它不接受任意 Provider key 或 JSON 浮点。Latency count 为 0 时对应 sum/max 必须为 0 且 min 为 null；count 大于 0 时 min 非空且 `min <= max`，防止“没有样本但显示 0 ms”。

两张 bucket 表必须可以从保留期内的原始事实重建。增量聚合与 terminal CAS 位于同一事务，避免没有 outbox/checkpoint 时的重复累计和漏算；周期性 rebuild 只负责校验或修复保留期内的派生数据。聚合只能由一个 owner 写入；API、Provider 和 Dashboard 不各自维护计数。`account_usage` 一类可漂移的累计表不进入终态。

Bucket delta 使用单条 PostgreSQL upsert 原子相加并检测 bigint/numeric overflow，不能先 select 再由应用回写。若 finalization 同时更新 request 与 attempt bucket，所有 writer 使用固定锁顺序，避免高流量下产生可预期的 bucket-row deadlock。

Rebuild 只替换已经超过最大 request deadline 与迟到宽限期的 closed bucket，并在窗口级锁内从事实表重算后原子替换；当前仍可能接收 terminal delta 的 open bucket 只做对比，不允许“先清空再重建”覆盖并发写入。

## PostgreSQL 删除与保留规则

| 数据 | 删除规则 |
| --- | --- |
| Provider instance | 先 disable 并从 Snapshot 移除；存在 route target、价格或未过期 native binding 时禁止删除 |
| Credential | 先 disable、停止新 lease 并 drain；未过期 native binding 存在时禁止删除；删除时清除 secret 和 Provider 专属子表，历史 attempt 保留快照 |
| Credential state | 随 credential 级联删除，不影响历史 attempt 快照 |
| Provider model | 缺失时设为 unavailable；被 model route target 或历史价格引用时禁止删除 |
| Model route | 删除时级联 model route targets；历史 request 的 live FK 置空 |
| Price version | 发布后不可修改；被 attempt 引用后禁止删除 |
| Client API key | 先 disable、拒绝新请求并 drain/recover running request；同一删除事务先显式删除 binding/transcript，再删除 Key，使历史 request FK 置空 |
| Gateway request | 按 retention 删除并级联 attempts；只要 binding 仍存在，FK 就拒绝删除，清理任务必须先删除已过期 binding |
| Conversation transcript | 先删除已过期 portable binding，再从 snapshot 叶子向根删除；items 随 transcript 级联删除 |
| Admin audit event | 不因 actor 或目标实体删除而删除，仅按 audit retention 清理 |
| Ops event | 按独立 retention 删除 |
| Metric bucket | 按 bucket retention 删除 |

`gateway_requests`、`request_attempts` 和 `ops_events` 初期使用普通表与 `(time desc, id desc)` 索引。只有生产数据量证明 retention delete 或索引维护成为瓶颈时，才按月进行时间分区；不在首轮迁移提前引入分区管理。

配置实体的 hard delete 不是普通更新：统一执行 disable → 发布 revision → 停止新租约 → 等待 running attempt/后台任务 drain → 检查 binding/引用 → delete。超时后是否强制终止必须是显式管理操作并写 audit，不能让 `on delete cascade` 意外中断在途请求。Client API key 的删除顺序固定为 binding → transcript snapshot/items（叶子到根）→ key，避免复合 owner FK 依赖多个 referential action 的触发顺序。Retention 按小批量、有界锁时长删除，并先运行 stale recovery；备份和只读副本也必须遵守同一数据保留与密钥退役策略，否则主库删除不等于隐私数据真正到期。

Control-plane validator 保证 usage retention 大于最大 request deadline、迟到宽限和 bucket rebuild 安全余量之和；否则可能在 request 尚未 recovery 或 bucket 尚未关闭时先删掉唯一事实。Retention cutoff 使用数据库时间并记录每批高水位，任务重跑不能越过未处理引用。

## Redis 终态边界

Redis 保存：

| Key 族 | 内容 | 性质 |
| --- | --- | --- |
| `admin:session:*` | 管理员会话 | TTL |
| `runtime:snapshot:*` | 配置版本通知 | 可重建 |
| `runtime:credential:*` | Credential revision 变更通知 | 可从 PostgreSQL 重载 |
| `lease:provider-instance:*` | 多实例共享的 endpoint 并发与请求间隔 | 短 TTL |
| `lease:credential:*` | 多实例 credential 并发或刷新租约 | 短 TTL |
| `lease:provider-task:*` | Catalog/health/quota 等后台任务 leader lease | 短 TTL、结果仍由数据库 CAS 防旧写 |
| `lease:continuation:*` | Provider 声明为单并发/单次消费的 native continuation | 短 TTL、fencing token |
| `cooldown:credential:*` | 热路径 cooldown 缓存 | PostgreSQL 状态的可丢失缓存 |
| `rate:client-key:*` | 调用方速率和并发计数 | 窗口 TTL |
| `budget:client-key:*` | 当前预算窗口的 estimated spend 与 in-flight reservation | 可由 request/attempt facts 重建 |
| `continuation:*` | Binding 与 connection-local topology 缓存 | 只缓存 opaque ID 或加密 envelope，PostgreSQL/上游状态可恢复 |
| `circuit:provider-instance:*` | 多实例共享熔断摘要 | TTL、可由 attempt 恢复 |

Redis 不保存上游 secret、对话正文、唯一模型目录或唯一计费事实。Provider model catalog 的权威数据进入 PostgreSQL，RuntimeSnapshot 在进程内使用不可变结构读取。

预算限额在 request 接入时从 `client_api_keys.policy_json` 冻结到 request，已发生的目录价格估算来自 `request_attempts`。Redis 只原子维护当前窗口的 in-flight reservation 和热聚合；接入时按可计算的保守上界预留，终止时按全部 attempts 的估算成本调整。Unknown estimate 不能按零释放 reservation，必须按 policy 保留估算、拒绝后续请求或显式降级为 soft budget。

严格预算接入顺序固定为：Redis 原子检查并预留、PostgreSQL 写入带预留金额的 running request、之后才允许建立 attempt。PostgreSQL 写入失败时释放 Redis reservation；进程在两步之间崩溃只会留下带 TTL 的保守孤儿 reservation，不会产生没有持久 request 的上游消费。

Redis 丢失后必须先按 `client_api_key_ref + budget_window_start/end + budget_currency` 从 terminal attempt facts 与仍为 `running` 的请求重建，再恢复严格预算接入。重建过程持有窗口级 fencing token，避免一边扫描旧事实一边接受新 reservation。若无法完成重建，应 fail closed 或显式降级为 soft budget，不能静默按零消费继续放行。

跨实例 semaphore、request interval、rate limit 和严格预算的多 Key 原子操作必须使用同一 Redis Cluster hash tag、Redis server time 和带 fencing token 的 Lua/function；不能依赖应用节点时钟或先 GET 后 SET。Redis 不可用时，上游并发/间隔和 strict budget 默认 fail closed；只允许经过显式配置的单节点 soft-limit 降级，并把降级状态暴露到健康检查和审计中。

Redis epoch 变化时先暂停新的受限操作：client concurrency 从 running requests 重建，RPM/TPM 从当前窗口 request facts 重建或等待窗口自然结束，provider/credential lease 从 running attempts 与仍存活 worker 的重新注册保守恢复。恢复完成前不能把“Redis 是空的”解释成“当前使用量为零”。

## Secret 与隐私

当前表中的 `client_api_keys.key`、`accounts.access_token`、`accounts.refresh_token` 和 `account_cookies.value` 都是可直接使用的明文，终态不允许继续存在。

终态使用三类互相独立的密钥材料：

1. Encryption root key：通过不同 domain 派生 AEAD key，分别加密上游 secret、Cookie、native continuation state 和 portable transcript item。
2. Credential fingerprint key：HMAC credential 用于去重，不可用于解密。
3. Identity pseudonym key：只用于 installation ID 和遥测资源伪名，不可兼作前两者。

加密 key 不存 PostgreSQL，应来自环境、KMS 或 secret manager。Envelope 自带 nonce、算法和格式版本，表中 `secret_key_id` 用于轮换；`key_hash`、credential fingerprint 和 resource ref 的文本值也携带算法/版本前缀，避免以后增加独立版本列。AEAD additional authenticated data 必须绑定用途、表、owner ID 和行主键/sequence，防止不同 credential、Cookie 或 conversation item 之间交换密文。轮换采用读旧写新，不修改历史迁移文件。

以下数据禁止进入任何 `metadata_json`、错误消息或日志：

- API Key、OAuth token、Cookie、AWS secret。
- 完整 request/response body。
- 未脱敏 Provider 原始错误正文。
- Portable conversation 明文内容和 capability-bearing native continuation handle。

每个 `metadata_json` 必须由该表 owner 使用字段 allowlist 构造，不能直接序列化外部 request、Provider response、Rust `Debug` 对象或任意 `serde_json::Value`。`request_attempts.upstream_request_id/upstream_response_id` 只允许保存 adapter 明确分类为非 bearer 的诊断 ID；任何可用于恢复、读取或劫持上游会话的值只能进入 `continuation_bindings.native_state_envelope`。需要新增可查询字段时优先增加普通列；只有确实属于 Provider 扩展且不参与通用查询时才进入 JSON。

## 当前表迁移归宿

| 当前表或字段 | 终态归宿 |
| --- | --- |
| `schema_migrations` | 原表保留，继续校验版本、名称和 checksum |
| `admin_users` | 原表保留 |
| 管理配置变更 | 新增不可变 `admin_audit_events` |
| `client_api_keys.key` | 迁移为 `key_hash`，完整 Key 只返回一次 |
| `runtime_settings.model_aliases_json` | `model_routes`、`model_route_targets` |
| `runtime_settings.rotation_strategy` | `model_routes.routing_strategy` |
| `runtime_settings.ops_error_retention_days` | `ops_event_retention_days` |
| Codex refresh/concurrency 设置 | Codex Provider config 与 credentials |
| `accounts` | `upstream_credentials` + `upstream_credential_states` + `codex_accounts` |
| `accounts.status` | 管理禁用映射到 credential `enabled`；过期、封禁和额度状态映射到 credential state `availability/state_json` |
| `accounts.quota_*`、cooldown 字段 | `upstream_credential_states.cooldown_until/state_json` |
| `accounts.access_token/refresh_token` | 加密 `secret_envelope` |
| `account_cookies` | `codex_account_cookies`，value 加密 |
| `account_usage` | 从 attempts 或 `attempt_metric_buckets` 查询，最终删除 |
| `usage_records` | `gateway_requests` + `request_attempts` |
| 请求相关 `ops_error_logs` | request/attempt failure 字段 |
| 后台 `ops_error_logs` | `ops_events` |
| `request_time_buckets` | `request_metric_buckets` + `attempt_metric_buckets` |
| Redis 模型快照 | PostgreSQL `provider_models` + 进程 RuntimeSnapshot |
| Redis response affinity | `continuation_bindings` 的缓存与 WS topology |

迁移顺序固定为：

1. 启动前验证独立 encryption root、credential fingerprint 和 identity pseudonym key 已就绪；密钥缺失时拒绝执行会产生明文回退的迁移。
2. Expand：只追加新表、加密列和兼容索引，创建 Provider、credential config/state、model、route、price、request/attempt、audit、continuation 与 bucket 结构；不在同一发布中删除旧列。request 与 continuation binding 的双向 FK 先建表、回填并验证，再以 `not valid` 约束分阶段添加，避免循环依赖阻塞 rollout。
3. Backfill：以有界批次和可恢复 checkpoint 加密 client key、token、Cookie，并回填唯一 Codex instance、账号和保留期内 telemetry。每批记录数量、checksum 和失败原因，允许安全重跑但不能重复生成事实；对仍会 OAuth 刷新的旧账号，cutover 前必须短暂冻结旧 writer、drain refresh lease 并做最后一轮 delta copy，不能让 backfill 与旧 writer 静默竞争。
4. Verify：用独立查询逐项对账行数、credential 身份、usage、终态、引用和解密抽样；新二进制可 shadow-read 比较，但同一事实始终只有旧或新 writer，不能双写后靠时间戳择胜。
5. Cutover：先切换 Codex RuntimeSnapshot 和请求执行读取，再在一个明确版本切换 request/attempt、continuation 和 bucket 的唯一 writer/readers；切换点持久化并可观测。
6. Contract：至少经过一个不再支持旧 writer 的稳定发布和备份保留窗口后，删除明文/旧表/旧 Redis 事实；回滚只能回到仍理解新 schema 且不会重新写明文的版本。

迁移期间可以离线回填和验证，但一个生产事实在任一版本只能有一个 writer。禁止长期 dual-write 后依靠人工判断哪一份正确。DDL 必须评估表锁：大表的 default、not-null、FK validation 和索引采用分阶段 `not valid`/validate 或 concurrent build，不能把终态约束直接变成不可控停机。`CREATE INDEX CONCURRENTLY` 不能放进当前的事务型 migration runner，届时必须增加受审计、可重试的 non-transactional migration phase，而不是把命令偷偷塞进普通 migration SQL。

现有迁移文件已经受 checksum 保护，不能修改 `0001_initial.sql` 或 `0002_cache_write_tokens.sql`；所有变化只能追加新版本。Telemetry 回填按非空旧 `request_id` 聚合 logical request，按 `attempt_index` 和时间恢复 attempts；缺少 request ID、commit、usage 或 price 证据的旧记录使用新生成 ID 和 `legacy` metadata，并保持 unknown，不能伪造成功、目录价格估算或 continuation。`account_usage` 累计值不反向生成请求事实。

## 明确不创建的结构

- 不创建 `providers` lookup 表；Provider code 由 Registry 决定。
- 不创建包含所有平台字段的通用 `accounts` 大表。
- 不创建 tenant、organization、project 等当前没有业务语义的层级。
- 不创建 Provider capability EAV 表；能力快照使用校验后的 JSON。
- 不创建 credential group；隔离资源池使用独立 Provider instance。
- 不创建成功表和失败表两套请求事实。
- 不创建通用 `subject_type/subject_id` usage ledger。
- 在只做目录价格估算的阶段不创建 invoice/payment ledger；接入真实账单对账时单独建模，不能复用或篡改 attempt estimate。
- 不保存默认 request/response payload archive。
- 当前不引入响应缓存；如果未来要本地命中成功响应，必须显式增加 response source/cache 事实并重写 request-success invariant，不能伪造一个上游 attempt。
- 不预留无法完成响应 replay 的空 idempotency 表；只有协议层明确承诺幂等语义时才单独设计。
- 不用 PostgreSQL 行锁实现热路径 semaphore、lease 或 circuit breaker。

## 数据与并发契约测试

终态迁移和 store 必须覆盖：

1. 空库完整迁移、从每个受支持旧版本升级、migration name/checksum 篡改拒绝。
2. 所有 FK 的 cascade、set-null 和 restrict 行为，以及每个 FK 是否有支持索引；行为 JSON 缺少/不支持 `schema_version`、超长文本/JSON 或保留 sentinel 必须被拒绝。
3. 两个并发控制面事务只能得到不同且递增的 `config_revision`，stale expected revision 必须 conflict；Snapshot 读取只能得到一个完整 revision，丢失 Redis 通知后轮询仍能收敛。
4. `credential_revision` 与 `state_revision` 独立 CAS，旧 `observed_at` 不能覆盖新状态；同一稳定 resource 不能复制到多个 instance 绕过限制。
5. 上游 payload 发送前 request/attempt 行已经提交；attempt target/instance 必须属于 request 冻结的 route；`upstream_send_state` 与 `downstream_committed` 不混淆；并发 create/commit 时同一 request 最多一个 downstream-committed attempt 且 commit 后没有新 attempt，重复 finalizer 只结算一次。
6. Stale running request/attempt 依据持久 deadline 收敛为 incomplete，不能因配置变化改变判断，也不能重复写 usage 或估算成本。
7. Price version 在 `effective_from` 边界正确选择；known、partial、unknown、明确零价、多货币和 estimated/invoice 语义均不混淆。
8. Native/portable continuation check、调用方 owner、response/request、credential/instance 对应关系、native state 与 transcript 加密边界由真实 FK/transaction 验证；同一 portable history 的分支不能读到彼此后续 delta，single-use native binding 在 Redis flush/进程崩溃后也不会重复消费。
9. Client key、Provider、credential、route 和 transcript 删除后的历史快照与隐私清理结果。
10. 从 request/attempt facts 重建 closed bucket 后，与增量聚合逐字段一致；unknown estimate 不进入已知金额，非 token metric 不丢失，任意客户端 model ID 不制造高基数 bucket。
11. 数据库、管理 API、Debug、错误和日志样本中不存在完整 client key、上游 secret、Cookie 或 conversation 明文。
12. Retention 先 recovery，再清理过期 binding、从 transcript leaf 向 root 清理，最后清理 request；未过期 continuation 不会被原始事实清理提前破坏，usage facts 不会早于 bucket 安全窗口删除。
13. Redis flush/epoch 变化时，rate、concurrency、lease 和 strict budget 在重建完成前不会把空状态当成零；预算窗口可仅从冻结 request 与 attempt facts 恢复。

涉及并发的测试必须使用真实 PostgreSQL transaction 和唯一约束，不能只用内存 mock 证明。

## 验收标准

1. 新增普通 API Key Provider 只增加 Provider adapter 和 instance/credential 数据，不修改通用 schema。
2. 新增 Provider 专属表时，只能承载真实可查询状态，不能复制通用 credential、model、request 或 attempt 字段。
3. 一次客户端请求始终只有一行 `gateway_requests`，所有上游调用都能按顺序关联到 `request_attempts`。
4. 最多一个 attempt 可以 `downstream_committed = true`，commit 后不能出现新的 attempt；ambiguous upstream send 不会自动重放。
5. 客户端 usage、上游 usage 和上游目录价格估算可以分别对账；known、partial、unknown estimate 可区分，未知价格不会显示为零。
6. 删除或禁用 Provider/credential 后，历史请求仍能按 Provider、instance、resource 和模型统计。
7. PostgreSQL、备份、Redis、日志和管理 API 都不能恢复下游完整 Key 或读取上游明文秘密。
8. Dashboard 聚合与 credential 用量可以从事实表重建，不依赖 `account_usage` 一类可漂移累计状态。
9. Native continuation 不能跨 Provider；portable continuation 的正文始终加密并与调用方 Key 隔离。
10. 所有 FK、外部 ID 与历史 `_ref` 均符合本文命名规则，不存在含义不明的 `route_id`、`request_id`、`credential_id` 或 `_key`。
11. 数据库设计不要求 Gateway Engine、API adapter 或 Provider adapter 互相了解对方的存储结构。
