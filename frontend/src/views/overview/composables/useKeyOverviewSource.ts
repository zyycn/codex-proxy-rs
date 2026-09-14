import type { ClientOverviewResponse } from '@/api'
import type { RequestTrendKind } from '@/views/overview/model/display'
import { useIntervalFn } from '@vueuse/core'
import { computed, onMounted, onScopeDispose, shallowRef } from 'vue'

import { getClientOverview } from '@/api'
import { errorMessage, withMinimumDuration } from '@/utils/async'
import { formatDateTime } from '@/utils/date'
import { dashboardSnapshotView } from '@/views/overview/model/admin'
import { keyOverviewMetrics, keyOverviewTrend } from '@/views/overview/model/key'
import { keyUsageRecordColumns } from '@/views/usage/model/columns'

export function useKeyOverviewSource() {
  const data = shallowRef<ClientOverviewResponse | null>(null)
  const activeTrendKind = shallowRef<RequestTrendKind>('usage')
  const loading = shallowRef(false)
  const refreshing = shallowRef(false)
  const error = shallowRef('')
  const lastRefreshedAt = shallowRef('')
  let controller: AbortController | undefined

  const metrics = computed(() => keyOverviewMetrics(data.value))
  const trend = computed(() => keyOverviewTrend(data.value, activeTrendKind.value))
  const emptyAdminData = dashboardSnapshotView(null)
  const healthTimeline = computed(() => data.value?.healthTimeline ?? emptyAdminData.healthTimeline)
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
    controller?.abort()
    controller = new AbortController()
    const initial = data.value === null
    if (initial)
      loading.value = true
    else
      refreshing.value = true
    error.value = ''
    try {
      data.value = await withMinimumDuration(
        getClientOverview({ silent, signal: controller.signal }),
        initial ? 220 : 0,
      )
      lastRefreshedAt.value = formatDateTime()
    }
    catch (cause: unknown) {
      if (!controller.signal.aborted)
        error.value = errorMessage(cause)
    }
    finally {
      loading.value = false
      refreshing.value = false
    }
  }

  const { pause: stopAutoRefresh, resume: startAutoRefresh } = useIntervalFn(
    () => void load(true),
    30_000,
    { immediate: false },
  )

  onMounted(() => {
    void load()
    startAutoRefresh()
  })

  onScopeDispose(() => {
    stopAutoRefresh()
    controller?.abort()
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
    accountUsage: emptyAdminData.accountUsage,
    wireProfiles,
    budget,
    limits,
    usageRecords,
    poolSummary: emptyAdminData.poolSummary,
    capacityInfo: emptyAdminData.capacityInfo,
    rotationStrategy: emptyAdminData.rotationStrategy,
    showAccountOverview: false,
    usageColumns: keyUsageRecordColumns,
    refresh: () => load(),
    loadTrend,
  }
}
