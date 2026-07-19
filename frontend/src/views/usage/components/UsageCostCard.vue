<script setup lang="ts">
import type { EChartsOption, LineSeriesOption } from 'echarts'
import type { getUsageRecordInsightsOverview } from '@/api'

import { computed, shallowRef } from 'vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseChart from '@/components/charts/BaseChart.vue'
import { useUsageChartPalette } from '../composables/useUsageChartPalette'

import {
  tooltipIndex,
  tooltipRows,
  usageCategoryAxis,
  usageTooltip,
  usageValueAxis,
} from '../utils/chart'
import {
  decimalDisplayNumber,
  escapeTooltip,
  formatCompactNumber,
  formatPercent,
  formatUsd,
  formatUsdAxis,
} from '../utils/format'

type Cost = Awaited<ReturnType<typeof getUsageRecordInsightsOverview>>['cost']
type UsageChartPalette = ReturnType<typeof useUsageChartPalette>['palette']['value']

const props = withDefaults(
  defineProps<{
    cost: Cost
    loading?: boolean
  }>(),
  {
    loading: false,
  },
)

const activeView = shallowRef('cost')
const { palette } = useUsageChartPalette()
const points = computed<any[]>(() => props.cost.points)

const viewOptions = [
  { label: '费用', value: 'cost' },
  { label: 'Token', value: 'tokens' },
  { label: '缓存', value: 'cache' },
]

const hasData = computed(() => {
  if (props.loading || points.value.length === 0)
    return false
  if (activeView.value === 'cost') {
    return points.value.some(point => point.estimatedCost != null || point.standardCost != null)
  }
  if (activeView.value === 'tokens') {
    return points.value.some(point => point.totalTokens > 0 || point.cachedTokens > 0)
  }
  return points.value.some(
    point => point.inputTokens > 0 || point.cachedTokenRate > 0 || point.cacheHitRequestRate > 0,
  )
})

const chartOption = computed<EChartsOption>(() => {
  const theme = palette.value
  const legend = legendNames()

  return {
    animationDuration: 240,
    grid: { left: 0, right: 0, top: 40, bottom: 0, containLabel: true },
    legend: {
      top: 0,
      right: 4,
      itemWidth: 8,
      itemHeight: 8,
      icon: 'circle',
      data: legend,
      textStyle: {
        color: theme.textSecondary,
        fontSize: 11,
        fontFamily: 'Inter Variable, Inter, system-ui, sans-serif',
        fontWeight: 650,
      },
    },
    tooltip: usageTooltip(theme, formatTooltip),
    xAxis: usageCategoryAxis(
      points.value.map(point => point.label),
      theme,
    ),
    yAxis: usageValueAxis(theme, axisFormatter(), {
      min: activeView.value === 'cache' ? 0 : undefined,
      max: activeView.value === 'cache' ? 1 : undefined,
    }),
    series: chartSeries(theme),
  }
})

function legendNames() {
  if (activeView.value === 'tokens')
    return ['未缓存输入', '缓存输入', '输出']
  if (activeView.value === 'cache')
    return ['缓存 Token 占比', '命中请求率']
  return ['预估费用', '标准费用']
}

function axisFormatter() {
  if (activeView.value === 'cost')
    return formatUsdAxis
  if (activeView.value === 'cache')
    return (value: number) => formatPercent(value)
  return (value: number) => formatCompactNumber(value)
}

function chartSeries(theme: UsageChartPalette): LineSeriesOption[] {
  const chartPoints = points.value
  if (activeView.value === 'tokens') {
    return [
      lineSeries(
        '未缓存输入',
        chartPoints.map(point => Math.max(0, point.inputTokens - point.cachedTokens)),
        theme.info,
        'tokens',
      ),
      lineSeries(
        '缓存输入',
        chartPoints.map(point => point.cachedTokens),
        theme.normal,
        'tokens',
      ),
      lineSeries(
        '输出',
        chartPoints.map(point => point.outputTokens),
        theme.success,
        'tokens',
      ),
    ]
  }

  if (activeView.value === 'cache') {
    return [
      lineSeries(
        '缓存 Token 占比',
        chartPoints.map(point => point.cachedTokenRate),
        theme.normal,
      ),
      lineSeries(
        '命中请求率',
        chartPoints.map(point => point.cacheHitRequestRate),
        theme.success,
      ),
    ]
  }

  return [
    lineSeries(
      '预估费用',
      chartPoints.map(point => decimalDisplayNumber(point.estimatedCost)),
      theme.success,
      undefined,
      true,
    ),
    lineSeries(
      '标准费用',
      chartPoints.map(point => decimalDisplayNumber(point.standardCost)),
      theme.textMuted,
    ),
  ]
}

function lineSeries(
  name: string,
  data: Array<number | null>,
  color: string,
  stack?: string,
  area = Boolean(stack),
): LineSeriesOption {
  return {
    name,
    type: 'line',
    data: data.map(value => value ?? null),
    connectNulls: false,
    stack,
    smooth: true,
    showSymbol: data.length <= 12,
    symbol: 'circle',
    symbolSize: 5,
    lineStyle: { color, width: 2.2 },
    itemStyle: { color },
    areaStyle: area
      ? {
          color: {
            type: 'linear',
            x: 0,
            y: 0,
            x2: 0,
            y2: 1,
            colorStops: [
              { offset: 0, color: `${color}30` },
              { offset: 1, color: `${color}08` },
            ],
          },
        }
      : undefined,
  }
}

function formatTooltip(params: unknown) {
  const rows = tooltipRows(params)
  const point = points.value[tooltipIndex(rows[0])]
  if (!point)
    return ''

  const title = escapeTooltip(point.label)
  if (activeView.value === 'tokens') {
    return [
      title,
      `未缓存输入: ${formatCompactNumber(Math.max(0, point.inputTokens - point.cachedTokens))}`,
      `缓存输入: ${formatCompactNumber(point.cachedTokens)}`,
      `输出: ${formatCompactNumber(point.outputTokens)}`,
      `总 Token: ${formatCompactNumber(point.totalTokens)}`,
    ].join('<br/>')
  }

  if (activeView.value === 'cache') {
    return [
      title,
      `缓存 Token 占比: ${formatPercent(point.cachedTokenRate)}`,
      `命中请求率: ${formatPercent(point.cacheHitRequestRate)}`,
    ].join('<br/>')
  }

  return [
    title,
    `预估费用: ${formatUsd(point.estimatedCost)}`,
    `标准费用: ${formatUsd(point.standardCost)}`,
  ].join('<br/>')
}
</script>

<template>
  <BaseCard
    as="article"
    :padded="false"
    title="成本效率"
    description="预估费用、Token 与缓存收益"
    header-collapse-at="none"
    header-class="px-5 pt-4"
    body-class="px-5 pt-3 pb-4"
    class="h-full min-h-90"
  >
    <template #actions>
      <BaseSegmented v-model="activeView" :options="viewOptions" :disabled="loading" class="w-50" />
    </template>

    <template #body>
      <div class="min-h-66">
        <BaseChart v-if="hasData" :option="chartOption" :height="264" />
        <BaseEmpty
          v-else
          compact
          plain
          :title="loading ? '正在加载成本数据' : '暂无成本效率数据'"
          description="当前范围没有可绘制的费用或 Token 数据"
          class="h-66 place-content-center"
        />
      </div>
    </template>
  </BaseCard>
</template>
