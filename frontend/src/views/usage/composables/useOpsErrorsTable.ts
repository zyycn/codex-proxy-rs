import type { Ref } from 'vue'
import type { UsageTimeRangeParams } from './useUsageTimeRange'
import { watchDebounced } from '@vueuse/core'

import { computed, onMounted, shallowRef, watch } from 'vue'
import { getOpsErrors } from '@/api'
import { toast } from '@/components/base/BaseToast'
import { useStablePagedQuery } from '@/composables/useStablePagedQuery'
import { errorMessage, withMinimumDuration } from '@/utils/async'

export function useOpsErrorsTable(timeRangeParams: Readonly<Ref<UsageTimeRangeParams>>) {
  const refreshing = shallowRef(false)
  const searchQuery = shallowRef('')
  const failureClass = shallowRef('')
  const route = shallowRef('')
  const query = useStablePagedQuery({
    initialPageSize: 10,
    load: ({ currentPage, pageSize }) => getOpsErrors({
      currentPage,
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
    currentPage: query.currentPage.value,
    pageSize: query.pageSize.value,
    total: query.total.value,
  }))

  function handlePageChange(nextPage: number) {
    void query.execute(nextPage)
  }

  function handlePageSizeChange(nextPageSize: number) {
    query.pageSize.value = nextPageSize
    void query.reloadFromStart()
  }

  async function refresh() {
    if (refreshing.value || query.loading.value)
      return
    refreshing.value = true
    try {
      await withMinimumDuration(() => query.execute(query.currentPage.value))
    }
    finally {
      refreshing.value = false
    }
  }

  watchDebounced(
    [searchQuery, failureClass, route],
    () => {
      void query.reloadFromStart()
    },
    { debounce: 250 },
  )

  watch(timeRangeParams, () => {
    void query.reloadFromStart()
  })

  onMounted(() => void query.execute(1))

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
