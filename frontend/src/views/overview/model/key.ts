import type { ClientOverviewResponse } from '@/api'
import type {
  MetricCardView,
  RequestTrendKind,
  RequestTrendPoint,
  RequestTrendSummaryItem,
} from '@/views/overview/model/display'
import { Activity, Coins, FileText, Timer } from '@lucide/vue'

import { formatCompactNumber } from '@/utils/number'

export function keyOverviewMetrics(data: ClientOverviewResponse | null): MetricCardView[] {
  const today = data?.today.totals
  const lifetime = data?.lifetime
  const points = data?.today.trend ?? []
  const daily = data?.budget.daily
  const weekly = data?.budget.weekly

  return [
    {
      title: '额度',
      value: formatUsd(daily?.usedUsd),
      icon: Coins,
      tone: 'normal',
      sparkline: costSparkline(points),
      details: [
        { label: '今日可用', value: budgetRemaining(daily?.remainingUsd), tone: 'normal' },
        { label: '本周可用', value: budgetRemaining(weekly?.remainingUsd), tone: 'normal' },
      ],
    },
    {
      title: '请求次数',
      value: formatCompactNumber(today?.requestCount ?? 0),
      valueRaw: today?.requestCount ?? 0,
      valueFormatter: formatCompactNumber,
      icon: Activity,
      tone: 'info',
      sparkline: numberSparkline(points.map(point => point.requestCount), 'info'),
      details: [
        { label: '总请求', value: formatCompactNumber(lifetime?.requestCount ?? 0), tone: 'info' },
        { label: '首字均值', value: formatDuration(today?.averageFirstTokenLatencyMs), tone: 'info' },
      ],
    },
    {
      title: 'Token',
      value: formatCompactNumber(today?.totalTokens ?? 0),
      valueRaw: today?.totalTokens ?? 0,
      valueFormatter: formatCompactNumber,
      icon: FileText,
      tone: 'success',
      sparkline: numberSparkline(points.map(point => point.totalTokens), 'success'),
      details: [
        { label: '总 Token', value: formatCompactNumber(lifetime?.totalTokens ?? 0), tone: 'success' },
        { label: '总计费', value: formatUsd(lifetime?.billedUsd), tone: 'success' },
      ],
    },
    {
      title: '缓存命中',
      value: formatRate(today?.cachedTokens, today?.inputTokens),
      icon: Timer,
      tone: (today?.cachedTokens ?? 0) > 0 ? 'warning' : 'normal',
      sparkline: numberSparkline(points.map(point => rate(point.cachedTokens, point.inputTokens)), 'warning'),
      details: [
        { label: '总命中', value: formatRate(lifetime?.cachedTokens, lifetime?.inputTokens), tone: 'warning' },
        { label: '总缓存', value: formatCompactNumber(lifetime?.cachedTokens ?? 0), tone: 'warning' },
      ],
    },
  ]
}

export function keyOverviewTrend(data: ClientOverviewResponse | null, kind: RequestTrendKind) {
  const points = (data?.today.trend ?? []).map<RequestTrendPoint>((point) => {
    const errors = point.failureCount
    const successRate = point.requestCount ? (point.successCount / point.requestCount) * 100 : 0
    const cacheHitRate = rate(point.cachedTokens, point.inputTokens)
    const uncachedInputTokens = Math.max(0, point.inputTokens - point.cachedTokens)
    return {
      time: formatTrendTime(point.bucketStart),
      bucket: point.bucketStart,
      label: formatTrendTime(point.bucketStart),
      requests: formatCompactNumber(point.requestCount),
      requestsValue: point.requestCount,
      inputTokens: formatCompactNumber(point.inputTokens),
      inputTokensValue: point.inputTokens,
      outputTokens: formatCompactNumber(point.outputTokens),
      outputTokensValue: point.outputTokens,
      cachedTokens: formatCompactNumber(point.cachedTokens),
      cachedTokensValue: point.cachedTokens,
      uncachedInputTokens: formatCompactNumber(uncachedInputTokens),
      uncachedInputTokensValue: uncachedInputTokens,
      effectiveTokens: formatCompactNumber(uncachedInputTokens + point.outputTokens),
      effectiveTokensValue: uncachedInputTokens + point.outputTokens,
      cacheHitRate: formatPercent(cacheHitRate),
      cacheHitRateValue: cacheHitRate,
      tokensValue: point.totalTokens,
      errors: formatCompactNumber(errors),
      errorsValue: errors,
      latency: formatDuration(point.latencyP50Ms),
      latencyValue: point.latencyP50Ms,
      maxLatency: formatDuration(point.latencyP95Ms),
      maxLatencyValue: point.latencyP95Ms,
      minLatency: formatDuration(point.latencyP50Ms),
      minLatencyValue: point.latencyP50Ms,
      successRate: `${successRate.toFixed(1)}%`,
      successRateValue: successRate,
      firstTokenP50Ms: point.firstTokenP50Ms,
      firstTokenP95Ms: point.firstTokenP95Ms,
      latencyP95Ms: point.latencyP95Ms,
      outputThroughputP50: point.outputThroughputP50,
    }
  })

  return { points, summary: trendSummary(points, kind) }
}

function trendSummary(points: RequestTrendPoint[], kind: RequestTrendKind): RequestTrendSummaryItem[] {
  if (kind === 'latency') {
    return [
      summaryItem('首字 P95', max(points, point => point.firstTokenP95Ms), 'normal', '--cp-color-cyan-solid', formatDuration),
      summaryItem('总耗时 P95', max(points, point => point.latencyP95Ms), 'warning', '--cp-color-orange-solid', formatDuration),
      summaryItem('吞吐 P50', average(points, point => point.outputThroughputP50), 'success', '--cp-color-green-solid', value => value == null ? '—' : `${Math.round(value)} tok/s`),
    ]
  }
  if (kind === 'errors') {
    const requests = sum(points, point => point.requestsValue)
    const errors = sum(points, point => point.errorsValue)
    return [
      { label: '错误数', value: formatCompactNumber(errors), tone: 'danger', colorVar: '--cp-color-red-solid' },
      { label: '成功率', value: requests ? `${(((requests - errors) / requests) * 100).toFixed(1)}%` : '0.0%', tone: 'success', colorVar: '--cp-color-green-solid' },
      { label: '总请求', value: formatCompactNumber(requests), tone: 'info', colorVar: '--cp-color-blue-solid' },
    ]
  }
  return [
    { label: '输入', value: formatCompactNumber(sum(points, point => point.inputTokensValue)), tone: 'info', colorVar: '--cp-color-blue-solid' },
    { label: '输出', value: formatCompactNumber(sum(points, point => point.outputTokensValue)), tone: 'success', colorVar: '--cp-color-green-solid' },
    { label: '缓存', value: formatCompactNumber(sum(points, point => point.cachedTokensValue)), tone: 'normal', colorVar: '--cp-color-text-tertiary' },
  ]
}

function summaryItem(
  label: string,
  value: number | null,
  tone: RequestTrendSummaryItem['tone'],
  colorVar: string,
  formatter: (value: number | null | undefined) => string,
): RequestTrendSummaryItem {
  return { label, value: formatter(value), tone, colorVar }
}

function sum(points: RequestTrendPoint[], selector: (point: RequestTrendPoint) => number) {
  return points.reduce((total, point) => total + selector(point), 0)
}

function max(points: RequestTrendPoint[], selector: (point: RequestTrendPoint) => number | null) {
  const values = points.map(selector).filter((value): value is number => value !== null)
  return values.length ? Math.max(...values) : null
}

function average(points: RequestTrendPoint[], selector: (point: RequestTrendPoint) => number | null) {
  const values = points.map(selector).filter((value): value is number => value !== null)
  return values.length ? values.reduce((total, value) => total + value, 0) / values.length : null
}

function numberSparkline(values: number[], tone: MetricCardView['tone']) {
  return values.some(value => value > 0) ? { values, tone } : undefined
}

function costSparkline(points: ClientOverviewResponse['today']['trend']) {
  return numberSparkline(points.map(point => Number(point.billedUsd) || 0), 'normal')
}

function budgetRemaining(value: string | null | undefined) {
  return value == null ? '无限制' : formatUsd(value)
}

export function formatUsd(value: string | number | null | undefined) {
  const amount = Number(value ?? 0)
  if (!Number.isFinite(amount))
    return '—'
  return `$${amount < 10_000 ? compactUsdAmount(amount) : `${compactUsdAmount(amount / 1_000)}k`}`
}

function compactUsdAmount(value: number) {
  const fixed = value.toFixed(2)
  return fixed.endsWith('.00') ? fixed.slice(0, -3) : fixed
}

function rate(numerator = 0, denominator = 0) {
  return denominator > 0 ? Math.min(1, numerator / denominator) : 0
}

function formatRate(numerator = 0, denominator = 0) {
  return formatPercent(rate(numerator, denominator))
}

function formatPercent(value: number) {
  return `${(value * 100).toFixed(1)}%`
}

export function formatDuration(value: number | null | undefined) {
  if (value == null || !Number.isFinite(value))
    return '—'
  if (value < 1_000)
    return `${Math.round(value)} ms`
  return `${(value / 1_000).toFixed(value < 10_000 ? 1 : 0)} s`
}

function formatTrendTime(value: string) {
  return new Intl.DateTimeFormat('zh-CN', {
    timeZone: 'Asia/Shanghai',
    hour: '2-digit',
    minute: '2-digit',
    hourCycle: 'h23',
  }).format(new Date(value))
}
