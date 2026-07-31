# Provider 账号观测快照方案

- 状态：已实现（迁移、Store/API/前端与集成测试完成）
- 日期：2026-07-31

## 1. 背景

请求明细当前只在 `model_requests` 中保存稳定账号引用 `provider_account_ref`，账号名称、
邮箱和认证类型则在查询时实时关联 `provider_accounts`。账号删除后，外键列
`provider_account_id` 被置空，实时关联失效，页面只能显示保留下来的 `acct_...` 引用。

另外两条观测链路也存在同类问题：

- 热点诊断的账号维度直接使用 `provider_account_ref` 作为分组名称，因此始终显示账号 ID；
- 错误明细虽然已经预留 `metadata.accountLabel`，但后端当前固定返回空值。

账号 ID 是稳定的排查、筛选和关联事实，不能用邮箱替换。名称、邮箱以及平台/类型属于请求
发生时的显示事实，应作为历史快照独立保存，不能依赖账号当前是否仍然存在。

## 2. 目标

1. 请求、attempt 和错误事件在账号删除后仍能显示请求发生时的账号信息。
2. 请求明细、热点诊断和错误明细统一采用“邮箱、名称、账号 ID”的显示优先级。
3. 平台/类型在账号删除后保持可解释：平台为 Provider，类型为认证方式。
4. 聚合、筛选和审计继续使用稳定账号 ID，不因同邮箱账号或账号改名而改变语义。
5. 不增加上游请求，不增加前端补查账号接口，不让显示需求进入调度与 Provider 逻辑。

## 3. 非目标

- 不把 `provider_account_ref` 改成邮箱或名称。
- 不根据历史请求反向恢复已经删除且没有任何可用资料的旧账号。
- 不在账号改名或换邮箱后批量改写既有请求快照。
- 不改变账号选择、亲和、额度、冷却或凭据刷新逻辑。

## 4. 快照契约

### 4.1 稳定标识与显示快照分离

| 事实 | 持久化字段 | 语义 |
| --- | --- | --- |
| 账号稳定引用 | `provider_account_ref` | 永久保存请求发生时使用的本地账号 ID，用于筛选、关联和审计 |
| 账号实时外键 | `provider_account_id` | 账号存在时指向 `provider_accounts.id`，账号删除时由外键置空 |
| 账号名称快照 | `provider_account_name_snapshot` | 请求或事件写入时的账号名称 |
| 账号邮箱快照 | `provider_account_email_snapshot` | 请求或事件写入时的账号邮箱 |
| 平台快照 | `provider_kind` | 请求或事件已经持久化的 Provider kind，例如 `openai`、`xai` |
| 类型快照 | `provider_account_authentication_kind_snapshot` | 请求或事件写入时的认证类型，例如 OAuth 或 API Key |

`provider_kind` 本身已经是请求和事件的历史事实，因此不再增加语义重复的
`provider_kind_snapshot` 列。实现和测试必须把它明确视为平台快照，读取时不得通过当前账号
重新推导。

### 4.2 表结构

`model_requests` 新增：

```sql
provider_account_name_snapshot text,
provider_account_email_snapshot text,
provider_account_authentication_kind_snapshot text
```

`ops_events` 新增同样三个字段。`ops_events.provider_kind` 继续承担平台快照；每条 attempt
错误必须保存该 attempt 实际使用账号的快照，不能复用请求最终账号的资料。

这些列保持可空，原因包括：请求可能尚未路由、历史账号可能已经删除、探测事件可能缺少完整
账号资料。新写入路径在存在 `provider_account_ref` 时应尽力写全快照，但数据库不能用
`NOT NULL` 阻止历史数据迁移。

## 5. 写入方案

快照由 PostgreSQL Store 在现有写入语句内按 `provider_account_id` 主键读取
`provider_accounts`，与请求或事件原子写入。Core 继续只传递稳定账号 ID，不把名称、邮箱等
展示资料扩散到调度领域。

需要覆盖的写入边界：

1. `insert_model_request_with_first_attempt`：首次 attempt 与请求行合并写入时保存快照。
2. `begin_model_request_attempt`：内部换号时更新请求行，使请求终态对应最终 attempt 的账号快照。
3. `append_ops_event`：保存事件所属 attempt 或账号探测使用的独立快照。

普通的 `insert_model_request` 尚未选出账号，不写快照；后续首次
`begin_model_request_attempt` 负责补齐。账号资料后续发生修改时，不回写历史快照。

主键读取合并在现有 PostgreSQL 语句中，不产生上游请求或前端额外请求。该读取只访问
`provider_accounts.id` 主键，性能成本相对现有观测写入可忽略，并继续服从现有异步、有界
观测队列语义。

## 6. 读取与展示方案

### 6.1 统一显示优先级

所有页面统一使用：

```text
非空邮箱 > 非空账号名称 > provider_account_ref > “—”
```

平台/类型统一使用：

```text
provider_kind + provider_account_authentication_kind_snapshot
```

原始 `accountId` 继续保留在 API 和详情中，不能因展示邮箱而丢失。

### 6.2 请求明细

- `UsageRecord` 直接读取名称、邮箱和认证类型快照，不再依赖实时账号关联；
- API 在现有 `accountId`、`accountEmail` 基础上增加 `accountName`；
- `authenticationKind` 改为读取类型快照，保证账号删除后平台/类型图标仍然正确；
- attempt API 使用对应 `ops_events` 快照填充 `credentialName`、`accountEmail` 和
  `authenticationKind`；
- 请求详情主要位置显示统一账号名称，深层字段仍保留“账号 ID”。

### 6.3 热点诊断

账号热点必须按 `provider_account_ref` 聚合，显示名称单独计算，不能直接按邮箱分组。否则两个
使用相同邮箱的本地账号会被错误合并。

诊断响应增加稳定 `key`：账号维度使用账号 ID，其他维度使用原始维度值。`name` 作为显示值，
账号维度从该账号最新的非空历史快照中选择邮箱或名称，最终回退账号 ID。费用聚合也按同一个
`key` 合并，避免名称变化导致请求数与费用错位。前端表格以 `key` 作为行键，显示 `name`。

### 6.4 错误明细

- request 级错误读取 `model_requests` 快照；
- attempt 和探测错误读取 `ops_events` 自身快照；
- API 保留 `accountId`，并填充现有 `metadata.accountLabel`；
- API 同时提供平台和认证类型快照，供详情或后续表格展示；
- 表格列由“账号 ID”改为“账号”，显示统一账号名称；详情同时显示“账号”和“账号 ID”。

## 7. 数据库迁移

`0001_initial.sql` 已在 3.0.30 基线中重新冻结。本功能属于之后的新 schema 变更，应新增：

```text
0002_snapshot_provider_account_identity.sql
```

迁移步骤：

1. 为 `model_requests` 和 `ops_events` 增加三个可空快照字段；
2. 对仍能通过 `provider_account_ref = provider_accounts.id` 找到账号的历史行回填快照；
3. 已删除账号的历史行保持空快照，读取时回退 `provider_account_ref`；
4. 将新迁移哈希追加到 `.frozen-sha256`，迁移文件合入后按字节冻结；
5. 更新迁移集成测试的迁移数量和最终 schema 断言。

迁移无法恢复在执行迁移前已经删除的账号名称、邮箱或认证类型，这是现有数据缺失造成的明确
边界。部署后的新请求和迁移时仍存在账号的历史请求不受此限制。

## 8. 删除与隐私语义

账号删除只删除可调度账号和凭据，不再删除请求日志中的历史名称、邮箱、平台和类型快照。
这些字段属于管理员可见的历史审计信息，生命周期跟随 `model_requests`、`ops_events` 的保留与
清理策略。

这也意味着“删除账号”不是历史邮箱的数据擦除操作。若未来需要满足单独的隐私擦除要求，应
增加显式的历史快照脱敏操作，不能复用普通账号删除接口隐式完成。

## 9. 测试方案

### 9.1 PostgreSQL 集成测试

1. 创建账号并写入请求，删除账号后查询请求明细，名称、邮箱、平台和类型仍保持原值。
2. 同一请求依次使用账号 A、账号 B，两个 attempt 分别保留自己的账号快照。
3. 错误事件和账号探测事件在账号删除后仍返回对应快照。
4. 无邮箱时回退账号名称；名称和邮箱都缺失时回退账号 ID。
5. 两个相同邮箱的账号在热点诊断中仍是两个稳定分组。
6. 账号改名或更换邮箱不改写已经完成请求的历史快照。
7. 迁移可以为仍存在账号的旧请求和事件完成回填。
8. 全新数据库执行 `0001`、`0002` 后二次启动，迁移记录和最终 schema 均正确。

### 9.2 API 契约测试

1. 请求列表和详情稳定输出 `accountId`、`accountName`、`accountEmail`、`provider`、
   `authenticationKind`。
2. attempt 输出自身的账号名称、邮箱和认证类型，不继承最终 attempt。
3. 错误明细同时输出原始 `accountId` 与显示用 `accountLabel`。
4. 诊断响应同时输出稳定 `key` 与显示 `name`。

### 9.3 前端验收

1. 请求明细列表和弹窗优先显示邮箱，账号删除后刷新页面仍不退化为 ID。
2. 热点诊断的账号页签显示邮箱或名称，不显示 `acct_...`，同邮箱账号不被合并。
3. 错误明细表格显示邮箱或名称，详情中仍可查看原始账号 ID。
4. 账号删除后，请求明细的平台/类型图标与删除前一致。
5. 长邮箱继续使用截断与完整 title，不改变表格布局。

## 10. 实施顺序与验收标准

建议按以下顺序实施：

1. 新增迁移和 Store 写入快照；
2. 完成 PostgreSQL 删除后查询回归测试；
3. 扩展 Store、Admin domain 与 API wire；
4. 调整三个前端展示入口；
5. 运行带真实 PostgreSQL 的 gateway-store 测试、gateway-admin/gateway-api 测试、workspace
   Clippy 与构建、前端 lint 与 build；
6. 使用全新数据库启动项目，创建请求与错误事件，删除账号后通过三个页面实测。

验收完成必须同时满足：历史显示不依赖实时账号表、稳定账号 ID 未被显示字段替代、多 attempt
账号不串号、平台/类型删除后不丢失、迁移前已删除账号的不可恢复边界被明确保留。
