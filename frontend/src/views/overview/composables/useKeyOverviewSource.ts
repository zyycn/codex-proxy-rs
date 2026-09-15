import type { ClientOverviewResponse } from '@/api'
import type { RequestTrendKind } from '@/views/overview/model/display'
import { useIntervalFn } from '@vueuse/core'
import { computed, onMounted, shallowRef } from 'vue'

import { getClientOverview } from '@/api'
import { useRequestState } from '@/composables/useRequestState'
import { withMinimumDuration } from '@/utils/async'
import { formatDateTime } from '@/utils/date'
import { emptyHealthTimeline } from '@/views/overview/model/health'
import { keyOverviewMetrics, keyOverviewTrend } from '@/views/overview/model/key'
import { keyUsageRecordColumns } from '@/views/usage/model/columns'

export function useKeyOverviewSource() {
  const data = shallowRef<ClientOverviewResponse | null>(null)
  const activeTrendKind = shallowRef<RequestTrendKind>('usage')
  const request = useRequestState()
  const { error } = request
  const loading = computed(() => request.loading.value && data.value === null)
  const refreshing = computed(() => request.loading.value && data.value !== null)
  const lastRefreshedAt = shallowRef('')

  const metrics = computed(() => keyOverviewMetrics(data.value))
  const trend = computed(() => keyOverviewTrend(data.value, activeTrendKind.value))
  const healthTimeline = computed(() => data.value?.healthTimeline ?? emptyHealthTimeline)
  const wireProfiles = computed(() => data.value?.wireProfiles ?? [])
  const budget = computed(() => data.value?.budget ?? null)
  const limits = computed(() => data.value?.limits ?? null)
  const usageRecords = computed(() => data.value?.usageRecords ?? [])
  const trendPoints = computed(() => trend.value.points)
  const trendSummary = computed(() => trend.value.summary)
  const fatalError = computed(() => data.value ? '' : error.value)

  function loadTrend(kind: string) {
    if (kind === 'latency' || kind === 'errors')
      activeTrendKind.value = kind
    else
      activeTrendKind.value = 'usage'
  }

  async function load(silent = false) {
    const requestId = request.start()
    const initial = data.value === null
    try {
      const result = await withMinimumDuration(
        getClientOverview({ silent, signal: request.signal }),
        initial ? 220 : 0,
      )
      if (!request.isCurrent(requestId))
        return
      data.value = result
      lastRefreshedAt.value = formatDateTime()
    }
    catch (cause: unknown) {
      request.fail(requestId, cause)
    }
    finally {
      request.finish(requestId)
    }
  }

  const { resume: startAutoRefresh } = useIntervalFn(
    () => void load(true),
    30_000,
    { immediate: false },
  )

  onMounted(() => {
    void load()
    startAutoRefresh()
  })

  return {
    activeTrendKind,
    lastRefreshedAt,
    loading,
    metrics,
    refreshing,
    trendPoints,
    trendSummary,
    trendLoading: loading,
    trendError: error,
    fatalError,
    healthTimeline,
    accountOverview: null,
    wireProfiles,
    budget,
    limits,
    usageRecords,
    usageColumns: keyUsageRecordColumns,
    refresh: () => load(),
    loadTrend,
  }
}
