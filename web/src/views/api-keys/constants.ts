const relaxedActionCellClass = '!px-4'

export const apiKeyColumns = [
  { key: 'selection', label: '', width: '48px', align: 'center' as const },
  { key: 'identity', label: '名称 / 标签', minWidth: '280px', flex: 1.25 },
  { key: 'prefix', label: '密钥前缀', minWidth: '300px', flex: 1.35 },
  { key: 'enabled', label: '状态', width: '112px', align: 'center' as const },
  {
    key: 'createdAtDisplay',
    label: '创建时间',
    width: '176px',
    mono: true,
    tabular: true,
    cellClass: 'text-(--cp-text-secondary)',
  },
  {
    key: 'lastUsedAtDisplay',
    label: '最后使用',
    width: '176px',
    mono: true,
    tabular: true,
    cellClass: 'text-(--cp-text-secondary)',
  },
  {
    key: 'actions',
    label: '操作',
    width: '108px',
    align: 'center' as const,
    headerClass: relaxedActionCellClass,
    cellClass: relaxedActionCellClass,
  },
]
