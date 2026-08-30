<script setup lang="ts">
import type { EChartsOption, LineSeriesOption } from 'echarts'
import type { getUsageRecordInsightsOverview } from '@/api'

import { computed, shallowRef } from 'vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseChart from '@/components/charts/BaseChart.vue'
import { useChartPalette } from '@/composables/useChartPalette'
import { formatLocalizedCompactNumber as formatCompactNumber } from '@/utils/number'

import {
  tooltipIndex,
  tooltipRows,
  usageCategoryAxis,
  usageLegend,
  usageLineSeries,
  usageTooltip,
  usageTooltipContent,
  usageValueAxis,
} from '../utils/chart'
import {
  decimalDisplayNumber,
  formatPercent,
  formatUsd,
  formatUsdAxis,
} from '../utils/format'

type Cost = Awaited<ReturnType<typeof getUsageRecordInsightsOverview>>['cost']
type UsageChartPalette = ReturnType<typeof useChartPalette>['palette']['value']

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
const { palette } = useChartPalette()
const points = computed(() => props.cost.points)
const hasNoCacheCost = computed(() =>
  points.value.some(point => point.noCacheCost != null),
)
const coverageRate = computed(() => {
  const { known, partial, unknown } = props.cost.coverage
  const total = known + partial + unknown
  return total > 0 ? (known + partial) / total : null
})

const viewOptions = [
  { label: '费用', value: 'cost' },
  { label: '节省', value: 'savings' },
  { label: '缓存', value: 'cache' },
]

const hasData = computed(() => {
  if (props.loading || points.value.length === 0)
    return false
  if (activeView.value === 'cost') {
    return points.value.some(point => point.estimatedCost != null || point.noCacheCost != null)
  }
  if (activeView.value === 'savings')
    return points.value.some(point => point.cacheSavings != null)
  return points.value.some(
    point => point.inputTokens > 0 || point.cachedTokenRate > 0 || (point.cacheHitRequestRate ?? 0) > 0,
  )
})

const chartOption = computed<EChartsOption>(() => {
  const theme = palette.value
  const legend = legendNames()

  return {
    animationDuration: 240,
    grid: {
      left: 0,
      right: 0,
      top: 40,
      bottom: 0,
      outerBoundsMode: 'same',
      outerBoundsContain: 'axisLabel',
    },
    legend: usageLegend(theme, legend),
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
  if (activeView.value === 'savings')
    return ['缓存节省']
  if (activeView.value === 'cache')
    return ['缓存 Token 占比', '命中请求率']
  return hasNoCacheCost.value ? ['实际费用', '无缓存费用'] : ['实际费用']
}

function axisFormatter() {
  if (activeView.value === 'cost' || activeView.value === 'savings')
    return formatUsdAxis
  if (activeView.value === 'cache')
    return (value: number) => formatPercent(value)
  return (value: number) => formatCompactNumber(value)
}

function chartSeries(theme: UsageChartPalette): LineSeriesOption[] {
  const chartPoints = points.value
  if (activeView.value === 'savings') {
    return [
      usageLineSeries(
        '缓存节省',
        chartPoints.map(point => decimalDisplayNumber(point.cacheSavings)),
        theme.success,
        { area: 'strong' },
      ),
    ]
  }

  if (activeView.value === 'cache') {
    return [
      usageLineSeries(
        '缓存 Token 占比',
        chartPoints.map(point => point.cachedTokenRate),
        theme.normal,
      ),
      usageLineSeries(
        '命中请求率',
        chartPoints.map(point => point.cacheHitRequestRate),
        theme.success,
      ),
    ]
  }

  const series = [
    usageLineSeries(
      '实际费用',
      chartPoints.map(point => decimalDisplayNumber(point.estimatedCost)),
      theme.success,
      { area: 'strong' },
    ),
  ]
  if (hasNoCacheCost.value) {
    series.push(
      usageLineSeries(
        '无缓存费用',
        chartPoints.map(point => decimalDisplayNumber(point.noCacheCost)),
        theme.textMuted,
      ),
    )
  }
  return series
}

function formatTooltip(params: unknown) {
  const rows = tooltipRows(params)
  const point = points.value[tooltipIndex(rows[0])]
  if (!point)
    return ''

  if (activeView.value === 'savings') {
    return usageTooltipContent(palette.value, point.label, [
      `缓存节省: ${formatUsd(point.cacheSavings)}`,
    ])
  }

  if (activeView.value === 'cache') {
    return usageTooltipContent(palette.value, point.label, [
      `缓存 Token 占比: ${formatPercent(point.cachedTokenRate)}`,
      `命中请求率: ${formatPercent(point.cacheHitRequestRate)}`,
    ])
  }

  const lines = [
    `实际费用: ${formatUsd(point.estimatedCost)}`,
  ]
  if (point.noCacheCost != null)
    lines.push(`无缓存费用: ${formatUsd(point.noCacheCost)}`)
  return usageTooltipContent(palette.value, point.label, lines)
}
</script>

<template>
  <BaseCard
    as="article"
    title="成本效率"
    description="实际费用、缓存节省、服务层溢价与单位成本"
    class="h-full min-h-90"
  >
    <template #actions>
      <BaseSegmented v-model="activeView" label="成本视图" :options="viewOptions" :disabled="loading" class="w-50" />
    </template>

    <template #body>
      <div class="grid min-h-66 gap-3">
        <div v-if="hasData" class="grid grid-cols-5 gap-1 rounded-xl bg-cp-fill-quaternary/45 p-2">
          <div class="grid min-w-0 gap-1 px-1.5">
            <span class="truncate text-[10px] font-bold text-cp-text-quaternary">实际费用</span>
            <strong class="truncate font-mono text-cp-sm font-heavy tabular-nums text-cp-text" :title="formatUsd(cost.estimatedCost)">
              {{ formatUsd(cost.estimatedCost) }}
            </strong>
          </div>
          <div class="grid min-w-0 gap-1 px-1.5">
            <span class="truncate text-[10px] font-bold text-cp-text-quaternary">缓存节省</span>
            <strong class="truncate font-mono text-cp-sm font-heavy tabular-nums text-cp-green-text" :title="formatUsd(cost.cacheSavings)">
              {{ formatUsd(cost.cacheSavings) }}
            </strong>
          </div>
          <div class="grid min-w-0 gap-1 px-1.5">
            <span class="truncate text-[10px] font-bold text-cp-text-quaternary">层级溢价</span>
            <strong class="truncate font-mono text-cp-sm font-heavy tabular-nums text-cp-orange-text" :title="formatUsd(cost.tierPremium)">
              {{ formatUsd(cost.tierPremium) }}
            </strong>
          </div>
          <div class="grid min-w-0 gap-1 px-1.5">
            <span class="truncate text-[10px] font-bold text-cp-text-quaternary">每成功请求</span>
            <strong class="truncate font-mono text-cp-sm font-heavy tabular-nums text-cp-text" :title="formatUsd(cost.costPerSuccessfulRequest, true)">
              {{ formatUsd(cost.costPerSuccessfulRequest, true) }}
            </strong>
          </div>
          <div class="grid min-w-0 gap-1 px-1.5">
            <span class="truncate text-[10px] font-bold text-cp-text-quaternary">费用覆盖</span>
            <strong class="truncate font-mono text-cp-sm font-heavy tabular-nums text-cp-text">
              {{ formatPercent(coverageRate) }}
            </strong>
          </div>
        </div>
        <BaseChart v-if="hasData" :option="chartOption" :height="210" />
        <BaseEmpty
          v-else
          size="sm"
          surface="none"
          :title="loading ? '正在加载成本数据' : '暂无成本效率数据'"
          description="当前范围没有可绘制的费用或缓存收益数据"
          class="h-52.5 place-content-center"
        />
      </div>
    </template>
  </BaseCard>
</template>
