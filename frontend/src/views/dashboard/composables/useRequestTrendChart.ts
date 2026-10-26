import type { BarSeriesOption, EChartsOption, LineSeriesOption } from 'echarts'
import type { Ref } from 'vue'
import type { dashboardTrendView, normalizeDashboardTrendKind } from '../presenter'
import type { DashboardTrendMetric } from '@/api'
import { usePreferredReducedMotion } from '@vueuse/core'
import { computed, shallowRef, watch } from 'vue'
import { sampleGapBridgeSeries } from '@/components/charts/timeSeriesGap'
import { chartTooltipContent, chartTooltipStyle, tooltipIndex } from '@/components/charts/tooltip'

import { useChartPalette } from '@/composables/useChartPalette'
import { t } from '@/i18n'
import { trendMetricView } from '../presenter'

type TrendKind = ReturnType<typeof normalizeDashboardTrendKind>
type TrendView = ReturnType<typeof dashboardTrendView>
type TrendPoint = TrendView['points'][number]
type TrendSummaryItem = TrendView['summary'][number]

interface LineSeriesOptions {
  area?: boolean
  areaStartAlpha?: string
  areaEndAlpha?: string
  lineType?: 'solid' | 'dashed' | 'dotted'
  smooth?: boolean | number
  stack?: string
  width?: number
  z?: number
  symbol?: string
  symbolSize?: number
  showSymbol?: boolean
  xAxisIndex?: number
  yAxisIndex?: number
}

interface BarSeriesOptions {
  maxWidth?: number
  xAxisIndex?: number
  yAxisIndex?: number
  silent?: boolean
  z?: number
  opacity?: number
  borderRadius?: number | [number, number, number, number]
}

const tabs = computed(() => ([
  {
    label: t('usage.usage'),
    value: 'usage',
  },
  {
    label: t('dashboard.trends.latency'),
    value: 'latency',
  },
  {
    label: t('dashboard.trends.errors'),
    value: 'errors',
  },
]))

export function useRequestTrendChart(options: {
  points: Readonly<Ref<TrendPoint[]>>
  activeKind: Ref<TrendKind>
  onTrendChange: (kind: TrendKind) => void
}) {
  const { points, activeKind } = options
  const { color: themeColor, palette } = useChartPalette()
  const preferredMotion = usePreferredReducedMotion()
  const pinnedSummaryMetric = shallowRef<DashboardTrendMetric>()

  const hasSamples = computed(() =>
    points.value.some(
      point =>
        point.requestsValue > 0
        || point.errorsValue > 0
        || (point.latencyValue ?? 0) > 0
        || (point.firstTokenP95Ms ?? 0) > 0
        || (point.latencyP95Ms ?? 0) > 0
        || (point.outputThroughputP50 ?? 0) > 0
        || point.tokensValue > 0
        || point.cachedInputTokensValue > 0,
    ),
  )

  const activeRequestBucketCount = computed(
    () => points.value.filter(point => Number(point.requestsValue) > 0).length,
  )
  const isSparseTrend = computed(
    () => activeRequestBucketCount.value > 0 && activeRequestBucketCount.value <= 3,
  )

  const chartOption = computed<EChartsOption>(() => {
    const buckets = points.value.map(point => point.bucket)
    return {
      ...getCoordinateSystem(buckets),
      series: getSeries(),
      animationDuration: preferredMotion.value === 'reduce'
        ? 0
        : 420,
      animationDurationUpdate: preferredMotion.value === 'reduce'
        ? 0
        : 220,
      animationEasing: 'cubicOut',
      animationEasingUpdate: 'cubicOut',
      tooltip: {
        trigger: 'axis',
        ...chartTooltipStyle(palette.value, {
          axisPointer: true,
          confine: true,
          fontFamily: themeColor('--font-sans', 'sans-serif'),
          fontWeight: 600,
        }),
        formatter: formatTooltip,
      },
    }
  })

  function getCoordinateSystem(buckets: string[]) {
    const muted = palette.value.textMuted
    const gridLine = palette.value.divider
    const axisLine = palette.value.border
    return {
      grid: {
        left: 8,
        right: activeKind.value === 'latency'
          ? 8
          : 10,
        top: 10,
        bottom: 2,
        outerBoundsMode: 'same' as const,
        outerBoundsContain: 'axisLabel' as const,
      },
      xAxis: {
        type: 'category' as const,
        data: buckets,
        boundaryGap: activeKind.value === 'errors',
        axisLine: {
          show: true,
          lineStyle: { color: axisLine },
        },
        axisTick: { show: false },
        axisLabel: {
          color: muted,
          fontSize: 10,
          fontFamily: themeColor('--font-mono', 'monospace'),
          hideOverlap: true,
          formatter: (_value: string, index: number) => points.value[index]?.time ?? '',
        },
      },
      yAxis: [
        {
          type: 'value' as const,
          min: 0,
          splitNumber: 3,
          axisLine: { show: false },
          axisTick: { show: false },
          axisLabel: {
            color: muted,
            fontSize: 9,
            fontFamily: themeColor('--font-mono', 'monospace'),
            formatter: formatPrimaryAxisValue,
          },
          splitLine: {
            show: true,
            lineStyle: { color: gridLine, type: 'dashed' as const, opacity: 0.72 },
          },
        },
        {
          type: 'value' as const,
          min: 0,
          max: activeKind.value === 'errors'
            ? 100
            : undefined,
          splitNumber: 3,
          axisLine: { show: false },
          axisTick: { show: false },
          axisLabel: {
            show: true,
            color: activeKind.value === 'usage'
              ? trendColor('outputTokens')
              : trendColor('successRate'),
            fontSize: 9,
            fontFamily: themeColor('--font-mono', 'monospace'),
            formatter: formatSecondaryAxisValue,
          },
          splitLine: { show: false },
        },
      ],
    }
  }

  function formatPrimaryAxisValue(value: number) {
    if (activeKind.value === 'latency') {
      return value >= 1000
        ? `${formatAxisNumber(value / 1000)} s`
        : `${Math.round(value)} ms`
    }
    return formatAxisCompact(value)
  }

  function formatSecondaryAxisValue(value: number) {
    if (activeKind.value === 'errors')
      return `${Math.round(value)}%`
    if (activeKind.value === 'latency')
      return `${formatAxisCompact(value)}/s`
    return formatAxisCompact(value)
  }

  function formatTooltip(params: unknown) {
    const rows = Array.isArray(params)
      ? params
      : [params]
    const point = points.value[tooltipIndex(rows[0])]
    const title = point?.label ?? ''

    if (activeKind.value === 'usage') {
      return chartTooltipContent(title, [
        tooltipItem(t('dashboard.metrics.inputTokens'), point?.inputTokens, trendColor('inputTokens')),
        tooltipItem(t('dashboard.metrics.outputTokens'), point?.outputTokens, trendColor('outputTokens')),
        tooltipItem(
          t('dashboard.metrics.cachedInputTokens'),
          point?.cachedInputTokens,
          trendColor('cachedInputTokens'),
        ),
        tooltipItem(
          t('dashboard.metrics.cacheHits'),
          zeroAwareRateText(point, 'cacheHitRate', 'inputTokensValue'),
          trendColor('cachedInputTokens'),
        ),
        tooltipItem(t('dashboard.metrics.requests'), point?.requests, palette.value.textSecondary),
      ])
    }

    const lines = rows.map((row) => {
      const name = tooltipValue(row, 'seriesName')
      const metric = tooltipValue(row, 'seriesId')
      const value = tooltipValue(row, 'value')
      const marker = tooltipValue(row, 'marker')
      return `${marker}${name}: ${tooltipDisplayValue(point, metric, value)}`
    })
    return chartTooltipContent(title, lines)
  }

  function tooltipDisplayValue(point: TrendPoint | undefined, metric: string, value: string) {
    switch (metric) {
      case 'firstTokenP95Ms':
        return formatLatency(point?.firstTokenP95Ms, value)
      case 'latencyP95Ms':
        return formatLatency(point?.latencyP95Ms, value)
      case 'outputThroughputP50':
        return point?.outputThroughputP50 == null
          ? value
          : `${point.outputThroughputP50} tok/s`
      case 'errors':
        return point?.errors ?? value
      case 'successRate':
        return zeroAwareRateText(point, 'successRate', 'requestsValue') ?? value
      case 'requests':
        return point?.requests ?? value
      default:
        return value
    }
  }

  function getSeries(): Array<BarSeriesOption | LineSeriesOption> {
    if (activeKind.value === 'usage') {
      const cacheColor = trendColor('cachedInputTokens')
      const inputColor = trendColor('inputTokens')
      const outputColor = trendColor('outputTokens')
      return [
        lineSeries('cachedInputTokens', seriesValues('cachedInputTokensValue'), cacheColor, {
          area: true,
          areaStartAlpha: '30',
          areaEndAlpha: '08',
          lineType: 'dashed',
          smooth: 0.26,
          stack: 'input-composition',
          width: 1.25,
          z: 1,
        }),
        lineSeries('inputTokens', seriesValues('uncachedInputTokensValue'), inputColor, {
          area: true,
          areaStartAlpha: '2A',
          areaEndAlpha: '05',
          smooth: 0.26,
          stack: 'input-composition',
          width: 2.3,
          z: 3,
        }),
        lineSeries('outputTokens', seriesValues('outputTokensValue'), outputColor, {
          smooth: 0.24,
          width: 2.1,
          yAxisIndex: 1,
          z: 4,
        }),
      ]
    }

    if (activeKind.value === 'latency') {
      return [
        ...gapLineSeries(
          'latencyP95Ms',
          activeSeriesValues('latencyP95Ms'),
          trendColor('latencyP95Ms'),
          {
            lineType: 'dashed',
            smooth: 0.2,
            width: 1.8,
            z: 2,
          },
        ),
        ...gapLineSeries(
          'firstTokenP95Ms',
          activeSeriesValues('firstTokenP95Ms'),
          trendColor('firstTokenP95Ms'),
          {
            area: true,
            smooth: 0.24,
            width: 2.5,
            z: 4,
          },
        ),
        ...gapLineSeries(
          'outputThroughputP50',
          activeSeriesValues('outputThroughputP50'),
          trendColor('outputThroughputP50'),
          {
            smooth: 0.2,
            width: 2,
            yAxisIndex: 1,
            z: 3,
          },
        ),
      ]
    }

    return [
      barSeries(
        'errors',
        seriesValues('errorsValue'),
        trendColor('errors'),
      ),
      ...gapLineSeries(
        'successRate',
        activeSeriesValues('successRateValue'),
        trendColor('successRate'),
        { width: 2.4, yAxisIndex: 1 },
      ),
      lineSeries(
        'requests',
        seriesValues('requestsValue'),
        trendColor('requests'),
        { lineType: 'dashed', width: 1.8 },
      ),
    ]
  }

  function seriesValues(key: string) {
    return points.value.map(point => trendPointNumber(point, key))
  }

  function activeSeriesValues(key: string) {
    return points.value.map(point =>
      Number(point.requestsValue) > 0
        ? trendPointNumber(point, key)
        : 0,
    )
  }

  function lineSeries(
    metric: DashboardTrendMetric,
    data: (number | null)[],
    color: string,
    lineOptions: LineSeriesOptions = {},
  ): LineSeriesOption {
    const area = lineOptions.area ?? false
    const muted = isSeriesMuted(metric)
    return {
      id: metric,
      name: trendMetricView(metric).label,
      type: 'line',
      data,
      connectNulls: false,
      smooth: lineOptions.smooth ?? true,
      symbol: lineOptions.symbol ?? (isSparseTrend.value ? 'circle' : 'none'),
      symbolSize: lineOptions.symbolSize ?? 5,
      showSymbol: lineOptions.showSymbol ?? isSparseTrend.value,
      stack: lineOptions.stack,
      xAxisIndex: lineOptions.xAxisIndex ?? 0,
      yAxisIndex: lineOptions.yAxisIndex ?? 0,
      z: lineOptions.z ?? 2,
      lineStyle: {
        color,
        type: lineOptions.lineType ?? 'solid',
        width: lineOptions.width ?? 2.2,
        opacity: muted ? 0.18 : 1,
      },
      itemStyle: {
        color,
        opacity: muted ? 0.18 : 1,
      },
      emphasis: { disabled: true },
      areaStyle: area
        ? {
            opacity: muted ? 0.12 : 1,
            color: {
              type: 'linear',
              x: 0,
              y: 0,
              x2: 0,
              y2: 1,
              colorStops: [
                { offset: 0, color: `${color}${lineOptions.areaStartAlpha ?? '18'}` },
                { offset: 1, color: `${color}${lineOptions.areaEndAlpha ?? '02'}` },
              ],
            },
          }
        : undefined,
    }
  }

  function gapLineSeries(
    metric: DashboardTrendMetric,
    data: (number | null)[],
    color: string,
    lineOptions: LineSeriesOptions = {},
  ) {
    const primary = lineSeries(metric, data, color, lineOptions)
    return [
      primary,
      ...sampleGapBridgeSeries(data, {
        name: metric,
        color,
        width: lineOptions.width,
        xAxisIndex: lineOptions.xAxisIndex,
        yAxisIndex: lineOptions.yAxisIndex,
        z: lineOptions.z,
      }),
    ]
  }

  function barSeries(
    metric: DashboardTrendMetric,
    data: (number | null)[],
    color: string,
    barOptions: BarSeriesOptions = {},
  ): BarSeriesOption {
    const muted = isSeriesMuted(metric)
    return {
      id: metric,
      name: trendMetricView(metric).label,
      type: 'bar',
      data,
      barMaxWidth: barOptions.maxWidth ?? 16,
      xAxisIndex: barOptions.xAxisIndex ?? 0,
      yAxisIndex: barOptions.yAxisIndex ?? 0,
      silent: barOptions.silent ?? false,
      z: barOptions.z ?? 2,
      itemStyle: {
        color,
        opacity: muted
          ? 0.14
          : (barOptions.opacity ?? 0.72),
        borderRadius: barOptions.borderRadius ?? [3, 3, 0, 0],
      },
      emphasis: { disabled: true },
    }
  }

  function summaryMarkerStyle(item: TrendSummaryItem) {
    return item.colorVar
      ? { backgroundColor: `var(${item.colorVar})` }
      : undefined
  }

  function trendColor(metric: DashboardTrendMetric) {
    return themeColor(trendMetricView(metric).colorVar, palette.value.textSecondary)
  }

  function handleTrendChange(value: string) {
    options.onTrendChange(value as TrendKind)
  }

  function toggleSummarySeries(metric: DashboardTrendMetric) {
    pinnedSummaryMetric.value = pinnedSummaryMetric.value === metric
      ? undefined
      : metric
  }

  function isSeriesMuted(metric: DashboardTrendMetric) {
    return Boolean(pinnedSummaryMetric.value && pinnedSummaryMetric.value !== metric)
  }

  function isSummarySeriesActive(metric: DashboardTrendMetric) {
    return pinnedSummaryMetric.value === metric
  }

  watch(activeKind, () => {
    pinnedSummaryMetric.value = undefined
  })

  return {
    tabs,
    pinnedSummaryMetric,
    hasSamples,
    chartOption,
    summaryMarkerStyle,
    handleTrendChange,
    toggleSummarySeries,
    isSummarySeriesActive,
  }
}

function formatAxisCompact(value: number) {
  const normalized = Math.abs(value)
  if (normalized >= 1_000_000_000)
    return `${formatAxisNumber(value / 1_000_000_000)}B`
  if (normalized >= 1_000_000)
    return `${formatAxisNumber(value / 1_000_000)}M`
  if (normalized >= 1_000)
    return `${formatAxisNumber(value / 1_000)}K`
  return `${Math.round(value)}`
}

function formatAxisNumber(value: number) {
  return value >= 10
    ? value.toFixed(0)
    : value.toFixed(1).replace(/\.0$/, '')
}

function formatLatency(value: number | null | undefined, fallback: string) {
  if (value == null || !Number.isFinite(value))
    return fallback
  if (value < 1_000)
    return `${Math.round(value)} ms`
  if (value < 60_000)
    return `${formatAxisNumber(value / 1_000)} s`
  return `${formatAxisNumber(value / 60_000)} min`
}

function tooltipItem(label: string, value: string | undefined, color: string) {
  if (!value)
    return ''
  const marker = `<span style="display:inline-block;width:7px;height:7px;margin-right:6px;border-radius:999px;background:${color}"></span>`
  return `${marker}${label}: ${value}`
}

function tooltipValue(source: unknown, key: string) {
  if (typeof source !== 'object' || source === null || !(key in source))
    return ''
  const value = (source as Record<string, unknown>)[key]
  return typeof value === 'number' || typeof value === 'string'
    ? String(value)
    : ''
}

function trendPointProperty(point: TrendPoint | undefined, key: string) {
  if (!point || !(key in point))
    return undefined
  return (point as unknown as Record<string, unknown>)[key]
}

function trendPointText(point: TrendPoint | undefined, key: string) {
  const value = trendPointProperty(point, key)
  return typeof value === 'string'
    ? value
    : undefined
}

function zeroAwareRateText(
  point: TrendPoint | undefined,
  valueKey: string,
  activityKey: string,
) {
  const activity = trendPointProperty(point, activityKey)
  if (typeof activity === 'number' && activity <= 0)
    return '0.0%'
  return trendPointText(point, valueKey)
}

function trendPointNumber(point: TrendPoint, key: string) {
  const value = trendPointProperty(point, key)
  return typeof value === 'number' && Number.isFinite(value)
    ? value
    : null
}
