import type { AccountGroup } from '@/api'
import type { BaseTableColumn } from '@/components/base/BaseTable/columns'

const cellClass = 'py-2 align-middle'

export const accountGroupColumns = [
  {
    key: 'selection',
    label: '',
    width: '48px',
    minWidth: '48px',
    align: 'center' as const,
    cellClass,
  },
  {
    key: 'identity',
    label: '分组',
    minWidth: '250px',
    flex: 1.2,
    cellClass,
  },
  {
    key: 'color',
    label: '颜色',
    width: '76px',
    minWidth: '76px',
    align: 'center' as const,
    cellClass,
  },
  {
    key: 'enabled',
    label: '状态',
    width: '100px',
    minWidth: '100px',
    align: 'center' as const,
    cellClass,
  },
  {
    key: 'accountCount',
    label: '账号数',
    width: '150px',
    minWidth: '150px',
    cellClass,
  },
  {
    key: 'capacity',
    label: '容量',
    width: '112px',
    minWidth: '112px',
    cellClass,
  },
  {
    key: 'usage',
    label: '用量',
    width: '144px',
    minWidth: '144px',
    cellClass,
  },
  {
    key: 'updatedAtDisplay',
    label: '更新时间',
    width: '176px',
    minWidth: '176px',
    cellClass: `${cellClass} font-mono text-[12px] text-(--cp-text-secondary)`,
  },
  {
    key: 'actions',
    label: '操作',
    width: '136px',
    minWidth: '136px',
    cellClass,
  },
] satisfies BaseTableColumn<AccountGroup>[]

export const accountGroupStatusOptions = [
  { label: '全部状态', value: '' },
  { label: '已启用', value: 'enabled' },
  { label: '已禁用', value: 'disabled' },
]
