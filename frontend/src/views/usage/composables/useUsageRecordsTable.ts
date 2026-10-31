import type { Ref } from 'vue'
import type { UsageTimeRangeParams } from './useUsageTimeRange'
import type { UsageRecordStatus } from '@/api'

import { computed, onMounted, shallowRef, watch } from 'vue'
import { getUsageRecords } from '@/api'
import { useStablePagedQuery } from '@/composables/useStablePagedQuery'
import { withMinimumDuration } from '@/utils/operation'
import { useUsageInsights } from './useUsageInsights'

interface UseUsageRecordsTableOptions {
  timeRangeParams: Readonly<Ref<UsageTimeRangeParams>>
  latestTimeRangeParams: () => UsageTimeRangeParams
  status: Readonly<Ref<UsageRecordStatus>>
}

type UsageLoadScope = 'all' | 'table'
export type UsageRecordFilterField = 'key' | 'account' | 'model'

interface UsageLoadOptions {
  scope?: UsageLoadScope
}

export function useUsageRecordsTable(options: UseUsageRecordsTableOptions) {
  const filterField = shallowRef<UsageRecordFilterField>('key')
  const filterValue = shallowRef('')
  const filterParams = computed(() => {
    const value = filterValue.value.trim() || undefined
    return {
      clientApiKeyName: filterField.value === 'key'
        ? value
        : undefined,
      accountName: filterField.value === 'account'
        ? value
        : undefined,
      model: filterField.value === 'model'
        ? value
        : undefined,
    }
  })
  const providerQuery = shallowRef('')
  let tableParams = snapshot()
  const table = useStablePagedQuery({
    initialPageSize: 10,
    load: (pagination, requestOptions) => getUsageRecords({
      ...pagination,
      ...tableParams,
    }, requestOptions),
  })
  const { currentPage, pageSize, loading, error, items: records, total: totalRecords } = table
  const refreshingList = shallowRef(false)
  const scopedParams = () => ({
    ...options.timeRangeParams.value,
    ...(providerQuery.value
      ? { provider: providerQuery.value }
      : {}),
  })
  const { loadUsageAnalytics, ...insights } = useUsageInsights(scopedParams)
  const usagePagination = computed(() => ({
    currentPage: currentPage.value,
    pageSize: pageSize.value,
    total: totalRecords.value,
  }))

  function snapshot() {
    return {
      ...options.latestTimeRangeParams(),
      provider: providerQuery.value || undefined,
      ...filterParams.value,
      status: options.status.value,
    }
  }

  function filterChanged() {
    const filter = filterParams.value
    return tableParams.clientApiKeyName !== filter.clientApiKeyName
      || tableParams.accountName !== filter.accountName
      || tableParams.model !== filter.model
  }

  function resetTable() {
    tableParams = snapshot()
    // 筛选范围变化后不能继续展示上一范围的数据，即使新查询失败。
    records.value = []
    currentPage.value = 1
    totalRecords.value = 0
  }

  async function loadUsageRecords(loadOptions: UsageLoadOptions = {}) {
    const { scope = 'all' } = loadOptions
    const globalParams = scopedParams()
    if (scope === 'all')
      resetTable()

    await Promise.all([
      table.execute(),
      ...(scope === 'all'
        ? [
            loadUsageAnalytics(globalParams),
          ]
        : []),
    ])
  }

  async function refreshUsageRecords() {
    if (refreshingList.value || loading.value)
      return
    refreshingList.value = true
    try {
      await withMinimumDuration(reloadLatestTable)
    }
    finally {
      refreshingList.value = false
    }
  }

  function reloadLatestTable() {
    resetTable()
    return loadUsageRecords({ scope: 'table' })
  }

  function handlePageChange(nextPage: number) {
    if (filterChanged()) {
      void reloadLatestTable()
      return
    }
    void table.execute(nextPage)
  }

  function handlePageSizeChange(nextPageSize: number) {
    pageSize.value = nextPageSize
    if (filterChanged()) {
      void reloadLatestTable()
      return
    }
    void table.reloadFromStart()
  }

  onMounted(() => {
    loadUsageRecords()
  })

  watch(providerQuery, () => {
    void loadUsageRecords()
  })

  watch(options.status, () => {
    void reloadLatestTable()
  })

  watch(filterField, () => {
    filterValue.value = ''
  }, { flush: 'sync' })

  watch(filterParams, (_value, _previous, onCleanup) => {
    const timer = setTimeout(() => {
      if (filterChanged())
        void reloadLatestTable()
    }, 250)
    onCleanup(() => clearTimeout(timer))
  })

  return {
    currentPage,
    pageSize,
    filterField,
    filterValue,
    providerQuery,
    usagePagination,
    loading,
    error,
    records,
    ...insights,
    refreshingList,
    loadUsageRecords,
    refreshUsageRecords,
    handlePageChange,
    handlePageSizeChange,
  }
}
