// @env browser
import { computed, onBeforeUnmount, ref, watch, type Ref } from 'vue'

export function useLogFilters(totalLogs: Ref<number>) {
  const page = ref(1)
  const pageSize = ref(20)
  const searchQuery = ref('')
  const filterLevel = ref('')
  let searchTimer: number | undefined
  let loadLogs: (() => Promise<void> | void) | undefined

  const logPagination = computed(() => ({
    page: page.value,
    pageSize: pageSize.value,
    total: totalLogs.value,
    pageSizes: [10, 20, 50, 100],
  }))

  function bindLogLoader(loader: () => Promise<void> | void) {
    loadLogs = loader
  }

  function requestLoad() {
    if (loadLogs) {
      void loadLogs()
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

  watch([searchQuery, filterLevel], () => {
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
    filterLevel,
    logPagination,
    bindLogLoader,
    handlePageChange,
    handlePageSizeChange,
  }
}
