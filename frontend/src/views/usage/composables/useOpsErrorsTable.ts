import type { Ref } from 'vue'
import type { UsageTimeRangeParams } from './useUsageTimeRange'
import { watchDebounced } from '@vueuse/core'

import { computed, onMounted, shallowRef, watch } from 'vue'
import { getOpsErrors } from '@/api'
import { toast } from '@/components/base/BaseToast'
import { usePagedQuery } from '@/composables/usePagedQuery'
import { errorMessage, withMinimumDuration } from '@/utils/async'

export function useOpsErrorsTable(timeRangeParams: Readonly<Ref<UsageTimeRangeParams>>) {
  const refreshing = shallowRef(false)
  const searchQuery = shallowRef('')
  const failureClass = shallowRef('')
  const route = shallowRef('')

  const query = usePagedQuery({
    initialPageSize: 10,
    load: ({ page, pageSize }) => getOpsErrors({
      page,
      pageSize,
      search: searchQuery.value.trim() || undefined,
      failureClass: failureClass.value.trim() || undefined,
      route: route.value.trim() || undefined,
      ...timeRangeParams.value,
    }),
    onError: error => toast.error(errorMessage(error, '加载错误明细失败')),
  })
  // 首屏进入即处于加载态；首次 execute 完成后由 usePagedQuery 维护。
  query.loading.value = true

  const pagination = computed(() => ({
    page: query.page.value,
    pageSize: query.pageSize.value,
    total: query.total.value,
  }))

  function handlePageChange(nextPage: number) {
    query.page.value = nextPage
    void query.execute()
  }

  function handlePageSizeChange(nextPageSize: number) {
    query.pageSize.value = nextPageSize
    query.page.value = 1
    void query.execute()
  }

  async function refresh() {
    if (refreshing.value || query.loading.value)
      return
    refreshing.value = true
    try {
      await withMinimumDuration(() => query.execute())
    }
    finally {
      refreshing.value = false
    }
  }

  watchDebounced(
    [searchQuery, failureClass, route],
    () => {
      query.page.value = 1
      void query.execute()
    },
    { debounce: 250 },
  )

  watch(timeRangeParams, () => {
    query.page.value = 1
    void query.execute()
  })

  onMounted(() => void query.execute())

  return {
    loading: query.loading,
    refreshing,
    records: query.items,
    searchQuery,
    failureClass,
    route,
    pagination,
    handlePageChange,
    handlePageSizeChange,
    refresh,
  }
}
