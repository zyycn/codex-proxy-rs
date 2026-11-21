import type { TableColumn } from '@codex-proxy/ui'
import type { Ref } from 'vue'
import type { UsageListRecord, UsageRecordStatus } from '@/api'
import { useTableColumns } from '@codex-proxy/ui'
import { clamp, partition } from 'es-toolkit'
import { computed } from 'vue'
import { usageRecordColumns } from '@/components/usage/shared/columns'
import { t } from '@/i18n'

type ColumnLayout = Record<string, Pick<TableColumn, 'width' | 'fixedWidth' | 'hideable'>>

const requestLayout: ColumnLayout = {
  clientApiKeyName: { fixedWidth: true },
  accountEmail: { width: 280, hideable: true },
  model: { fixedWidth: true },
  reasoningEffort: { fixedWidth: true },
}

const errorActionWidth = 72
const maxErrorWidth = 370
const errorLayout: ColumnLayout = {
  ...requestLayout,
  transport: { width: 112 },
  actions: { width: errorActionWidth },
}

function columnsWithLayout(layout: ColumnLayout) {
  return usageRecordColumns.value.map(column => ({
    ...column,
    ...layout[column.key],
  }))
}

export function useUsageRecordColumns(status: Ref<UsageRecordStatus>, tableWidth: Readonly<Ref<number>>) {
  const usage = useTableColumns(() => columnsWithLayout(requestLayout), 'usage-records')
  const errors = useTableColumns(() => {
    const [time, details] = partition(columnsWithLayout(errorLayout), column => column.key === 'createdAtDisplay')
    return [
      {
        key: 'errorMessage',
        label: t('usage.filters.error'),
        kind: 'custom',
        fixed: 'left',
        minWidth: tableWidth.value
          ? clamp((tableWidth.value - errorActionWidth) / 2, 96, maxErrorWidth)
          : maxErrorWidth,
        hideable: false,
      },
      ...time,
      ...details,
    ] satisfies TableColumn<UsageListRecord>[]
  }, 'usage-error-records')
  return computed(() => status.value === 'error'
    ? errors
    : usage)
}
