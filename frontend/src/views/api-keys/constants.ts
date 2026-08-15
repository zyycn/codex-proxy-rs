import type { getApiKeys } from '@/api'
import { defineTableColumns } from '@/components/base/BaseTable/columns'

type ApiKeyRow = Awaited<ReturnType<typeof getApiKeys>>['items'][number] & {
  createdAtDisplay: string
}

export const apiKeyColumns = defineTableColumns<ApiKeyRow>([
  { key: 'selection', kind: 'selection' },
  { key: 'identity', label: '名称', kind: 'identity', size: 'xl', sortable: 'name' },
  { key: 'prefix', label: '密钥前缀', kind: 'mono', size: '2xl' },
  { key: 'enabled', label: '状态', kind: 'status', sortable: true },
  { key: 'scope', label: '分组', kind: 'status' },
  { key: 'createdAtDisplay', label: '创建时间', kind: 'datetime', sortable: 'createdAt' },
  {
    key: 'lastUsedAt',
    label: '最后使用',
    kind: 'datetime',
    sortable: true,
    emptyText: '',
  },
  { key: 'actions', label: '操作', kind: 'actions', size: 'xl' },
])
