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

const chartOption = computed<EChartsOption>(() => {
  const times = props.points.map((p) => `${p.time}:00`)
  const series = getSeries()
  return {
    grid: { left: 4, right: 0, top: 8, bottom: 24 },
    xAxis: {
      type: 'category',
      data: times,
      axisLabel: {
        color: themeColor('--cp-text-muted', '#94A3B8'),
        fontSize: 10,
        fontFamily: 'JetBrains Mono Variable, JetBrains Mono',
      },
      axisLine: { show: false },
      axisTick: { show: false },
    },
    yAxis: [
      {
        type: 'value',
        splitLine: { lineStyle: { color: themeColor('--cp-bg-muted', '#F1F5F9') } },
        axisLabel: { show: false },
      },
      { type: 'value', min: 0, max: 100, splitLine: { show: false }, axisLabel: { show: false } },
    ],
    series,
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

function themeColor(name: string, fallback: string) {
  themeRevision.value
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || fallback
}

function formatTooltip(params: unknown) {
  const rows = Array.isArray(params) ? params : [params]
  const title = tooltipValue(rows[0], 'axisValueLabel')
  const point = props.points[tooltipIndex(rows[0])]
  const lines = rows.map((row) => {
    const name = tooltipValue(row, 'seriesName')
    const value = tooltipValue(row, 'value')
    const marker = tooltipValue(row, 'marker')
    const unitValue = tooltipDisplayValue(point, name, value)
    return `${marker}${name}: ${unitValue}`
  })
  return [title, ...lines].filter(Boolean).join('<br/>')
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
        props.points.map((p) => p.inputTokensValue),
        themeColor('--cp-info', '#2563EB'),
        true,
      ),
      lineSeries(
        '输出',
        props.points.map((p) => p.outputTokensValue),
        themeColor('--cp-success', '#10B981'),
      ),
      lineSeries(
        '缓存',
        props.points.map((p) => p.cachedTokensValue),
        themeColor('--cp-text-tertiary', '#94A3B8'),
      ),
    ]
  }
  if (activeKind.value === 'latency') {
    return [
      lineSeries(
        '平均',
        props.points.map((p) => p.latencyValue),
        themeColor('--cp-normal', '#0F9F9A'),
        true,
      ),
      lineSeries(
        '最高',
        props.points.map((p) => p.maxLatencyValue),
        themeColor('--cp-warning', '#F59E0B'),
      ),
      lineSeries(
        '最低',
        props.points.map((p) => p.minLatencyValue),
        themeColor('--cp-success', '#10B981'),
      ),
    ]
  }
  return [
    lineSeries(
      '错误数',
      props.points.map((p) => p.errorsValue),
      themeColor('--cp-danger', '#EF4444'),
      true,
    ),
    lineSeries(
      '成功率',
      props.points.map((p) => p.successRateValue),
      themeColor('--cp-success', '#10B981'),
      true,
      1,
    ),
    lineSeries(
      '总请求',
      props.points.map((p) => p.requestsValue),
      themeColor('--cp-info', '#2563EB'),
    ),
  ]
}

function lineSeries(name: string, data: number[], color: string, area = true, yAxisIndex = 0) {
  return {
    name,
    type: 'line' as const,
    data,
    smooth: true,
    symbol: 'none',
    yAxisIndex,
    lineStyle: { color, width: 2.5 },
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

function summaryMarkerStyle(item: any) {
  return item.colorVar ? { backgroundColor: `var(${item.colorVar})` } : undefined
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
        class="mt-4.75 grid grid-cols-1 gap-5 lg:grid-cols-[minmax(0,1fr)_minmax(150px,180px)] lg:gap-7.5"
      >
        <div class="relative h-67 w-full overflow-hidden">
          <BaseChart v-if="hasSamples" :option="chartOption" :height="268" />
          <BaseEmpty
            v-if="!hasSamples"
            compact
            title="暂无趋势数据"
            description="当日暂无请求日志。"
            class="h-full place-content-center bg-transparent"
          />
        </div>

        <aside class="grid h-67 w-full grid-rows-3 rounded-2xl bg-(--cp-bg-subtle) px-5 py-4.5">
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
