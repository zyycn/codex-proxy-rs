import type { Component } from 'vue'
import { Activity, FileText, Timer, Users } from '@lucide/vue'
import { useIntervalFn } from '@vueuse/core'
import { clamp } from 'es-toolkit'
import { computed, onMounted, shallowRef } from 'vue'

import { getDashboardSummary, getDashboardTrend } from '@/api'
import { withMinimumDuration } from '@/utils/async'
import { formatDateTime } from '@/utils/date'
import { formatCompactNumber } from '@/utils/number'
import { normalizeUsageRecord } from '@/views/usage/utils/records'

export function useDashboard() {
  const activeTrendKind = shallowRef(normalizeDashboardTrendKind('usage'))
  const snapshot = shallowRef(dashboardSnapshotView(null))
  const trend = shallowRef(dashboardTrendView(null))
  const loading = shallowRef(false)
  const refreshing = shallowRef(false)
  const lastRefreshedAt = shallowRef('')
  let trendRequestId = 0

  const metrics = computed(() => snapshot.value.metrics)
  const healthTimeline = computed(() => snapshot.value.healthTimeline)
  const accountUsage = computed(() => snapshot.value.accountUsage)
  const wireProfiles = computed(() => snapshot.value.wireProfiles)
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
      const result = await getDashboardTrend({ kind: trendKind })
      if (isCurrentTrendRequest(requestId, trendKind))
        trend.value = dashboardTrendView(result)
    }
    catch {
      // 趋势请求失败时保留当前趋势，下一次刷新继续尝试。
    }
  }

  async function loadDashboardSnapshot() {
    const trendKind = activeTrendKind.value
    const requestId = ++trendRequestId
    const summary = await getDashboardSummary({ kind: trendKind })
    snapshot.value = dashboardSnapshotView(summary)
    lastRefreshedAt.value = formatDateTime()
    if (isCurrentTrendRequest(requestId, trendKind)) {
      trend.value = dashboardTrendView(summary.trend)
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
    activeTrendKind,
    lastRefreshedAt,
    metrics,
    trendPoints,
    trendSummary,
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

type DashboardSummary = Awaited<ReturnType<typeof getDashboardSummary>>
type DashboardTrend = Awaited<ReturnType<typeof getDashboardTrend>>
type DashboardTrendPoint = DashboardTrend['points'][number]
type DashboardTrendKind = DashboardTrend['kind']

export type MetricTone = 'normal' | 'info' | 'success' | 'warning' | 'danger'

export interface MetricCardView {
  title: string
  value: string
  valueRaw?: number | null
  valueFormatter?: (value: number) => string
  icon: Component
  tone: MetricTone
  sparkline?: {
    values: number[]
    tone: MetricTone
  }
  trend?: {
    direction: 'up' | 'down' | 'flat'
    tone: MetricTone
  }
  details: Array<{
    label: string
    value: string
    tone?: MetricTone
  }>
}

const metricSparklineBuckets = 12

const emptyCards: DashboardSummary['cards'] = {
  credentials: {
    total: '0',
    totalValue: 0,
    available: '0',
    availableValue: 0,
    unavailable: '0',
    unavailableValue: 0,
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
}

const emptyHealthTimeline: DashboardSummary['healthTimeline'] = {
  title: '请求健康时间线',
  description: '有效请求可用性',
  reliabilityDisplay: '-',
  status: 'no_data',
  successRequests: 0,
  failedRequests: 0,
  cancelledRequests: 0,
  incompleteRequests: 0,
  callerErrorRequests: 0,
  points: [],
}

const emptyCapacityInfo = {
  maxConcurrentPerAccount: null,
  totalSlots: null,
  usedSlots: null,
  availableSlots: null,
}

export function dashboardSnapshotView(summary: DashboardSummary | null) {
  const trendPoints = summary?.trend.points ?? []
  return {
    metrics: metricCards(summary?.cards ?? emptyCards, trendPoints),
    healthTimeline: summary?.healthTimeline ?? emptyHealthTimeline,
    accountUsage: (summary?.accountUsage ?? []).map(accountUsageItem),
    wireProfiles: summary?.wireProfiles ?? [],
    usageRecords: (summary?.usageRecords ?? []).map(normalizeUsageRecord),
    poolSummary: summary?.poolSummary ?? null,
    capacityInfo: summary?.capacityInfo ?? emptyCapacityInfo,
    rotationStrategy: summary?.rotationStrategy ?? null,
  }
}

export function dashboardTrendView(trend: DashboardTrend | null) {
  if (!trend) {
    const points: ReturnType<typeof aggregateUsageTrend> = []
    const summary: ReturnType<typeof usageTrendSummary> = []
    return { points, summary }
  }

  if (trend.kind === 'usage') {
    const points = aggregateUsageTrend(trend.points)
    return { points, summary: usageTrendSummary(points) }
  }

  const summary = []
  for (const item of trend.summary) {
    summary.push({
      label: item.label,
      value: trend.kind === 'errors' && item.ratio !== null ? item.ratio : item.value,
      tone: trendSummaryTone(item.label),
      colorVar: trendSummaryColorVar(trend.kind, item.label),
    })
  }
  return { points: trend.points, summary }
}

export function normalizeDashboardTrendKind(kind: string): DashboardTrendKind {
  if (kind === 'latency' || kind === 'errors')
    return kind
  return 'usage'
}

function metricCards(
  cards: DashboardSummary['cards'],
  points: DashboardTrendPoint[],
): MetricCardView[] {
  const { credentials, traffic, tokens, cache } = cards
  const recentPoints = recentTrendWindow(points)
  return [
    {
      title: '账号',
      value: credentials.total,
      valueRaw: credentials.totalValue,
      valueFormatter: formatCompactNumber,
      icon: Users,
      tone: 'normal',
      details: [
        {
          label: '可用',
          value: credentials.available,
          tone: credentials.availableValue > 0 ? 'success' : 'normal',
        },
        {
          label: '不可用',
          value: credentials.unavailable,
          tone: credentials.unavailableValue > 0 ? 'danger' : 'normal',
        },
      ],
    },
    {
      title: '请求次数',
      value: traffic.todayRequests,
      valueRaw: traffic.todayRequestsValue,
      valueFormatter: formatCompactNumber,
      icon: Activity,
      tone: 'info',
      sparkline: sparkline(
        recentPoints.map(point => point.requestsValue),
        'info',
      ),
      trend: trendState(traffic.todayRequestsValue, traffic.yesterdayRequestsValue, 'info'),
      details: [
        { label: '总请求', value: traffic.totalRequests, tone: 'info' },
        { label: '首字均值', value: cache.averageFirstTokenLatencyMs, tone: 'info' },
      ],
    },
    {
      title: 'Token',
      value: tokens.todayTokens,
      valueRaw: tokens.todayTokensValue,
      valueFormatter: formatCompactNumber,
      icon: FileText,
      tone: 'success',
      sparkline: sparkline(
        recentPoints.map(point => point.tokensValue),
        'success',
      ),
      trend: trendState(tokens.todayTokensValue, tokens.yesterdayTokensValue, 'success'),
      details: [
        { label: '总 Token', value: tokens.totalTokens, tone: 'success' },
        { label: '总计费', value: formatDashboardUsd(tokens.totalBillingAmountUsd), tone: 'success' },
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
        recentPoints.map(point => point.cacheHitRateValue),
        'warning',
      ),
      trend: trendState(
        cache.todayHitRateValue ?? 0,
        cache.yesterdayHitRateValue ?? 0,
        'warning',
      ),
      details: [
        { label: '总缓存命中', value: cache.totalHitRate, tone: 'warning' },
        { label: '总缓存', value: cache.totalCachedTokens, tone: 'warning' },
      ],
    },
  ]
}

function sparkline(values: number[], tone: MetricTone) {
  return values.some(value => value > 0) ? { values, tone } : undefined
}

function recentTrendWindow(points: DashboardTrendPoint[]) {
  let lastActiveIndex = points.length - 1
  while (lastActiveIndex >= 0 && points[lastActiveIndex].requestsValue <= 0) lastActiveIndex -= 1
  if (lastActiveIndex < 0)
    return []
  return points.slice(
    Math.max(0, lastActiveIndex - (metricSparklineBuckets - 1)),
    lastActiveIndex + 1,
  )
}

function formatDashboardRate(value: number) {
  return Number.isFinite(value) ? `${(value * 100).toFixed(1)}%` : '—'
}

function aggregateUsageTrend(points: DashboardTrendPoint[]) {
  return Array.from({ length: Math.ceil(points.length / 2) }, (_, groupIndex) => {
    const group = points.slice(groupIndex * 2, groupIndex * 2 + 2)
    const first = group[0]
    const requestsValue = sum(group, point => point.requestsValue)
    const errorsValue = sum(group, point => point.errorsValue)
    const inputTokensValue = sum(group, point => point.inputTokensValue)
    const outputTokensValue = sum(group, point => point.outputTokensValue)
    const cachedTokensValue = Math.min(
      inputTokensValue,
      sum(group, point => point.cachedTokensValue),
    )
    const uncachedInputTokensValue = inputTokensValue - cachedTokensValue
    const effectiveTokensValue = uncachedInputTokensValue + outputTokensValue
    const cacheHitRateValue = inputTokensValue ? cachedTokensValue / inputTokensValue : null
    const successRateValue = requestsValue
      ? ((requestsValue - errorsValue) / requestsValue) * 100
      : null

    return {
      ...first,
      requests: formatCompactNumber(requestsValue),
      requestsValue,
      inputTokens: formatCompactNumber(inputTokensValue),
      inputTokensValue,
      outputTokens: formatCompactNumber(outputTokensValue),
      outputTokensValue,
      cachedTokens: formatCompactNumber(cachedTokensValue),
      cachedTokensValue,
      uncachedInputTokens: formatCompactNumber(uncachedInputTokensValue),
      uncachedInputTokensValue,
      effectiveTokens: formatCompactNumber(effectiveTokensValue),
      effectiveTokensValue: effectiveTokensValue > 0 ? effectiveTokensValue : null,
      cacheHitRate: cacheHitRateValue === null ? '—' : formatDashboardRate(cacheHitRateValue),
      cacheHitRateValue,
      tokensValue: inputTokensValue + outputTokensValue,
      errors: formatCompactNumber(errorsValue),
      errorsValue,
      successRate: successRateValue === null ? '—' : `${successRateValue.toFixed(1)}%`,
      successRateValue,
    }
  })
}

function sum(points: DashboardTrendPoint[], selector: (point: DashboardTrendPoint) => number) {
  return points.reduce((total, point) => total + selector(point), 0)
}

function usageTrendSummary(points: ReturnType<typeof aggregateUsageTrend>) {
  const inputTokens = points.reduce((total, point) => total + point.inputTokensValue, 0)
  const outputTokens = points.reduce((total, point) => total + point.outputTokensValue, 0)
  const cachedTokens = Math.min(
    inputTokens,
    points.reduce((total, point) => total + point.cachedTokensValue, 0),
  )

  return [
    {
      label: '输入',
      value: formatCompactNumber(inputTokens),
      tone: 'info',
      colorVar: '--cp-info',
    },
    {
      label: '输出',
      value: formatCompactNumber(outputTokens),
      tone: 'success',
      colorVar: '--cp-success',
    },
    {
      label: '缓存',
      value: formatCompactNumber(cachedTokens),
      tone: 'normal',
      colorVar: '--cp-text-tertiary',
    },
  ]
}

function accountUsageItem(item: DashboardSummary['accountUsage'][number]) {
  const usageWindow = dashboardUsageWindow(item)
  const requestCount = usageWindow.localUsage?.requestCount
  const usesDailyRequests = typeof requestCount === 'number'
  return {
    id: item.id,
    provider: item.provider,
    authenticationKind: item.authenticationKind,
    email: item.email,
    planType: item.planType,
    tokens: item.tokens,
    lastUsed: item.lastUsed,
    metricLabel: usesDailyRequests ? '次数' : '今日 Token',
    metricValue: usesDailyRequests
      ? requestCount > 0 ? usageWindow.localUsage?.requestCountDisplay : '—'
      : item.tokens,
    usageWindow,
  }
}

function dashboardUsageWindow(item: DashboardSummary['accountUsage'][number]) {
  const provider = item.provider.trim().toLowerCase()
  const planType = item.planType?.trim().toLowerCase() || 'free'
  if (provider !== 'xai' || planType !== 'free') {
    const usedPercent = quotaUsedPercent(item.quotaUsedPercent)
    return {
      key: 'quota',
      group: 'other',
      labelDisplay: '额度',
      usedPercent,
      usedPercentDisplay: usedPercent === null ? '—' : `${usedPercent}%`,
      limitReached: (usedPercent ?? 0) >= 100,
      windowSeconds: null,
      resetAtDisplay: '—',
    }
  }

  const requestCount = typeof item.requestCount === 'number' && Number.isFinite(item.requestCount)
    ? Math.max(0, item.requestCount)
    : 0
  const requestBuckets = Array.isArray(item.requestBuckets)
    ? item.requestBuckets.map((bucket: { bucketStart: string, requestCount: number }) => ({
        bucketStart: bucket.bucketStart,
        requestCount: bucket.requestCount,
      }))
    : []

  return {
    key: 'dailyRequests',
    group: 'other',
    labelDisplay: '日请求',
    usedPercent: null,
    usedPercentDisplay: '—',
    limitReached: false,
    windowSeconds: 86_400,
    resetAtDisplay: '—',
    localUsage: {
      requestCount,
      requestCountDisplay: formatCompactNumber(requestCount),
      requestBuckets,
    },
  }
}

function quotaUsedPercent(value: number | null) {
  if (typeof value !== 'number' || !Number.isFinite(value))
    return null
  return clamp(Math.round(value), 0, 100)
}

function formatDashboardUsd(value: string) {
  const normalized = value.trim()
  if (!normalized.startsWith('$'))
    return value
  const amount = Number(normalized.slice(1).replaceAll(',', ''))
  return Number.isFinite(amount) ? `$${amount.toFixed(2)}` : value
}

function trendState(
  current: number,
  previous: number,
  fallbackTone: MetricTone,
): MetricCardView['trend'] {
  if (current > previous)
    return { direction: 'up', tone: 'success' }
  if (current < previous)
    return { direction: 'down', tone: 'danger' }
  return previous > 0 || current > 0 ? { direction: 'flat', tone: fallbackTone } : undefined
}

function trendSummaryTone(label: string) {
  if (label.includes('错误'))
    return 'danger'
  if (label.includes('最高'))
    return 'warning'
  if (label.includes('输出') || label.includes('最低') || label.includes('成功'))
    return 'success'
  if (label.includes('缓存'))
    return 'normal'
  return 'info'
}

function trendSummaryColorVar(kind: DashboardTrendKind, label: string) {
  if (kind === 'latency') {
    if (label.includes('最高'))
      return '--cp-warning'
    if (label.includes('最低'))
      return '--cp-success'
    return '--cp-normal'
  }
  if (kind === 'errors') {
    if (label.includes('错误'))
      return '--cp-danger'
    if (label.includes('成功'))
      return '--cp-success'
    return '--cp-info'
  }
  if (label.includes('输出'))
    return '--cp-success'
  if (label.includes('缓存'))
    return '--cp-text-tertiary'
  return '--cp-info'
}
