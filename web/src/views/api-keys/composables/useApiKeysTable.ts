import { computed, ref, type Ref } from 'vue'

export function useApiKeysTable(pagedKeys: Ref<any[]>, selectedIds = ref<Set<string>>(new Set())) {
  const allSelected = computed(
    () =>
      pagedKeys.value.length > 0 && pagedKeys.value.every((key) => selectedIds.value.has(key.id)),
  )

  const indeterminate = computed(
    () => pagedKeys.value.some((key) => selectedIds.value.has(key.id)) && !allSelected.value,
  )

  const selectedRowKeys = computed(() => [...selectedIds.value])

  function toggleSelection(keyId: string) {
    const next = new Set(selectedIds.value)
    if (next.has(keyId)) {
      next.delete(keyId)
    } else {
      next.add(keyId)
    }
    selectedIds.value = next
  }

  function toggleAll() {
    const next = new Set(selectedIds.value)
    if (allSelected.value) {
      pagedKeys.value.forEach((key) => next.delete(key.id))
    } else {
      pagedKeys.value.forEach((key) => next.add(key.id))
    }
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
