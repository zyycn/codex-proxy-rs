import type { UsageErrorRecord } from './errors'
import type { UsageDisplayRecord } from '@/views/usage/model/records'
import { defineTableColumns } from '@/components/base/BaseTable/columns'
import { formatProvider } from '@/views/usage/model/format'

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
  { key: 'reasoningEffort', label: '推理强度', kind: 'status', size: 'lg' },
  { key: 'route', label: '端点', kind: 'mono' },
  { key: 'upstreamTransport', label: '上游', kind: 'status', size: 'md' },
  { key: 'clientTransport', label: '接入', kind: 'status', size: 'md' },
  { key: 'tokenDetails', label: 'TOKEN', kind: 'numeric', size: 'xl' },
  { key: 'billing', label: '费用', kind: 'numeric', size: 'lg' },
  { key: 'latency', label: '延迟', kind: 'numeric', size: 'xl' },
  { key: 'createdAtDisplay', label: '时间', kind: 'datetime' },
  { key: 'clientIp', label: 'IP', kind: 'custom', size: '3xl' },
  { key: 'userAgent', label: 'User-Agent', kind: 'custom', size: '4xl' },
  { key: 'actions', label: '操作', kind: 'actions', size: 'sm' },
])

export const opsErrorColumns = defineTableColumns<UsageErrorRecord>([
  { key: 'accountId', label: '账号', kind: 'identity', size: '3xl', emptyText: '未知账号' },
  { key: 'provider', label: '平台/类型', kind: 'custom', size: 'sm' },
  { key: 'summary', label: '错误', kind: 'custom', size: '4xl' },
  { key: 'upstreamSendState', label: '发送状态', kind: 'custom', size: 'xl' },
  { key: 'model', label: '模型', kind: 'custom', size: 'xl', emptyText: '未记录模型' },
  { key: 'route', label: '端点', kind: 'mono', size: 'xl', emptyText: '未记录' },
  { key: 'createdAtDisplay', label: '时间', kind: 'datetime' },
  { key: 'requestId', label: '请求 ID', kind: 'mono', size: '2xl', emptyText: '未记录' },
  { key: 'clientIp', label: 'IP', kind: 'custom', size: '3xl', emptyText: '未记录' },
  { key: 'userAgent', label: 'User-Agent', kind: 'custom', size: '4xl', emptyText: '未记录' },
  { key: 'actions', label: '操作', kind: 'actions', size: 'sm' },
])

export const keyOpsErrorColumns = defineTableColumns<UsageErrorRecord>([
  { key: 'summary', label: '错误', kind: 'custom', size: '4xl' },
  { key: 'upstreamSendState', label: '发送状态', kind: 'custom', size: 'xl' },
  { key: 'model', label: '模型', kind: 'custom', size: 'xl', emptyText: '未记录模型' },
  { key: 'route', label: '端点', kind: 'mono', size: 'xl', emptyText: '未记录' },
  { key: 'createdAtDisplay', label: '时间', kind: 'datetime' },
  { key: 'requestId', label: '请求 ID', kind: 'mono', size: '2xl', emptyText: '未记录' },
  { key: 'clientIp', label: 'IP', kind: 'custom', size: '3xl', emptyText: '未记录' },
  { key: 'userAgent', label: 'User-Agent', kind: 'custom', size: '4xl', emptyText: '未记录' },
])

// Key 表格从列合同上排除账号、平台、认证类型和详情入口。
export const keyUsageRecordColumns = defineTableColumns<UsageDisplayRecord>([
  { key: 'model', label: '模型', kind: 'custom', size: 'xl' },
  { key: 'reasoningEffort', label: '推理强度', kind: 'status', size: 'lg' },
  { key: 'route', label: '端点', kind: 'mono' },
  { key: 'upstreamTransport', label: '上游', kind: 'status', size: 'md' },
  { key: 'clientTransport', label: '接入', kind: 'status', size: 'md' },
  { key: 'tokenDetails', label: 'TOKEN', kind: 'numeric', size: 'xl' },
  { key: 'billing', label: '费用', kind: 'numeric', size: 'lg' },
  { key: 'latency', label: '延迟', kind: 'numeric', size: 'xl' },
  { key: 'createdAtDisplay', label: '时间', kind: 'datetime' },
  { key: 'clientIp', label: 'IP', kind: 'custom', size: '3xl' },
  { key: 'userAgent', label: 'User-Agent', kind: 'custom', size: '4xl' },
])

export const keyDiagnosticDimensionOptions = [
  { label: '模型', value: 'model' },
  { label: '传输', value: 'transport' },
  { label: '错误', value: 'failureClass' },
]
