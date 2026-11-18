import type { TableColumn } from '@codex-proxy/ui'
import type { Ref } from 'vue'
import type { UsageListRecord, UsageRecordStatus } from '@/api'
import { useTableColumns } from '@codex-proxy/ui'
import { computed } from 'vue'
import { usageRecordColumns } from '@/components/usage/shared/columns'
import { t } from '@/i18n'

export function useUsageRecordColumns(status: Ref<UsageRecordStatus>, tableWidth: Readonly<Ref<number>>) {
  const columns = computed(() => usageRecordColumns.value
    .filter(column => column.key !== 'reasoningEffort')
    .map(column => column.key === 'model'
      ? { ...column, minWidth: 176 }
      : column))
  const usage = useTableColumns(columns, 'usage-records')
  const errors = useTableColumns(() => {
    const byKey = new Map(columns.value.map(column => [column.key, column]))
    const column = (key: string, minWidth: number, fixedWidth = false) => ({
      ...byKey.get(key)!,
      minWidth,
      fixedWidth,
    })
    const actionWidth = 72
    return [
      {
        key: 'errorMessage',
        label: t('usage.filters.error'),
        kind: 'custom',
        minWidth: tableWidth.value
          ? Math.max(160, Math.min(310, tableWidth.value - actionWidth))
          : 310,
        hideable: false,
      },
      column('createdAtDisplay', 112, true),
      column('model', 176),
      column('accountEmail', 148),
      column('clientApiKeyName', 104),
      column('transport', 112, true),
      ...columns.value.filter(col => [
        'accountPlanType',
        'provider',
        'route',
        'clientIp',
        'userAgent',
      ].includes(col.key))
        .map(col => ({ ...col, defaultHidden: true })),
      column('actions', actionWidth, true),
    ] satisfies TableColumn<UsageListRecord>[]
  }, 'usage-error-records')
  return computed(() => status.value === 'error'
    ? errors
    : usage)
}
