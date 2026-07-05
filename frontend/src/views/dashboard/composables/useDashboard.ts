import { useIntervalFn } from '@vueuse/core'
import { clamp } from 'es-toolkit'
import { onMounted, ref } from 'vue'

import {
  Activity,
  CloudCheck,
  FileText,
  Gauge,
  MonitorCheck,
  RefreshCw,
  Timer,
  Users,
} from '@lucide/vue'

import { getDashboardSummary, getDashboardTrend } from '@/api'
import { withMinimumDuration } from '@/utils/async'
import { formatDateTime } from '@/utils/date'

type DashboardTrendKind = 'usage' | 'latency' | 'errors'

export function useDashboard(): any {
  const activeTrendKind = ref<DashboardTrendKind>('usage')
  const metrics = ref<any[]>(metricCards(emptyDashboardSummary()))
  const trendPoints = ref<any[]>([])
  const trendSummary = ref<any[]>([])
  const healthTimeline = ref<any>(emptyDashboardSummary().healthTimeline)
  const accountUsage = ref<any[]>([])
  const serviceStatuses = ref<any[]>([])
  const usageRecords = ref<any[]>([])
  const poolSummary = ref<any>(null)
  const capacityInfo = ref<any>(null)
  const rotationStrategy = ref<any>(null)
  const loading = ref(false)
  const refreshing = ref(false)
  const trendLoading = ref(false)
  const lastRefreshedAt = ref('')
  let trendRequestId = 0
  const { resume: startAutoRefresh } = useIntervalFn(
    () => {
      void loadDashboardData()
    },
    30000,
    { immediate: false },
  )

  async function loadDashboardData() {
    if (loading.value || refreshing.value) return
    try {
      loading.value = true
      await loadDashboardSnapshot()
    } catch (error) {
      console.error('Failed to load dashboard data:', error)
    } finally {
      loading.value = false
    }
  }

  async function refreshDashboardData() {
    if (loading.value || refreshing.value) return
    refreshing.value = true
    try {
      await withMinimumDuration(async () => {
        await loadDashboardSnapshot()
      })
    } catch (error) {
      console.error('Failed to refresh dashboard data:', error)
    } finally {
      refreshing.value = false
    }
  }

  async function loadTrend(kind: string) {
    const trendKind = normalizeTrendKind(kind)
    activeTrendKind.value = trendKind
    const requestId = nextTrendRequestId()
    try {
      trendLoading.value = true
      const trend = await getDashboardTrend({ kind: trendKind })
      if (isCurrentTrendRequest(requestId, trendKind)) {
        applyTrend(trend)
      }
    } catch (error) {
      console.error('Failed to load dashboard trend:', error)
    } finally {
      if (isCurrentTrendRequest(requestId, trendKind)) {
        trendLoading.value = false
      }
    }
  }

  async function loadDashboardSnapshot() {
    const trendKind = activeTrendKind.value
    const requestId = nextTrendRequestId()
    try {
      const summary = await getDashboardSummary({ kind: trendKind })
      applySummary(summary)
      if (isCurrentTrendRequest(requestId, trendKind)) {
        applyTrend(summary.trend)
      }
    } finally {
      if (isCurrentTrendRequest(requestId, trendKind)) {
        trendLoading.value = false
      }
    }
  }

  function applySummary(summary: any) {
    metrics.value = metricCards(summary)
    healthTimeline.value = summary.healthTimeline
    accountUsage.value = summary.accountUsage.map(accountUsageItem)
    serviceStatuses.value = summary.serviceStatuses.map(serviceStatusItem)
    usageRecords.value = summary.usageRecords
    poolSummary.value = summary.poolSummary
    capacityInfo.value = summary.capacityInfo
    rotationStrategy.value = summary.rotationStrategy ?? null
    lastRefreshedAt.value = formatDateTime()
  }

  function emptyDashboardSummary() {
    return {
      cards: {
        accounts: {
          total: '0',
          totalValue: 0,
          enabled: '0',
          enabledValue: 0,
          abnormal: '0',
          abnormalValue: 0,
        },
        traffic: {
          todayRequests: '0',
          todayRequestsValue: 0,
          yesterdayRequests: '0',
          yesterdayRequestsValue: 0,
          totalRequests: '0',
          rpm: '0',
          tpm: '0',
        },
        tokens: {
          todayTokens: '0',
          todayTokensValue: 0,
          yesterdayTokens: '0',
          yesterdayTokensValue: 0,
          totalTokens: '0',
          todayCostUsd: '—',
          todayCostUsdValue: 0,
          totalCostUsd: '—',
          totalCostUsdValue: 0,
        },
        cache: {
          todayHitRate: '—',
          todayHitRateValue: null,
          yesterdayHitRate: '—',
          yesterdayHitRateValue: null,
          totalHitRate: '—',
          totalCachedTokens: '0',
          firstTokenLatencyMs: '—',
          completionLatencyMs: '—',
        },
      },
      trend: {
        kind: 'usage',
        points: [],
        summary: [],
      },
      healthTimeline: {
        title: '请求健康时间线',
        description: '今日请求可靠性',
        rangeDisplay: '-',
        reliabilityDisplay: '-',
        oldestLabel: '最早',
        newestLabel: '最新',
        points: '',
      },
      accountUsage: [],
      serviceStatuses: [],
      usageRecords: [],
      poolSummary: {
        total: 0,
        active: 0,
        expired: 0,
        quotaExhausted: 0,
        refreshing: 0,
        disabled: 0,
        banned: 0,
      },
      capacityInfo: {
        maxConcurrentPerAccount: 0,
        totalSlots: 0,
        usedSlots: 0,
        availableSlots: 0,
      },
      rotationStrategy: undefined,
    }
  }

  function metricCards(summary: any) {
    const { accounts, traffic, tokens, cache } = summary.cards
    const points = summary.trend.points as any[]
    return [
      {
        title: '账号',
        value: accounts.total,
        icon: Users,
        tone: 'normal',
        details: [
          {
            label: '启用',
            value: accounts.enabled,
            tone: accounts.enabledValue > 0 ? 'success' : 'normal',
          },
          {
            label: '异常',
            value: accounts.abnormal,
            tone: accounts.abnormalValue > 0 ? 'danger' : 'normal',
          },
        ],
      },
      {
        title: '今日请求',
        value: traffic.todayRequests,
        icon: Activity,
        tone: 'info',
        sparkline: sparkline(
          points.map((point) => point.requestsValue),
          'info',
        ),
        trend: trendState(traffic.todayRequestsValue, traffic.yesterdayRequestsValue, 'info'),
        details: [
          { label: '总请求', value: traffic.totalRequests, tone: 'info' },
          {
            label: '首 Token',
            value: cache.firstTokenLatencyMs,
            tone: 'info',
          },
        ],
      },
      {
        title: '今日 Token',
        value: tokens.todayTokens,
        icon: FileText,
        tone: 'success',
        sparkline: sparkline(
          points.map((point) => point.tokensValue),
          'success',
        ),
        trend: trendState(tokens.todayTokensValue, tokens.yesterdayTokensValue, 'success'),
        details: [
          { label: '总 Token', value: tokens.totalTokens, tone: 'success' },
          {
            label: '计费',
            value: `${tokens.totalCostUsd} / ${tokens.todayCostUsd}`,
            tone: 'success',
          },
        ],
      },
      {
        title: '今日缓存命中',
        value: cache.todayHitRate,
        icon: Timer,
        tone: cache.todayHitRateValue && cache.todayHitRateValue > 0 ? 'warning' : 'normal',
        sparkline: sparkline(
          points.map((point) => point.cachedTokensValue),
          'warning',
        ),
        trend: trendState(
          cache.todayHitRateValue ?? 0,
          cache.yesterdayHitRateValue ?? 0,
          'warning',
        ),
        details: [
          { label: '总缓存命中', value: cache.totalHitRate, tone: 'warning' },
          {
            label: '缓存 Token',
            value: cache.totalCachedTokens,
            tone: 'warning',
          },
        ],
      },
    ]
  }

  function sparkline(values: number[], tone: string) {
    return values.some((value) => value > 0) ? { values, tone } : undefined
  }

  function applyTrend(trend: any) {
    trendPoints.value = trend.points
    trendSummary.value = (trend.summary as any[]).map((item) => {
      if (trend.kind === 'latency') {
        return {
          label: item.label,
          value: item.value,
          tone: trendSummaryTone(item.label),
          colorVar: trendSummaryColorVar(trend.kind, item.label),
        }
      }
      if (item.ratio !== null && item.ratio !== undefined) {
        return {
          label: item.label,
          value: item.ratio,
          tone: trendSummaryTone(item.label),
          colorVar: trendSummaryColorVar(trend.kind, item.label),
        }
      }
      return {
        label: item.label,
        value: item.value,
        tone: trendSummaryTone(item.label),
        colorVar: trendSummaryColorVar(trend.kind, item.label),
      }
    })
  }

  function accountUsageItem(item: any) {
    const tone = accountTone(item.status)
    const quotaPercent = quotaUsedPercent(item.quotaUsedPercent)
    return {
      name: item.name,
      email: item.email || '-',
      planType: item.plan || 'free',
      requests: item.requests,
      tokens: item.tokens,
      lastUsed: item.lastUsed || '-',
      tone,
      quotaPercent,
      quotaTone: quotaTone(quotaPercent),
    }
  }

  function quotaUsedPercent(value: number | null | undefined): number {
    if (typeof value !== 'number' || !Number.isFinite(value)) return 0
    return clamp(Math.round(value), 0, 100)
  }

  function quotaTone(percent: number) {
    if (percent >= 95) return 'danger'
    if (percent >= 80) return 'warning'
    if (percent > 0) return 'success'
    return 'normal'
  }

  function serviceStatusItem(item: any, index: number) {
    const icons = [MonitorCheck, MonitorCheck, CloudCheck, RefreshCw, Gauge]
    return {
      label: item.label,
      value: item.value || '-',
      detail: item.detail || '-',
      tone: item.tone,
      icon: icons[index] ?? MonitorCheck,
    }
  }

  function trendState(current: number, previous: number, fallbackTone: string) {
    if (current > previous) return { direction: 'up', tone: 'success' }
    if (current < previous) return { direction: 'down', tone: 'danger' }
    return previous > 0 || current > 0 ? { direction: 'flat', tone: fallbackTone } : undefined
  }

  function normalizeTrendKind(kind: string): DashboardTrendKind {
    if (kind === 'latency' || kind === 'errors') return kind
    return 'usage'
  }

  function nextTrendRequestId() {
    trendRequestId += 1
    return trendRequestId
  }

  function isCurrentTrendRequest(requestId: number, kind: DashboardTrendKind) {
    return requestId === trendRequestId && activeTrendKind.value === kind
  }

  function trendSummaryTone(label: string) {
    if (label.includes('错误')) return 'danger'
    if (label.includes('最高')) return 'warning'
    if (label.includes('输出') || label.includes('最低') || label.includes('成功')) return 'success'
    if (label.includes('缓存')) return 'normal'
    return 'info'
  }

  function trendSummaryColorVar(kind: string, label: string) {
    if (kind === 'latency') {
      if (label.includes('最高')) return '--cp-warning'
      if (label.includes('最低')) return '--cp-success'
      return '--cp-normal'
    }
    if (kind === 'errors') {
      if (label.includes('错误')) return '--cp-danger'
      if (label.includes('成功')) return '--cp-success'
      return '--cp-info'
    }
    if (label.includes('输出')) return '--cp-success'
    if (label.includes('缓存')) return '--cp-text-tertiary'
    return '--cp-info'
  }

  function accountTone(status: string) {
    if (status === 'active') return 'success'
    if (status === 'quota_exhausted' || status === 'expired') return 'warning'
    if (status === 'banned' || status === 'disabled') return 'danger'
    if (status === 'refreshing') return 'info'
    return 'normal'
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
    serviceStatuses,
    usageRecords,
    poolSummary,
    capacityInfo,
    rotationStrategy,
    refresh: refreshDashboardData,
    loadTrend,
  }
}
