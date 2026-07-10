import { clamp } from 'es-toolkit'
import { onMounted, ref, watch, type Ref } from 'vue'

import {
  getUsageRecordEndpointDistribution,
  getUsageRecordLatencyTrend,
  getUsageRecordModelDistribution,
  getUsageRecordSummary,
  getUsageRecordTokenTrend,
  getUsageRecords,
} from '@/api'
import { toast } from '@/components/base/BaseToast'
import { withMinimumDuration } from '@/utils/async'

export function useUsageRecordsTable(options: {
  page: Ref<number>
  pageSize: Ref<number>
  searchQuery: Ref<string>
  timeRangeParams: Ref<Record<string, string>>
  totalRecords: Ref<number>
}) {
  const loading = ref(true)
  const analyticsLoading = ref(true)
  const records = ref<any[]>([])
  const summary = ref(emptySummary())
  const insights = ref(emptyInsights())
  const refreshingList = ref(false)
  const modelDistributionSource = ref('requested')
  const endpointDistributionSource = ref('inbound')
  let modelDistributionRequestId = 0
  let endpointDistributionRequestId = 0
  const scopedParams = () => ({ ...options.timeRangeParams.value })
  const filterParams = () => ({
    search: options.searchQuery.value || undefined,
  })

  async function loadUsageRecords(scope: 'all' | 'table' = 'all') {
    try {
      loading.value = true
      if (scope === 'all') {
        analyticsLoading.value = true
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

      records.value = result.items
      summary.value = nextAnalytics.summary
      insights.value = nextAnalytics.insights
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
      toast.error(error.message || '加载失败')
    } finally {
      loading.value = false
      if (scope === 'all') {
        analyticsLoading.value = false
      }
    }
  }

  async function loadUsageAnalytics(globalParams = scopedParams()) {
    const [nextSummary, modelDistribution, endpointDistribution, tokenTrend, latencyTrend] =
      await Promise.all([
        getUsageRecordSummary(globalParams),
        getUsageRecordModelDistribution({
          ...globalParams,
          source: modelDistributionSource.value,
        }),
        getUsageRecordEndpointDistribution({
          ...globalParams,
          source: endpointDistributionSource.value,
        }),
        getUsageRecordTokenTrend(globalParams),
        getUsageRecordLatencyTrend(globalParams),
      ])

    return {
      summary: nextSummary,
      insights: {
        ...emptyInsights(),
        modelDistribution,
        endpointDistribution,
        tokenTrend,
        latencyTrend,
      },
    }
  }

  async function loadModelDistribution() {
    const requestId = ++modelDistributionRequestId
    try {
      const modelDistribution = await getUsageRecordModelDistribution({
        ...scopedParams(),
        source: modelDistributionSource.value,
      })
      if (requestId !== modelDistributionRequestId) return
      insights.value = {
        ...insights.value,
        modelDistribution,
      }
    } catch (error: any) {
      toast.error(error.message || '加载失败')
    }
  }

  async function loadEndpointDistribution() {
    const requestId = ++endpointDistributionRequestId
    try {
      const endpointDistribution = await getUsageRecordEndpointDistribution({
        ...scopedParams(),
        source: endpointDistributionSource.value,
      })
      if (requestId !== endpointDistributionRequestId) return
      insights.value = {
        ...insights.value,
        endpointDistribution,
      }
    } catch (error: any) {
      toast.error(error.message || '加载失败')
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

  watch(modelDistributionSource, () => {
    void loadModelDistribution()
  })

  watch(endpointDistributionSource, () => {
    void loadEndpointDistribution()
  })

  return {
    loading,
    analyticsLoading,
    records,
    summary,
    insights,
    refreshingList,
    modelDistributionSource,
    endpointDistributionSource,
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
    modelDistribution: [],
    endpointDistribution: [],
    tokenTrend: [],
    latencyTrend: [],
  }
}
