import type { Ref } from 'vue'
import type { UsageTimeRangeParams } from './useUsageTimeRange'

import { computed, shallowRef, watch } from 'vue'
import { getOpsErrors } from '@/api'
import { useStablePagedQuery } from '@/composables/useStablePagedQuery'
import { withMinimumDuration } from '@/utils/operation'

interface UseOpsErrorsTableOptions {
  timeRangeParams: Readonly<Ref<UsageTimeRangeParams>>
  latestTimeRangeParams: () => UsageTimeRangeParams
  provider: Readonly<Ref<string>>
  active: Readonly<Ref<boolean>>
}

export function useOpsErrorsTable(options: UseOpsErrorsTableOptions) {
  const refreshing = shallowRef(false)
  const searchQuery = shallowRef('')
  const search = computed(() => searchQuery.value.trim() || undefined)
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
    load: ({ currentPage, pageSize }, requestOptions) => getOpsErrors({
      currentPage,
      pageSize,
      ...tableParams,
    }, requestOptions),
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

  watch(search, (_value, _previous, onCleanup) => {
    const timer = setTimeout(() => {
      if (options.active.value && tableParams.search !== search.value)
        void reloadLatest()
    }, 250)
    onCleanup(() => clearTimeout(timer))
  })

  watch([options.timeRangeParams, options.provider, options.active], () => {
    if (options.active.value)
      void reloadLatest()
    else
      query.invalidate()
  }, { immediate: true })

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
