import type { Ref } from 'vue'
import type { UsageTimeRangeParams } from '@/views/usage/composables/useUsageTimeRange'
import { watchDebounced } from '@vueuse/core'
import { computed, onMounted, onScopeDispose, shallowRef, watch } from 'vue'

import {
  getClientUsageDiagnostics,
  getClientUsageInsights,
  getClientUsageRecords,
  getClientUsageSummary,
} from '@/api'
import { useStablePagedQuery } from '@/composables/useStablePagedQuery'
import { withMinimumDuration } from '@/utils/async'
import { emptyUsageInsights, emptyUsageSummary } from '@/views/usage/model/state'

export function useKeyUsageSource(options: {
  timeRangeParams: Readonly<Ref<UsageTimeRangeParams>>
  latestTimeRangeParams: () => UsageTimeRangeParams
  active: Readonly<Ref<boolean>>
}) {
  const analyticsLoading = shallowRef(true)
  const summary = shallowRef(emptyUsageSummary())
  const insights = shallowRef(emptyUsageInsights())
  const searchQuery = shallowRef('')
  const providerQuery = shallowRef('')
  const search = computed(() => searchQuery.value.trim() || undefined)
  const refreshingList = shallowRef(false)
  const diagnosticDimension = shallowRef('model')
  let tableParams = snapshot()
  let analyticsRequestId = 0
  let diagnosticRequestId = 0
  let analyticsController: AbortController | undefined
  let diagnosticController: AbortController | undefined
  let disposed = false

  const query = useStablePagedQuery({
    initialPageSize: 10,
    load: (pagination, requestOptions) => getClientUsageRecords({
      ...pagination,
      ...tableParams,
    }, requestOptions),
  })
  const { currentPage, pageSize, loading, items: records, error: tableError } = query
  const usagePagination = computed(() => ({
    currentPage: currentPage.value,
    pageSize: pageSize.value,
    total: query.total.value,
  }))

  function snapshot() {
    return {
      ...options.latestTimeRangeParams(),
      model: search.value,
    }
  }

  async function loadUsageRecords(loadOptions: { scope?: 'all' | 'table', background?: boolean } = {}) {
    const { scope = 'all', background = false } = loadOptions
    if (scope === 'all')
      tableParams = snapshot()
    await Promise.all([
      ...(options.active.value
        ? [scope === 'all' ? query.reloadFromStart({ background }) : query.execute(currentPage.value, { background })]
        : []),
      ...(scope === 'all' ? [loadUsageAnalytics(options.timeRangeParams.value, background)] : []),
    ])
  }

  async function loadUsageAnalytics(range: UsageTimeRangeParams, background: boolean) {
    const requestId = ++analyticsRequestId
    const diagnosticsId = ++diagnosticRequestId
    analyticsController?.abort()
    diagnosticController?.abort()
    const controller = new AbortController()
    analyticsController = controller
    const dimension = diagnosticDimension.value
    analyticsLoading.value = !background
    try {
      const [nextSummary, overview, diagnostics] = await Promise.all([
        getClientUsageSummary(range, { signal: controller.signal }),
        getClientUsageInsights(range, { signal: controller.signal }),
        getClientUsageDiagnostics({ ...range, dimension }, { signal: controller.signal }),
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
      if (requestId === analyticsRequestId)
        analyticsLoading.value = false
    }
  }

  async function loadDiagnostics() {
    const requestId = ++diagnosticRequestId
    diagnosticController?.abort()
    const controller = new AbortController()
    diagnosticController = controller
    const dimension = diagnosticDimension.value
    try {
      const diagnostics = await getClientUsageDiagnostics({
        ...options.timeRangeParams.value,
        dimension,
      }, { signal: controller.signal })
      if (requestId === diagnosticRequestId && dimension === diagnosticDimension.value) {
        insights.value = { ...insights.value, diagnostics }
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
    return query.reloadFromStart()
  }

  function handlePageChange(nextPage: number) {
    if (tableParams.model !== search.value) {
      void reloadLatestTable()
      return
    }
    void query.execute(nextPage)
  }

  function handlePageSizeChange(nextPageSize: number) {
    pageSize.value = nextPageSize
    if (tableParams.model !== search.value) {
      void reloadLatestTable()
      return
    }
    void query.reloadFromStart()
  }

  onMounted(() => void loadUsageRecords())

  watch(diagnosticDimension, () => void loadDiagnostics())
  watchDebounced(
    search,
    () => {
      if (!disposed && options.active.value && tableParams.model !== search.value)
        void reloadLatestTable()
    },
    { debounce: 250 },
  )
  watch(options.active, (active) => {
    if (active) {
      void reloadLatestTable()
    }
    else {
      query.invalidate()
    }
  })

  onScopeDispose(() => {
    disposed = true
    analyticsRequestId += 1
    diagnosticRequestId += 1
    analyticsController?.abort()
    diagnosticController?.abort()
  })

  return {
    currentPage,
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
