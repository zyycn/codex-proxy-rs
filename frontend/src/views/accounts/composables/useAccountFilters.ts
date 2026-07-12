import { watchDebounced } from '@vueuse/core'
import { computed, ref, watch, type Ref } from 'vue'

import type { BaseTableSort } from '@/components/base/BaseTable/columns'

export function useAccountFilters(totalAccounts: Ref<number>) {
  const page = ref(1)
  const pageSize = ref(20)
  const searchQuery = ref('')
  const statusQuery = ref('')
  const sort = ref<BaseTableSort>()
  let loadAccounts: (() => Promise<void> | void) | undefined

  const accountPagination = computed(() => ({
    page: page.value,
    pageSize: pageSize.value,
    total: totalAccounts.value,
    pageSizes: [10, 20, 50, 100],
  }))

  function bindAccountLoader(loader: () => Promise<void> | void) {
    loadAccounts = loader
  }

  function requestLoad() {
    if (loadAccounts) {
      void loadAccounts()
    }
  }

  function handlePageChange(nextPage: number) {
    page.value = nextPage
    requestLoad()
  }

  function handlePageSizeChange(nextPageSize: number) {
    pageSize.value = nextPageSize
    page.value = 1
    requestLoad()
  }

  function handleSortChange(nextSort: BaseTableSort | undefined) {
    sort.value = nextSort
    page.value = 1
    requestLoad()
  }

  watchDebounced(
    searchQuery,
    () => {
      page.value = 1
      requestLoad()
    },
    { debounce: 250 },
  )

  watch(statusQuery, () => {
    page.value = 1
    requestLoad()
  })

  return {
    page,
    pageSize,
    searchQuery,
    statusQuery,
    sort,
    accountPagination,
    bindAccountLoader,
    handlePageChange,
    handlePageSizeChange,
    handleSortChange,
  }
}
