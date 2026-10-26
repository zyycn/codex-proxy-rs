import type { TableColumn, TableSort } from '@codex-proxy/ui'
import type { MaybeRefOrGetter } from 'vue'
import { useStorage } from '@vueuse/core'
import { toValue } from 'vue'

export function useTableSort<Row extends object>(
  source: MaybeRefOrGetter<TableColumn<Row>[]>,
  tableId: string,
) {
  function readSort(value: string): TableSort | undefined {
    try {
      const parsed: unknown = JSON.parse(value)
      if (!parsed || typeof parsed !== 'object' || !('key' in parsed) || !('direction' in parsed))
        return undefined

      const { key, direction } = parsed
      if (typeof key === 'string' && (direction === 'asc' || direction === 'desc')
        && toValue(source).some(column => column.sortable && (column.sortable === true
          ? column.key
          : column.sortable) === key)) {
        return { key, direction }
      }
    }
    catch {
      // 损坏的本地偏好回退到默认排序，不影响列表查询
    }
    return undefined
  }

  return useStorage<TableSort | undefined>(`codex-proxy:table-sort:${tableId}`, undefined, undefined, {
    shallow: true,
    writeDefaults: false,
    // 其他标签页的偏好在下次进入时读取，避免当前排序与分页游标失配
    listenToStorageChanges: false,
    serializer: { read: readSort, write: JSON.stringify },
  })
}
