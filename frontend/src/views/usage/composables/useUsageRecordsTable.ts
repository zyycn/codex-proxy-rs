import type { Ref } from 'vue'
import type { UsageTimeRangeParams } from './useUsageTimeRange'
import { watchDebounced } from '@vueuse/core'
import { clamp } from 'es-toolkit'

import { computed, onMounted, shallowRef, watch } from 'vue'
import {
  getUsageRecordInsightsDiagnostics,
  getUsageRecordInsightsOverview,
  getUsageRecords,
  getUsageRecordSummary,
} from '@/api'
import { toast } from '@/components/base/BaseToast'

import { errorMessage, withMinimumDuration } from '@/utils/async'

interface UseUsageRecordsTableOptions {
  timeRangeParams: Readonly<Ref<UsageTimeRangeParams>>
  latestTimeRangeParams: () => UsageTimeRangeParams
}

type UsageLoadScope = 'all' | 'table'

export function useUsageRecordsTable(options: UseUsageRecordsTableOptions) {
  const loading = shallowRef(true)
  const analyticsLoading = shallowRef(true)
  const records = shallowRef<Awaited<ReturnType<typeof getUsageRecords>>['items']>([])
  const summary = shallowRef(emptySummary())
  const insights = shallowRef(emptyInsights())
  const page = shallowRef(1)
  const pageSize = shallowRef(10)
  const totalRecords = shallowRef(0)
  const searchQuery = shallowRef('')
  const providerQuery = shallowRef('')
  const tableTimeRangeParams = shallowRef<UsageTimeRangeParams>({
    ...options.timeRangeParams.value,
  })
  const refreshingList = shallowRef(false)
  const diagnosticDimension = shallowRef('model')
  let loadRequestId = 0
  let diagnosticRequestId = 0
  const scopedParams = () => ({
    ...options.timeRangeParams.value,
    ...(providerQuery.value ? { provider: providerQuery.value } : {}),
  })
  const filterParams = () => ({
    search: searchQuery.value || undefined,
  })
  const usagePagination = computed(() => ({
    page: page.value,
    pageSize: pageSize.value,
    total: totalRecords.value,
    pageSizes: [10, 20, 50, 100],
  }))

  async function loadUsageRecords(scope: UsageLoadScope = 'all') {
    const requestId = ++loadRequestId
    try {
      loading.value = true
      if (scope === 'all') {
        analyticsLoading.value = true
        diagnosticRequestId += 1
      }

      const globalParams = scopedParams()
      if (scope === 'all') {
        tableTimeRangeParams.value = { ...globalParams }
      }
      const tableParams = {
        ...tableTimeRangeParams.value,
        ...filterParams(),
      }
      const resultPromise = getUsageRecords({
        page: page.value,
        pageSize: pageSize.value,
        ...tableParams,
      })
      const analyticsPromise
        = scope === 'all'
          ? loadUsageAnalytics(globalParams)
          : Promise.resolve({
              summary: summary.value,
              insights: insights.value,
            })
      const [result, nextAnalytics] = await Promise.all([resultPromise, analyticsPromise])
      if (requestId !== loadRequestId)
        return

      records.value = result.items
      summary.value = nextAnalytics.summary
      insights.value = {
        ...nextAnalytics.insights,
        diagnostics:
          nextAnalytics.insights.diagnostics.dimension === diagnosticDimension.value
            ? nextAnalytics.insights.diagnostics
            : insights.value.diagnostics,
      }
      pageSize.value = result.page.pageSize
      totalRecords.value = result.page.total
      page.value = result.page.page

      if (records.value.length === 0 && totalRecords.value > 0 && page.value > 1) {
        page.value = clamp(result.page.totalPages, 1, Number.POSITIVE_INFINITY)
        await loadUsageRecords(scope)
      }
    }
    catch (error: unknown) {
      if (requestId !== loadRequestId)
        return
      toast.error(errorMessage(error, '加载失败'))
    }
    finally {
      if (requestId === loadRequestId) {
        loading.value = false
        if (scope === 'all') {
          analyticsLoading.value = false
        }
      }
    }
  }

  async function loadUsageAnalytics(globalParams = scopedParams()) {
    const dimension = diagnosticDimension.value
    const [nextSummary, overview, diagnostics] = await Promise.all([
      getUsageRecordSummary(globalParams),
      getUsageRecordInsightsOverview(globalParams),
      getUsageRecordInsightsDiagnostics({
        ...globalParams,
        dimension,
      }),
    ])

    return {
      summary: nextSummary,
      insights: { overview, diagnostics },
    }
  }

  async function loadDiagnostics() {
    const requestId = ++diagnosticRequestId
    const dimension = diagnosticDimension.value
    const params = scopedParams()
    try {
      const diagnostics = await getUsageRecordInsightsDiagnostics({
        ...params,
        dimension,
      })
      if (requestId !== diagnosticRequestId || dimension !== diagnosticDimension.value)
        return
      insights.value = {
        ...insights.value,
        diagnostics,
      }
    }
    catch (error: unknown) {
      toast.error(errorMessage(error, '加载失败'))
    }
  }

  async function refreshUsageRecords() {
    if (refreshingList.value || loading.value)
      return
    refreshingList.value = true
    try {
      tableTimeRangeParams.value = options.latestTimeRangeParams()
      await withMinimumDuration(() => loadUsageRecords('table'))
    }
    finally {
      refreshingList.value = false
    }
  }

  function handlePageChange(nextPage: number) {
    page.value = nextPage
    void loadUsageRecords('table')
  }

  function handlePageSizeChange(nextPageSize: number) {
    pageSize.value = nextPageSize
    page.value = 1
    void loadUsageRecords('table')
  }

  onMounted(() => {
    loadUsageRecords()
  })

  watch(diagnosticDimension, () => {
    void loadDiagnostics()
  })

  watch(providerQuery, () => {
    page.value = 1
    void loadUsageRecords('all')
  })

  watchDebounced(
    searchQuery,
    () => {
      page.value = 1
      void loadUsageRecords('table')
    },
    { debounce: 250 },
  )

  return {
    page,
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
    loadUsageRecords,
    refreshUsageRecords,
    handlePageChange,
    handlePageSizeChange,
  }
}

function emptySummary() {
  const summary: Awaited<ReturnType<typeof getUsageRecordSummary>> = {
    totalRequests: '0',
    inputTokens: '0',
    outputTokens: '0',
    cachedTokens: '0',
    cacheWriteTokens: '0',
    totalTokens: '0',
    averageLatencyMs: '—',
  }
  return summary
}

function emptyInsights() {
  return {
    overview: emptyOverview(),
    diagnostics: emptyDiagnostics(),
  }
}

function emptyOverview() {
  const overview: Awaited<ReturnType<typeof getUsageRecordInsightsOverview>> = {
    granularity: '1d',
    health: {
      totalRequests: 0,
      successRequests: 0,
      failedRequests: 0,
      cancelledRequests: 0,
      incompleteRequests: 0,
      callerErrorRequests: 0,
      successRate: 0,
      requestChangeRate: null,
      successRateChange: null,
      points: [],
    },
    performance: {
      latencyP50Ms: null,
      latencyP95Ms: null,
      latencyP99Ms: null,
      firstTokenP50Ms: null,
      firstTokenP95Ms: null,
      firstTokenP99Ms: null,
      latencyCoverage: 0,
      firstTokenCoverage: 0,
      points: [],
    },
    cost: {
      estimatedCost: null,
      standardCost: null,
      costPerRequest: null,
      tokensPerRequest: 0,
      cachedTokenRate: 0,
      cacheHitRequestRate: 0,
      inputTokens: 0,
      outputTokens: 0,
      cachedTokens: 0,
      totalTokens: 0,
      points: [],
    },
  }
  return overview
}

function emptyDiagnostics() {
  const diagnostics: Awaited<ReturnType<typeof getUsageRecordInsightsDiagnostics>> = {
    dimension: 'model',
    items: [],
  }
  return diagnostics
}
