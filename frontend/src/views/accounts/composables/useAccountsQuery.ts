import type { BaseTableSort } from '@/components/base/BaseTable/columns'
import { watchDebounced } from '@vueuse/core'

import { computed, onMounted, shallowRef, watch } from 'vue'
import { getAccounts } from '@/api'
import { toast } from '@/components/base/BaseToast'
import { usePagedQuery } from '@/composables/usePagedQuery'
import { errorMessage } from '@/utils/async'

type AccountRow = Awaited<ReturnType<typeof getAccounts>>['items'][number]
type AccountSummary = Awaited<ReturnType<typeof getAccounts>>['summary']
type AccountSummaryStatusKey = Exclude<keyof AccountSummary, 'total'>

const summaryKeyByStatus = {
  normal: 'normal',
  quota_exhausted: 'quotaExhausted',
  rate_limited: 'rateLimited',
  disabled: 'disabled',
  error: 'error',
} as const satisfies Record<AccountRow['status'], AccountSummaryStatusKey>

export function useAccountsQuery() {
  const searchQuery = shallowRef('')
  const providerQuery = shallowRef('')
  const statusQuery = shallowRef('')
  const sort = shallowRef<BaseTableSort>()
  const accountSummary = shallowRef({
    total: 0,
    normal: 0,
    quotaExhausted: 0,
    rateLimited: 0,
    disabled: 0,
    error: 0,
  })

  const query = usePagedQuery({
    initialPageSize: 20,
    load: ({ page, pageSize }) =>
      getAccounts({
        page,
        pageSize,
        search: searchQuery.value,
        provider: providerQuery.value || undefined,
        status: statusQuery.value || undefined,
        sortBy: sort.value?.key,
        sortDirection: sort.value?.direction,
      }),
    onSuccess: (result) => {
      accountSummary.value = result.summary
    },
    onError: (error) => {
      toast.error(errorMessage(error, '账号加载失败'))
    },
  })

  const accountPagination = computed(() => ({
    page: query.page.value,
    pageSize: query.pageSize.value,
    total: query.total.value,
  }))

  function handlePageChange(page: number) {
    query.page.value = page
    void query.execute()
  }

  function handlePageSizeChange(pageSize: number) {
    query.pageSize.value = pageSize
    query.page.value = 1
    void query.execute()
  }

  function handleSortChange(nextSort: BaseTableSort | undefined) {
    sort.value = nextSort
    query.page.value = 1
    void query.execute()
  }

  function replaceAccount(updated: AccountRow) {
    const index = query.items.value.findIndex(account => account.id === updated.id)
    if (index < 0)
      return false

    const current = query.items.value[index]
    if (!current)
      return false

    updateAccountSummary(current.status, updated.status)

    if (statusQuery.value && statusQuery.value !== updated.status) {
      query.items.value = query.items.value.filter(account => account.id !== updated.id)
      query.total.value = Math.max(0, query.total.value - 1)
      return false
    }

    const accounts = [...query.items.value]
    accounts[index] = updated
    query.items.value = accounts
    return true
  }

  function updateAccountSummary(
    previousStatus: AccountRow['status'],
    nextStatus: AccountRow['status'],
  ) {
    if (previousStatus === nextStatus)
      return

    const previousKey = summaryKeyByStatus[previousStatus]
    const nextKey = summaryKeyByStatus[nextStatus]
    const summary = { ...accountSummary.value }
    summary[previousKey] = Math.max(0, summary[previousKey] - 1)
    summary[nextKey] += 1
    accountSummary.value = summary
  }

  watchDebounced(
    searchQuery,
    () => {
      query.page.value = 1
      void query.execute()
    },
    { debounce: 250 },
  )

  watch([providerQuery, statusQuery], () => {
    query.page.value = 1
    void query.execute()
  })

  onMounted(() => {
    void query.execute()
  })

  return {
    page: query.page,
    pageSize: query.pageSize,
    totalAccounts: query.total,
    loading: query.loading,
    accounts: query.items,
    loadAccounts: query.execute,
    searchQuery,
    providerQuery,
    statusQuery,
    sort,
    accountSummary,
    accountPagination,
    replaceAccount,
    handlePageChange,
    handlePageSizeChange,
    handleSortChange,
  }
}
