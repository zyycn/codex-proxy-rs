import type { Component } from 'vue'
import type { DashboardSummaryResponse, DashboardTrendKind, DashboardTrendMetric, DashboardTrendPoint, DashboardTrendResponse } from '@/api'
import { Activity, FileText, Timer, Users } from '@lucide/vue'
import { computed } from 'vue'
import { t } from '@/i18n'
import { formatCompactNumber, formatInteger } from '@/utils/format'

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

const emptyCards: DashboardSummaryResponse['cards'] = {
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
    totalCachedInputTokens: '0',
    averageFirstTokenLatencyMs: '—',
  },
}

const emptyHealthTimeline = computed<DashboardSummaryResponse['healthTimeline']>(() => ({
  title: t('dashboard.health.title'),
  description: t('dashboard.health.description'),
  reliabilityDisplay: '-',
  status: 'no_data',
  successRequests: 0,
  failedRequests: 0,
  cancelledRequests: 0,
  incompleteRequests: 0,
  callerErrorRequests: 0,
  points: [],
}))

const emptyCapacityInfo = {
  maxConcurrentPerAccount: null,
  totalSlots: null,
  usedSlots: null,
  availableSlots: null,
}

export function dashboardSnapshotView(summary: DashboardSummaryResponse | null) {
  const trendPoints = summary?.trend.points ?? []
  return {
    analytics: summary?.analytics ?? null,
    metrics: metricCards(summary?.cards ?? emptyCards, trendPoints),
    healthTimeline: summary?.healthTimeline ?? emptyHealthTimeline.value,
    accountUsage: (summary?.accountUsage ?? []),
    wireProfiles: summary?.wireProfiles ?? [],
    usageRecords: summary?.usageRecords ?? [],
    poolSummary: summary?.poolSummary ?? null,
    capacityInfo: summary?.capacityInfo ?? emptyCapacityInfo,
    rotationStrategy: summary?.rotationStrategy ?? null,
  }
}

export function dashboardTrendView(trend: DashboardTrendResponse | null) {
  if (!trend) {
    const points: ReturnType<typeof aggregateUsageTrend> = []
    const summary: ReturnType<typeof usageTrendSummary> = []
    return { points, summary }
  }

  if (trend.kind === 'usage') {
    const points = aggregateUsageTrend(trend.points)
    return {
      points,
      summary: usageTrendSummary(points),
    }
  }

  const summary = trend.summary.map(item => ({
    ...trendMetricView(item.metric),
    value: trend.kind === 'errors' && item.ratio !== null
      ? item.ratio
      : item.value,
  }))
  return { points: trend.points, summary }
}

export function normalizeDashboardTrendKind(kind: string): DashboardTrendKind {
  if (kind === 'latency' || kind === 'errors')
    return kind
  return 'usage'
}

function metricCards(
  cards: DashboardSummaryResponse['cards'],
  points: DashboardTrendPoint[],
): MetricCardView[] {
  const { credentials, traffic, tokens, cache } = cards
  const recentPoints = recentTrendWindow(points)
  const hasTodayRequests = traffic.todayRequestsValue > 0
  return [
    {
      title: t('accounts.fields.count'),
      value: credentials.total,
      valueRaw: credentials.totalValue,
      valueFormatter: formatAccountCount,
      icon: Users,
      tone: 'normal',
      details: [
        {
          label: t('common.status.available'),
          value: credentials.available,
          tone: 'success',
        },
        {
          label: t('common.status.unavailable'),
          value: credentials.unavailable,
          tone: 'danger',
        },
      ],
    },
    {
      title: t('dashboard.metrics.requestCount'),
      value: traffic.todayRequests,
      valueRaw: traffic.todayRequestsValue,
      valueFormatter: formatCompactNumber,
      icon: Activity,
      tone: 'info',
      sparkline: sparkline(
        recentPoints.map(point => point.requestsValue),
        'info',
      ),
      trend: hasTodayRequests
        ? trendState(traffic.todayRequestsValue, traffic.yesterdayRequestsValue, 'info')
        : undefined,
      details: [
        {
          label: t('dashboard.metrics.totalRequests'),
          value: traffic.totalRequests,
          tone: 'info',
        },
        {
          label: t('usage.averageTtft'),
          value: cache.averageFirstTokenLatencyMs,
          tone: 'info',
        },
      ],
    },
    {
      title: t('common.units.tokens'),
      value: tokens.todayTokens,
      valueRaw: tokens.todayTokensValue,
      valueFormatter: formatCompactNumber,
      icon: FileText,
      tone: 'success',
      sparkline: sparkline(
        recentPoints.map(point => point.tokensValue),
        'success',
      ),
      trend: hasTodayRequests
        ? trendState(tokens.todayTokensValue, tokens.yesterdayTokensValue, 'success')
        : undefined,
      details: [
        {
          label: t('dashboard.metrics.totalTokens'),
          value: tokens.totalTokens,
          tone: 'success',
        },
        {
          label: t('dashboard.metrics.totalCharges'),
          value: formatDashboardUsd(tokens.totalBillingAmountUsd),
          tone: 'success',
        },
      ],
    },
    {
      title: t('dashboard.metrics.cacheHits'),
      value: cache.todayHitRate,
      valueRaw: cache.todayHitRateValue,
      valueFormatter: formatDashboardRate,
      icon: Timer,
      tone: cache.todayHitRateValue && cache.todayHitRateValue > 0
        ? 'warning'
        : 'normal',
      sparkline: sparkline(
        recentPoints.map(point => point.cacheHitRateValue),
        'warning',
      ),
      trend: hasTodayRequests
        ? trendState(cache.todayHitRateValue, cache.yesterdayHitRateValue, 'warning')
        : undefined,
      details: [
        {
          label: t('dashboard.metrics.totalCacheHits'),
          value: cache.totalHitRate,
          tone: 'warning',
        },
        {
          label: t('dashboard.metrics.totalCache'),
          value: cache.totalCachedInputTokens,
          tone: 'warning',
        },
      ],
    },
  ]
}

function formatAccountCount(value: number) {
  return formatInteger(Math.max(0, Math.round(value)))
}

function sparkline(values: number[], tone: MetricTone) {
  return values.some(value => value > 0)
    ? { values, tone }
    : undefined
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
  return Number.isFinite(value)
    ? `${(value * 100).toFixed(1)}%`
    : '—'
}

function aggregateUsageTrend(points: DashboardTrendPoint[]) {
  return Array.from({
    length: Math.ceil(points.length / 2),
  }, (_, groupIndex) => {
    const group = points.slice(groupIndex * 2, groupIndex * 2 + 2)
    const first = group[0]
    const requestsValue = sum(group, point => point.requestsValue)
    const errorsValue = sum(group, point => point.errorsValue)
    const inputTokensValue = sum(group, point => point.inputTokensValue)
    const outputTokensValue = sum(group, point => point.outputTokensValue)
    const cachedInputTokensValue = Math.min(
      inputTokensValue,
      sum(group, point => point.cachedInputTokensValue),
    )
    const uncachedInputTokensValue = inputTokensValue - cachedInputTokensValue
    const effectiveTokensValue = uncachedInputTokensValue + outputTokensValue
    const cacheHitRateValue = inputTokensValue
      ? cachedInputTokensValue / inputTokensValue
      : 0
    const successRateValue = requestsValue
      ? ((requestsValue - errorsValue) / requestsValue) * 100
      : 0

    return {
      ...first,
      requests: formatCompactNumber(requestsValue),
      requestsValue,
      inputTokens: formatCompactNumber(inputTokensValue),
      inputTokensValue,
      outputTokens: formatCompactNumber(outputTokensValue),
      outputTokensValue,
      cachedInputTokens: formatCompactNumber(cachedInputTokensValue),
      cachedInputTokensValue,
      uncachedInputTokens: formatCompactNumber(uncachedInputTokensValue),
      uncachedInputTokensValue,
      effectiveTokens: formatCompactNumber(effectiveTokensValue),
      effectiveTokensValue: effectiveTokensValue > 0
        ? effectiveTokensValue
        : null,
      cacheHitRate: formatDashboardRate(cacheHitRateValue),
      cacheHitRateValue,
      tokensValue: inputTokensValue + outputTokensValue,
      errors: formatCompactNumber(errorsValue),
      errorsValue,
      successRate: `${successRateValue.toFixed(1)}%`,
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
  const cachedInputTokens = Math.min(
    inputTokens,
    points.reduce((total, point) => total + point.cachedInputTokensValue, 0),
  )

  return [
    {
      ...trendMetricView('inputTokens'),
      value: formatCompactNumber(inputTokens),
    },
    {
      ...trendMetricView('outputTokens'),
      value: formatCompactNumber(outputTokens),
    },
    {
      ...trendMetricView('cachedInputTokens'),
      value: formatCompactNumber(cachedInputTokens),
    },
  ]
}

function formatDashboardUsd(value: string) {
  const normalized = value.trim()
  if (!normalized.startsWith('$'))
    return value
  const amount = Number(normalized.slice(1).replaceAll(',', ''))
  if (!Number.isFinite(amount))
    return value
  if (amount < 10_000)
    return `$${amount.toFixed(2)}`
  return `$${(amount / 1_000).toFixed(3)}k`
}

function trendState(
  current: number | null,
  previous: number | null,
  fallbackTone: MetricTone,
): MetricCardView['trend'] {
  if (current === null || previous === null)
    return undefined
  if (current > previous)
    return { direction: 'up', tone: 'success' }
  if (current < previous)
    return { direction: 'down', tone: 'danger' }
  return previous > 0 || current > 0
    ? { direction: 'flat', tone: fallbackTone }
    : undefined
}

// 展示与交互共享指标身份，切换语言不改变高亮或颜色
const trendMetrics = computed(() => ({
  inputTokens: {
    label: t('dashboard.metrics.inputTokens'),
    tone: 'info',
    colorVar: '--cp-color-blue-solid',
  },
  outputTokens: {
    label: t('dashboard.metrics.outputTokens'),
    tone: 'success',
    colorVar: '--cp-color-green-solid',
  },
  cachedInputTokens: {
    label: t('dashboard.metrics.cachedInputTokens'),
    tone: 'normal',
    colorVar: '--cp-color-text-tertiary',
  },
  firstTokenP95Ms: {
    label: t('dashboard.metrics.firstTokenP95Ms'),
    tone: 'normal',
    colorVar: '--cp-color-cyan-solid',
  },
  latencyP95Ms: {
    label: t('dashboard.metrics.latencyP95Ms'),
    tone: 'warning',
    colorVar: '--cp-color-orange-solid',
  },
  outputThroughputP50: {
    label: t('dashboard.metrics.outputThroughputP50'),
    tone: 'success',
    colorVar: '--cp-color-green-solid',
  },
  errors: {
    label: t('dashboard.metrics.errors'),
    tone: 'danger',
    colorVar: '--cp-color-red-solid',
  },
  successRate: {
    label: t('dashboard.metrics.successRate'),
    tone: 'success',
    colorVar: '--cp-color-green-solid',
  },
  requests: {
    label: t('dashboard.metrics.totalRequests'),
    tone: 'info',
    colorVar: '--cp-color-blue-solid',
  },
} satisfies Record<DashboardTrendMetric, { label: string, tone: MetricTone, colorVar: string }>))

export function trendMetricView(metric: DashboardTrendMetric) {
  return { metric, ...trendMetrics.value[metric] }
}
