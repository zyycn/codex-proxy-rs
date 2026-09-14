import type { Ref } from 'vue'
import type { UsageTimeRangeParams } from './useUsageTimeRange'
import { watchDebounced } from '@vueuse/core'

import { computed, onScopeDispose, shallowRef, watch } from 'vue'
import { getClientOpsErrors, getOpsErrors } from '@/api'
import { useStablePagedQuery } from '@/composables/useStablePagedQuery'
import { useAuthStore } from '@/stores/modules/auth'

import { withMinimumDuration } from '@/utils/async'
import { adminUsageError, keyUsageError } from '../model/errors'

interface UseUsageErrorsOptions {
  timeRangeParams: Readonly<Ref<UsageTimeRangeParams>>
  latestTimeRangeParams: () => UsageTimeRangeParams
  provider: Readonly<Ref<string>>
  active: Readonly<Ref<boolean>>
}

interface ErrorQuerySnapshot {
  startTime: string
  endTime: string
  provider?: string
  search?: string
}

export function useUsageErrors(options: UseUsageErrorsOptions) {
  const authStore = useAuthStore()
  const isAdmin = authStore.session?.type === 'admin'
  const refreshing = shallowRef(false)
  const searchQuery = shallowRef('')
  const search = computed(() => searchQuery.value.trim() || undefined)
  let disposed = false
  let tableParams = snapshot()

  function snapshot(): ErrorQuerySnapshot {
    return {
      ...options.latestTimeRangeParams(),
      provider: isAdmin ? options.provider.value || undefined : undefined,
      search: search.value,
    }
  }

  const query = useStablePagedQuery({
    initialPageSize: 10,
    async load({ currentPage, pageSize }, requestOptions) {
      if (isAdmin) {
        const result = await getOpsErrors({
          currentPage,
          pageSize,
          ...tableParams,
        }, requestOptions)
        return { ...result, items: result.items.map(adminUsageError) }
      }

      const result = await getClientOpsErrors({
        currentPage,
        pageSize,
        startTime: tableParams.startTime,
        endTime: tableParams.endTime,
        model: tableParams.search,
      }, requestOptions)
      return { ...result, items: result.items.map(keyUsageError) }
    },
  })

  const pagination = computed(() => ({
    currentPage: query.currentPage.value,
    pageSize: query.pageSize.value,
    total: query.total.value,
  }))

  function reloadLatest() {
    tableParams = snapshot()
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

  function handlePageChange(page: number) {
    if (tableParams.search !== search.value) {
      void reloadLatest()
      return
    }
    void query.execute(page)
  }

  function handlePageSizeChange(pageSize: number) {
    query.pageSize.value = pageSize
    if (tableParams.search !== search.value)
      void reloadLatest()
    else
      void query.reloadFromStart()
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
    isAdmin,
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
