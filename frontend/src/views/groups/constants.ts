import type { AccountGroup } from '@/api'
import { defineTableColumns } from '@/components/base/BaseTable/columns'

export const DEFAULT_ACCOUNT_GROUP_COLOR = '#60A5FA80'

export const ACCOUNT_GROUP_COLOR_PRESETS = [
  '#60A5FA80',
  '#22D3EE80',
  '#2DD4BF80',
  '#4ADE8080',
  '#FBBF2480',
  '#FB718580',
  '#A78BFA80',
  '#94A3B880',
] as const

export const accountGroupColumns = defineTableColumns<AccountGroup>([
  { key: 'selection', kind: 'selection' },
  { key: 'identity', label: '分组', kind: 'identity' },
  { key: 'color', label: '颜色', kind: 'status', size: 'sm' },
  { key: 'enabled', label: '状态', kind: 'status' },
  { key: 'accountCount', label: '账号数', kind: 'custom' },
  { key: 'capacity', label: '容量', kind: 'custom', size: 'md' },
  { key: 'usage', label: '用量', kind: 'custom' },
  { key: 'updatedAtDisplay', label: '更新时间', kind: 'datetime' },
  { key: 'actions', label: '操作', kind: 'actions', size: 'lg' },
])

export const accountGroupStatusOptions = [
  { label: '全部状态', value: '' },
  { label: '已启用', value: 'enabled' },
  { label: '已禁用', value: 'disabled' },
]
