import type { AccountRow } from './quota'
import type { BaseTableColumn } from '@/components/base/BaseTable/columns'

const relaxedCellClass = 'py-3 align-middle'

export const accountColumns = [
  {
    key: 'expander',
    label: '',
    width: '40px',
    minWidth: '40px',
    align: 'center' as const,
    headerClass: '!px-2',
    cellClass: `!px-2 ${relaxedCellClass}`,
  },
  {
    key: 'selection',
    label: '',
    width: '40px',
    minWidth: '40px',
    align: 'center' as const,
    headerClass: '!px-2',
    cellClass: `!px-2 ${relaxedCellClass}`,
  },
  {
    key: 'identity',
    label: '邮箱',
    sortable: true,
    sortKey: 'email',
    width: '340px',
    minWidth: '340px',
    cellClass: relaxedCellClass,
  },
  {
    key: 'status',
    label: '状态',
    sortable: true,
    flex: 0.6,
    minWidth: '60px',
    cellClass: relaxedCellClass,
  },
  {
    key: 'planType',
    label: '套餐',
    sortable: true,
    flex: 0.8,
    minWidth: '112px',
    cellClass: relaxedCellClass,
  },
  {
    key: 'usage',
    label: '用量',
    sortable: true,
    flex: 1.3,
    minWidth: '220px',
    cellClass: relaxedCellClass,
  },
  {
    key: 'lastUsedAt',
    label: '最后使用',
    sortable: true,
    flex: 1.2,
    minWidth: '160px',
    format: (_value: unknown, row: AccountRow) => row.usage.lastUsedAtDisplay,
    cellClass: `${relaxedCellClass} text-(--cp-text-secondary)`,
  },
  {
    key: 'accessTokenExpiresAtDisplay',
    label: '过期时间',
    sortable: true,
    sortKey: 'expiresAt',
    flex: 1.2,
    minWidth: '160px',
    cellClass: `${relaxedCellClass} text-(--cp-text-secondary)`,
  },
  {
    key: 'actions',
    label: '操作',
    width: '110px',
    minWidth: '110px',
  },
] satisfies BaseTableColumn<AccountRow>[]

export const statusLabels: Record<string, string> = {
  active: '正常',
  expired: '已过期',
  disabled: '已禁用',
  banned: '已封禁',
  quota_exhausted: '配额耗尽',
  refreshing: '刷新中',
}

export const statusTones: Record<string, 'success' | 'danger' | 'warning' | 'info' | 'normal'> = {
  active: 'success',
  expired: 'warning',
  disabled: 'normal',
  banned: 'danger',
  quota_exhausted: 'warning',
  refreshing: 'info',
}

export const accountStatusFilterOptions = [
  { label: '全部状态', value: '' },
  { label: statusLabels.active, value: 'active' },
  { label: statusLabels.expired, value: 'expired' },
  { label: statusLabels.quota_exhausted, value: 'quota_exhausted' },
  { label: statusLabels.disabled, value: 'disabled' },
  { label: statusLabels.banned, value: 'banned' },
]
