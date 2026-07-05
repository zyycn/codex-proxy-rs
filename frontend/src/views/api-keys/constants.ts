const relaxedActionCellClass = '!px-4'

export const apiKeyColumns = [
  { key: 'selection', label: '', width: '48px', minWidth: '48px', align: 'center' as const },
  { key: 'identity', label: '名称', minWidth: '220px', flex: 0.9, fixed: 'left' as const },
  { key: 'prefix', label: '密钥前缀', minWidth: '300px', flex: 1.35 },
  { key: 'enabled', label: '状态', width: '112px', minWidth: '112px', align: 'center' as const },
  {
    key: 'createdAtDisplay',
    label: '创建时间',
    width: '176px',
    minWidth: '176px',
    cellClass: 'font-mono text-[12px] font-[650] tabular-nums text-(--cp-text-secondary)',
  },
  {
    key: 'lastUsedAtDisplay',
    label: '最后使用',
    width: '176px',
    minWidth: '176px',
    cellClass: 'font-mono text-[12px] font-[650] tabular-nums text-(--cp-text-secondary)',
  },
  {
    key: 'actions',
    label: '操作',
    width: '184px',
    minWidth: '184px',
    fixed: 'right' as const,
    headerClass: relaxedActionCellClass,
    cellClass: relaxedActionCellClass,
  },
]
