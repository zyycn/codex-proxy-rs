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
    axisPointer:
      activeKind.value === 'usage'
        ? {
            link: [{ xAxisIndex: [0, 1, 2] }],
          }
        : undefined,
    tooltip: {
      trigger: 'axis',
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
  const gridLine = themeColor('--cp-bg-muted', '#F1F5F9')
  const timeLabels = {
    color: muted,
    fontSize: 10,
    fontFamily: 'JetBrains Mono Variable, JetBrains Mono',
    hideOverlap: true,
    interval: showTwoHourLabel,
  }

  if (activeKind.value === 'usage') {
    return getUsageCoordinateSystem(times, timeLabels, gridLine)
  }

  const xAxis = {
    type: 'category' as const,
    data: times,
    axisLine: { show: false },
    axisTick: { show: false },
  }
  const yAxis = {
    type: 'value' as const,
    min: 0,
    splitNumber: 2,
    axisLabel: { show: false },
    axisLine: { show: false },
    axisTick: { show: false },
    splitLine: { lineStyle: { color: gridLine } },
  }

  return {
    grid: { left: 4, right: 0, top: 8, bottom: 24 },
    xAxis: {
      ...xAxis,
      boundaryGap: activeKind.value !== 'latency',
      axisLabel: timeLabels,
    },
    yAxis: [
      yAxis,
      {
        type: 'value' as const,
        min: 0,
        max: 100,
        splitLine: { show: false },
        axisLabel: { show: false },
      },
    ],
  }
}

function getUsageCoordinateSystem(times: string[], timeLabels: any, gridLine: string) {
  const xAxis = (gridIndex: number, showLabels = false) => ({
    type: 'category' as const,
    data: times,
    gridIndex,
    boundaryGap: false,
    axisLine: {
      show: true,
      lineStyle: { color: gridLine },
    },
    axisTick: { show: false },
    axisLabel: showLabels ? timeLabels : { show: false },
    axisPointer: { show: true },
  })
  const yAxis = (gridIndex: number) => ({
    type: 'value' as const,
    gridIndex,
    min: 0,
    max: (range: any) => (range.max > 0 ? range.max * 1.12 : 1),
    axisLabel: { show: false },
    axisLine: { show: false },
    axisTick: { show: false },
    splitLine: { show: false },
  })

  return {
    grid: [
      { left: 4, right: 0, top: 0, height: 76 },
      { left: 4, right: 0, top: 91, height: 66 },
      { left: 4, right: 0, top: 172, bottom: 18 },
    ],
    xAxis: [xAxis(0), xAxis(1), xAxis(2, true)],
    yAxis: [yAxis(0), yAxis(1), yAxis(2)],
  }
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
    return [
      lineSeries(
        '输入',
        activeSeriesValues('inputTokensValue'),
        trendColor('输入', '--cp-info', '#2563EB'),
        {
          area: true,
          smooth: 0.22,
          width: 2,
          xAxisIndex: 0,
          yAxisIndex: 0,
        },
      ),
      lineSeries(
        '输出',
        activeSeriesValues('outputTokensValue'),
        trendColor('输出', '--cp-success', '#10B981'),
        {
          area: true,
          smooth: 0.22,
          width: 1.9,
          xAxisIndex: 1,
          yAxisIndex: 1,
        },
      ),
      lineSeries(
        '缓存',
        activeSeriesValues('cachedTokensValue'),
        trendColor('缓存', '--cp-text-tertiary', '#94A3B8'),
        {
          area: true,
          smooth: 0.22,
          width: 1.8,
          xAxisIndex: 2,
          yAxisIndex: 2,
        },
      ),
    ]
  }

  if (isSparseTrend.value) return getSparseSeries()

  if (activeKind.value === 'latency') {
    return [
      lineSeries(
        '平均',
        seriesValues('latencyValue'),
        trendColor('平均', '--cp-normal', '#0F9F9A'),
        {
          area: true,
          width: 2.6,
        },
      ),
      lineSeries(
        '最高',
        seriesValues('maxLatencyValue'),
        trendColor('最高', '--cp-warning', '#F59E0B'),
        {
          lineType: 'dashed',
          width: 1.8,
        },
      ),
      lineSeries(
        '最低',
        seriesValues('minLatencyValue'),
        trendColor('最低', '--cp-success', '#10B981'),
        {
          lineType: 'dotted',
          width: 1.8,
        },
      ),
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

function getSparseSeries() {
  if (activeKind.value === 'latency') {
    return [
      barSeries(
        '平均',
        activeSeriesValues('latencyValue'),
        trendColor('平均', '--cp-normal', '#0F9F9A'),
        {
          maxWidth: 9,
          opacity: 0.78,
        },
      ),
      barSeries(
        '最高',
        activeSeriesValues('maxLatencyValue'),
        trendColor('最高', '--cp-warning', '#F59E0B'),
        { maxWidth: 9, opacity: 0.58 },
      ),
      barSeries(
        '最低',
        activeSeriesValues('minLatencyValue'),
        trendColor('最低', '--cp-success', '#10B981'),
        { maxWidth: 9, opacity: 0.58 },
      ),
    ]
  }

  return [
    barSeries(
      '错误数',
      activeSeriesValues('errorsValue'),
      trendColor('错误', '--cp-danger', '#EF4444'),
      {
        maxWidth: 10,
        opacity: 0.72,
      },
    ),
    pulseSeries(
      '成功率',
      activeSeriesValues('successRateValue'),
      trendColor('成功', '--cp-success', '#10B981'),
      { yAxisIndex: 1 },
    ),
    barSeries(
      '总请求',
      activeSeriesValues('requestsValue'),
      trendColor('请求', '--cp-info', '#2563EB'),
      {
        maxWidth: 10,
        opacity: 0.5,
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

function lineSeries(name: string, data: (number | null)[], color: string, options: any = {}) {
  const area = options.area ?? false

  return {
    name,
    type: 'line' as const,
    data,
    connectNulls: false,
    smooth: options.smooth ?? true,
    symbol: options.symbol ?? 'none',
    symbolSize: options.symbolSize ?? 5,
    xAxisIndex: options.xAxisIndex ?? 0,
    yAxisIndex: options.yAxisIndex ?? 0,
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
              { offset: 0, color: `${color}18` },
              { offset: 1, color: `${color}02` },
            ],
          },
        }
      : undefined,
  }
}

function pulseSeries(name: string, data: (number | null)[], color: string, options: any = {}) {
  return {
    name,
    type: 'line' as const,
    data,
    connectNulls: false,
    smooth: false,
    symbol: 'roundRect',
    symbolSize: [5, 16],
    yAxisIndex: options.yAxisIndex ?? 0,
    lineStyle: { opacity: 0 },
    itemStyle: { color },
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
      <div
        class="mt-4.75 grid grid-cols-1 gap-5 lg:h-70 lg:grid-cols-[minmax(0,1fr)_minmax(150px,180px)] lg:gap-7.5"
      >
        <div class="relative h-70 w-full overflow-hidden lg:h-full">
          <BaseChart v-if="hasSamples" :option="chartOption" :height="280" />
          <BaseEmpty
            v-if="!hasSamples"
            compact
            title="暂无趋势数据"
            description="当日暂无请求日志"
            class="h-full place-content-center bg-transparent"
          />
        </div>

        <aside
          class="grid h-70 w-full grid-rows-3 rounded-2xl bg-(--cp-bg-subtle) px-5 py-4.5 lg:h-full"
        >
          <div
            v-for="item in props.summary"
            :key="item.label"
            class="grid grid-cols-[minmax(0,1fr)_8px] items-center gap-x-3 py-2"
          >
            <span class="grid gap-1.75">
              <span class="text-xs leading-[1.15] font-bold text-(--cp-text-secondary)">{{
                item.label
              }}</span>
              <strong
                class="font-mono text-2xl leading-[1.15] font-[760] tabular-nums text-(--cp-text-primary)"
                >{{ item.value }}</strong
              >
            </span>
            <i
              class="size-2 justify-self-end rounded-full"
              :style="summaryMarkerStyle(item)"
              :class="{
                'bg-(--cp-info)': item.tone === 'info',
                'bg-(--cp-success)': item.tone === 'success',
                'bg-(--cp-warning)': item.tone === 'warning',
                'bg-(--cp-danger)': item.tone === 'danger',
                'bg-(--cp-normal)': item.tone === 'normal',
              }"
            />
          </div>
        </aside>
      </div>
    </template>
  </BaseCard>
</template>
