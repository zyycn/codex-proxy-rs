# 用量维护与计价

`call::services::usage` 提供请求明细、聚合统计和 Token 费用试算。插件通过 `call.host.service::<Operation>(input)` 调用，沿用公开服务的权限与调用期限

## 基础操作

| SDK 操作 | 服务名 | 输入与结果 |
| --- | --- | --- |
| `GetRecord` | `usage.records.get` | 按 `id` 读取原生请求明细，包含费用快照与 Provider 观测 |
| `ListRecords` | `usage.records.list` | 按时间、Provider、模型、账号、Key 筛选，返回明细与下一页游标 |
| `WriteRecords` | `usage.records.write` | 批量 `put`、`update`、`delete`，返回实际变更的 ID |
| `ListAggregates` | `usage.aggregates.list` | 分页读取调用方维护的聚合贡献，支持同样的时间与维度筛选 |
| `WriteAggregates` | `usage.aggregates.write` | 按聚合键批量覆盖或删除统计贡献 |
| `EvaluatePricing` | `pricing.evaluate` | 根据当前价目或指定价格试算，逐项返回 `estimated` 或 `not_estimable` |

原生总量与趋势通过既有统计查询读取。明细列表只列请求记录，聚合贡献参与统计但不生成请求明细

## 明细维护

- `put` 使用调用方提供的稳定 `id` 创建记录，已有 ID 则覆盖合同中的明细字段
- `update` 按 ID 替换全部 Token 字段或费用；未提供的部分保持原值，目标不存在返回 `not_found`
- `delete` 删除指定 ID；目标不存在时成功，且不加入 `changed_ids`
- 写入对象必须是已结束的请求，不能修改或删除仍在运行的请求
- `put` 和 `update` 清除原计价明细快照，后续读取使用写入的 Token 与费用

`UsageTokens` 中的缓存 Token 包含在输入 Token 中，推理 Token 包含在输出 Token 中。`null` 表示未知，`"0"` 表示已知为零；调用方负责从外部系统转换口径，宿主不核对各项业务关系

费用为 `unavailable` 或 `known { source, amount, currency }`。零费用属于已知费用；清空费用使用 `unavailable`。明细维护影响原生历史统计，不重新执行请求结算或修改 Key 预算账本

## 聚合维护

`UsageAggregateKey` 由 `bucket_start_ms`、`dimensions` 和调用方的 `key` 共同构成。相同完整键的 `put` 覆盖旧值，重复提交相同内容不会累计；改变时间、维度或 `key` 会形成另一个目标，需要移动时由调用方删除旧键

`dimensions` 支持 Provider、模型、账号、Key、操作和传输类型。`counts` 的键由 `UsageMetric` 定义，包括请求次数、各终态次数、已完成次数和各项 Token；`costs` 按币种保存十进制金额。计数与费用允许负值，供调用方表达统计修正

聚合按 `bucket_start_ms` 这个归属时间纳入 `[start_ms, end_ms)` 查询及趋势时间桶，宿主不自动摊分。导入日汇总时，调用方选择该日的归属时间；需要小时分布时，调用方自行提供小时数据

查询只按实际提供的维度匹配，缺少的维度不会自动补全。未提供某个维度的贡献参与总量，但不进入该维度的分组或筛选；分组与总量是否对齐由调用方提供的数据决定。请求 ID、响应 ID、状态码、请求结果及文字搜索等明细筛选不包含聚合贡献；聚合也不生成延迟样本、尝试次数或请求列表页数。原生统计的无符号计数与非负金额若因贡献合计超出表示范围，查询返回错误，由调用方修正输入

聚合贡献参与原生总量、趋势、维度统计和账号用量，统计重建保留这些数据。贡献由调用方删除，不随请求历史保留任务清理；明细沿用请求历史保留策略

## 费用试算与写回

`EvaluatePricing` 使用 Provider 的原生单请求 Token 计价公式，支持当前价目和调用方指定的价格覆盖。OpenAI 与 xAI 提供此能力；不支持的 Provider、缺少必要 Token、图像用量或无法确定的价目返回 `not_estimable`

试算输入需要明确输入、输出、缓存读取、缓存写入以及图像 Token；纯文本用量的图像 Token 填零。结果只包含 Token 费用，不自动处理图像、工具或其他附加费用，也不写入历史

调用方可查询明细、选择目标、逐条试算，再通过 `WriteRecords::Update` 写回费用。补全缺失时只处理 `unavailable`；全部覆盖时自行选择整个范围。日汇总若缺少逐请求分布，长上下文档位和逐请求舍入可能无法复现，计算方法由调用方决定

## 批量、错误与重试

- 每次写入或试算接受 1～500 项，编码后的输入不超过 2 MiB
- 查询每页默认 100 条，最多 200 条；明细页累计内容超过 2 MiB 时需减小页大小
- 时间为 UTC 毫秒，查询范围为左闭右开；游标不透明，仅用于相同筛选条件的后续页，分页不冻结跨调用快照
- `UsageCount`、`UsageDelta` 使用十进制字符串，避免 JavaScript 丢失整数精度；持久化数值受 PostgreSQL bigint 与费用精度范围限制
- 单次写入按输入顺序在一个事务中执行，任一项失败则整批回滚；跨批次进度由调用方保存
- 存储操作共享宿主观测连接与执行预算，超时、锁等待或取消会结束该次执行并释放资源
- 错误类型包括 `invalid_input`、`not_found`、`conflict`、`capacity_exceeded`、`unavailable`

响应丢失时，调用方按稳定 ID 或聚合键查询、重试覆盖。宿主不保存任务、来源登记、幂等回执、调整历史或撤销计划；调用方负责来源映射、业务去重、重试策略、并发写入协调和恢复。服务输入不能指定审计主体，宿主从当前调用身份生成普通变更审计
