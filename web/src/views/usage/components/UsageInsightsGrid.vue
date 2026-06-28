<script setup lang="ts">
import { computed, shallowRef } from 'vue'
import type { EChartsOption } from 'echarts'
import { storeToRefs } from 'pinia'

import BaseCard from '@/components/base/BaseCard.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseChart from '@/components/charts/BaseChart.vue'
import { useUiStore } from '@/stores/modules/ui'
import { formatCostMetric } from '../constants'
import UsageDistributionPanel from './UsageDistributionPanel.vue'

const props = defineProps<{
  insights: any
  loading?: boolean
}>()

const chartHeight = 236

const modelSource = shallowRef('requested')
const endpointSource = shallowRef('inbound')
const { themeRevision } = storeToRefs(useUiStore())

const modelSourceOptions = [
  { label: '请求', value: 'requested' },
  { label: '上游', value: 'upstream' },
  { label: '映射', value: 'mapping' },
]

const endpointSourceOptions = [
  { label: '入站', value: 'inbound' },
  { label: '上游', value: 'upstream' },
  { label: '路径', value: 'path' },
]

const modelItems = computed(() => {
  if (modelSource.value === 'upstream') return props.insights?.upstreamModels ?? []
  if (modelSource.value === 'mapping') return props.insights?.modelMappings ?? []
  return props.insights?.models ?? []
})

const endpointItems = computed(() => {
  if (endpointSource.value === 'upstream') return props.insights?.upstreamEndpoints ?? []
  if (endpointSource.value === 'path') return props.insights?.endpointPaths ?? []
  return props.insights?.endpoints ?? []
})

const trendPoints = computed(() => props.insights?.trend ?? [])
const hasTrend = computed(() =>
  trendPoints.value.some(
    (point: any) =>
      point.totalTokens > 0 ||
      point.cacheCreationTokens > 0 ||
      point.cachedTokens > 0 ||
      point.averageLatencyMs > 0,
  ),
)

const trendOption = computed<EChartsOption>(() => ({
  grid: { left: 8, right: 10, top: 36, bottom: 28 },
  legend: {
    top: 10,
    right: 12,
    itemWidth: 8,
    itemHeight: 8,
    icon: 'circle',
    data: ['输入', '输出', '缓存写入', '缓存读取', '缓存命中率'],
    textStyle: {
      color: themeColor('--cp-text-secondary', '#64748B'),
      fontSize: 12,
      fontFamily: 'Inter Variable, Inter, system-ui, sans-serif',
      fontWeight: 700,
    },
  },
  tooltip: {
    trigger: 'axis',
    backgroundColor: themeColor('--cp-bg-surface', '#fff'),
    borderColor: 'transparent',
    borderWidth: 0,
    padding: [10, 14],
    textStyle: {
      color: themeColor('--cp-text-primary', '#334155'),
      fontSize: 12,
      fontFamily: 'Inter Variable, Inter, system-ui, sans-serif',
      fontWeight: 650,
    },
    extraCssText: 'border-radius: 12px; box-shadow: var(--cp-shadow-popover);',
    axisPointer: {
      type: 'line',
      lineStyle: { color: chartPointerLineColor(), type: 'dashed', width: 1 },
    },
    formatter: formatTooltip,
  },
  xAxis: {
    type: 'category',
    data: trendPoints.value.map((point: any) => point.date),
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
      axisLine: { show: false },
      axisTick: { show: false },
      axisLabel: { show: false },
      splitLine: { lineStyle: { color: chartGridLineColor(), width: 1 } },
    },
    {
      type: 'value',
      min: 0,
      max: 100,
      axisLine: { show: false },
      axisTick: { show: false },
      splitLine: { show: false },
      axisLabel: { show: false },
    },
  ],
  series: trendSeries(),
}))

const latencyOption = computed<EChartsOption>(() => ({
  grid: { left: 8, right: 10, top: 18, bottom: 28 },
  tooltip: {
    trigger: 'axis',
    backgroundColor: themeColor('--cp-bg-surface', '#fff'),
    borderColor: 'transparent',
    borderWidth: 0,
    padding: [10, 14],
    textStyle: {
      color: themeColor('--cp-text-primary', '#334155'),
      fontSize: 12,
      fontFamily: 'Inter Variable, Inter, system-ui, sans-serif',
      fontWeight: 650,
    },
    extraCssText: 'border-radius: 12px; box-shadow: var(--cp-shadow-popover);',
    axisPointer: {
      type: 'line',
      lineStyle: { color: chartPointerLineColor(), type: 'dashed', width: 1 },
    },
    formatter: formatLatencyTooltip,
  },
  xAxis: {
    type: 'category',
    data: trendPoints.value.map((point: any) => point.date),
    axisLabel: {
      color: themeColor('--cp-text-muted', '#94A3B8'),
      fontSize: 10,
      fontFamily: 'JetBrains Mono Variable, JetBrains Mono',
    },
    axisLine: { show: false },
    axisTick: { show: false },
  },
  yAxis: {
    type: 'value',
    axisLine: { show: false },
    axisTick: { show: false },
    axisLabel: { show: false },
    splitLine: { lineStyle: { color: chartGridLineColor(), width: 1 } },
  },
  series: [
    lineSeries(
      '平均延迟',
      trendPoints.value.map((point: any) => Number(point.averageLatencyMs || 0)),
      themeColor('--cp-warning', '#D97706'),
      true,
    ),
  ],
}))

function trendSeries() {
  return [
    lineSeries(
      '输入',
      trendPoints.value.map((point: any) => point.inputTokens),
      themeColor('--cp-info', '#2563EB'),
      true,
    ),
    lineSeries(
      '输出',
      trendPoints.value.map((point: any) => point.outputTokens),
      themeColor('--cp-success', '#10B981'),
    ),
    lineSeries(
      '缓存写入',
      trendPoints.value.map((point: any) => point.cacheCreationTokens),
      themeColor('--cp-warning', '#D97706'),
    ),
    lineSeries(
      '缓存读取',
      trendPoints.value.map((point: any) => point.cachedTokens),
      themeColor('--cp-info-text', '#0EA5E9'),
    ),
    lineSeries(
      '缓存命中率',
      trendPoints.value.map((point: any) => cacheHitRate(point)),
      themeColor('--cp-focus-ring', '#8B5CF6'),
      false,
      1,
    ),
  ]
}

function lineSeries(name: string, data: number[], color: string, area = false, yAxisIndex = 0) {
  return {
    name,
    type: 'line' as const,
    data,
    smooth: true,
    symbol: data.length <= 2 ? 'circle' : 'none',
    symbolSize: 6,
    yAxisIndex,
    lineStyle: { color, width: 2.4 },
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
              { offset: 0, color: `${color}16` },
              { offset: 1, color: `${color}02` },
            ],
          },
        }
      : undefined,
  }
}

function cacheHitRate(point: any) {
  const inputTokens = Number(point.inputTokens || 0)
  const cacheCreationTokens = Number(point.cacheCreationTokens || 0)
  const cachedTokens = Number(point.cachedTokens || 0)
  const promptTokens = inputTokens + cacheCreationTokens + cachedTokens
  if (!promptTokens) return 0
  return Math.round((cachedTokens / promptTokens) * 100)
}

function formatTooltip(params: unknown) {
  const rows = Array.isArray(params) ? params : [params]
  const title = tooltipValue(rows[0], 'axisValueLabel')
  const point = trendPoints.value[tooltipIndex(rows[0])]
  const lines = rows.map((row) => {
    const name = tooltipValue(row, 'seriesName')
    const value = Number(tooltipValue(row, 'value') || 0)
    const marker = tooltipValue(row, 'marker')
    const display = name === '缓存命中率' ? `${value}%` : formatCompactNumber(value)
    return `${marker}${name}: ${display}`
  })
  const costLine = point
    ? `实际: ${point.actualCostDisplay || formatCostMetric(point.actualCost)} ｜ 标准: ${
        point.costDisplay || formatCostMetric(point.cost)
      }`
    : ''
  return [title, ...lines, costLine].filter(Boolean).join('<br/>')
}

function formatLatencyTooltip(params: unknown) {
  const rows = Array.isArray(params) ? params : [params]
  const title = tooltipValue(rows[0], 'axisValueLabel')
  const value = Number(tooltipValue(rows[0], 'value') || 0)
  return [title, `平均延迟: ${Math.round(value)} ms`].filter(Boolean).join('<br/>')
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

function formatCompactNumber(value: number) {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(2)}M`
  if (value >= 1_000) return `${(value / 1_000).toFixed(1)}K`
  return new Intl.NumberFormat('zh-CN').format(value || 0)
}

function chartGridLineColor() {
  return themeColor('--cp-default-border', '#E7EDF5')
}

function chartPointerLineColor() {
  return themeColor('--cp-default-border-hover', '#CBD5E1')
}

function themeColor(name: string, fallback: string) {
  themeRevision.value
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || fallback
}
</script>

<template>
  <section class="mt-5 grid grid-cols-1 gap-3 xl:grid-cols-2" aria-label="使用记录分析">
    <UsageDistributionPanel
      v-model:source="modelSource"
      title="模型分布"
      description="按请求模型、上游模型与映射关系聚合"
      name-label="模型"
      :items="modelItems"
      color="--cp-info"
      :source-options="modelSourceOptions"
    />

    <UsageDistributionPanel
      v-model:source="endpointSource"
      title="端点分布"
      description="按入站端点、上游端点与路径聚合"
      name-label="端点"
      :items="endpointItems"
      color="--cp-normal"
      :source-options="endpointSourceOptions"
      :show-cost-column="false"
    />

    <BaseCard
      :padded="false"
      title="Token 使用趋势"
      description="按日期聚合输入、输出与缓存命中"
      header-class="px-5 pt-4"
      body-class="px-5 pt-3 pb-4"
    >
      <template #body>
        <div class="h-64 rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-3 pt-3 pb-2">
          <div v-if="hasTrend" class="flex h-full min-h-50 flex-col">
            <BaseChart :option="trendOption" :height="chartHeight" />
          </div>
          <BaseEmpty
            v-else
            compact
            title="暂无趋势数据"
            description="当前筛选范围还没有可绘制的 Token 趋势。"
            class="min-h-50 place-content-center bg-transparent"
          />
        </div>
      </template>
    </BaseCard>

    <BaseCard
      :padded="false"
      title="延迟分布"
      description="按日期聚合平均响应耗时"
      header-class="px-5 pt-4"
      body-class="px-5 pt-3 pb-4"
    >
      <template #body>
        <div class="h-64 rounded-(--cp-input-radius-base) bg-(--cp-bg-subtle) px-3 pt-3 pb-2">
          <BaseChart v-if="hasTrend" :option="latencyOption" :height="chartHeight" />
          <BaseEmpty
            v-else
            compact
            title="暂无延迟数据"
            description="当前时间范围还没有可绘制的延迟趋势。"
            class="min-h-50 place-content-center bg-transparent"
          />
        </div>
      </template>
    </BaseCard>
  </section>
</template>
