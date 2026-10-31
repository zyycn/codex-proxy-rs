import type { UsageCostMetric, UsagePerformanceMetric } from '@/api'
import type { RequestOptions } from '@/api/request'
import { computed, shallowRef, watch } from 'vue'
import { getUsageCost, getUsageHealth, getUsagePerformance, getUsageRecordInsightsDiagnostics, getUsageRecordSummary } from '@/api'
import { useRequestState } from '@/composables/useRequestState'

type UsageRange = Parameters<typeof getUsageRecordSummary>[0]

export interface UsageQueryState<T> {
  data: T | null
  loading: boolean
  error: string
}

export function useUsageInsights(scopedParams: () => UsageRange) {
  const performanceMetric = shallowRef<UsagePerformanceMetric>('latency')
  const costMetric = shallowRef<UsageCostMetric>('cost')
  const diagnosticDimension = shallowRef('model')
  const summary = useUsageQuery(getUsageRecordSummary)
  const health = useUsageQuery(getUsageHealth)
  const performance = useUsageQuery((params, options) => getUsagePerformance({ ...params, metric: performanceMetric.value }, options))
  const cost = useUsageQuery((params, options) => getUsageCost({ ...params, metric: costMetric.value }, options))
  const diagnostics = useUsageQuery((params, options) => getUsageRecordInsightsDiagnostics({ ...params, dimension: diagnosticDimension.value }, options))

  watch(performanceMetric, () => void performance.load(scopedParams()))
  watch(costMetric, () => void cost.load(scopedParams()))
  watch(diagnosticDimension, () => void diagnostics.load(scopedParams()))

  function loadUsageAnalytics(params: UsageRange = scopedParams()) {
    // 各查询收到结果即发布，慢指标不会阻塞其他卡片
    return Promise.all([
      summary,
      health,
      performance,
      cost,
      diagnostics,
    ].map(query => query.load(params)))
  }

  return {
    summary: summary.state,
    health: health.state,
    performance: performance.state,
    cost: cost.state,
    diagnostics: diagnostics.state,
    performanceMetric,
    costMetric,
    diagnosticDimension,
    loadUsageAnalytics,
  }
}

function useUsageQuery<T>(fetch: (params: UsageRange, options: RequestOptions) => Promise<T>) {
  const data = shallowRef<T | null>(null)
  const request = useRequestState()
  const state = computed<UsageQueryState<T>>(() => ({ data: data.value, loading: request.loading.value, error: request.error.value }))

  async function load(params: UsageRange) {
    const id = request.start()
    data.value = null
    try {
      const result = await fetch(params, { signal: request.signal })
      if (request.isCurrent(id))
        data.value = result
    }
    catch (cause) {
      request.fail(id, cause)
    }
    finally {
      request.finish(id)
    }
  }

  return { state, load }
}
