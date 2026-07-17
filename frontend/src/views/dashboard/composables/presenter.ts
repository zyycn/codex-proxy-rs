import type { Component } from 'vue'
import type { getDashboardSummary, getDashboardTrend } from '@/api'
import { Activity, FileText, Timer, Users } from '@lucide/vue'

import { clamp } from 'es-toolkit'

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
}

const emptyHealthTimeline: DashboardSummary['healthTimeline'] = {
  title: '请求健康时间线',
  description: '有效请求可用性',
  reliabilityDisplay: '-',
  status: 'no_data',
  successRequests: 0,
  failedRequests: 0,
  cancelledRequests: 0,
  callerErrorRequests: 0,
  points: [],
}

const emptyPoolSummary: DashboardSummary['poolSummary'] = {
  total: 0,
  active: 0,
  expired: 0,
  quotaExhausted: 0,
  refreshing: 0,
  disabled: 0,
  banned: 0,
}

const emptyCapacityInfo: DashboardSummary['capacityInfo'] = {
  maxConcurrentPerAccount: 0,
  totalSlots: 0,
  usedSlots: 0,
  availableSlots: 0,
}

export function dashboardSnapshotView(summary: DashboardSummary | null) {
  const trendPoints = summary?.trend.points ?? []
  return {
    metrics: metricCards(summary?.cards ?? emptyCards, trendPoints),
    healthTimeline: summary?.healthTimeline ?? emptyHealthTimeline,
    accountUsage: (summary?.accountUsage ?? []).map(accountUsageItem),
    wireProfile: summary?.wireProfile ?? null,
    usageRecords: summary?.usageRecords ?? [],
    poolSummary: summary?.poolSummary ?? emptyPoolSummary,
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
  const { accounts, traffic, tokens, cache } = cards
  const recentPoints = recentTrendWindow(points)
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
      valueFormatter: formatDashboardCompactNumber,
      icon: FileText,
      tone: 'success',
      sparkline: sparkline(
        recentPoints.map(point => point.tokensValue),
        'success',
      ),
      trend: trendState(tokens.todayTokensValue, tokens.yesterdayTokensValue, 'success'),
      details: [
        { label: '总 Token', value: tokens.totalTokens, tone: 'success' },
        { label: '总计费', value: tokens.totalBillingAmountUsd, tone: 'success' },
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

export function formatDashboardCompactNumber(value: number) {
  const normalized = Math.max(0, Math.round(value))
  if (normalized < 1_000)
    return new Intl.NumberFormat('zh-CN').format(normalized)

  for (const [unit, threshold] of [
    ['P', 1_000_000_000_000_000],
    ['T', 1_000_000_000_000],
    ['B', 1_000_000_000],
    ['M', 1_000_000],
    ['K', 1_000],
  ] as const) {
    if (normalized >= threshold) {
      const scaled = normalized / threshold
      const rounded = scaled >= 10 ? scaled.toFixed(1) : scaled.toFixed(2)
      return `${rounded.replace(/\.?0+$/, '')}${unit}`
    }
  }

  return new Intl.NumberFormat('zh-CN').format(normalized)
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

function accountUsageItem(item: DashboardSummary['accountUsage'][number]) {
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

function quotaUsedPercent(value: number | null) {
  if (typeof value !== 'number' || !Number.isFinite(value))
    return 0
  return clamp(Math.round(value), 0, 100)
}

function quotaTone(percent: number) {
  if (percent >= 95)
    return 'danger'
  if (percent >= 80)
    return 'warning'
  if (percent > 0)
    return 'success'
  return 'normal'
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
