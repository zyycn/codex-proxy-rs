<script setup lang="ts">
import { computed, ref } from 'vue'
import type { EChartsOption } from 'echarts'
import BaseCard from '../../../components/base/BaseCard.vue'
import BaseChart from '../../../components/charts/BaseChart.vue'
import BaseEmpty from '../../../components/base/BaseEmpty.vue'
import BaseSegmented from '../../../components/base/BaseSegmented.vue'

const props = defineProps<{
  points: any[]
  summary: any[]
  loading?: boolean
}>()

const emit = defineEmits<{
  trendChange: [tab: string]
}>()

const tabs = [
  { label: '用量', value: '用量' },
  { label: '延迟', value: '延迟' },
  { label: '错误', value: '错误' },
]
const activeTab = ref('用量')

const hasSamples = computed(() =>
  props.points.some(
    (point) =>
      point.requests > 0 ||
      point.errors > 0 ||
      point.latency > 0 ||
      point.tokens > 0 ||
      point.cachedTokens > 0,
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
      axisLabel: { color: '#94A3B8', fontSize: 10, fontFamily: 'JetBrains Mono' },
      axisLine: { show: false },
      axisTick: { show: false },
    },
    yAxis: [
      { type: 'value', splitLine: { lineStyle: { color: '#F1F5F9' } }, axisLabel: { show: false } },
      { type: 'value', min: 0, max: 100, splitLine: { show: false }, axisLabel: { show: false } },
    ],
    series,
    tooltip: {
      trigger: 'axis',
      backgroundColor: '#fff',
      borderColor: 'transparent',
      borderWidth: 0,
      padding: [10, 14],
      textStyle: {
        color: '#334155',
        fontSize: 12,
        fontFamily: 'Inter, system-ui, sans-serif',
        fontWeight: 600,
      },
      extraCssText: 'border-radius: 12px; box-shadow: 0 4px 20px rgba(0,0,0,0.08);',
      axisPointer: { type: 'line', lineStyle: { color: '#E2E8F0', type: 'dashed' } },
      formatter: formatTooltip,
    },
  }
})

function formatTooltip(params: unknown) {
  const rows = Array.isArray(params) ? params : [params]
  const title = tooltipValue(rows[0], 'axisValueLabel')
  const lines = rows.map((row) => {
    const name = tooltipValue(row, 'seriesName')
    const value = tooltipValue(row, 'value')
    const marker = tooltipValue(row, 'marker')
    const unitValue = tooltipDisplayValue(name, value)
    return `${marker}${name}: ${unitValue}`
  })
  return [title, ...lines].filter(Boolean).join('<br/>')
}

function tooltipDisplayValue(name: string, value: string): string {
  if (name === '成功率') return `${value}%`
  if (activeTab.value === '延迟') return formatLatency(Number(value))
  return value
}

function tooltipValue(source: unknown, key: string): string {
  if (typeof source !== 'object' || source === null || !(key in source)) return ''
  const value = (source as Record<string, unknown>)[key]
  return typeof value === 'number' || typeof value === 'string' ? String(value) : ''
}

function getSeries() {
  if (activeTab.value === '用量') {
    return [
      lineSeries(
        '输入',
        props.points.map((p) => p.inputTokens),
        '#2563EB',
        true,
      ),
      lineSeries(
        '输出',
        props.points.map((p) => p.outputTokens),
        '#10B981',
      ),
      lineSeries(
        '缓存',
        props.points.map((p) => p.cachedTokens),
        '#94A3B8',
      ),
    ]
  }
  if (activeTab.value === '延迟') {
    return [
      lineSeries(
        '平均',
        props.points.map((p) => p.latency),
        '#0F9F9A',
        true,
      ),
      lineSeries(
        '最高',
        props.points.map((p) => p.maxLatency),
        '#F59E0B',
      ),
      lineSeries(
        '最低',
        props.points.map((p) => p.minLatency),
        '#10B981',
      ),
    ]
  }
  return [
    lineSeries(
      '错误数',
      props.points.map((p) => p.errors),
      '#EF4444',
      true,
    ),
    lineSeries(
      '成功率',
      props.points.map((p) => p.successRate),
      '#10B981',
      false,
      1,
    ),
    lineSeries(
      '总请求',
      props.points.map((p) => p.requests),
      '#2563EB',
    ),
  ]
}

function lineSeries(name: string, data: number[], color: string, area = false, yAxisIndex = 0) {
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

function formatLatency(ms: number): string {
  if (!ms) return '—'
  if (ms >= 1000) return `${(ms / 1000).toFixed(2)}s`
  return `${ms}ms`
}
</script>

<template>
  <BaseCard
    as="article"
    variant="dashboard"
    title="使用趋势"
    description="最近 24 小时"
    class="min-h-95 w-full"
  >
    <template #actions>
      <BaseSegmented
        v-model="activeTab"
        :options="tabs"
        class="w-full max-w-61.5 sm:w-61.5"
        @update:model-value="emit('trendChange', $event)"
      />
    </template>

    <template #body>
      <div
        class="mt-4.75 grid grid-cols-1 gap-5 lg:grid-cols-[minmax(0,1fr)_minmax(150px,180px)] lg:gap-7.5"
      >
        <div class="relative h-67 w-full overflow-hidden rounded-[10px] bg-white">
          <BaseChart v-if="hasSamples" :option="chartOption" :height="268" />
          <BaseEmpty
            v-if="!hasSamples"
            compact
            title="暂无趋势数据"
            description="最近 24 小时还没有可用于绘制趋势的请求日志。"
            class="h-full place-content-center bg-white"
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
