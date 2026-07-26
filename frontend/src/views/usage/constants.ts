import { formatProvider } from './utils/format'

export const usageRecordColumns = [
  {
    key: 'accountEmail',
    label: '账号',
    width: '280px',
    ellipsis: true,
  },
  {
    key: 'provider',
    label: '平台/类型',
    width: '100px',
    align: 'center' as const,
    ellipsis: false,
    format: (value: unknown) => formatProvider(typeof value === 'string' ? value : null),
  },
  {
    key: 'model',
    label: '模型',
    width: '160px',
    ellipsis: false,
  },
  {
    key: 'reasoningEffort',
    label: '推理强度',
    width: '98px',
    ellipsis: false,
    cellClass: 'whitespace-nowrap text-[12px] font-bold text-(--cp-text-primary)',
  },
  {
    key: 'route',
    label: '端点',
    width: '185px',
    ellipsis: false,
    cellClass: 'font-mono text-[12px] font-emphasis',
  },
  {
    key: 'recordType',
    label: '类型',
    width: '78px',
    align: 'center' as const,
    ellipsis: false,
  },
  {
    key: 'tokenDetails',
    label: 'TOKEN',
    width: '184px',
    align: 'right' as const,
    ellipsis: false,
  },
  {
    key: 'billing',
    label: '费用',
    width: '132px',
    align: 'right' as const,
    ellipsis: false,
  },
  {
    key: 'latency',
    label: '延迟',
    width: '156px',
    align: 'right' as const,
    ellipsis: false,
  },
  {
    key: 'createdAtDisplay',
    label: '时间',
    width: '190px',
    ellipsis: false,
    cellClass:
      'whitespace-nowrap font-mono text-[12px] font-emphasis tabular-nums text-(--cp-text-secondary)',
  },
  {
    key: 'clientIp',
    label: 'IP',
    width: '240px',
    ellipsis: false,
  },
  {
    key: 'userAgent',
    label: 'User-Agent',
    width: '340px',
    ellipsis: false,
    cellClass:
      'whitespace-normal break-words text-[12px] leading-[1.45] font-emphasis text-(--cp-text-secondary)',
  },
  {
    key: 'actions',
    label: '操作',
    width: '92px',
    ellipsis: false,
    headerClass: '!px-4',
    cellClass: '!px-4',
  },
]

export const opsErrorColumns = [
  {
    key: 'createdAtDisplay',
    label: '时间',
    width: '190px',
    cellClass:
      'whitespace-nowrap font-mono text-[12px] font-emphasis tabular-nums text-(--cp-text-secondary)',
  },
  { key: 'upstreamStatusCode', label: '上游状态', width: '96px', align: 'center' as const },
  {
    key: 'failureClass',
    label: '失败分类',
    width: '170px',
    cellClass: 'font-mono text-[12px] font-emphasis',
  },
  {
    key: 'kind',
    label: '事件',
    width: '170px',
    cellClass: 'font-mono text-[12px] font-emphasis',
  },
  {
    key: 'route',
    label: '端点',
    width: '190px',
    cellClass: 'font-mono text-[12px] font-emphasis',
  },
  {
    key: 'model',
    label: '模型',
    width: '180px',
    cellClass: 'font-mono text-[12px] font-emphasis',
  },
  {
    key: 'accountId',
    label: '账号 ID',
    width: '230px',
    cellClass: 'font-mono text-[12px] font-emphasis',
  },
  {
    key: 'requestId',
    label: '请求 ID',
    width: '250px',
    cellClass: 'font-mono text-[12px] font-emphasis',
  },
  { key: 'message', label: '消息', minWidth: '300px', flex: 1 },
  {
    key: 'actions',
    label: '操作',
    width: '80px',
    headerClass: '!px-4',
    cellClass: '!px-4',
  },
]

export const usageTimeRangeOptions = [
  { label: '今天', value: 'today' },
  { label: '最近 7 天', value: '7d' },
  { label: '最近 30 天', value: '30d' },
]
