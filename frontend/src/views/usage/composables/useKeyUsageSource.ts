import type { Ref } from 'vue'
import type { ClientUsageRecord } from '@/api'
import type { UsageTimeRangeParams } from '@/views/usage/composables/useUsageTimeRange'
import { watchDebounced } from '@vueuse/core'
import { computed, onMounted, onScopeDispose, shallowRef, watch } from 'vue'

import {
  getClientUsageDiagnostics,
  getClientUsageInsights,
  getClientUsageRecords,
  getClientUsageSummary,
} from '@/api'
import { errorMessage, withMinimumDuration } from '@/utils/async'
import { emptyUsageInsights, emptyUsageSummary } from '@/views/usage/model/state'

export function useKeyUsageSource(options: {
  timeRangeParams: Readonly<Ref<UsageTimeRangeParams>>
  latestTimeRangeParams: () => UsageTimeRangeParams
  active: Readonly<Ref<boolean>>
}) {
  const loading = shallowRef(true)
  const analyticsLoading = shallowRef(true)
  const records = shallowRef<ClientUsageRecord[]>([])
  const summary = shallowRef(emptyUsageSummary())
  const insights = shallowRef(emptyUsageInsights())
  const currentPage = shallowRef(1)
  const pageSize = shallowRef(10)
  const totalRecords = shallowRef(0)
  const searchQuery = shallowRef('')
  const providerQuery = shallowRef('')
  const search = computed(() => searchQuery.value.trim() || undefined)
  const refreshingList = shallowRef(false)
  const diagnosticDimension = shallowRef('model')
  const tableError = shallowRef('')
  let tableParams = snapshot()
  let tableRequestId = 0
  let analyticsRequestId = 0
  let diagnosticRequestId = 0
  let tableController: AbortController | undefined
  let analyticsController: AbortController | undefined
  let diagnosticController: AbortController | undefined
  let disposed = false

  const usagePagination = computed(() => ({
    currentPage: currentPage.value,
    pageSize: pageSize.value,
    total: totalRecords.value,
  }))

  function snapshot() {
    return {
      ...options.latestTimeRangeParams(),
      model: search.value,
    }
  }

  function resetPagination() {
    currentPage.value = 1
    totalRecords.value = 0
  }

  async function loadUsageRecords(loadOptions: { scope?: 'all' | 'table', background?: boolean } = {}) {
    const { scope = 'all', background = false } = loadOptions
    if (scope === 'all') {
      resetPagination()
      tableParams = snapshot()
    }
    await Promise.all([
      ...(options.active.value ? [loadUsagePage(background)] : []),
      ...(scope === 'all' ? [loadUsageAnalytics(options.timeRangeParams.value, background)] : []),
    ])
  }

  async function loadUsagePage(background: boolean) {
    const requestId = ++tableRequestId
    tableController?.abort()
    const controller = new AbortController()
    tableController = controller
    loading.value = !background
    tableError.value = ''
    try {
      const result = await getClientUsageRecords({
        currentPage: currentPage.value,
        pageSize: pageSize.value,
        ...tableParams,
      }, { signal: controller.signal })
      if (requestId !== tableRequestId)
        return
      records.value = result.items
      pageSize.value = result.pageSize
      totalRecords.value = result.total
      currentPage.value = result.currentPage
    }
    catch (cause: unknown) {
      if (requestId === tableRequestId && !controller.signal.aborted)
        tableError.value = errorMessage(cause)
    }
    finally {
      if (requestId === tableRequestId)
        loading.value = false
    }
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
    resetPagination()
    return loadUsageRecords({ scope: 'table' })
  }

  function handlePageChange(nextPage: number) {
    if (tableParams.model !== search.value) {
      void reloadLatestTable()
      return
    }
    currentPage.value = nextPage
    void loadUsageRecords({ scope: 'table' })
  }

  function handlePageSizeChange(nextPageSize: number) {
    pageSize.value = nextPageSize
    if (tableParams.model !== search.value) {
      void reloadLatestTable()
      return
    }
    resetPagination()
    void loadUsageRecords({ scope: 'table' })
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
      tableRequestId += 1
      tableController?.abort()
    }
  })

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
