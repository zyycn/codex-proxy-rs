import type { MetricTone } from '../../constants'
import type { DashboardTrendMetric, DashboardTrendPoint, DashboardTrendResponse } from '@/api'
import { computed } from 'vue'
import { t } from '@/i18n'
import { formatCompactNumber } from '@/utils/format'
import { formatDashboardRate } from '../../utils/format'

export function trendSeries(trend: DashboardTrendResponse | null) {
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
    value: trend.kind === 'errors' && item.metric === 'successRate'
      ? item.ratio ?? t('usage.health.noSamples')
      : item.value,
  }))
  return { points: trend.points, summary }
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

// 展示与交互共享指标身份，切换语言不改变高亮或颜色
const trendMetrics = computed(() => ({
  inputTokens: {
    label: t('dashboard.metrics.inputTokens'),
    tone: 'info',
    colorToken: 'color-blue-solid',
  },
  outputTokens: {
    label: t('dashboard.metrics.outputTokens'),
    tone: 'success',
    colorToken: 'color-green-solid',
  },
  cachedInputTokens: {
    label: t('dashboard.metrics.cachedInputTokens'),
    tone: 'normal',
    colorToken: 'color-text-tertiary',
  },
  firstTokenP95Ms: {
    label: t('dashboard.metrics.firstTokenP95Ms'),
    tone: 'normal',
    colorToken: 'color-cyan-solid',
  },
  latencyP95Ms: {
    label: t('dashboard.metrics.latencyP95Ms'),
    tone: 'warning',
    colorToken: 'color-orange-solid',
  },
  outputThroughputP50: {
    label: t('dashboard.metrics.outputThroughputP50'),
    tone: 'success',
    colorToken: 'color-green-solid',
  },
  errors: {
    label: t('dashboard.metrics.errors'),
    tone: 'danger',
    colorToken: 'color-red-solid',
  },
  successRate: {
    label: t('dashboard.metrics.successRate'),
    tone: 'success',
    colorToken: 'color-green-solid',
  },
  requests: {
    label: t('dashboard.metrics.totalRequests'),
    tone: 'info',
    colorToken: 'color-blue-solid',
  },
} satisfies Record<DashboardTrendMetric, { label: string, tone: MetricTone, colorToken: string }>))

export function trendMetricView(metric: DashboardTrendMetric) {
  return { metric, ...trendMetrics.value[metric] }
}
