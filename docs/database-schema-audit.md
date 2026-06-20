# 数据库表设计与字段命名审计

审计日期：2026-06-20

本文档只审计当前 Rust 项目的 SQLite 表设计、字段设计、字段冗余和后续指纹持久化方向。本文不包含真实账号、token、cookie、API key 等敏感数据。

## 目标

- 把当前表结构中不够清晰、不够稳定、容易制造重复数据或迁移债的问题固定下来。
- 给出一套可以长期维护的命名规范和表职责边界。
- 明确本轮指纹设计：`config.yaml` 只作为首次默认种子，数据库持久化后以数据库为准；后续自动更新只写数据库，运行时只从数据库加载。
- 保持项目定位为 OpenAI / Codex 相关能力，不引入通用 proxy、VPN 或其它 provider 的泛化字段。

## 本轮实施结果

项目尚未上线，本轮按“新库最终结构”直接迁移，不写旧结构兼容代码。已经落地的结构变更：

- `schema.sql` 成为新库最终结构，移除 `connect_sqlite()` 里的 ad hoc 轻量补丁逻辑。
- 新增 `schema_migrations` 表作为后续上线后的迁移账本。
- `accounts` 上游身份物理列改为 `chatgpt_account_id` / `chatgpt_user_id`，并增加 `ux_accounts_chatgpt_identity` 唯一索引。
- `account_usage` 补齐并贯通 `reasoning_tokens`、`total_tokens`，协议解析、仓储写入、管理端列表和汇总都使用同一语义。
- `fingerprints` 使用固定当前槽位 `id = 'current'`，完整保存 `originator`、默认请求头、请求头顺序和更新时间；`config.yaml` 只做首次种子。
- `fingerprint_update_history` 单独保存自动更新历史，不参与运行时当前指纹选择。
- `session_affinities.account_id` 改为本地账号外键并级联删除。
- `event_logs` 补齐 `transport`、`attempt_index`、`upstream_status_code`、`failure_class`、`response_id`、`upstream_request_id` 顶层排查字段，并保留 `metadata_json` 作为补充上下文。

仍然暂不拆 `accounts`、`account_usage` 等大表。拆表属于 P2 长期清洁项，等功能边界继续稳定后再做，避免为了形式拆表制造更多搬运代码。

## 审计基线

改造前 schema 已经能支撑基础运行，但还不是“教科书级”的表设计。主要问题不是缺表，而是职责边界和命名语义混在一起：

1. `accounts` 同时承载账号身份、token、刷新计划、配额快照、配额冷却、Cloudflare 恢复状态，字段越来越像“运行时大杂烩”。
2. `accounts.account_id` / `accounts.user_id` 命名过泛。它们实际是 ChatGPT/OpenAI 上游身份，不应该和本地账号主键 `id` 混淆。
3. 上游账号身份只有普通索引，没有唯一约束。导入相同上游账号时，数据库层不能阻止重复记录。
4. `schema.sql` 和实际运行库曾经出现 drift：运行库 `account_usage` 有 `reasoning_tokens`、`total_tokens`，但 `schema.sql` 没有；`session_affinities.function_call_ids_json` 的默认值表达也不一致。
5. 迁移机制还是 ad hoc：`connect_sqlite()` 运行 `schema.sql` 后只补一个 `accounts.next_refresh_at`。缺少 `schema_migrations` 这种可追踪迁移账本。
6. `fingerprints` 表只存了部分指纹字段。`originator`、默认请求头、请求头顺序仍从代码默认值补齐，不满足“后续都去数据库里拿”。
7. 时间字段命名不统一：`added_at` 和 `created_at` 混用；`fingerprints.created_at` 在 upsert 时被覆盖，语义更像 `updated_at`。
8. 多处 JSON 字段承担外部快照或小型结构化状态，这可以接受，但必须有命名、校验和索引边界，不能让核心查询长期依赖 `metadata_json like ...`。

## 表清单

| 表 | 当前职责 | 总体判断 |
| --- | --- | --- |
| `admin_users` | 管理员账号 | 简单可用，命名基本清晰 |
| `admin_sessions` | 管理员登录会话 | 简单可用 |
| `client_api_keys` | 客户端访问 key | 可用，但 `name` / `label` 语义重叠，唯一约束不足 |
| `accounts` | OpenAI/ChatGPT 账号、token、刷新、配额、恢复状态 | 已完成上游身份重命名和唯一约束；后续再评估拆表 |
| `account_refresh_leases` | 多任务刷新租约 | 职责清晰，字段可小幅命名优化 |
| `account_usage` | 账号累计用量和窗口用量 | 已补齐 reasoning/total；拆 totals/windows 属于 P2 |
| `account_cookies` | 账号级 cookie 持久化 | 基本合理，`id` 可选冗余 |
| `fingerprints` | Codex Desktop 请求指纹版本 | 已补齐当前槽位完整指纹字段 |
| `event_logs` | 管理端结构化事件日志 | 已补齐常用链路排查字段 |
| `model_plan_snapshots` | 套餐模型列表快照 | 简单可用 |
| `session_affinities` | response/session 到账号的亲和性 | 已补账号外键和默认值一致性 |

## 命名规范

后续新字段和迁移建议遵守以下规则。

### 标识符

- 本地表主键统一用 `id`。
- 外部系统 ID 必须加来源前缀，不使用裸 `account_id` / `user_id`：
  - 推荐：`chatgpt_account_id`、`chatgpt_user_id`
  - 可接受：`openai_account_id`、`openai_user_id`
  - 不推荐：`account_id`、`user_id`，因为它们和本地外键语义冲突。
- 外键字段使用被引用实体名加 `_id`，例如 `account_id` 表示本地 `accounts.id`，不是上游账号 ID。

### 时间字段

- 时间字段统一使用 RFC3339 UTC 字符串，字段名以 `_at` 或 `_until` 结尾。
- 创建时间统一 `created_at`，更新时间统一 `updated_at`。
- `added_at` 应逐步迁移为 `created_at`。
- 如果字段表示“未来允许执行时间”，命名用 `next_*_at`，例如 `next_refresh_at`。
- 如果字段表示“冷却截止时间”，命名用 `*_cooldown_until`，当前命名可保留。

### 数值字段

- 计数用 `_count`，token 用 `_tokens`，毫秒用 `_ms`，秒用 `_seconds`。
- 累计总量和窗口总量不要混在命名里：
  - 累计：`request_count`、`input_tokens`
  - 窗口：`window_request_count`、`window_input_tokens`
- 如果保存 `total_tokens`，必须定义为“上游返回的原始 total”，不要同时把它当作 `input_tokens + output_tokens` 的派生值。

### 布尔字段

- SQLite 仍用 `integer check (value in (0, 1))`。
- 命名应表达肯定语义：
  - `enabled` 可接受，但 `is_enabled` 更清晰。
  - `quota_verify_required` 建议迁移为 `requires_quota_verification`。
  - `quota_limit_reached` 可接受。

### 密文和 JSON

- 密文字段建议统一用 `_ciphertext`，比 `_cipher` 更准确。
  - 当前字段 `access_token_cipher`、`refresh_token_cipher`、`value_cipher` 可先保留，后续大迁移再统一。
- JSON 字段必须以 `_json` 结尾。
- `_json` 字段只用于以下情况：
  - 外部接口原始快照，短期不做复杂查询。
  - 小型原子结构，作为整体读写，例如请求头顺序。
  - 事件元数据，作为补充上下文。
- 需要筛选、排序、聚合的字段不要长期只放在 JSON 里。

## 全局问题

### 1. 缺少迁移账本

改造前启动流程：

1. `connect_sqlite()` 创建连接。
2. 执行 `schema.sql`。
3. 执行 `apply_lightweight_migrations()`。
4. 只补 `accounts.next_refresh_at`。

问题：

- 迁移没有版本号，不知道一个运行库应用过哪些结构变更。
- 迁移散在 Rust 函数里，后续越补越难审计。
- `schema.sql` 与运行库 drift 时，没有统一入口修正。

本轮处理：

- 项目未上线，直接让 `schema.sql` 表达最终新库结构。
- 移除 Rust 里的 ad hoc lightweight migration。
- 增加 `schema_migrations` 表，为上线后的版本化迁移做准备。

后续上线后建议：

```sql
create table if not exists schema_migrations (
  version integer primary key,
  name text not null,
  applied_at text not null
);
```

后续每个结构变更用单独版本：

```text
0001_initial_schema
0002_accounts_next_refresh_at
0003_account_usage_reasoning_total_tokens
0004_fingerprint_full_profile
0005_accounts_chatgpt_identity_unique
```

`schema.sql` 保持“新库最终结构”，迁移脚本负责“旧库升级路径”。本轮不写旧库升级路径。

### 2. 运行库和 schema.sql 曾经 drift

改造前观察到的 drift：

- 实际运行库 `account_usage` 有 `reasoning_tokens`、`total_tokens`。
- `schema.sql` 的 `account_usage` 没有这两个字段。
- 实际运行库 `session_affinities.function_call_ids_json` 默认值显示为 `"[]"`。
- `schema.sql` 写的是 `'[]'`。

风险：

- 新装库、老运行库、测试库结构不一致。
- 代码一旦开始读写这些字段，某些环境会失败。
- 审计数据库时无法判断哪个结构才是事实来源。

本轮处理：

- `schema.sql` 已直接补齐新库最终字段。
- `account_usage.reasoning_tokens` / `account_usage.total_tokens` 已从协议解析贯通到 SQLite 写入、管理端列表和汇总。
- 平台层 schema 测试已覆盖关键列、唯一索引和外键。

### 3. 账号唯一身份需要数据库兜底

改造前只有普通身份索引：

```sql
create index on accounts(account_id, user_id)
where account_id is not null;
```

这是普通索引，不是唯一索引。它能加速查找，但不能阻止重复。

真实问题已经出现：同一个上游账号身份导入后出现两条本地记录，而不是更新。

本轮已落地：

```sql
create unique index if not exists ux_accounts_chatgpt_identity
on accounts(chatgpt_account_id, coalesce(chatgpt_user_id, ''))
where chatgpt_account_id is not null;
```

本次没有旧线上库，不做重复数据兼容迁移。如果本地 `.runtime` 已有旧表，直接重建运行库。

导入适配器也必须遵守同一套身份优先级：

1. 优先用 `chatgpt_account_id + chatgpt_user_id` 更新。
2. 其次用导入文件中的稳定本地 `id` 更新。
3. 仅邮箱不可作为唯一身份，只能作为展示或弱匹配辅助。

## 逐表审计

### `admin_users`

改造前字段：

```text
id, password_hash, created_at, updated_at
```

判断：职责清晰。

建议：

- 保持当前结构即可。
- 如果后续支持多管理员，可以补 `is_enabled`、`last_login_at`。
- `id` 当前如果承载用户名，需要明确它就是登录名；如果未来支持独立用户 ID，应新增 `username` 并迁移。

### `admin_sessions`

当前字段：

```text
id, user_id, expires_at, created_at
```

判断：职责清晰。

建议：

- `user_id references admin_users(id) on delete cascade` 是正确方向。
- 可选补 `last_seen_at`，但不是当前必须项。

### `client_api_keys`

当前字段：

```text
id, name, prefix, key_hash, label, enabled, created_at, last_used_at
```

问题：

- `name` 和 `label` 语义重叠。一个面向机器标识，一个面向展示标签时可以共存，但当前文档和字段名没有说明。
- `prefix` 只有 enabled 条件索引，没有唯一约束。
- `key_hash` 没有唯一约束。
- 缺 `updated_at`，管理端修改名称、禁用 key 后无法统一排序或审计。

建议：

- 如果只保留一个展示名，保留 `name`，删除或废弃 `label`。
- 如果两个都保留，定义清楚：
  - `name`：用户可见名称，必填。
  - `label`：短标签或分组，可选。
- 添加：

```sql
create unique index if not exists ux_client_api_keys_key_hash
on client_api_keys(key_hash);
```

- `enabled` 可后续迁移为 `is_enabled`。
- 补 `updated_at`、`revoked_at` 会更完整。

### `accounts`

当前字段职责混合：

```text
id
email, chatgpt_account_id, chatgpt_user_id, label, plan_type
access_token_cipher, refresh_token_cipher, access_token_expires_at, next_refresh_at
status
quota_json, quota_fetched_at, quota_limit_reached, quota_verify_required, quota_cooldown_until
cloudflare_cooldown_until
added_at, updated_at
```

已处理问题：

- 上游身份物理列已改为 `chatgpt_account_id` / `chatgpt_user_id`。
- 数据库已增加 `ux_accounts_chatgpt_identity` 唯一索引，防止同一上游身份重复导入。
- 仓储 SQL 用 alias 继续向当前 Rust 领域结构返回 `account_id` / `user_id`，后续再做 Rust 字段大改名。

仍然存在的问题：

- `added_at` 和其它表的 `created_at` 不统一。
- token、刷新调度、配额、Cloudflare 恢复状态都塞在账号主表里。
- `status` 同时表达可服务状态、刷新状态、终态禁用状态，需要明确状态机。

建议命名：

| 当前字段 | 建议字段 | 说明 |
| --- | --- | --- |
| `account_id` | `chatgpt_account_id` | 对应请求头 `chatgpt-account-id` 和上游账号身份 |
| `user_id` | `chatgpt_user_id` | 上游用户身份 |
| `added_at` | `created_at` | 与全库统一 |
| `access_token_cipher` | `access_token_ciphertext` | 后续大迁移再统一 |
| `refresh_token_cipher` | `refresh_token_ciphertext` | 后续大迁移再统一 |
| `quota_verify_required` | `requires_quota_verification` | 布尔语义更自然 |

本轮方案：

- 不拆表，先把数据库物理列名和唯一约束改干净。
- Rust 领域结构暂时保留 `account_id` / `user_id`，仓储层用 SQL alias 做边界转换；这不是旧库兼容代码，而是避免本轮把本地外键 `account_id` 和上游身份重命名混在一起造成无意义 churn。

长期目标拆分：

```text
accounts
  id
  email
  label
  plan_type
  status
  chatgpt_account_id
  chatgpt_user_id
  created_at
  updated_at

account_tokens
  account_id
  access_token_ciphertext
  refresh_token_ciphertext
  access_token_expires_at
  next_refresh_at
  token_updated_at

account_quota_state
  account_id
  quota_json
  quota_fetched_at
  quota_limit_reached
  requires_quota_verification
  quota_cooldown_until
  updated_at

account_recovery_state
  account_id
  cloudflare_cooldown_until
  updated_at
```

不是所有拆分都要一次做。优先级是：

1. 先加唯一身份约束，解决重复导入。
2. 再补 `created_at` 命名兼容。
3. 最后考虑 token/quota/recovery 拆表。

状态机建议：

| 状态 | 语义 |
| --- | --- |
| `active` | access token 当前可用于请求 |
| `expired` | access token 不可用，但 refresh token 可能还能自救 |
| `refreshing` | 临时刷新中，必须有租约或短期超时兜底 |
| `quota_exhausted` | 当前配额不可用，但不是账号封禁 |
| `disabled` | 本地确认不可调度，例如 refresh token 永久失效 |
| `banned` | 上游封禁或账号停用，不可再参与刷新和请求 |

关键约束：

- `disabled` / `banned` 启动时不应重新进入刷新调度。
- `expired` 是否参与恢复，必须同时看 `refresh_token_ciphertext` 和 `next_refresh_at`。
- `next_refresh_at` 到达前，启动扫描和后台扫描都不应刷新。

### `account_refresh_leases`

当前字段：

```text
account_id, owner, expires_at, updated_at
```

判断：职责清晰。

建议：

- `account_id` 在这张表里表示本地账号外键，命名正确。
- `owner` 可以更名为 `lease_owner`，但不是必须。
- 保持 `account_id primary key references accounts(id) on delete cascade`。

### `account_usage`

当前 schema.sql 字段：

```text
account_id
request_count, empty_response_count
input_tokens, output_tokens, cached_tokens
reasoning_tokens, total_tokens
image_input_tokens, image_output_tokens
image_request_count, image_request_failed_count
window_*
window_started_at, window_reset_at, limit_window_seconds
last_used_at
```

问题：

- 累计用量和窗口用量在同一行，写入路径集中，字段数量会继续膨胀。
- `total_tokens` 如果只是 `input_tokens + output_tokens`，就是冗余；如果保存上游原始 `total_tokens`，必须在字段注释和代码里明确。
- 图片用量、reasoning 用量、窗口用量混在一张表，后续每加一个计量维度都会扩字段。

本轮处理：

- `reasoning_tokens`、`total_tokens` 已写入 `schema.sql`。
- `TokenUsage` 已解析 `output_tokens_details.reasoning_tokens` 和上游 `total_tokens`。
- `AccountUsageDelta`、SQLite upsert、管理端 usage list/summary 已全部贯通。
- 明确定义：
  - `input_tokens`：上游 usage 输入 token。
  - `output_tokens`：上游 usage 输出 token。
  - `cached_tokens`：输入侧缓存 token。
  - `reasoning_tokens`：输出 token details 中的 reasoning token。
  - `total_tokens`：上游返回的原始 total token；缺失时才用输入加输出兜底。
- 窗口字段继续保留，但只作为当前 rate-limit 窗口快照。

长期目标：

> 优先级说明：拆分 `account_usage_totals` / `account_usage_windows` 是 P2 长期清洁项，不进入本轮 P0/P1。本轮已解决 schema drift 和字段定义不清的问题。只有当窗口统计、历史统计、管理端聚合继续扩张时，才值得拆表。

```text
account_usage_totals
  account_id
  request_count
  empty_response_count
  input_tokens
  output_tokens
  cached_tokens
  reasoning_tokens
  total_tokens
  image_input_tokens
  image_output_tokens
  image_request_count
  image_request_failed_count
  last_used_at

account_usage_windows
  account_id
  window_started_at
  window_reset_at
  limit_window_seconds
  request_count
  input_tokens
  output_tokens
  cached_tokens
  image_input_tokens
  image_output_tokens
  image_request_count
  image_request_failed_count
  updated_at
```

这样累计统计和限流窗口可以独立重置、独立迁移。

### `account_cookies`

当前字段：

```text
id, account_id, domain, name, value_cipher, path, expires_at, updated_at
unique(account_id, domain, name, path)
```

判断：整体合理。

问题：

- `id` 对数据库唯一性不是必须，因为自然键已经是 `(account_id, domain, name, path)`。
- `value_cipher` 建议后续统一为 `value_ciphertext`。
- `expires_at` 可能来自 RFC2822 cookie 日期，也可能来自 RFC3339 `max-age` 计算值。代码读取时可解析两种格式，但清理 SQL 使用字符串比较，若混合格式会有边界风险。

建议：

- 如果管理端不需要按 cookie 单条 ID 操作，可以考虑去掉 `id`，以自然键为主键。
- 如果保留 `id`，当前设计也可接受。
- 将入库 `expires_at` 统一规范化为 RFC3339 UTC，再做 SQL 清理。

### `fingerprints`

当前字段：

```text
id
app_version, build_number
platform, arch, chromium_version
user_agent_template
source
created_at
```

改造前代码行为：

- `Fingerprint::default_codex_desktop()` 在 core 里硬编码默认值。
- 旧仓储只从自动更新快照里读版本、平台、架构、Chromium 和 UA 模板。
- `originator`、`default_headers`、`header_order` 仍由代码默认值补齐。
- 旧自动更新写入会覆盖 `created_at`，这个字段实际承担了 `updated_at` 的语义。
- 改造前 `server/main.rs` 构造 `AppState` 时没有加载数据库指纹，运行时仍可能使用硬编码默认指纹。

结论：改造前表不满足“指纹默认来自 config.yaml，持久化到数据库，后续都从数据库拿”。

本轮已按“当前槽位 + 可选历史”的语义落地，不再把运行时当前快照和历史记录混在同一套 `source + created_at` 查询里。

### 当前槽位表

```text
request_fingerprints
  id
  originator
  app_version
  build_number
  platform
  arch
  chromium_version
  user_agent_template
  default_headers_json
  header_order_json
  source
  created_at
  updated_at
```

本轮保留现有表名 `fingerprints`，直接在该表上补齐同样字段。

字段定义：

| 字段 | 语义 |
| --- | --- |
| `id` | 指纹槽位。运行时当前槽位固定使用 `current` |
| `originator` | `originator` 请求头值，例如 `Codex Desktop` |
| `app_version` | Codex Desktop 应用版本 |
| `build_number` | Codex Desktop 构建号 |
| `platform` | UA 模板中的平台字段，例如 `darwin` |
| `arch` | UA 模板中的架构字段，例如 `arm64` |
| `chromium_version` | `sec-ch-ua` 的 Chromium 主版本 |
| `user_agent_template` | UA 模板 |
| `default_headers_json` | 默认请求头集合，建议保存为有序数组 |
| `header_order_json` | 最终请求头排序优先级 |
| `source` | 最近一次写入来源：`config_seed`、`auto_update`、`manual` |
| `created_at` | 首次创建时间 |
| `updated_at` | 最近更新时间 |

槽位规则：

- `config_seed` 首次写入时使用 `id = 'current'`。
- `auto_update` 更新时覆盖同一行 `id = 'current'`，只更新远端更新源明确提供的字段，例如 `app_version`、`build_number`、`chromium_version`、`source`、`updated_at`。
- `manual` 管理端调整也覆盖同一行 `id = 'current'`。
- `load_current()` 固定执行 `select ... from fingerprints where id = 'current'`，不按 `source` 或 `created_at desc` 猜测当前指纹。
- 项目尚未上线，本轮不保留旧自动更新槽位的兼容路径；实现时直接使用 `CURRENT_FINGERPRINT_ID = 'current'`。
- 本地开发库里如果已有旧自动更新行，可以直接清空 `fingerprints` 或重建 `.runtime` 数据库，不写生产兼容迁移。
- `source` 只是“最近一次写入来源”，不是槽位选择条件。

这样可以回答三个关键问题：

- 首次配置种子写哪一行：写 `id = 'current'`。
- 自动更新写哪一行：覆盖 `id = 'current'`。
- 运行时读哪一行：只读 `id = 'current'`。

### 可选历史表

如果需要保留每次 appcast 检查或手动变更历史，应单独建追加表，不建议继续把历史记录塞进当前槽位表：

```text
fingerprint_update_history
  id
  current_fingerprint_id
  app_version
  build_number
  chromium_version
  source
  manifest_json
  created_at
```

历史表规则：

- `id` 使用 UUID。
- 每次检查到更新可以 insert 一行历史。
- 历史只用于审计和排查，不参与运行时 `load_current()`。
- `insert_update()` 已改写到历史表，不写入当前槽位表。

不推荐的方案：

- 不推荐在同一张 `fingerprints` 表里一会儿用固定 ID 表示当前，一会儿用 UUID 表示历史，再靠 `source = 'runtime' order by created_at desc` 推断当前。这会让 `created_at` / `updated_at` / `source` 三个字段语义互相污染。

`default_headers_json` 建议格式：

```json
[
  {"name": "Accept-Encoding", "value": "gzip, deflate, br, zstd"},
  {"name": "Accept-Language", "value": "en-US,en;q=0.9"},
  {"name": "sec-ch-ua-mobile", "value": "?0"},
  {"name": "sec-ch-ua-platform", "value": "\"macOS\""}
]
```

不用单独建 `fingerprint_headers` 表的理由：

- 请求头集合是小型原子配置。
- 查询不会按单个 header 聚合。
- 保持 JSON 原子读写更简单，也更符合“运行时指纹快照”的语义。

如果后续要做管理端逐项编辑、审计每个 header 的历史，再考虑拆表。

启动流程建议：

1. 读取 `config.yaml` 的 `fingerprint` 默认配置。
2. 连接 SQLite 并初始化当前 schema。由于尚未上线，本轮可以直接更新 `schema.sql` 并重建本地运行库。
3. 调用 `fingerprints.ensure_current_seed(config_fingerprint)`。
4. 如果 `id = 'current'` 不存在，插入 config 默认值，`source = config_seed`。
5. 如果 `id = 'current'` 已存在，不用 config 覆盖数据库。
6. 从 `id = 'current'` 读取完整指纹，构造 `AppState`。
7. 后台自动更新只覆盖 `id = 'current'` 中明确由更新源提供的字段。
8. 请求链路和诊断接口只读运行时加载的数据库指纹。

重要约束：

- `config.yaml` 是首次默认值，不是持续覆盖源。
- 数据库是启动后的事实来源。
- 自动更新不得把 `originator`、`platform`、`arch`、`default_headers_json`、`header_order_json` 重置成代码默认值。
- core 层不应依赖 platform 配置类型。转换应在 runtime 或 adapters 层完成，避免依赖倒置。

### `event_logs`

当前字段：

```text
id, request_id, kind, level, account_id, route, model, status_code,
transport, attempt_index, upstream_status_code, failure_class, response_id, upstream_request_id,
latency_ms, message, metadata_json, created_at
```

判断：基础可用，常用排查字段已从 `metadata_json` 提升为顶层列。

仍需注意：

- `attempt_index` 只有请求重试上下文明示时才会有值；不会凭空生成。
- `upstream_request_id` 需要上游响应头或 metadata 提供；没有来源时保持空。

已补充高频查询字段：

```text
transport
attempt_index
upstream_status_code
failure_class
response_id
upstream_request_id
```

字段来源建议：

| 字段 | 数据来源 |
| --- | --- |
| `transport` | runtime 请求分发结果或上游客户端返回的 transport 标记，例如 `http_sse`、`websocket`、`websocket_fallback_http_sse` |
| `attempt_index` | 账号请求重试循环里的第几次尝试，从 runtime dispatch / retry context 递增产生 |
| `upstream_status_code` | `CodexClientError::Upstream` 或 HTTP response status；成功响应可为空或等于最终 `status_code` |
| `failure_class` | runtime 现有错误分类结果，例如 auth、quota、cloudflare、rate_limit、transport、empty_response、history_recovery 等 |
| `response_id` | 上游 Responses / Chat / WebSocket completed 事件里的 response id；失败前没有 response id 时为空 |
| `upstream_request_id` | 上游响应头中能拿到的 request id，例如 `x-request-id`、`cf-ray` 或 OpenAI 相关 request id；没有则为空 |

实施原则：

- 字段从现有请求上下文、上游响应头、错误类型中提取，不新增模拟值。
- 同一个用户请求的每次账号尝试都应能用同一个 `request_id` 串起来。
- `metadata_json` 继续保存完整上下文，但管理端常用筛选项必须升为顶层列。

保留 `metadata_json`，但它只做补充上下文，不承担主要筛选维度。

### `model_plan_snapshots`

当前字段：

```text
plan_type, models_json, fetched_at
```

判断：简单合理。

建议：

- `plan_type` 作为自然主键可保留。
- 如果后续需要区分来源，可加 `source`，但当前项目只做 OpenAI/Codex，不需要泛化 provider 字段。

### `session_affinities`

当前字段：

```text
response_id
account_id
conversation_id
turn_state
instructions_hash
input_tokens
function_call_ids_json
variant_hash
expires_at
created_at
```

判断：职责清晰。

问题：

- `account_id` 没有在 schema 中声明为 `references accounts(id) on delete cascade`。
- `function_call_ids_json` 的默认值在运行库和 `schema.sql` 表达不一致。
- `turn_state` 如果包含上游续链敏感状态，后续需要确认是否要加密或至少严格控制日志输出。

建议：

- 增加账号外键，账号删除时 cascade 删除 affinity。
- 统一 `function_call_ids_json default '[]'`。
- `conversation_id`、`variant_hash` 可以保留当前命名，它们表达的是本项目内部续链匹配键。

## 指纹配置和数据库持久化方案

用户要求：

- 本次指纹默认值写在 `config.yaml`。
- 启动时持久化到数据库。
- 后续更新只更新数据库。
- 后续运行都从数据库拿。

建议配置结构：

```yaml
fingerprint:
  originator: Codex Desktop
  app_version: 26.519.81530
  build_number: "3178"
  platform: darwin
  arch: arm64
  chromium_version: "146"
  user_agent_template: "Codex Desktop/{version} ({platform}; {arch})"
  default_headers:
    - name: Accept-Encoding
      value: gzip, deflate, br, zstd
    - name: Accept-Language
      value: en-US,en;q=0.9
    - name: sec-ch-ua-mobile
      value: "?0"
    - name: sec-ch-ua-platform
      value: "\"macOS\""
    - name: sec-fetch-site
      value: same-origin
    - name: sec-fetch-mode
      value: cors
    - name: sec-fetch-dest
      value: empty
  header_order:
    - authorization
    - chatgpt-account-id
    - originator
    - x-openai-internal-codex-residency
    - x-client-request-id
    - x-codex-installation-id
    - x-codex-turn-state
    - openai-beta
    - user-agent
    - sec-ch-ua
    - sec-ch-ua-mobile
    - sec-ch-ua-platform
    - accept-encoding
    - accept-language
    - sec-fetch-site
    - sec-fetch-mode
    - sec-fetch-dest
    - content-type
    - accept
    - cookie
```

说明：

- `default_headers` 用数组，不用 map，是为了保留顺序和大小写。
- `header_order` 统一使用小写 header name，因为排序匹配通常按大小写无关处理。
- 配置文件中不放任何 token、cookie 或账号身份。

本轮数据库结构调整：

项目尚未上线，不需要为旧 `fingerprints` 表写兼容迁移。直接更新 `schema.sql` 的最终结构，并让新库包含完整字段即可。

建议最终字段约束：

```text
id text primary key
originator text not null
app_version text not null
build_number text not null
platform text not null
arch text not null
chromium_version text not null
user_agent_template text not null
default_headers_json text not null
header_order_json text not null
source text not null
created_at text not null
updated_at text not null
```

本地已有 `.runtime` 数据库时，处理方式是删除运行库、重建运行库，或只清空并重建 `fingerprints` 表；不写旧槽位到当前槽位的兼容升级逻辑。

已落地仓储 API：

```text
CURRENT_FINGERPRINT_ID = "current"
ensure_current_seed(default_fingerprint) -> Fingerprint
load_current() -> Option<Fingerprint>
update_current_version(app_version, build_number, chromium_version) -> ()
insert_update_history(update) -> ()  # 可选；写 fingerprint_update_history，不参与运行时读取
```

旧自动更新槽位相关 API 已从运行时代码移除。运行时需要的是“当前指纹”，不是“最新自动更新历史”。

## 建议目标结构

如果允许做一次较完整的 schema v2，目标结构如下。

```text
admin_users
admin_sessions
client_api_keys

accounts
account_tokens
account_quota_state
account_recovery_state
account_refresh_leases
account_usage_totals
account_usage_windows
account_cookies

request_fingerprints
event_logs
model_plan_snapshots
session_affinities
schema_migrations
```

本轮已实现这些：

1. 增加 `schema_migrations` 作为未来上线后的迁移纪律；本轮未上线，本地库可以直接重建。
2. 对齐 `schema.sql` 和运行库 drift。
3. 补全 `fingerprints` 字段和 `id = 'current'` 当前槽位加载链路。
4. 解决 `accounts` 上游身份唯一约束和重复导入。
5. 数据库物理列改为 `chatgpt_account_id` / `chatgpt_user_id`；Rust 领域结构暂时通过仓储 alias 过渡。
6. 补齐 `event_logs` 顶层排查字段。

## 实施优先级

### P0：必须先做

- 已完成：新增迁移账本表，停止继续堆 ad hoc 迁移；本轮本地库可直接重建，不写旧结构兼容升级。
- 已完成：直接对齐 `schema.sql` 与运行库 drift。
- 已完成：补全 `fingerprints` 表，使 `id = 'current'` 的完整指纹可以从数据库恢复。
- 已完成：`config.yaml` 增加 `fingerprint` 默认种子。
- 已完成：启动时 seed 数据库，运行时从数据库加载指纹。
- 已完成：账号导入按上游身份更新，数据库增加唯一约束。

### P1：命名和职责边界

- 已完成：数据库物理列使用 `chatgpt_account_id`、`chatgpt_user_id`。
- 已完成：`event_logs` 补结构化排查字段。
- 待做：逐步把 `added_at` 迁移为 `created_at`。
- 待做：明确 `status` 状态机和刷新调度不变量。
- 待做：如后续确实需要，再把 Rust 领域结构字段也改为 `chatgpt_account_id` / `chatgpt_user_id`。

### P2：拆表和长期清洁

- 从 `accounts` 拆出 `account_tokens`、`account_quota_state`、`account_recovery_state`。
- 从 `account_usage` 拆出 totals / windows。
- 统一 `_ciphertext` 命名。
- 评估 `account_cookies.id` 是否保留。

## 不建议做

- 不引入 `provider`、`provider_id`、`proxy_api_key` 之类泛化字段。当前项目只做 OpenAI/Codex。
- 不把配置文件当作每次启动的覆盖源。配置只是首次种子，数据库才是运行时事实来源。
- 不把请求头字段继续硬编码在 core 里再由数据库补一部分。要么完整默认，要么完整落库。
- 不先加唯一索引再清理历史重复数据。否则迁移会直接失败。
- 不把真实 token、cookie、账号邮箱写进文档或测试 fixture。

## 下一步建议

下一步不再补旧库兼容迁移，优先继续做真实链路验证和状态机收敛：

1. 继续用真实账号跑请求链路，验证刷新调度、禁用账号、封禁账号、配额冷却和 websocket/http_sse fallback。
2. 明确 `status` 状态机，把 disabled/banned/expired/refreshing 的启动扫描和后台调度不变量固化。
3. 如管理端长期需要窗口级 reasoning/total，再评估是否把 `account_usage` 拆成 totals/windows。
