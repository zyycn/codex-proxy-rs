import type { DashboardTrendResponse } from '@/api'
import { useIntervalFn } from '@vueuse/core'
import { computed, onMounted, onScopeDispose, shallowRef } from 'vue'

import { getDashboardSummary, getDashboardTrend } from '@/api'
import { errorMessage, withMinimumDuration } from '@/utils/async'
import { formatDateTime } from '@/utils/date'
import {
  dashboardSnapshotView,
  dashboardTrendView,
  normalizeDashboardTrendKind,
} from '@/views/overview/model/admin'
import { usageRecordColumns } from '@/views/usage/model/columns'

export function useAdminOverviewSource() {
  const activeTrendKind = shallowRef(normalizeDashboardTrendKind('usage'))
  const snapshot = shallowRef(dashboardSnapshotView(null))
  const trend = shallowRef<DashboardTrendResponse | null>(null)
  const trendLoading = shallowRef(false)
  const trendError = shallowRef('')
  const loading = shallowRef(false)
  const refreshing = shallowRef(false)
  const lastRefreshedAt = shallowRef('')
  const hasData = shallowRef(false)
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
  const fatalError = computed(() => hasData.value ? '' : trendError.value)
  const overviewUsageColumns = usageRecordColumns.filter(column => column.key !== 'actions')

  const { resume: startAutoRefresh } = useIntervalFn(
    () => {
      void loadDashboardData(true)
    },
    30_000,
    { immediate: false },
  )

  async function loadDashboardData(silent = false) {
    if (loading.value || refreshing.value)
      return
    try {
      loading.value = true
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
    trendController?.abort()
    trendController = new AbortController()
    trendLoading.value = true
    trendError.value = ''
    try {
      const result = await getDashboardTrend({ kind: trendKind }, { signal: trendController.signal })
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
    const requestId = ++trendRequestId
    summaryController?.abort()
    trendController?.abort()
    summaryController = new AbortController()
    trendLoading.value = true
    trendError.value = ''
    try {
      const summary = await getDashboardSummary({ kind: trendKind }, { silent, signal: summaryController.signal })
      if (disposed)
        return
      snapshot.value = dashboardSnapshotView(summary)
      hasData.value = true
      lastRefreshedAt.value = formatDateTime()
      if (isCurrentTrendRequest(requestId, trendKind)) {
        trend.value = summary.trend
      }
    }
    catch (error: unknown) {
      if (isCurrentTrendRequest(requestId, trendKind))
        trendError.value = errorMessage(error)
      throw error
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
    fatalError,
    healthTimeline,
    accountUsage,
    wireProfiles,
    budget: null,
    limits: null,
    usageRecords,
    poolSummary,
    capacityInfo,
    rotationStrategy,
    showAccountOverview: true,
    usageColumns: overviewUsageColumns,
    refresh: refreshDashboardData,
    loadTrend,
  }
}
