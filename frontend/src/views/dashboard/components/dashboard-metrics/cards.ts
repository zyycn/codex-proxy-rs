import type { Component } from 'vue'
import type { MetricTone } from '../../constants'
import type { DashboardCards, DashboardTrendPoint } from '@/api'
import { Activity, FileText, Timer, Users } from '@lucide/vue'
import { t } from '@/i18n'
import { formatCompactNumber, formatInteger } from '@/utils/format'
import { formatDashboardRate } from '../../utils/format'

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

const emptyCards: DashboardCards = {
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
    totalBillingAmountUsd: '$0.00',
  },
  cache: {
    todayHitRate: '0%',
    todayHitRateValue: null,
    yesterdayHitRateValue: null,
    totalHitRate: '0%',
    totalCachedInputTokens: '0',
    averageFirstTokenLatencyMs: '0 ms',
  },
}

export function metricCards(
  cards: DashboardCards = emptyCards,
  points: DashboardTrendPoint[] = [],
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
          value: numericMetricDisplay(cache.averageFirstTokenLatencyMs, '0 ms'),
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
          value: formatDashboardUsd(numericMetricDisplay(tokens.totalBillingAmountUsd, '$0.00')),
          tone: 'success',
        },
      ],
    },
    {
      title: t('dashboard.metrics.cacheHits'),
      value: numericMetricDisplay(cache.todayHitRate, '0%'),
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
          value: numericMetricDisplay(cache.totalHitRate, '0%'),
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

function numericMetricDisplay(value: string, zero: string) {
  return !value || value === '—' || value === '-'
    ? zero
    : value
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
