import { watchDebounced } from '@vueuse/core'
import { computed, shallowRef } from 'vue'

export function useUsageFilters(totalRecords: any) {
  const page = shallowRef(1)
  const pageSize = shallowRef(10)
  const searchQuery = shallowRef('')
  let loadUsageRecords: any

  const usagePagination = computed(() => ({
    page: page.value,
    pageSize: pageSize.value,
    total: totalRecords.value,
    pageSizes: [10, 20, 50, 100],
  }))

  function bindUsageRecordLoader(loader: any) {
    loadUsageRecords = loader
  }

  function requestLoad(scope = 'table') {
    if (loadUsageRecords) {
      void loadUsageRecords(scope)
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

  watchDebounced(
    searchQuery,
    () => {
      page.value = 1
      requestLoad('table')
    },
    { debounce: 250 },
  )

  return {
    page,
    pageSize,
    searchQuery,
    usagePagination,
    bindUsageRecordLoader,
    handlePageChange,
    handlePageSizeChange,
  }
}
