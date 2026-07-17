import type { BaseTableSort } from '@/components/base/BaseTable/columns'
import { watchDebounced } from '@vueuse/core'

import { computed, onMounted, shallowRef } from 'vue'
import { getApiKeys } from '@/api'
import { toast } from '@/components/base/BaseToast'
import { usePagedQuery } from '@/composables/usePagedQuery'
import { errorMessage } from '@/utils/async'

export function useApiKeysQuery() {
  const searchQuery = shallowRef('')
  const sort = shallowRef<BaseTableSort>()
  const query = usePagedQuery({
    initialPageSize: 20,
    load: ({ page, pageSize }) =>
      getApiKeys({
        page,
        pageSize,
        search: searchQuery.value.trim() || undefined,
        sortBy: sort.value?.key,
        sortDirection: sort.value?.direction,
      }),
    onError: (error) => {
      toast.error(errorMessage(error, 'API Key 加载失败'))
    },
  })

  const apiKeyPagination = computed(() => ({
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

  onMounted(() => {
    void query.execute()
  })

  return {
    loading: query.loading,
    apiKeys: query.items,
    loadApiKeys: query.execute,
    searchQuery,
    sort,
    apiKeyPagination,
    handlePageChange,
    handlePageSizeChange,
    handleSortChange,
  }
}
