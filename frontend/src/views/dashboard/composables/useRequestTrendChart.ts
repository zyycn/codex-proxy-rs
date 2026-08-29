import type { BarSeriesOption, EChartsOption, LineSeriesOption } from 'echarts'
import type { Ref } from 'vue'
import type { dashboardTrendView, normalizeDashboardTrendKind } from './useDashboard'
import { usePreferredReducedMotion } from '@vueuse/core'
import { computed, shallowRef, watch } from 'vue'

import { chartTooltipStyle, tooltipIndex } from '@/components/charts/tooltip'
import { useChartPalette } from '@/composables/useChartPalette'

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

const tabs = [
  { label: '用量', value: 'usage' },
  { label: '延迟', value: 'latency' },
  { label: '错误', value: 'errors' },
]

export function useRequestTrendChart(options: {
  points: Readonly<Ref<TrendPoint[]>>
  summary: Readonly<Ref<TrendSummaryItem[]>>
  activeKind: Ref<TrendKind>
  onTrendChange: (kind: TrendKind) => void
}) {
  const { points, summary, activeKind } = options
  const { color: themeColor, palette } = useChartPalette()
  const preferredMotion = usePreferredReducedMotion()
  const pinnedSummaryLabel = shallowRef<string>()

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
        || point.cachedTokensValue > 0,
    ),
  )

  const activeRequestBucketCount = computed(
    () => points.value.filter(point => Number(point.requestsValue) > 0).length,
  )
  const isSparseTrend = computed(
    () => activeRequestBucketCount.value > 0 && activeRequestBucketCount.value <= 3,
  )

  const chartOption = computed<EChartsOption>(() => {
    const times = points.value.map(point => point.time)
    return {
      ...getCoordinateSystem(times),
      series: getSeries(),
      animationDuration: preferredMotion.value === 'reduce' ? 0 : 420,
      animationDurationUpdate: preferredMotion.value === 'reduce' ? 0 : 220,
      animationEasing: 'cubicOut',
      animationEasingUpdate: 'cubicOut',
      tooltip: {
        trigger: 'axis',
        ...chartTooltipStyle(palette.value, {
          axisPointer: true,
          confine: true,
          fontFamily: 'Inter, system-ui, sans-serif',
          fontWeight: 600,
        }),
        formatter: formatTooltip,
      },
    }
  })

  function getCoordinateSystem(times: string[]) {
    const muted = palette.value.textMuted
    const gridLine = palette.value.divider
    const axisLine = palette.value.border
    return {
      grid: {
        left: 8,
        right: activeKind.value === 'latency' ? 8 : 10,
        top: 10,
        bottom: 2,
        outerBoundsMode: 'same' as const,
        outerBoundsContain: 'axisLabel' as const,
      },
      xAxis: {
        type: 'category' as const,
        data: times,
        boundaryGap: activeKind.value === 'errors',
        axisLine: { show: true, lineStyle: { color: axisLine } },
        axisTick: { show: false },
        axisLabel: {
          color: muted,
          fontSize: 10,
          fontFamily: 'JetBrains Mono Variable, JetBrains Mono',
          hideOverlap: true,
          interval: showTwoHourLabel,
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
            fontFamily: 'JetBrains Mono Variable, JetBrains Mono',
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
          max: activeKind.value === 'errors' ? 100 : undefined,
          splitNumber: 3,
          axisLine: { show: false },
          axisTick: { show: false },
          axisLabel: {
            show: true,
            color: activeKind.value === 'usage'
              ? trendColor('输出', '--cp-color-green-solid', '#12B981')
              : trendColor('成功', '--cp-color-green-solid', '#12B981'),
            fontSize: 9,
            fontFamily: 'JetBrains Mono Variable, JetBrains Mono',
            formatter: formatSecondaryAxisValue,
          },
          splitLine: { show: false },
        },
      ],
    }
  }

  function formatPrimaryAxisValue(value: number) {
    if (activeKind.value === 'latency') {
      return value >= 1000 ? `${formatAxisNumber(value / 1000)}s` : `${Math.round(value)}ms`
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
    const rows = Array.isArray(params) ? params : [params]
    const title = tooltipValue(rows[0], 'axisValueLabel')
    const point = points.value[tooltipIndex(rows[0])]

    if (activeKind.value === 'usage') {
      return [
        title,
        tooltipItem('输入', point?.inputTokens, trendColor('输入', '--cp-color-blue-solid', '#5983F4')),
        tooltipItem('输出', point?.outputTokens, trendColor('输出', '--cp-color-green-solid', '#12B981')),
        tooltipItem(
          '缓存',
          point?.cachedTokens,
          trendColor('缓存', '--cp-color-text-tertiary', '#94A3B8'),
        ),
        tooltipItem(
          '缓存命中',
          trendPointText(point, 'cacheHitRate'),
          trendColor('缓存', '--cp-color-text-tertiary', '#94A3B8'),
        ),
        tooltipItem('请求', point?.requests, palette.value.textSecondary),
      ]
        .filter(Boolean)
        .join('<br/>')
    }

    const lines = rows.map((row) => {
      const name = tooltipValue(row, 'seriesName')
      const value = tooltipValue(row, 'value')
      const marker = tooltipValue(row, 'marker')
      return `${marker}${name}: ${tooltipDisplayValue(point, name, value)}`
    })
    return [title, ...lines].filter(Boolean).join('<br/>')
  }

  function tooltipDisplayValue(point: TrendPoint | undefined, name: string, value: string) {
    if (activeKind.value === 'usage') {
      if (name === '输入')
        return point?.inputTokens ?? value
      if (name === '输出')
        return point?.outputTokens ?? value
      if (name === '缓存')
        return point?.cachedTokens ?? value
    }
    if (activeKind.value === 'latency') {
      if (name === '首字 P95')
        return formatLatency(point?.firstTokenP95Ms, value)
      if (name === '总耗时 P95')
        return formatLatency(point?.latencyP95Ms, value)
      if (name === '吞吐 P50')
        return point?.outputThroughputP50 == null ? value : `${point.outputThroughputP50} tok/s`
    }
    if (name === '错误数')
      return point?.errors ?? value
    if (name === '成功率')
      return point?.successRate ?? value
    if (name === '总请求')
      return point?.requests ?? value
    return value
  }

  function getSeries() {
    if (activeKind.value === 'usage') {
      const cacheColor = trendColor('缓存', '--cp-color-text-tertiary', '#94A3B8')
      const inputColor = trendColor('输入', '--cp-color-blue-solid', '#5983F4')
      const outputColor = trendColor('输出', '--cp-color-green-solid', '#12B981')
      return [
        lineSeries('缓存', activeSeriesValues('cachedTokensValue'), cacheColor, {
          area: true,
          areaStartAlpha: '30',
          areaEndAlpha: '08',
          lineType: 'dashed',
          smooth: 0.26,
          stack: 'input-composition',
          width: 1.25,
          z: 1,
        }),
        lineSeries('输入', activeSeriesValues('uncachedInputTokensValue'), inputColor, {
          area: true,
          areaStartAlpha: '2A',
          areaEndAlpha: '05',
          smooth: 0.26,
          stack: 'input-composition',
          width: 2.3,
          z: 3,
        }),
        lineSeries('输出', activeSeriesValues('outputTokensValue'), outputColor, {
          smooth: 0.24,
          width: 2.1,
          yAxisIndex: 1,
          z: 4,
        }),
      ]
    }

    if (activeKind.value === 'latency') {
      return [
        lineSeries(
          '总耗时 P95',
          activeSeriesValues('latencyP95Ms'),
          trendColor('总耗时', '--cp-color-orange-solid', '#F59E0B'),
          { lineType: 'dashed', smooth: 0.2, width: 1.8, z: 2 },
        ),
        lineSeries(
          '首字 P95',
          activeSeriesValues('firstTokenP95Ms'),
          trendColor('首字', '--cp-color-blue-solid', '#5983F4'),
          { area: true, smooth: 0.24, width: 2.5, z: 4 },
        ),
        lineSeries(
          '吞吐 P50',
          activeSeriesValues('outputThroughputP50'),
          trendColor('吞吐', '--cp-color-green-solid', '#12B981'),
          { smooth: 0.2, width: 2, yAxisIndex: 1, z: 3 },
        ),
      ]
    }

    return [
      barSeries(
        '错误数',
        seriesValues('errorsValue'),
        trendColor('错误', '--cp-color-red-solid', '#EF4444'),
      ),
      lineSeries(
        '成功率',
        seriesValues('successRateValue'),
        trendColor('成功', '--cp-color-green-solid', '#12B981'),
        { width: 2.4, yAxisIndex: 1 },
      ),
      lineSeries(
        '总请求',
        seriesValues('requestsValue'),
        trendColor('请求', '--cp-color-blue-solid', '#5983F4'),
        { lineType: 'dashed', width: 1.8 },
      ),
    ]
  }

  function seriesValues(key: string) {
    return points.value.map(point => trendPointNumber(point, key))
  }

  function activeSeriesValues(key: string) {
    return points.value.map(point =>
      Number(point.requestsValue) > 0 ? trendPointNumber(point, key) : null,
    )
  }

  function lineSeries(
    name: string,
    data: (number | null)[],
    color: string,
    lineOptions: LineSeriesOptions = {},
  ): LineSeriesOption {
    const area = lineOptions.area ?? false
    const muted = isSeriesMuted(name)
    return {
      name,
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
      itemStyle: { color, opacity: muted ? 0.18 : 1 },
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

  function barSeries(
    name: string,
    data: (number | null)[],
    color: string,
    barOptions: BarSeriesOptions = {},
  ): BarSeriesOption {
    const muted = isSeriesMuted(name)
    return {
      name,
      type: 'bar',
      data,
      barMaxWidth: barOptions.maxWidth ?? 16,
      xAxisIndex: barOptions.xAxisIndex ?? 0,
      yAxisIndex: barOptions.yAxisIndex ?? 0,
      silent: barOptions.silent ?? false,
      z: barOptions.z ?? 2,
      itemStyle: {
        color,
        opacity: muted ? 0.14 : (barOptions.opacity ?? 0.72),
        borderRadius: barOptions.borderRadius ?? [3, 3, 0, 0],
      },
      emphasis: { disabled: true },
    }
  }

  function summaryMarkerStyle(item: TrendSummaryItem) {
    return item.colorVar ? { backgroundColor: `var(${item.colorVar})` } : undefined
  }

  function trendColor(label: string, fallbackVar: string, fallback: string) {
    const colorVar = summary.value.find(item => String(item.label).includes(label))?.colorVar
    return themeColor(colorVar || fallbackVar, fallback)
  }

  function handleTrendChange(value: string) {
    options.onTrendChange(value as TrendKind)
  }

  function toggleSummarySeries(label: string) {
    pinnedSummaryLabel.value = pinnedSummaryLabel.value === label ? undefined : label
  }

  function isSeriesMuted(name: string) {
    return Boolean(pinnedSummaryLabel.value && pinnedSummaryLabel.value !== name)
  }

  function isSummarySeriesActive(label: string) {
    return pinnedSummaryLabel.value === label
  }

  watch(activeKind, () => {
    pinnedSummaryLabel.value = undefined
  })

  return {
    tabs,
    pinnedSummaryLabel,
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
  return value >= 10 ? value.toFixed(0) : value.toFixed(1).replace(/\.0$/, '')
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

function showTwoHourLabel(_index: number, value: string) {
  const [hour, minute] = value.split(':').map(Number)
  return minute === 0 && hour % 2 === 0
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
  return typeof value === 'number' || typeof value === 'string' ? String(value) : ''
}

function trendPointProperty(point: TrendPoint | undefined, key: string) {
  if (!point || !(key in point))
    return undefined
  return (point as Record<string, unknown>)[key]
}

function trendPointText(point: TrendPoint | undefined, key: string) {
  const value = trendPointProperty(point, key)
  return typeof value === 'string' ? value : undefined
}

function trendPointNumber(point: TrendPoint, key: string) {
  const value = trendPointProperty(point, key)
  return typeof value === 'number' && Number.isFinite(value) ? value : null
}
