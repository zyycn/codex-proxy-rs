<script setup lang="ts">
import { computed } from 'vue'
import type { EChartsOption } from 'echarts'
import { storeToRefs } from 'pinia'
import BaseCard from '../../../components/base/BaseCard.vue'
import BaseChart from '../../../components/charts/BaseChart.vue'
import BaseEmpty from '../../../components/base/BaseEmpty.vue'
import BaseSegmented from '../../../components/base/BaseSegmented.vue'
import { useUiStore } from '../../../stores/modules/ui'

type TrendKind = 'usage' | 'latency' | 'errors'

const props = defineProps<{
  points: any[]
  summary: any[]
  loading?: boolean
}>()

const emit = defineEmits<{
  trendChange: [kind: TrendKind]
}>()

const tabs = [
  { label: '用量', value: 'usage' },
  { label: '延迟', value: 'latency' },
  { label: '错误', value: 'errors' },
]
const activeKind = defineModel<TrendKind>('kind', { required: true })
const { themeRevision } = storeToRefs(useUiStore())

const hasSamples = computed(() =>
  props.points.some(
    (point) =>
      point.requestsValue > 0 ||
      point.errorsValue > 0 ||
      point.latencyValue > 0 ||
      point.tokensValue > 0 ||
      point.cachedTokensValue > 0,
  ),
)

const activeRequestBucketCount = computed(
  () => props.points.filter((point) => Number(point.requestsValue) > 0).length,
)
const isSparseTrend = computed(
  () => activeRequestBucketCount.value > 0 && activeRequestBucketCount.value <= 3,
)

const chartOption = computed<EChartsOption>(() => {
  const times = props.points.map((p) => p.time)
  const series = getSeries()
  const coordinate = getCoordinateSystem(times)

  return {
    ...coordinate,
    series,
    animationDuration: 420,
    animationDurationUpdate: 220,
    animationEasing: 'cubicOut',
    animationEasingUpdate: 'cubicOut',
    tooltip: {
      trigger: 'axis',
      confine: true,
      backgroundColor: themeColor('--cp-bg-surface', '#fff'),
      borderColor: 'transparent',
      borderWidth: 0,
      padding: [10, 14],
      textStyle: {
        color: themeColor('--cp-text-primary', '#334155'),
        fontSize: 12,
        fontFamily: 'Inter, system-ui, sans-serif',
        fontWeight: 600,
      },
      extraCssText: 'border-radius: 12px; box-shadow: var(--cp-shadow-popover);',
      axisPointer: {
        type: 'line',
        lineStyle: { color: themeColor('--cp-default-border-hover', '#E2E8F0'), type: 'dashed' },
      },
      formatter: formatTooltip,
    },
  }
})

function getCoordinateSystem(times: string[]) {
  const muted = themeColor('--cp-text-muted', '#94A3B8')
  const gridLine = themeColor('--cp-divider-subtle', '#E2E8F0')
  const axisLine = themeColor('--cp-default-border', '#E2E8F0')
  const timeLabels = {
    color: muted,
    fontSize: 10,
    fontFamily: 'JetBrains Mono Variable, JetBrains Mono',
    hideOverlap: true,
    interval: showTwoHourLabel,
  }

  return {
    grid: {
      left: 8,
      right: activeKind.value === 'latency' ? 8 : 10,
      top: 10,
      bottom: 2,
      containLabel: true,
    },
    xAxis: {
      type: 'category' as const,
      data: times,
      boundaryGap: activeKind.value === 'errors',
      axisLine: { show: true, lineStyle: { color: axisLine } },
      axisTick: { show: false },
      axisLabel: timeLabels,
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
          show: activeKind.value !== 'latency',
          color:
            activeKind.value === 'usage'
              ? trendColor('输出', '--cp-success', '#10B981')
              : trendColor('成功', '--cp-success', '#10B981'),
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
  if (activeKind.value === 'errors') return `${Math.round(value)}%`
  return formatAxisCompact(value)
}

function formatAxisCompact(value: number) {
  const normalized = Math.abs(value)
  if (normalized >= 1_000_000_000) return `${formatAxisNumber(value / 1_000_000_000)}B`
  if (normalized >= 1_000_000) return `${formatAxisNumber(value / 1_000_000)}M`
  if (normalized >= 1_000) return `${formatAxisNumber(value / 1_000)}K`
  return `${Math.round(value)}`
}

function formatAxisNumber(value: number) {
  return value >= 10 ? value.toFixed(0) : value.toFixed(1).replace(/\.0$/, '')
}

function showTwoHourLabel(_index: number, value: string) {
  const [hour, minute] = value.split(':').map(Number)
  return minute === 0 && hour % 2 === 0
}

function themeColor(name: string, fallback: string) {
  themeRevision.value
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || fallback
}

function formatTooltip(params: unknown) {
  const rows = Array.isArray(params) ? params : [params]
  const title = tooltipValue(rows[0], 'axisValueLabel')
  const point = props.points[tooltipIndex(rows[0])]

  if (activeKind.value === 'usage') {
    return [
      title,
      tooltipItem('输入', point?.inputTokens, trendColor('输入', '--cp-info', '#2563EB')),
      tooltipItem('输出', point?.outputTokens, trendColor('输出', '--cp-success', '#10B981')),
      tooltipItem('缓存', point?.cachedTokens, trendColor('缓存', '--cp-text-tertiary', '#94A3B8')),
      tooltipItem(
        '缓存命中',
        point?.cacheHitRate,
        trendColor('缓存', '--cp-text-tertiary', '#94A3B8'),
      ),
      tooltipItem('请求', point?.requests, themeColor('--cp-text-secondary', '#64748B')),
    ]
      .filter(Boolean)
      .join('<br/>')
  }

  const lines = rows.map((row) => {
    const name = tooltipValue(row, 'seriesName')
    const value = tooltipValue(row, 'value')
    const marker = tooltipValue(row, 'marker')
    const unitValue = tooltipDisplayValue(point, name, value)
    return `${marker}${name}: ${unitValue}`
  })
  return [title, ...lines].filter(Boolean).join('<br/>')
}

function tooltipItem(label: string, value: string | undefined, color: string) {
  if (!value) return ''
  const marker = `<span style="display:inline-block;width:7px;height:7px;margin-right:6px;border-radius:999px;background:${color}"></span>`
  return `${marker}${label}: ${value}`
}

function tooltipDisplayValue(point: any, name: string, value: string): string {
  if (activeKind.value === 'usage') {
    if (name === '输入') return point?.inputTokens ?? value
    if (name === '输出') return point?.outputTokens ?? value
    if (name === '缓存') return point?.cachedTokens ?? value
  }
  if (activeKind.value === 'latency') {
    if (name === '平均') return point?.latency ?? value
    if (name === '最高') return point?.maxLatency ?? value
    if (name === '最低') return point?.minLatency ?? value
  }
  if (name === '错误数') return point?.errors ?? value
  if (name === '成功率') return point?.successRate ?? value
  if (name === '总请求') return point?.requests ?? value
  return value
}

function tooltipValue(source: unknown, key: string): string {
  if (typeof source !== 'object' || source === null || !(key in source)) return ''
  const value = (source as Record<string, unknown>)[key]
  return typeof value === 'number' || typeof value === 'string' ? String(value) : ''
}

function tooltipIndex(source: unknown) {
  if (typeof source !== 'object' || source === null || !('dataIndex' in source)) return -1
  const value = (source as Record<string, unknown>).dataIndex
  return typeof value === 'number' ? value : -1
}

function getSeries() {
  if (activeKind.value === 'usage') {
    const cacheColor = trendColor('缓存', '--cp-text-tertiary', '#94A3B8')
    const inputColor = trendColor('输入', '--cp-info', '#2563EB')
    const outputColor = trendColor('输出', '--cp-success', '#10B981')

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
    const averageColor = trendColor('平均', '--cp-normal', '#0F9F9A')
    const maximumColor = trendColor('最高', '--cp-warning', '#F59E0B')
    const minimumColor = trendColor('最低', '--cp-success', '#10B981')

    return [
      lineSeries('最低', activeSeriesValues('minLatencyValue'), minimumColor, {
        lineType: 'dotted',
        smooth: 0.2,
        stack: 'latency-range',
        width: 1.2,
        z: 1,
      }),
      lineSeries('最高', latencyRangeValues(), maximumColor, {
        area: true,
        areaStartAlpha: '26',
        areaEndAlpha: '10',
        lineType: 'dashed',
        smooth: 0.2,
        stack: 'latency-range',
        width: 1.2,
        z: 1,
      }),
      lineSeries('平均', activeSeriesValues('latencyValue'), averageColor, {
        smooth: 0.24,
        width: 2.5,
        z: 4,
      }),
    ]
  }
  return [
    barSeries('错误数', seriesValues('errorsValue'), trendColor('错误', '--cp-danger', '#EF4444')),
    lineSeries(
      '成功率',
      seriesValues('successRateValue'),
      trendColor('成功', '--cp-success', '#10B981'),
      {
        width: 2.4,
        yAxisIndex: 1,
      },
    ),
    lineSeries(
      '总请求',
      seriesValues('requestsValue'),
      trendColor('请求', '--cp-info', '#2563EB'),
      {
        lineType: 'dashed',
        width: 1.8,
      },
    ),
  ]
}

function seriesValues(key: string) {
  return props.points.map((point) => point[key] ?? null)
}

function activeSeriesValues(key: string) {
  return props.points.map((point) =>
    Number(point.requestsValue) > 0 ? (point[key] ?? null) : null,
  )
}

function latencyRangeValues() {
  return props.points.map((point) => {
    if (Number(point.requestsValue) <= 0) return null
    if (point.minLatencyValue === null || point.maxLatencyValue === null) return null
    if (point.minLatencyValue === undefined || point.maxLatencyValue === undefined) return null
    const minimum = Number(point.minLatencyValue)
    const maximum = Number(point.maxLatencyValue)
    if (!Number.isFinite(minimum) || !Number.isFinite(maximum)) return null
    return Math.max(0, maximum - minimum)
  })
}

function lineSeries(name: string, data: (number | null)[], color: string, options: any = {}) {
  const area = options.area ?? false

  return {
    name,
    type: 'line' as const,
    data,
    connectNulls: false,
    smooth: options.smooth ?? true,
    symbol: options.symbol ?? (isSparseTrend.value ? 'circle' : 'none'),
    symbolSize: options.symbolSize ?? 5,
    showSymbol: options.showSymbol ?? isSparseTrend.value,
    stack: options.stack,
    xAxisIndex: options.xAxisIndex ?? 0,
    yAxisIndex: options.yAxisIndex ?? 0,
    z: options.z ?? 2,
    lineStyle: {
      color,
      type: options.lineType ?? 'solid',
      width: options.width ?? 2.2,
    },
    itemStyle: { color },
    areaStyle: area
      ? {
          color: {
            type: 'linear' as const,
            x: 0,
            y: 0,
            x2: 0,
            y2: 1,
            colorStops: [
              { offset: 0, color: `${color}${options.areaStartAlpha ?? '18'}` },
              { offset: 1, color: `${color}${options.areaEndAlpha ?? '02'}` },
            ],
          },
        }
      : undefined,
  }
}

function barSeries(name: string, data: (number | null)[], color: string, options: any = {}) {
  return {
    name,
    type: 'bar' as const,
    data,
    barMaxWidth: options.maxWidth ?? 16,
    xAxisIndex: options.xAxisIndex ?? 0,
    yAxisIndex: options.yAxisIndex ?? 0,
    silent: options.silent ?? false,
    z: options.z ?? 2,
    itemStyle: {
      color,
      opacity: options.opacity ?? 0.72,
      borderRadius: options.borderRadius ?? [3, 3, 0, 0],
    },
  }
}

function summaryMarkerStyle(item: any) {
  return item.colorVar ? { backgroundColor: `var(${item.colorVar})` } : undefined
}

function trendColor(label: string, fallbackVar: string, fallback: string) {
  const colorVar = props.summary.find((item) => String(item.label).includes(label))?.colorVar
  return themeColor(colorVar || fallbackVar, fallback)
}

function handleTrendChange(value: string) {
  emit('trendChange', value as TrendKind)
}
</script>

<template>
  <BaseCard as="article" variant="dashboard" title="使用趋势" class="min-h-95 w-full">
    <template #actions>
      <BaseSegmented
        v-model="activeKind"
        :options="tabs"
        class="w-full max-w-61.5 sm:w-61.5"
        @update:model-value="handleTrendChange"
      />
    </template>

    <template #body>
      <div class="mt-4.5 grid gap-3.5">
        <div
          class="grid h-14.25 min-w-0 grid-cols-3 gap-1.5 rounded-xl bg-(--cp-bg-subtle)/45 p-1.5"
        >
          <div
            v-for="item in props.summary"
            :key="item.label"
            class="grid min-w-0 grid-cols-[8px_minmax(0,1fr)] items-center gap-x-2 rounded-lg px-2.5 py-2"
          >
            <i
              class="size-2 rounded-full"
              :style="summaryMarkerStyle(item)"
              :class="{
                'bg-(--cp-info)': item.tone === 'info',
                'bg-(--cp-success)': item.tone === 'success',
                'bg-(--cp-warning)': item.tone === 'warning',
                'bg-(--cp-danger)': item.tone === 'danger',
                'bg-(--cp-normal)': item.tone === 'normal',
              }"
            />
            <span class="grid min-w-0 gap-1">
              <span class="truncate text-[10px] leading-none font-[680] text-(--cp-text-secondary)">
                {{ item.label }}
              </span>
              <strong
                class="truncate font-mono text-[15px] leading-none font-[760] tabular-nums text-(--cp-text-primary)"
                :title="item.value"
              >
                {{ item.value }}
              </strong>
            </span>
          </div>
        </div>

        <div class="relative h-55 w-full overflow-hidden">
          <BaseChart v-if="hasSamples" :option="chartOption" :height="220" />
          <BaseEmpty
            v-if="!hasSamples"
            compact
            title="暂无趋势数据"
            description="当日暂无请求日志"
            class="h-full place-content-center bg-transparent"
          />
        </div>
      </div>
    </template>
  </BaseCard>
</template>
