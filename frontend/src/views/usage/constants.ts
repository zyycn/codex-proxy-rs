import type { UsageDisplayRecord } from './utils/records'
import type { getOpsErrors } from '@/api'
import { defineTableColumns } from '@/components/base/BaseTable/columns'
import { formatProvider } from './utils/format'

type OpsErrorRow = Awaited<ReturnType<typeof getOpsErrors>>['items'][number]

export const usageRecordColumns = defineTableColumns<UsageDisplayRecord>([
  {
    key: 'accountEmail',
    label: '账号',
    kind: 'identity',
    size: '3xl',
  },
  {
    key: 'provider',
    label: '平台/类型',
    kind: 'status',
    size: 'sm',
    format: (value: unknown) => formatProvider(typeof value === 'string' ? value : null),
  },
  { key: 'model', label: '模型', kind: 'custom', size: 'xl' },
  { key: 'reasoningEffort', label: '推理强度', kind: 'status' },
  { key: 'route', label: '端点', kind: 'mono' },
  { key: 'recordType', label: '类型', kind: 'status', size: 'sm' },
  { key: 'tokenDetails', label: 'TOKEN', kind: 'numeric', size: 'xl' },
  { key: 'billing', label: '费用', kind: 'numeric', size: 'lg' },
  { key: 'latency', label: '延迟', kind: 'numeric', size: 'xl' },
  { key: 'createdAtDisplay', label: '时间', kind: 'datetime' },
  { key: 'clientIp', label: 'IP', kind: 'custom', size: '3xl' },
  { key: 'userAgent', label: 'User-Agent', kind: 'custom', size: '4xl' },
  { key: 'actions', label: '操作', kind: 'actions', size: 'sm' },
])

export const opsErrorColumns = defineTableColumns<OpsErrorRow>([
  { key: 'accountId', label: '账号', kind: 'mono', size: '3xl' },
  { key: 'createdAtDisplay', label: '时间', kind: 'datetime' },
  { key: 'upstreamStatusCode', label: '上游状态', kind: 'status', size: 'sm' },
  { key: 'clientStatusCode', label: '客户端状态', kind: 'status', size: 'md' },
  { key: 'failureClass', label: '失败分类', kind: 'mono' },
  { key: 'kind', label: '事件', kind: 'mono' },
  { key: 'route', label: '端点', kind: 'mono' },
  { key: 'model', label: '模型', kind: 'mono' },
  { key: 'requestId', label: '请求 ID', kind: 'mono', size: '2xl' },
  { key: 'message', label: '错误摘要', kind: 'custom', size: '3xl' },
  { key: 'actions', label: '操作', kind: 'actions', size: 'sm' },
])

export const usageTimeRangeOptions = [
  { label: '今天', value: 'today' },
  { label: '最近 7 天', value: '7d' },
  { label: '最近 30 天', value: '30d' },
]
