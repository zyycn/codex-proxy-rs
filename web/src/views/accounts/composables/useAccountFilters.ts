// @env browser
import { computed, onBeforeUnmount, ref, watch, type Ref } from 'vue'

export function useAccountFilters(totalAccounts: Ref<number>) {
  const page = ref(1)
  const pageSize = ref(20)
  const searchQuery = ref('')
  let searchTimer: number | undefined
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

  watch(searchQuery, () => {
    page.value = 1
    if (searchTimer) {
      window.clearTimeout(searchTimer)
    }
    searchTimer = window.setTimeout(requestLoad, 250)
  })

  onBeforeUnmount(() => {
    if (searchTimer) {
      window.clearTimeout(searchTimer)
    }
  })

  return {
    page,
    pageSize,
    searchQuery,
    accountPagination,
    bindAccountLoader,
    handlePageChange,
    handlePageSizeChange,
  }
}
