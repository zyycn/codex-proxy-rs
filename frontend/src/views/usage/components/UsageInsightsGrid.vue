<script setup lang="ts">
import { computed } from 'vue'
import type { EChartsOption } from 'echarts'
import { storeToRefs } from 'pinia'

import BaseCard from '@/components/base/BaseCard.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseChart from '@/components/charts/BaseChart.vue'
import { useUiStore } from '@/stores/modules/ui'
import UsageDistributionPanel from './UsageDistributionPanel.vue'

const props = defineProps<{
  insights: any
  loading?: boolean
}>()

const chartHeight = 236
const chartSplitNumber = 3

const modelSource = defineModel<string>('modelSource', { default: 'requested' })
const { themeRevision } = storeToRefs(useUiStore())

const modelSourceOptions = [
  { label: '请求模型', value: 'requested' },
  { label: '上游模型', value: 'upstream' },
  { label: '映射', value: 'mapping' },
]

const modelItems = computed(() => props.insights?.modelDistribution ?? [])
const endpointItems = computed(() => props.insights?.endpointDistribution ?? [])

const trendPoints = computed(() => props.insights?.tokenTrend ?? [])
const latencyPoints = computed(() => props.insights?.latencyTrend ?? [])
const hasTrend = computed(() =>
  trendPoints.value.some(
    (point: any) =>
      point.totalTokensValue > 0 || point.cachedTokensValue > 0 || point.averageLatencyMsValue > 0,
  ),
)
const hasLatencyTrend = computed(() =>
  latencyPoints.value.some((point: any) => Number(point.averageLatencyMsValue || 0) > 0),
)

const trendOption = computed<EChartsOption>(() => ({
  grid: { left: 8, right: 10, top: 36, bottom: 28 },
  legend: {
    top: 10,
    right: 12,
    itemWidth: 8,
    itemHeight: 8,
    icon: 'circle',
    data: ['输入', '输出', '缓存读取', '缓存命中率'],
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
      splitNumber: chartSplitNumber,
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
    data: latencyPoints.value.map((point: any) => point.date),
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
    splitNumber: chartSplitNumber,
    axisLine: { show: false },
    axisTick: { show: false },
    axisLabel: { show: false },
    splitLine: { lineStyle: { color: chartGridLineColor(), width: 1 } },
  },
  series: [
    lineSeries(
      '平均延迟',
      latencyPoints.value.map((point: any) => Number(point.averageLatencyMsValue || 0)),
      themeColor('--cp-warning', '#D97706'),
      true,
    ),
  ],
}))

function trendSeries() {
  return [
    lineSeries(
      '输入',
      trendPoints.value.map((point: any) => point.inputTokensValue),
      themeColor('--cp-info', '#2563EB'),
      true,
    ),
    lineSeries(
      '输出',
      trendPoints.value.map((point: any) => point.outputTokensValue),
      themeColor('--cp-success', '#10B981'),
    ),
    lineSeries(
      '缓存读取',
      trendPoints.value.map((point: any) => point.cachedTokensValue),
      themeColor('--cp-info-text', '#0EA5E9'),
    ),
    lineSeries(
      '缓存命中率',
      trendPoints.value.map((point: any) => point.cacheHitRateValue),
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

function formatTooltip(params: unknown) {
  const rows = Array.isArray(params) ? params : [params]
  const title = tooltipValue(rows[0], 'axisValueLabel')
  const point = trendPoints.value[tooltipIndex(rows[0])]
  const lines = rows.map((row) => {
    const name = tooltipValue(row, 'seriesName')
    const marker = tooltipValue(row, 'marker')
    const display = trendPointDisplay(point, name)
    return `${marker}${name}: ${display}`
  })
  const billingLine = point
    ? `实际: ${point.actualBillingAmount} ｜ 标准: ${point.standardBillingAmount}`
    : ''
  return [title, ...lines, billingLine].filter(Boolean).join('<br/>')
}

function formatLatencyTooltip(params: unknown) {
  const rows = Array.isArray(params) ? params : [params]
  const title = tooltipValue(rows[0], 'axisValueLabel')
  const point = latencyPoints.value[tooltipIndex(rows[0])]
  return [title, `平均延迟: ${point?.averageLatencyMs}`].filter(Boolean).join('<br/>')
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

function trendPointDisplay(point: any, name: string) {
  if (name === '输入') return point?.inputTokens
  if (name === '输出') return point?.outputTokens
  if (name === '缓存读取') return point?.cachedTokens
  if (name === '缓存命中率') return point?.cacheHitRate
  return ''
}

function chartGridLineColor() {
  return themeColor('--cp-bg-muted', '#F1F5F9')
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
      description="请求模型与上游模型"
      name-label="模型"
      :items="modelItems"
      color="--cp-info"
      :source-options="modelSourceOptions"
    />

    <UsageDistributionPanel
      title="端点分布"
      description="客户端 API 路由"
      name-label="端点"
      :items="endpointItems"
      color="--cp-normal"
      :show-account-billing-column="false"
    />

    <BaseCard
      :padded="false"
      title="Token 使用趋势"
      description="每日 Token 用量"
      header-class="px-5 pt-4"
      body-class="px-5 pt-3 pb-4"
    >
      <template #body>
        <div class="h-64 px-3 pt-3 pb-2">
          <div v-if="hasTrend" class="flex h-full min-h-50 flex-col">
            <BaseChart :option="trendOption" :height="chartHeight" />
          </div>
          <BaseEmpty
            v-else
            compact
            title="暂无趋势数据"
            description="当前范围暂无 Token 数据"
            class="min-h-50 place-content-center bg-transparent"
          />
        </div>
      </template>
    </BaseCard>

    <BaseCard
      :padded="false"
      title="延迟分布"
      description="每日平均响应耗时"
      header-class="px-5 pt-4"
      body-class="px-5 pt-3 pb-4"
    >
      <template #body>
        <div class="h-64 px-3 pt-3 pb-2">
          <BaseChart v-if="hasLatencyTrend" :option="latencyOption" :height="chartHeight" />
          <BaseEmpty
            v-else
            compact
            title="暂无延迟数据"
            description="当前范围暂无延迟数据"
            class="min-h-50 place-content-center bg-transparent"
          />
        </div>
      </template>
    </BaseCard>
  </section>
</template>
