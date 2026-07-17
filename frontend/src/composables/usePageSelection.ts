import type { Ref } from 'vue'
import { computed, ref } from 'vue'

export function usePageSelection<Row extends { id: string }>(
  rows: Ref<Row[]>,
  selectedIds = ref<Set<string>>(new Set()),
) {
  const allSelected = computed(
    () => rows.value.length > 0 && rows.value.every(row => selectedIds.value.has(row.id)),
  )
  const indeterminate = computed(
    () => rows.value.some(row => selectedIds.value.has(row.id)) && !allSelected.value,
  )
  const selectedRowKeys = computed(() => [...selectedIds.value])

  function toggleSelection(rowId: string) {
    const next = new Set(selectedIds.value)
    if (next.has(rowId))
      next.delete(rowId)
    else next.add(rowId)
    selectedIds.value = next
  }

  function toggleAll() {
    const next = new Set(selectedIds.value)
    if (allSelected.value)
      rows.value.forEach(row => next.delete(row.id))
    else rows.value.forEach(row => next.add(row.id))
    selectedIds.value = next
  }

  return {
    selectedIds,
    allSelected,
    indeterminate,
    selectedRowKeys,
    toggleSelection,
    toggleAll,
  }
}
