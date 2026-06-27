import { computed, ref, type Ref } from 'vue'

export function useAccountsTable(accounts: Ref<any[]>, selectedIds = ref<Set<string>>(new Set())) {
  const expandedAccountIds = ref<Set<string>>(new Set())

  const allSelected = computed(
    () =>
      accounts.value.length > 0 &&
      accounts.value.every((account) => selectedIds.value.has(account.id)),
  )

  const indeterminate = computed(
    () => accounts.value.some((account) => selectedIds.value.has(account.id)) && !allSelected.value,
  )

  const selectedRowKeys = computed(() => [...selectedIds.value])
  const expandedRowKeys = computed(() => [...expandedAccountIds.value])

  function toggleSelection(accountId: string) {
    const next = new Set(selectedIds.value)
    if (next.has(accountId)) {
      next.delete(accountId)
    } else {
      next.add(accountId)
    }
    selectedIds.value = next
  }

  function toggleExpanded(accountId: string) {
    const next = new Set(expandedAccountIds.value)
    if (next.has(accountId)) {
      next.delete(accountId)
    } else {
      next.add(accountId)
    }
    expandedAccountIds.value = next
  }

  function toggleAll() {
    const next = new Set(selectedIds.value)
    if (allSelected.value) {
      accounts.value.forEach((account) => next.delete(account.id))
    } else {
      accounts.value.forEach((account) => next.add(account.id))
    }
    selectedIds.value = next
  }

  return {
    selectedIds,
    expandedAccountIds,
    allSelected,
    indeterminate,
    selectedRowKeys,
    expandedRowKeys,
    toggleSelection,
    toggleExpanded,
    toggleAll,
  }
}
