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
    width: '340px',
    minWidth: '340px',
    fixed: 'left' as const,
    cellClass: relaxedCellClass,
  },
  { key: 'status', label: '状态', flex: 0.6, minWidth: '60px', cellClass: relaxedCellClass },
  { key: 'planType', label: '套餐', flex: 0.8, minWidth: '112px', cellClass: relaxedCellClass },
  { key: 'usage', label: '用量', flex: 1.3, minWidth: '220px', cellClass: relaxedCellClass },
  {
    key: 'updatedAtDisplay',
    label: '最后使用',
    flex: 1.2,
    minWidth: '160px',
    cellClass: `${relaxedCellClass} text-(--cp-text-secondary)`,
  },
  {
    key: 'accessTokenExpiresAtDisplay',
    label: '过期时间',
    flex: 1.2,
    minWidth: '160px',
    cellClass: `${relaxedCellClass} text-(--cp-text-secondary)`,
  },
  {
    key: 'actions',
    label: '操作',
    width: '116px',
    minWidth: '116px',
    fixed: 'right' as const,
    headerClass: '!pr-3',
    cellClass: `!px-2 ${relaxedCellClass}`,
  },
]

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
