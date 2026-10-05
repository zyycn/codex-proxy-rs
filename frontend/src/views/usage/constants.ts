import type { OpsError } from '@/api'
import { defineTableColumns } from '@codex-proxy/ui'

export const opsErrorColumns = defineTableColumns<OpsError>([
  { key: 'clientApiKeyName', label: '密钥名称', kind: 'identity', size: 'xl' },
  { key: 'accountId', label: '账号', kind: 'identity', size: '3xl', emptyText: '未知账号', hideable: false },
  { key: 'accountPlanType', label: '订阅', kind: 'status', size: 'sm', fixedWidth: true },
  { key: 'provider', label: '平台/类型', kind: 'custom', size: 'sm' },
  { key: 'message', label: '错误', kind: 'custom', size: '4xl', hideable: false },
  { key: 'upstreamSendState', label: '发送状态', kind: 'custom', size: 'xl' },
  { key: 'model', label: '模型', kind: 'custom', size: 'xl', emptyText: '未记录模型' },
  { key: 'route', label: '端点', kind: 'mono', size: 'xl', emptyText: '未记录' },
  { key: 'createdAtDisplay', label: '时间', kind: 'datetime' },
  { key: 'requestId', label: '请求 ID', kind: 'mono', size: '2xl', emptyText: '未记录' },
  { key: 'clientIp', label: 'IP', kind: 'custom', size: '3xl', emptyText: '未记录' },
  { key: 'userAgent', label: 'User-Agent', kind: 'custom', size: '4xl', emptyText: '未记录' },
  { key: 'actions', label: '操作', kind: 'actions', size: 'sm', hideable: false },
])

export const usageTimeRangeOptions = [
  { label: '今天', value: 'today' },
  { label: '最近 7 天', value: '7d' },
  { label: '最近 30 天', value: '30d' },
]
