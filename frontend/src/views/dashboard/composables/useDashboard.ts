import type { DashboardTrendResponse } from '@/api'
import { useIntervalFn } from '@vueuse/core'
import { computed, onMounted, onScopeDispose, shallowRef } from 'vue'

import { getDashboardSummary, getDashboardTrend } from '@/api'
import { errorMessage, withMinimumDuration } from '@/utils/operation'
import { dashboardSnapshotView, dashboardTrendView, normalizeDashboardTrendKind } from '../presenter'

export function useDashboard() {
  const activeTrendKind = shallowRef(normalizeDashboardTrendKind('usage'))
  const snapshot = shallowRef(dashboardSnapshotView(null))
  const trend = shallowRef<DashboardTrendResponse | null>(null)
  const trendLoading = shallowRef(false)
  const trendError = shallowRef('')
  const loading = shallowRef(false)
  const refreshing = shallowRef(false)
  const lastRefreshedAt = shallowRef('')
  let rangeAsOf = Date.now()
  let trendRequestId = 0
  let disposed = false
  let summaryController: AbortController | undefined
  let trendController: AbortController | undefined

  const metrics = computed(() => snapshot.value.metrics)
  const healthTimeline = computed(() => snapshot.value.healthTimeline)
  const accountUsage = computed(() => snapshot.value.accountUsage)
  const wireProfiles = computed(() => snapshot.value.wireProfiles)
  const usageRecords = computed(() => snapshot.value.usageRecords)
  const poolSummary = computed(() => snapshot.value.poolSummary)
  const capacityInfo = computed(() => snapshot.value.capacityInfo)
  const rotationStrategy = computed(() => snapshot.value.rotationStrategy)
  const trendView = computed(() => dashboardTrendView(
    trend.value?.kind === activeTrendKind.value ? trend.value : null,
  ))
  const trendPoints = computed(() => trendView.value.points)
  const trendSummary = computed(() => trendView.value.summary)

  const { resume: startAutoRefresh } = useIntervalFn(
    () => {
      void loadDashboardData(true)
    },
    30_000,
    { immediate: false },
  )

  async function loadDashboardData(silent = false) {
    if (summaryController || refreshing.value)
      return
    try {
      loading.value = !silent
      await loadDashboardSnapshot(silent)
    }
    catch {
      // 自动刷新会继续重试，保留最后一次成功快照。
    }
    finally {
      loading.value = false
    }
  }

  async function refreshDashboardData() {
    if (summaryController || refreshing.value)
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
    trendController?.abort()
    trendController = new AbortController()
    trendLoading.value = true
    trendError.value = ''
    try {
      const result = await getDashboardTrend({ kind: trendKind, period: 'today', asOf: rangeAsOf }, { signal: trendController.signal })
      if (isCurrentTrendRequest(requestId, trendKind))
        trend.value = result
    }
    catch (error: unknown) {
      if (isCurrentTrendRequest(requestId, trendKind))
        trendError.value = errorMessage(error)
    }
    finally {
      if (isCurrentTrendRequest(requestId, trendKind))
        trendLoading.value = false
    }
  }

  async function loadDashboardSnapshot(silent = false) {
    const trendKind = activeTrendKind.value
    const asOf = Date.now()
    const requestId = ++trendRequestId
    trendController?.abort()
    summaryController = new AbortController()
    // 自动刷新保留当前空态和错误，等待成功快照后再更新展示。
    if (!silent) {
      trendLoading.value = true
      trendError.value = ''
    }
    try {
      const summary = await getDashboardSummary({ kind: trendKind, period: 'today', asOf }, { silent, signal: summaryController.signal })
      if (disposed)
        return
      rangeAsOf = asOf
      snapshot.value = dashboardSnapshotView(summary)
      lastRefreshedAt.value = summary.asOfDisplay
      if (isCurrentTrendRequest(requestId, trendKind)) {
        trend.value = summary.trend
        trendError.value = ''
      }
      else {
        // 快照加载期间切换指标时，用已提交快照的同一锚点重新查询趋势。
        void loadTrend(activeTrendKind.value)
      }
    }
    catch (error: unknown) {
      if (!silent && isCurrentTrendRequest(requestId, trendKind))
        trendError.value = errorMessage(error)
      throw error
    }
    finally {
      summaryController = undefined
      if (isCurrentTrendRequest(requestId, trendKind))
        trendLoading.value = false
    }
  }

  function isCurrentTrendRequest(
    requestId: number,
    kind: ReturnType<typeof normalizeDashboardTrendKind>,
  ) {
    return !disposed && requestId === trendRequestId && activeTrendKind.value === kind
  }

  onMounted(() => {
    void loadDashboardData()
    startAutoRefresh()
  })

  onScopeDispose(() => {
    disposed = true
    trendRequestId += 1
    summaryController?.abort()
    trendController?.abort()
  })

  return {
    loading,
    refreshing,
    activeTrendKind,
    lastRefreshedAt,
    metrics,
    trendPoints,
    trendSummary,
    trendLoading,
    trendError,
    healthTimeline,
    accountUsage,
    wireProfiles,
    usageRecords,
    poolSummary,
    capacityInfo,
    rotationStrategy,
    refresh: refreshDashboardData,
    loadTrend,
  }
}
