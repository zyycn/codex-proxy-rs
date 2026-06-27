# Frontend Optimization Plan

## 目标

前端优化先解决结构坏味道，再处理局部实现细节。核心目标是让页面入口保持清晰，让业务流程、表格配置、弹窗和局部展示各自有稳定位置。

这份目录是第一批目标结构，不代表一次性创建所有空文件。只有当代码真实迁移进去时，才新增对应文件，避免为了目录完整而制造空壳。

## 原则

- 页面出口统一使用 `index.vue`。
- 大页面先拆业务流程，再拆展示组件。
- `constants.ts` 只放稳定配置，例如表格列、固定选项、状态映射。
- `composables/` 放页面内可复用的状态与副作用流程，例如筛选、分页、请求、批量操作。
- `components/` 放页面私有展示块或弹窗，不放跨页面基础组件。
- 不为了抽象而抽象。组件提取必须减少页面职责或消除真实重复。
- API 模块继续保持轻量，避免为每个接口参数声明大量类型。
- 日期时间统一使用 `dayjs` 封装后的工具，不在页面里手写格式化。

## 第一批目标目录

```text
web/src/views/accounts/
  index.vue
  constants.ts
  composables/
    useAccountConnectionTest.ts
    useAccountFilters.ts
    useAccountMutations.ts
    useAccountsTable.ts
  components/
    AccountOverviewCards.vue
    AccountIdentityCell.vue
    AccountStatusBadge.vue
    AccountCreateModal.vue
    AccountEditModal.vue
    AccountConnectionTestModal.vue
    AccountQuotaPanel.vue
    AccountQuotaSummaryCell.vue
    AccountTableActions.vue
    AccountUsagePanel.vue

web/src/views/api-keys/
  index.vue
  constants.ts
  composables/
    useApiKeyFilters.ts
    useApiKeyMutations.ts
    useApiKeysTable.ts
  components/
    ApiKeyFilters.vue
    ApiKeyCreateModal.vue
    ApiKeyIdentityCell.vue
    ApiKeyPrefixCell.vue
    ApiKeyStatusToggle.vue

web/src/views/logs/
  index.vue
  constants.ts
  composables/
    useLogFilters.ts
    useLogDetail.ts
    useLogsTable.ts
  components/
    LogDetailModal.vue
    LogFilters.vue
    LogLevelBadge.vue
    LogStatusCodeBadge.vue

web/src/components/base/BaseTable/
  index.vue
  columns.ts
  pagination.ts
```

## 落地顺序

1. `accounts`

先处理最大页面。连接测试、筛选分页、账号变更请求、表格配置分别收口。弹窗和展开内容在逻辑稳定后再拆组件，避免过早组件化。

2. `api-keys`

拆出表格列、筛选分页、创建/删除/状态/标签更新流程。弹窗组件只在页面脚本明显变轻后再迁移。

3. `logs`

事件日志先整理筛选、表格和详情弹窗。详情弹窗里的元数据滚动与展示规则需要保持当前设计系统一致。

4. `BaseTable`

最后处理基础表格。先抽列归一化和分页逻辑，再决定是否把 `BaseTable.vue` 迁移为目录入口 `BaseTable/index.vue`。基础组件变更影响面大，必须在页面侧坏味道收敛后再动。

## 当前进度

第一批已完成真实迁移：

- `accounts` 已拆出连接测试、筛选分页、表格状态、账号变更流程、创建/编辑/测试弹窗、额度面板和用量面板。
- `api-keys` 已拆出筛选分页、表格选择、密钥变更流程、表格列配置和创建弹窗。
- `logs` 已拆出筛选区、详情弹窗、日志加载/刷新/清空流程、详情状态和表格列配置。
- `components/base/BaseTable` 已迁移为目录入口，列解析与分页逻辑分别收口到 `columns.ts` 和 `pagination.ts`。
- `utils/date.ts` 已落地，日期时间格式化统一走工具函数。

已完成的结构修正：

- 筛选分页 composable 显式接收 `total` ref，不通过闭包读取外部总数。
- 表格选择状态统一使用替换 `Set` 的方式更新，避免依赖原地 mutation。
- 弹窗表单组件使用字段级 computed setter，避免子组件直接改父级表单对象的嵌套字段。
- 日志列表命名回到 `logs`，避免把后端筛选结果误导为前端二次筛选。
- 账号概览卡片、表格身份、额度摘要和行操作已拆为页面私有展示组件，让 `accounts/index.vue` 继续收敛为页面编排层。
- 连接测试弹窗复用账号身份展示组件，不再维护第二套头像、标题和副标题展示逻辑。
- 账号状态展示已收敛为页面私有 badge 组件，表格与连接测试弹窗共享同一套状态文案和色值。
- API Key 筛选与顶部操作区已拆为页面私有组件，`api-keys/index.vue` 只保留状态接线和表格编排。
- API Key 名称/标签编辑和完整密钥复制已拆为页面私有单元组件，标签编辑 composable 不再接收 DOM 事件。
- API Key 启用状态切换已拆为页面私有单元组件，入口页不再直接维护状态按钮样式。
- 日志级别与状态码展示已拆为页面私有 badge 组件，列表和详情弹窗共享同一套展示规则。
- 账号表格不再保留 `filteredAccounts` 这类无意义身份别名，页面直接使用后端返回的 `accounts`。
- 筛选 composable 不再向页面暴露未消费的内部派生状态，保持对外 API 更小。

后续继续优化时，必须按本文件的目标目录和落地顺序推进。
