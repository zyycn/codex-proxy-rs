import type { BaseTableSort } from '@/components/base/BaseTable/columns'
import { watchDebounced } from '@vueuse/core'

import { computed, onMounted, shallowRef, watch } from 'vue'
import { getAccounts } from '@/api'
import { toast } from '@/components/base/BaseToast'
import { usePagedQuery } from '@/composables/usePagedQuery'
import { errorMessage } from '@/utils/async'

export function useAccountsQuery() {
  const searchQuery = shallowRef('')
  const providerQuery = shallowRef('')
  const statusQuery = shallowRef('')
  const configRevision = shallowRef(0)
  const sort = shallowRef<BaseTableSort>()
  const accountSummary = shallowRef({
    total: 0,
    active: 0,
    quotaExhausted: 0,
    attention: 0,
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
      configRevision.value = result.configRevision || configRevision.value
    },
    onError: (error) => {
      toast.error(errorMessage(error, '账号加载失败'))
    },
  })

  const accountPagination = computed(() => ({
    page: query.page.value,
    pageSize: query.pageSize.value,
    total: query.total.value,
    pageSizes: [10, 20, 50, 100],
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
    configRevision,
    accountPagination,
    handlePageChange,
    handlePageSizeChange,
    handleSortChange,
  }
}
