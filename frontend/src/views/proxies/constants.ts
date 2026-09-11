import type { OutboundProxy } from '@/api'
import { defineTableColumns } from '@/components/base/BaseTable/columns'

export const proxyColumns = defineTableColumns<OutboundProxy>([
  { key: 'identity', label: '代理', kind: 'identity' },
  { key: 'endpoint', label: '地址', kind: 'custom', size: 'lg' },
  { key: 'accountCount', label: '使用账号数', kind: 'custom' },
  { key: 'test', label: '连通性', kind: 'custom', size: 'md' },
  { key: 'updatedAtDisplay', label: '更新时间', kind: 'datetime' },
  { key: 'actions', label: '操作', kind: 'actions', size: 'lg' },
])
