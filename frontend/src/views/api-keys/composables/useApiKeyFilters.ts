import { watchDebounced } from '@vueuse/core'
import { computed, shallowRef, type Ref } from 'vue'

import type { BaseTableSort } from '@/components/base/BaseTable/columns'

export function useApiKeyFilters(totalApiKeys: Ref<number>) {
  const page = shallowRef(1)
  const pageSize = shallowRef(20)
  const searchQuery = shallowRef('')
  const sort = shallowRef<BaseTableSort>()
  let loadApiKeys: (() => Promise<void> | void) | undefined

  const apiKeyPagination = computed(() => ({
    page: page.value,
    pageSize: pageSize.value,
    total: totalApiKeys.value,
    pageSizes: [10, 20, 50, 100],
  }))

  function bindApiKeyLoader(loader: () => Promise<void> | void) {
    loadApiKeys = loader
  }

  function requestLoad() {
    if (loadApiKeys) {
      void loadApiKeys()
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

  return {
    page,
    pageSize,
    searchQuery,
    sort,
    apiKeyPagination,
    bindApiKeyLoader,
    handlePageChange,
    handlePageSizeChange,
    handleSortChange,
  }
}
