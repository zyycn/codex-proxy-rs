import { useIntervalFn } from '@vueuse/core'
import { clamp } from 'es-toolkit'
import { onMounted, ref } from 'vue'

import { Activity, FileText, Timer, Users } from '@lucide/vue'

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
  const wireProfile = ref<any>(null)
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
    } catch {
      // 自动刷新会继续重试，保留最后一次成功快照
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
    } catch {
      // 手动刷新失败时保留当前数据，不打断概览操作
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
    } catch {
      // 趋势请求失败时保留当前趋势，下一次刷新继续尝试
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
    wireProfile.value = summary.wireProfile ?? null
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
          yesterdayRequestsValue: 0,
          totalRequests: '0',
        },
        tokens: {
          todayTokens: '0',
          todayTokensValue: 0,
          yesterdayTokensValue: 0,
          totalTokens: '0',
          totalBillingAmountUsd: '—',
        },
        cache: {
          todayHitRate: '—',
          todayHitRateValue: null,
          yesterdayHitRateValue: null,
          totalHitRate: '—',
          totalCachedTokens: '0',
          averageFirstTokenLatencyMs: '—',
        },
      },
      trend: {
        kind: 'usage',
        points: [],
        summary: [],
      },
      healthTimeline: {
        title: '请求健康时间线',
        description: '请求可靠性',
        reliabilityDisplay: '-',
        points: '',
      },
      accountUsage: [],
      wireProfile: null,
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
    const points = recentTrendWindow(summary.trend.points as any[])
    return [
      {
        title: '账号',
        value: accounts.total,
        valueRaw: accounts.totalValue,
        valueFormatter: formatDashboardCompactNumber,
        icon: Users,
        tone: 'normal',
        details: [
          {
            label: '启用',
            value: accounts.enabled,
            tone: accounts.enabledValue > 0 ? 'success' : 'normal',
          },
          {
            label: '不可用',
            value: accounts.abnormal,
            tone: accounts.abnormalValue > 0 ? 'danger' : 'normal',
          },
        ],
      },
      {
        title: '请求次数',
        value: traffic.todayRequests,
        valueRaw: traffic.todayRequestsValue,
        valueFormatter: formatDashboardCompactNumber,
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
            label: '今日首字',
            value: cache.averageFirstTokenLatencyMs,
            tone: 'info',
          },
        ],
      },
      {
        title: 'Token',
        value: tokens.todayTokens,
        valueRaw: tokens.todayTokensValue,
        valueFormatter: formatDashboardCompactNumber,
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
            label: '总计费',
            value: tokens.totalBillingAmountUsd,
            tone: 'success',
          },
        ],
      },
      {
        title: '缓存命中',
        value: cache.todayHitRate,
        valueRaw: cache.todayHitRateValue,
        valueFormatter: formatDashboardRate,
        icon: Timer,
        tone: cache.todayHitRateValue && cache.todayHitRateValue > 0 ? 'warning' : 'normal',
        sparkline: sparkline(
          points.map((point) => point.cacheHitRateValue),
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
            label: '总缓存',
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

  function recentTrendWindow(points: any[]) {
    let lastActiveIndex = points.length - 1
    while (lastActiveIndex >= 0 && Number(points[lastActiveIndex]?.requestsValue) <= 0) {
      lastActiveIndex -= 1
    }
    if (lastActiveIndex < 0) return []

    return points.slice(Math.max(0, lastActiveIndex - 8), lastActiveIndex + 1)
  }

  function formatDashboardCompactNumber(value: number) {
    const normalized = Math.max(0, Math.round(value))

    if (normalized < 1_000) {
      return new Intl.NumberFormat('zh-CN').format(normalized)
    }

    for (const [unit, threshold] of [
      ['P', 1_000_000_000_000_000],
      ['T', 1_000_000_000_000],
      ['B', 1_000_000_000],
      ['M', 1_000_000],
      ['K', 1_000],
    ] as const) {
      if (normalized >= threshold) {
        return `${formatDashboardCompactScaled(normalized / threshold)}${unit}`
      }
    }

    return new Intl.NumberFormat('zh-CN').format(normalized)
  }

  function formatDashboardCompactScaled(value: number) {
    const rounded = value >= 10 ? value.toFixed(1) : value.toFixed(2)
    return rounded.replace(/\.?0+$/, '')
  }

  function formatDashboardRate(value: number) {
    return Number.isFinite(value) ? `${(value * 100).toFixed(1)}%` : '—'
  }

  function applyTrend(trend: any) {
    const points = trend.kind === 'usage' ? aggregateUsageTrend(trend.points) : trend.points
    trendPoints.value = points

    if (trend.kind === 'usage') {
      trendSummary.value = usageTrendSummary(points)
      return
    }

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

  function aggregateUsageTrend(points: any[]) {
    return Array.from({ length: Math.ceil(points.length / 2) }, (_, groupIndex) => {
      const group = points.slice(groupIndex * 2, groupIndex * 2 + 2)
      const first = group[0]
      const sum = (key: string) =>
        group.reduce((total, point) => total + Number(point[key] ?? 0), 0)
      const requestsValue = sum('requestsValue')
      const errorsValue = sum('errorsValue')
      const inputTokensValue = sum('inputTokensValue')
      const outputTokensValue = sum('outputTokensValue')
      const cachedTokensValue = Math.min(inputTokensValue, sum('cachedTokensValue'))
      const uncachedInputTokensValue = inputTokensValue - cachedTokensValue
      const effectiveTokensValue = uncachedInputTokensValue + outputTokensValue
      const cacheHitRateValue = inputTokensValue ? cachedTokensValue / inputTokensValue : null
      const successRateValue = requestsValue
        ? ((requestsValue - errorsValue) / requestsValue) * 100
        : null

      return {
        ...first,
        requests: formatDashboardCompactNumber(requestsValue),
        requestsValue,
        inputTokens: formatDashboardCompactNumber(inputTokensValue),
        inputTokensValue,
        outputTokens: formatDashboardCompactNumber(outputTokensValue),
        outputTokensValue,
        cachedTokens: formatDashboardCompactNumber(cachedTokensValue),
        cachedTokensValue,
        uncachedInputTokens: formatDashboardCompactNumber(uncachedInputTokensValue),
        uncachedInputTokensValue,
        effectiveTokens: formatDashboardCompactNumber(effectiveTokensValue),
        effectiveTokensValue: effectiveTokensValue > 0 ? effectiveTokensValue : null,
        cacheHitRate: cacheHitRateValue === null ? '—' : formatDashboardRate(cacheHitRateValue),
        cacheHitRateValue,
        tokensValue: inputTokensValue + outputTokensValue,
        errors: formatDashboardCompactNumber(errorsValue),
        errorsValue,
        successRate: successRateValue === null ? '—' : `${successRateValue.toFixed(1)}%`,
        successRateValue,
      }
    })
  }

  function usageTrendSummary(points: any[]) {
    const total = (key: string) => points.reduce((sum, point) => sum + Number(point[key] ?? 0), 0)
    const inputTokens = total('inputTokensValue')
    const outputTokens = total('outputTokensValue')
    const cachedTokens = Math.min(inputTokens, total('cachedTokensValue'))

    return [
      {
        label: '输入',
        value: formatDashboardCompactNumber(inputTokens),
        tone: 'info',
        colorVar: '--cp-info',
      },
      {
        label: '输出',
        value: formatDashboardCompactNumber(outputTokens),
        tone: 'success',
        colorVar: '--cp-success',
      },
      {
        label: '缓存',
        value: formatDashboardCompactNumber(cachedTokens),
        tone: 'normal',
        colorVar: '--cp-text-tertiary',
      },
    ]
  }

  function accountUsageItem(item: any) {
    const quotaPercent = quotaUsedPercent(item.quotaUsedPercent)
    return {
      id: item.id,
      email: item.email,
      planType: item.planType,
      tokens: item.tokens,
      lastUsed: item.lastUsed,
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
