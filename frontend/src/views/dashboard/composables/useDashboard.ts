import { useIntervalFn } from '@vueuse/core'
import { computed, onMounted, shallowRef } from 'vue'

import { getDashboardSummary, getDashboardTrend } from '@/api'
import { withMinimumDuration } from '@/utils/async'
import { formatDateTime } from '@/utils/date'

import {
  dashboardSnapshotView,
  dashboardTrendView,
  normalizeDashboardTrendKind,
} from './presenter'

export function useDashboard() {
  const activeTrendKind = shallowRef(normalizeDashboardTrendKind('usage'))
  const snapshot = shallowRef(dashboardSnapshotView(null))
  const trend = shallowRef(dashboardTrendView(null))
  const loading = shallowRef(false)
  const refreshing = shallowRef(false)
  const trendLoading = shallowRef(false)
  const lastRefreshedAt = shallowRef('')
  let trendRequestId = 0

  const metrics = computed(() => snapshot.value.metrics)
  const healthTimeline = computed(() => snapshot.value.healthTimeline)
  const accountUsage = computed(() => snapshot.value.accountUsage)
  const wireProfile = computed(() => snapshot.value.wireProfile)
  const usageRecords = computed(() => snapshot.value.usageRecords)
  const poolSummary = computed(() => snapshot.value.poolSummary)
  const capacityInfo = computed(() => snapshot.value.capacityInfo)
  const rotationStrategy = computed(() => snapshot.value.rotationStrategy)
  const trendPoints = computed(() => trend.value.points)
  const trendSummary = computed(() => trend.value.summary)

  const { resume: startAutoRefresh } = useIntervalFn(
    () => {
      void loadDashboardData()
    },
    30_000,
    { immediate: false },
  )

  async function loadDashboardData() {
    if (loading.value || refreshing.value)
      return
    try {
      loading.value = true
      await loadDashboardSnapshot()
    }
    catch {
      // 自动刷新会继续重试，保留最后一次成功快照。
    }
    finally {
      loading.value = false
    }
  }

  async function refreshDashboardData() {
    if (loading.value || refreshing.value)
      return
    refreshing.value = true
    try {
      await withMinimumDuration(loadDashboardSnapshot)
    }
    catch {
      // 手动刷新失败时保留当前数据，不打断概览操作。
    }
    finally {
      refreshing.value = false
    }
  }

  async function loadTrend(kind: string) {
    const trendKind = normalizeDashboardTrendKind(kind)
    activeTrendKind.value = trendKind
    const requestId = ++trendRequestId
    try {
      trendLoading.value = true
      const result = await getDashboardTrend({ kind: trendKind })
      if (isCurrentTrendRequest(requestId, trendKind))
        trend.value = dashboardTrendView(result)
    }
    catch {
      // 趋势请求失败时保留当前趋势，下一次刷新继续尝试。
    }
    finally {
      if (isCurrentTrendRequest(requestId, trendKind))
        trendLoading.value = false
    }
  }

  async function loadDashboardSnapshot() {
    const trendKind = activeTrendKind.value
    const requestId = ++trendRequestId
    try {
      const summary = await getDashboardSummary({ kind: trendKind })
      snapshot.value = dashboardSnapshotView(summary)
      lastRefreshedAt.value = formatDateTime()
      if (isCurrentTrendRequest(requestId, trendKind)) {
        trend.value = dashboardTrendView(summary.trend)
      }
    }
    finally {
      if (isCurrentTrendRequest(requestId, trendKind))
        trendLoading.value = false
    }
  }

  function isCurrentTrendRequest(
    requestId: number,
    kind: ReturnType<typeof normalizeDashboardTrendKind>,
  ) {
    return requestId === trendRequestId && activeTrendKind.value === kind
  }

  onMounted(() => {
    void loadDashboardData()
    startAutoRefresh()
  })

  return {
    loading,
    refreshing,
    trendLoading,
    activeTrendKind,
    lastRefreshedAt,
    metrics,
    trendPoints,
    trendSummary,
    healthTimeline,
    accountUsage,
    wireProfile,
    usageRecords,
    poolSummary,
    capacityInfo,
    rotationStrategy,
    refresh: refreshDashboardData,
    loadTrend,
  }
}
