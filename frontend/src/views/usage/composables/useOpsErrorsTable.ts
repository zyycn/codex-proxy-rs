import type { Ref } from 'vue'
import type { UsageTimeRangeParams } from './useUsageTimeRange'
import { watchDebounced } from '@vueuse/core'

import { computed, onScopeDispose, shallowRef, watch } from 'vue'
import { getOpsErrors } from '@/api'
import { toast } from '@/components/base/BaseToast'
import { useStablePagedQuery } from '@/composables/useStablePagedQuery'
import { errorMessage, withMinimumDuration } from '@/utils/async'
import { usageSearchParam } from '../utils/search'

interface UseOpsErrorsTableOptions {
  timeRangeParams: Readonly<Ref<UsageTimeRangeParams>>
  latestTimeRangeParams: () => UsageTimeRangeParams
  provider: Readonly<Ref<string>>
  active: Readonly<Ref<boolean>>
}

export function useOpsErrorsTable(options: UseOpsErrorsTableOptions) {
  const refreshing = shallowRef(false)
  const searchQuery = shallowRef('')
  const search = computed(() => usageSearchParam(searchQuery.value))
  let disposed = false
  // 时间、平台和搜索共同构成分页快照，避免翻页混入另一组筛选结果。
  let tableParams = snapshot()

  function snapshot() {
    return {
      ...options.latestTimeRangeParams(),
      provider: options.provider.value || undefined,
      search: search.value,
    }
  }

  const query = useStablePagedQuery({
    initialPageSize: 10,
    load: ({ currentPage, pageSize }) => getOpsErrors({
      currentPage,
      pageSize,
      ...tableParams,
    }),
    onError: error => toast.error(errorMessage(error, '加载错误明细失败')),
  })
  const pagination = computed(() => ({
    currentPage: query.currentPage.value,
    pageSize: query.pageSize.value,
    total: query.total.value,
  }))

  function handlePageChange(nextPage: number) {
    if (tableParams.search !== search.value) {
      void reloadLatest()
      return
    }
    void query.execute(nextPage)
  }

  function handlePageSizeChange(nextPageSize: number) {
    query.pageSize.value = nextPageSize
    if (tableParams.search !== search.value)
      void reloadLatest()
    else
      void query.reloadFromStart()
  }

  function reloadLatest() {
    tableParams = snapshot()
    // 筛选失败不能继续展示上一范围的数据，错误态由面板单独呈现。
    query.items.value = []
    return query.reloadFromStart()
  }

  async function refresh() {
    if (refreshing.value || query.loading.value)
      return
    refreshing.value = true
    try {
      await withMinimumDuration(reloadLatest)
    }
    finally {
      refreshing.value = false
    }
  }

  watchDebounced(
    search,
    () => {
      if (!disposed && options.active.value && tableParams.search !== search.value)
        void reloadLatest()
    },
    { debounce: 250 },
  )

  watch([options.timeRangeParams, options.provider, options.active], () => {
    if (options.active.value)
      void reloadLatest()
    else
      query.invalidate()
  }, { immediate: true })

  onScopeDispose(() => disposed = true)

  return {
    loading: query.loading,
    error: query.error,
    refreshing,
    records: query.items,
    searchQuery,
    pagination,
    handlePageChange,
    handlePageSizeChange,
    refresh,
  }
}
