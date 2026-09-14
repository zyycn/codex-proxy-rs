import type { Ref } from 'vue'
import type { UsageTimeRangeParams } from '@/views/usage/composables/useUsageTimeRange'
import type { UsageDisplayRecord } from '@/views/usage/model/records'
import { watchDebounced } from '@vueuse/core'

import { computed, onMounted, onScopeDispose, shallowRef, watch } from 'vue'
import {
  getUsageRecordInsightsDiagnostics,
  getUsageRecordInsightsOverview,
  getUsageRecords,
  getUsageRecordSummary,
} from '@/api'
import { errorMessage, withMinimumDuration } from '@/utils/async'
import { emptyUsageInsights, emptyUsageSummary } from '@/views/usage/model/state'

interface UseUsageRecordsTableOptions {
  timeRangeParams: Readonly<Ref<UsageTimeRangeParams>>
  latestTimeRangeParams: () => UsageTimeRangeParams
  active: Readonly<Ref<boolean>>
}

type UsageLoadScope = 'all' | 'table'

export interface UsageLoadOptions {
  scope?: UsageLoadScope
  background?: boolean
}

export function useAdminUsageSource(options: UseUsageRecordsTableOptions) {
  const loading = shallowRef(true)
  const analyticsLoading = shallowRef(true)
  const records = shallowRef<UsageDisplayRecord[]>([])
  const summary = shallowRef(emptyUsageSummary())
  const insights = shallowRef(emptyUsageInsights())
  const currentPage = shallowRef(1)
  const pageSize = shallowRef(10)
  const totalRecords = shallowRef(0)
  const searchQuery = shallowRef('')
  const search = computed(() => searchQuery.value.trim() || undefined)
  const providerQuery = shallowRef('')
  let tableParams = snapshot()
  const refreshingList = shallowRef(false)
  const diagnosticDimension = shallowRef('model')
  const tableError = shallowRef('')
  let tableRequestId = 0
  let analyticsRequestId = 0
  let diagnosticRequestId = 0
  let tableController: AbortController | undefined
  let analyticsController: AbortController | undefined
  let diagnosticController: AbortController | undefined
  let disposed = false
  const scopedParams = () => ({
    ...options.timeRangeParams.value,
    ...(providerQuery.value ? { provider: providerQuery.value } : {}),
  })
  const usagePagination = computed(() => ({
    currentPage: currentPage.value,
    pageSize: pageSize.value,
    total: totalRecords.value,
  }))

  function snapshot() {
    return {
      ...options.latestTimeRangeParams(),
      provider: providerQuery.value || undefined,
      search: search.value,
    }
  }

  function resetPagination() {
    currentPage.value = 1
    totalRecords.value = 0
  }

  async function loadUsageRecords(loadOptions: UsageLoadOptions = {}) {
    const { scope = 'all', background = false } = loadOptions
    const globalParams = scopedParams()
    if (scope === 'all') {
      resetPagination()
      tableParams = snapshot()
    }

    await Promise.all([
      ...(options.active.value ? [loadUsagePage(background)] : []),
      ...(scope === 'all' ? [loadUsageAnalytics(globalParams, background)] : []),
    ])
  }

  async function loadUsagePage(background: boolean) {
    const requestId = ++tableRequestId
    tableController?.abort()
    tableController = new AbortController()
    loading.value = !background
    tableError.value = ''
    try {
      const result = await getUsageRecords({
        currentPage: currentPage.value,
        pageSize: pageSize.value,
        ...tableParams,
      }, { signal: tableController.signal })
      if (requestId !== tableRequestId)
        return

      records.value = result.items
      pageSize.value = result.pageSize
      totalRecords.value = result.total
      currentPage.value = result.currentPage
    }
    catch (cause: unknown) {
      if (requestId === tableRequestId && !tableController.signal.aborted)
        tableError.value = errorMessage(cause)
    }
    finally {
      if (requestId === tableRequestId) {
        loading.value = false
      }
    }
  }

  async function loadUsageAnalytics(globalParams: ReturnType<typeof scopedParams>, background: boolean) {
    const requestId = ++analyticsRequestId
    const diagnosticsId = ++diagnosticRequestId
    analyticsController?.abort()
    diagnosticController?.abort()
    analyticsController = new AbortController()
    const requestOptions = { signal: analyticsController.signal }
    const dimension = diagnosticDimension.value
    analyticsLoading.value = !background
    try {
      const [nextSummary, overview, diagnostics] = await Promise.all([
        getUsageRecordSummary(globalParams, requestOptions),
        getUsageRecordInsightsOverview(globalParams, requestOptions),
        getUsageRecordInsightsDiagnostics({
          ...globalParams,
          dimension,
        }, requestOptions),
      ])
      if (requestId !== analyticsRequestId)
        return

      summary.value = nextSummary
      insights.value = {
        overview,
        diagnostics:
          diagnosticsId === diagnosticRequestId && dimension === diagnosticDimension.value
            ? diagnostics
            : insights.value.diagnostics,
      }
    }
    catch {}
    finally {
      if (requestId === analyticsRequestId) {
        analyticsLoading.value = false
      }
    }
  }

  async function loadDiagnostics() {
    const requestId = ++diagnosticRequestId
    diagnosticController?.abort()
    diagnosticController = new AbortController()
    const dimension = diagnosticDimension.value
    const params = scopedParams()
    try {
      const diagnostics = await getUsageRecordInsightsDiagnostics({
        ...params,
        dimension,
      }, { signal: diagnosticController.signal })
      if (requestId !== diagnosticRequestId || dimension !== diagnosticDimension.value)
        return
      insights.value = {
        ...insights.value,
        diagnostics,
      }
    }
    catch {}
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
    tableParams = snapshot()
    resetPagination()
    return loadUsageRecords({ scope: 'table' })
  }

  function handlePageChange(nextPage: number) {
    if (tableParams.search !== search.value) {
      void reloadLatestTable()
      return
    }
    currentPage.value = nextPage
    void loadUsageRecords({ scope: 'table' })
  }

  function handlePageSizeChange(nextPageSize: number) {
    pageSize.value = nextPageSize
    if (tableParams.search !== search.value) {
      void reloadLatestTable()
      return
    }
    resetPagination()
    void loadUsageRecords({ scope: 'table' })
  }

  onMounted(() => {
    loadUsageRecords()
  })

  watch(diagnosticDimension, () => {
    void loadDiagnostics()
  })

  watch(providerQuery, () => {
    void loadUsageRecords({ background: true })
  })

  watch(options.active, (active) => {
    if (active) {
      void reloadLatestTable()
    }
    else {
      tableRequestId += 1
      tableController?.abort()
    }
  })

  watchDebounced(
    search,
    () => {
      if (!disposed && options.active.value && tableParams.search !== search.value)
        void reloadLatestTable()
    },
    { debounce: 250 },
  )

  onScopeDispose(() => {
    disposed = true
    tableRequestId += 1
    analyticsRequestId += 1
    diagnosticRequestId += 1
    tableController?.abort()
    analyticsController?.abort()
    diagnosticController?.abort()
  })

  return {
    currentPage,
    pageSize,
    searchQuery,
    providerQuery,
    usagePagination,
    loading,
    analyticsLoading,
    records,
    summary,
    insights,
    refreshingList,
    diagnosticDimension,
    tableError,
    loadUsageRecords,
    refreshUsageRecords,
    handlePageChange,
    handlePageSizeChange,
  }
}
