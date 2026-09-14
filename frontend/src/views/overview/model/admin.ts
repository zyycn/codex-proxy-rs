import type { getDashboardSummary, getDashboardTrend } from '@/api'
import type {
  MetricCardView,
  MetricTone,
  RequestTrendPoint,
  RequestTrendSummaryItem,
} from '@/views/overview/model/display'
import { Activity, FileText, Timer, Users } from '@lucide/vue'

import { formatCompactNumber, formatInteger } from '@/utils/number'
import { emptyHealthTimeline } from '@/views/overview/model/health'

type DashboardSummary = Awaited<ReturnType<typeof getDashboardSummary>>
type DashboardTrend = Awaited<ReturnType<typeof getDashboardTrend>>
type DashboardTrendPoint = DashboardTrend['points'][number]
type DashboardTrendKind = DashboardTrend['kind']

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
    accountUsage: (summary?.accountUsage ?? []),
    wireProfiles: summary?.wireProfiles ?? [],
    usageRecords: summary?.usageRecords ?? [],
    poolSummary: summary?.poolSummary ?? null,
    capacityInfo: summary?.capacityInfo ?? emptyCapacityInfo,
    rotationStrategy: summary?.rotationStrategy ?? null,
  }
}

export function dashboardTrendView(trend: DashboardTrend | null): {
  points: RequestTrendPoint[]
  summary: RequestTrendSummaryItem[]
} {
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
      valueFormatter: formatAccountCount,
      icon: Users,
      tone: 'normal',
      details: [
        {
          label: '可用',
          value: credentials.available,
          tone: 'success',
        },
        {
          label: '不可用',
          value: credentials.unavailable,
          tone: 'danger',
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

function formatAccountCount(value: number) {
  return formatInteger(Math.max(0, Math.round(value)))
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
    const cacheHitRateValue = inputTokensValue ? cachedTokensValue / inputTokensValue : 0
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
      cachedTokens: formatCompactNumber(cachedTokensValue),
      cachedTokensValue,
      uncachedInputTokens: formatCompactNumber(uncachedInputTokensValue),
      uncachedInputTokensValue,
      effectiveTokens: formatCompactNumber(effectiveTokensValue),
      effectiveTokensValue: effectiveTokensValue > 0 ? effectiveTokensValue : null,
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

function usageTrendSummary(
  points: ReturnType<typeof aggregateUsageTrend>,
): RequestTrendSummaryItem[] {
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
      colorVar: '--cp-color-blue-solid',
    },
    {
      label: '输出',
      value: formatCompactNumber(outputTokens),
      tone: 'success',
      colorVar: '--cp-color-green-solid',
    },
    {
      label: '缓存',
      value: formatCompactNumber(cachedTokens),
      tone: 'normal',
      colorVar: '--cp-color-text-tertiary',
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

function trendSummaryTone(label: string): MetricTone {
  if (label.includes('错误'))
    return 'danger'
  if (label.includes('总耗时'))
    return 'warning'
  if (label.includes('吞吐') || label.includes('输出') || label.includes('成功'))
    return 'success'
  if (label.includes('首字') || label.includes('缓存'))
    return 'normal'
  return 'info'
}

function trendSummaryColorVar(kind: DashboardTrendKind, label: string) {
  if (kind === 'latency') {
    if (label.includes('总耗时'))
      return '--cp-color-orange-solid'
    if (label.includes('吞吐'))
      return '--cp-color-green-solid'
    return '--cp-color-cyan-solid'
  }
  if (kind === 'errors') {
    if (label.includes('错误'))
      return '--cp-color-red-solid'
    if (label.includes('成功'))
      return '--cp-color-green-solid'
    return '--cp-color-blue-solid'
  }
  if (label.includes('输出'))
    return '--cp-color-green-solid'
  if (label.includes('缓存'))
    return '--cp-color-text-tertiary'
  return '--cp-color-blue-solid'
}
