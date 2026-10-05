import type { UsageListRecord } from '@/api'
import { defineTableColumns } from '@codex-proxy/ui'
import { formatProviderLabel } from '@/utils/providers'

export const usageRecordColumns = defineTableColumns<UsageListRecord>([
  { key: 'clientApiKeyName', label: '密钥名称', kind: 'identity', size: 'xl' },
  {
    key: 'accountEmail',
    hideable: false,
    label: '账号',
    kind: 'identity',
    size: '3xl',
  },
  { key: 'accountPlanType', label: '订阅', kind: 'status', size: 'sm', fixedWidth: true },
  {
    key: 'provider',
    label: '平台/类型',
    kind: 'status',
    size: 'md',
    fixedWidth: true,
    format: (value: unknown) => formatProviderLabel(typeof value === 'string' ? value : null),
  },
  { key: 'model', label: '模型', kind: 'custom', size: 'xl' },
  { key: 'reasoningEffort', label: '推理强度', kind: 'status', size: 'md', align: 'left' },
  { key: 'route', label: '端点', kind: 'mono' },
  { key: 'upstreamTransport', label: '上游', kind: 'status', size: 'md' },
  { key: 'clientTransport', label: '接入', kind: 'status', size: 'md' },
  { key: 'tokenDetails', label: 'TOKEN', kind: 'numeric', size: 'xl' },
  { key: 'billing', label: '费用', kind: 'numeric', size: 'xl' },
  { key: 'latency', label: '延迟', kind: 'numeric', size: 'xl' },
  { key: 'createdAtDisplay', label: '时间', kind: 'datetime' },
  { key: 'clientIp', label: 'IP', kind: 'custom', size: '3xl' },
  { key: 'userAgent', label: 'User-Agent', kind: 'custom', size: '4xl' },
  { key: 'actions', label: '操作', kind: 'actions', size: 'sm', hideable: false },
])
