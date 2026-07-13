import { computed, ref, type Ref } from 'vue'

export function useApiKeysTable(pageKeys: Ref<any[]>, selectedIds = ref<Set<string>>(new Set())) {
  const allSelected = computed(
    () => pageKeys.value.length > 0 && pageKeys.value.every((key) => selectedIds.value.has(key.id)),
  )

  const indeterminate = computed(
    () => pageKeys.value.some((key) => selectedIds.value.has(key.id)) && !allSelected.value,
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
      pageKeys.value.forEach((key) => next.delete(key.id))
    } else {
      pageKeys.value.forEach((key) => next.add(key.id))
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
