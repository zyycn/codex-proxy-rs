// @env browser
import { watchDebounced } from '@vueuse/core'
import { computed, ref, type Ref } from 'vue'

export function useUsageFilters(totalRecords: Ref<number>) {
  const page = ref(1)
  const pageSize = ref(10)
  const searchQuery = ref('')
  let loadUsageRecords: ((scope?: 'all' | 'table') => Promise<void> | void) | undefined

  const usagePagination = computed(() => ({
    page: page.value,
    pageSize: pageSize.value,
    total: totalRecords.value,
    pageSizes: [10, 20, 50, 100],
  }))

  function bindUsageRecordLoader(loader: (scope?: 'all' | 'table') => Promise<void> | void) {
    loadUsageRecords = loader
  }

  function requestLoad(scope: 'all' | 'table' = 'table') {
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
