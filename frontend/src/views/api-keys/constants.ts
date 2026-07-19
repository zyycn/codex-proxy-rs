const relaxedActionCellClass = '!px-4'

export const apiKeyColumns = [
  { key: 'selection', label: '', width: '48px', minWidth: '48px', align: 'center' as const },
  {
    key: 'identity',
    label: '名称',
    sortable: true,
    sortKey: 'name',
    minWidth: '220px',
    flex: 0.9,
  },
  { key: 'prefix', label: '密钥前缀', minWidth: '240px', flex: 1.35 },
  {
    key: 'providerKind',
    label: '平台',
    width: '120px',
    minWidth: '120px',
  },
  {
    key: 'enabled',
    label: '状态',
    sortable: true,
    width: '112px',
    minWidth: '112px',
    align: 'center' as const,
  },
  {
    key: 'createdAtDisplay',
    label: '创建时间',
    sortable: true,
    sortKey: 'createdAt',
    width: '176px',
    minWidth: '176px',
    cellClass: 'font-mono text-[12px] font-[650] tabular-nums text-(--cp-text-secondary)',
  },
  {
    key: 'lastUsedAtDisplay',
    label: '最后使用',
    sortable: true,
    sortKey: 'lastUsedAt',
    width: '176px',
    minWidth: '176px',
    cellClass: 'font-mono text-[12px] font-[650] tabular-nums text-(--cp-text-secondary)',
  },
  {
    key: 'actions',
    label: '操作',
    width: '140px',
    minWidth: '140px',
    headerClass: relaxedActionCellClass,
    cellClass: relaxedActionCellClass,
  },
]
