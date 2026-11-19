import type { TableColumn } from '@codex-proxy/ui'
import type { Ref } from 'vue'
import type { UsageListRecord, UsageRecordStatus } from '@/api'
import { useTableColumns } from '@codex-proxy/ui'
import { computed } from 'vue'
import { usageRecordColumns } from '@/components/usage/shared/columns'
import { t } from '@/i18n'

export function useUsageRecordColumns(status: Ref<UsageRecordStatus>, tableWidth: Readonly<Ref<number>>) {
  const columns = computed(() => usageRecordColumns.value.map((column) => {
    if (column.key === 'createdAtDisplay')
      return { ...column, width: 128 }
    if (column.key === 'accountEmail')
      return { ...column, width: 280, hideable: true }
    return ['clientApiKeyName', 'model', 'reasoningEffort'].includes(column.key)
      ? { ...column, fixedWidth: true }
      : column
  }))
  const usage = useTableColumns(columns, 'usage-records')
  const errors = useTableColumns(() => {
    const time = columns.value.find(column => column.key === 'createdAtDisplay')!
    const actionWidth = 72
    const errorWidth = 370
    return [
      {
        key: 'errorMessage',
        label: t('usage.filters.error'),
        kind: 'custom',
        fixed: 'left',
        minWidth: tableWidth.value
          ? Math.min(errorWidth, Math.max(96, (tableWidth.value - actionWidth) / 2))
          : errorWidth,
        hideable: false,
      },
      time,
      ...columns.value.filter(column => column.key !== time.key).map((column) => {
        if (column.key === 'transport')
          return { ...column, width: 112 }
        if (column.key === 'actions')
          return { ...column, width: actionWidth }
        return column
      }),
    ] satisfies TableColumn<UsageListRecord>[]
  }, 'usage-error-records')
  return computed(() => status.value === 'error'
    ? errors
    : usage)
}
