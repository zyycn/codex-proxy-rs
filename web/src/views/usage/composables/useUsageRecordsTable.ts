import { clamp } from 'es-toolkit'
import { onMounted, ref, type Ref } from 'vue'

import { getUsageRecordInsights, getUsageRecordSummary, getUsageRecords } from '@/api'
import { toast } from '@/components/base/BaseToast'
import { withMinimumDuration } from '@/utils/async'

export function useUsageRecordsTable(options: {
  page: Ref<number>
  pageSize: Ref<number>
  searchQuery: Ref<string>
  filterStatus: Ref<string>
  timeRangeParams: Ref<Record<string, string>>
  totalRecords: Ref<number>
}) {
  const loading = ref(true)
  const analyticsLoading = ref(true)
  const records = ref<any[]>([])
  const summary = ref(emptySummary())
  const insights = ref(emptyInsights())
  const refreshingList = ref(false)
  const filterParams = () => ({
    level: options.filterStatus.value || undefined,
    search: options.searchQuery.value || undefined,
  })

  async function loadUsageRecords(scope: 'all' | 'table' = 'all') {
    try {
      loading.value = true
      if (scope === 'all') {
        analyticsLoading.value = true
      }

      const globalParams = options.timeRangeParams.value
      const tableParams = {
        ...globalParams,
        ...filterParams(),
      }
      const resultPromise = getUsageRecords({
        page: options.page.value,
        pageSize: options.pageSize.value,
        ...tableParams,
      })
      const [result, nextSummary, nextInsights] =
        scope === 'all'
          ? await Promise.all([
              resultPromise,
              getUsageRecordSummary(globalParams),
              getUsageRecordInsights(globalParams),
            ])
          : await Promise.all([
              resultPromise,
              Promise.resolve(summary.value),
              Promise.resolve(insights.value),
            ])

      records.value = result.items
      summary.value = nextSummary
      insights.value = nextInsights
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

  async function refreshUsageRecords() {
    if (refreshingList.value || loading.value) return
    refreshingList.value = true
    try {
      await withMinimumDuration(loadUsageRecords)
    } finally {
      refreshingList.value = false
    }
  }

  onMounted(() => {
    loadUsageRecords()
  })

  return {
    loading,
    analyticsLoading,
    records,
    summary,
    insights,
    refreshingList,
    loadUsageRecords,
    refreshUsageRecords,
  }
}

function emptySummary() {
  return {
    totalRequests: 0,
    errorRequests: 0,
    inputTokens: 0,
    outputTokens: 0,
    cachedTokens: 0,
    totalTokens: 0,
    averageLatencyMs: null,
  }
}

function emptyInsights() {
  return {
    models: [],
    upstreamModels: [],
    modelMappings: [],
    endpoints: [],
    upstreamEndpoints: [],
    endpointPaths: [],
    types: [],
    trend: [],
  }
}
