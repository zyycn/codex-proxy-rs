import type { Ref } from 'vue'
import type { Account } from '@/api'

import { computed, ref } from 'vue'
import { usePageSelection } from '@/composables/usePageSelection'

export function useAccountsTable(
  accounts: Ref<Account[]>,
  selectedIds = ref<Set<string>>(new Set()),
) {
  const expandedAccountIds = ref<Set<string>>(new Set())
  const selection = usePageSelection(accounts, selectedIds)
  const expandedRowKeys = computed(() => [...expandedAccountIds.value])

  function toggleExpanded(accountId: string) {
    const next = new Set(expandedAccountIds.value)
    if (next.has(accountId)) {
      next.delete(accountId)
    }
    else {
      next.add(accountId)
    }
    expandedAccountIds.value = next
  }

  return {
    ...selection,
    expandedAccountIds,
    expandedRowKeys,
    toggleExpanded,
  }
}
