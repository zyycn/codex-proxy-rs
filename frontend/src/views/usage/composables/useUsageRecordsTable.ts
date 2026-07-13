import { clamp } from 'es-toolkit'
import { onMounted, shallowRef, watch } from 'vue'

import {
  getUsageRecordInsightsDiagnostics,
  getUsageRecordInsightsOverview,
  getUsageRecordSummary,
  getUsageRecords,
} from '@/api'
import { toast } from '@/components/base/BaseToast'
import { withMinimumDuration } from '@/utils/async'

export function useUsageRecordsTable(options: any) {
  const loading = shallowRef(true)
  const analyticsLoading = shallowRef(true)
  const records = shallowRef<any[]>([])
  const summary = shallowRef(emptySummary())
  const insights = shallowRef(emptyInsights())
  const refreshingList = shallowRef(false)
  const diagnosticDimension = shallowRef('model')
  const diagnosticLoading = shallowRef(false)
  let loadRequestId = 0
  let diagnosticRequestId = 0
  const scopedParams = () => ({ ...options.timeRangeParams.value })
  const filterParams = () => ({
    search: options.searchQuery.value || undefined,
  })

  async function loadUsageRecords(scope = 'all') {
    const requestId = ++loadRequestId
    try {
      loading.value = true
      if (scope === 'all') {
        analyticsLoading.value = true
        diagnosticRequestId += 1
        diagnosticLoading.value = false
      }

      const globalParams = scopedParams()
      const tableParams = {
        ...globalParams,
        ...filterParams(),
      }
      const resultPromise = getUsageRecords({
        page: options.page.value,
        pageSize: options.pageSize.value,
        ...tableParams,
      })
      const analyticsPromise =
        scope === 'all'
          ? loadUsageAnalytics(globalParams)
          : Promise.resolve({
              summary: summary.value,
              insights: insights.value,
            })
      const [result, nextAnalytics] = await Promise.all([resultPromise, analyticsPromise])
      if (requestId !== loadRequestId) return

      records.value = result.items
      summary.value = nextAnalytics.summary
      insights.value = {
        ...nextAnalytics.insights,
        diagnostics:
          nextAnalytics.insights.diagnostics.dimension === diagnosticDimension.value
            ? nextAnalytics.insights.diagnostics
            : insights.value.diagnostics,
      }
      options.pageSize.value = result.page.pageSize ?? options.pageSize.value
      options.totalRecords.value = result.page.total ?? result.items.length
      options.page.value = result.page.page ?? options.page.value

      if (records.value.length === 0 && options.totalRecords.value > 0 && options.page.value > 1) {
        options.page.value = clamp(
          result.page.totalPages ?? options.page.value - 1,
          1,
          Number.POSITIVE_INFINITY,
        )
        await loadUsageRecords(scope)
      }
    } catch (error: any) {
      if (requestId !== loadRequestId) return
      toast.error(error.message || '加载失败')
    } finally {
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
      diagnosticLoading.value = true
      const diagnostics = await getUsageRecordInsightsDiagnostics({
        ...params,
        dimension,
      })
      if (requestId !== diagnosticRequestId || dimension !== diagnosticDimension.value) return
      insights.value = {
        ...insights.value,
        diagnostics,
      }
    } catch (error: any) {
      toast.error(error.message || '加载失败')
    } finally {
      if (requestId === diagnosticRequestId) {
        diagnosticLoading.value = false
      }
    }
  }

  async function refreshUsageRecords() {
    if (refreshingList.value || loading.value) return
    refreshingList.value = true
    try {
      await withMinimumDuration(() => loadUsageRecords('table'))
    } finally {
      refreshingList.value = false
    }
  }

  onMounted(() => {
    loadUsageRecords()
  })

  watch(diagnosticDimension, () => {
    void loadDiagnostics()
  })

  return {
    loading,
    analyticsLoading,
    records,
    summary,
    insights,
    refreshingList,
    diagnosticDimension,
    diagnosticLoading,
    loadUsageRecords,
    refreshUsageRecords,
  }
}

function emptySummary() {
  return {
    totalRequests: '0',
    inputTokens: '0',
    outputTokens: '0',
    cachedTokens: '0',
    totalTokens: '0',
    averageLatencyMs: '—',
  }
}

function emptyInsights() {
  return {
    overview: emptyOverview(),
    diagnostics: emptyDiagnostics(),
  }
}

function emptyOverview() {
  return {
    granularity: '1d',
    health: {
      totalRequests: 0,
      successRequests: 0,
      failedRequests: 0,
      successRate: 0,
      requestChangeRate: null,
      successRateChange: null,
      points: [],
    },
    performance: {
      latencyP50Ms: null,
      latencyP95Ms: null,
      latencyP99Ms: null,
      ttftP50Ms: null,
      ttftP95Ms: null,
      ttftP99Ms: null,
      latencyCoverage: 0,
      ttftCoverage: 0,
      points: [],
    },
    cost: {
      estimatedCost: 0,
      standardCost: 0,
      costPerRequest: 0,
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
}

function emptyDiagnostics() {
  return {
    dimension: 'model',
    items: [],
  }
}
