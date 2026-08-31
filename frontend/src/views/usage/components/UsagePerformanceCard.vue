<script setup lang="ts">
import type { EChartsOption } from 'echarts'
import type { getUsageRecordInsightsOverview } from '@/api'

import { computed, shallowRef } from 'vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseChart from '@/components/charts/BaseChart.vue'
import { requestActivityByBucket, zeroInactiveValues } from '@/components/charts/timeSeriesGap'
import { useChartPalette } from '@/composables/useChartPalette'
import { formatLocalizedCompactNumber as formatCompactNumber } from '@/utils/number'

import {
  tooltipIndex,
  tooltipRows,
  usageCategoryAxis,
  usageGapAwareLineSeries,
  usageLegend,
  usageTooltip,
  usageTooltipContent,
  usageTooltipItem,
  usageValueAxis,
} from '../utils/chart'
import { formatDuration, formatDurationAxis, formatPercent } from '../utils/format'

type Performance = Awaited<ReturnType<typeof getUsageRecordInsightsOverview>>['performance']
type PerformancePoint = Performance['points'][number]
type Activity = Awaited<ReturnType<typeof getUsageRecordInsightsOverview>>['health']['points']

const props = withDefaults(
  defineProps<{
    performance: Performance
    activity: Activity
    loading?: boolean
  }>(),
  {
    loading: false,
  },
)

const activeView = shallowRef('total')
const { palette } = useChartPalette()
const performancePoints = computed<PerformancePoint[]>(() => props.performance.points)
const requestActivity = computed(() => requestActivityByBucket(
  performancePoints.value.map(point => point.bucket),
  props.activity,
))

const viewOptions = [
  { label: '总耗时', value: 'total' },
  { label: '首字', value: 'firstToken' },
  { label: '吞吐', value: 'throughput' },
  { label: '调度', value: 'scheduling' },
]

const seriesLabels = computed(() => {
  if (activeView.value === 'throughput')
    return ['P10', 'P50', 'P90'] as const
  if (activeView.value === 'scheduling')
    return ['准入 P95', '选号 P95', '容量 P95'] as const
  return ['P50', 'P95', 'P99'] as const
})

const selectedPoints = computed(() => {
  const points = performancePoints.value
  const active = requestActivity.value
  const firstValues = zeroInactiveValues(
    points.map(point => seriesValue(point, 'first')),
    active,
  )
  const secondValues = zeroInactiveValues(
    points.map(point => seriesValue(point, 'second')),
    active,
  )
  const thirdValues = zeroInactiveValues(
    points.map(point => seriesValue(point, 'third')),
    active,
  )
  return points.map((point, index) => ({
    point,
    first: firstValues[index] ?? null,
    second: secondValues[index] ?? null,
    third: thirdValues[index] ?? null,
  }))
})

const summaryMetrics = computed(() => {
  const performance = props.performance
  if (activeView.value === 'throughput') {
    return [
      { label: 'P10', value: formatThroughput(performance.outputThroughputP10) },
      { label: 'P50', value: formatThroughput(performance.outputThroughputP50) },
      { label: 'P90', value: formatThroughput(performance.outputThroughputP90) },
    ]
  }
  if (activeView.value === 'scheduling') {
    return [
      { label: '判定覆盖', value: formatPercent(performance.admissionDecisionCoverage) },
      { label: '选号覆盖', value: formatPercent(performance.accountSelectionWaitCoverage) },
      { label: '容量覆盖', value: formatPercent(performance.capacityCoverage) },
    ]
  }
  const prefix = activeView.value === 'firstToken' ? 'firstToken' : 'latency'
  return [
    { label: 'P50', value: formatDuration(performance[`${prefix}P50Ms`]) },
    { label: 'P95', value: formatDuration(performance[`${prefix}P95Ms`]) },
    { label: 'P99', value: formatDuration(performance[`${prefix}P99Ms`]) },
  ]
})

const hasData = computed(
  () =>
    !props.loading
    && selectedPoints.value.some(
      point => point.first != null || point.second != null || point.third != null,
    ),
)

const chartOption = computed<EChartsOption>(() => {
  const theme = palette.value
  const points = selectedPoints.value
  const series = [
    ...usageGapAwareLineSeries(
      seriesLabels.value[0],
      points.map(point => point.first),
      theme.info,
      { area: 'subtle' },
    ),
    ...usageGapAwareLineSeries(
      seriesLabels.value[1],
      points.map(point => point.second),
      theme.warning,
    ),
    ...usageGapAwareLineSeries(
      seriesLabels.value[2],
      points.map(point => point.third),
      activeView.value === 'scheduling' ? theme.success : theme.danger,
      { yAxisIndex: activeView.value === 'scheduling' ? 1 : 0 },
    ),
  ]

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
    legend: usageLegend(theme, [...seriesLabels.value]),
    tooltip: usageTooltip(theme, formatTooltip),
    xAxis: usageCategoryAxis(
      points.map(({ point }) => point.label),
      theme,
    ),
    yAxis: activeView.value === 'scheduling'
      ? [
          usageValueAxis(theme, formatDurationAxis),
          usageValueAxis(theme, value => formatPercent(value), {
            min: 0,
            max: 1,
            splitLine: false,
          }),
        ]
      : usageValueAxis(
          theme,
          activeView.value === 'throughput'
            ? value => `${formatCompactNumber(value)}/s`
            : formatDurationAxis,
        ),
    series,
  }
})

function seriesValue(point: PerformancePoint, position: 'first' | 'second' | 'third') {
  if (activeView.value === 'throughput') {
    if (position === 'first')
      return point.outputThroughputP10
    if (position === 'second')
      return point.outputThroughputP50
    return point.outputThroughputP90
  }
  if (activeView.value === 'scheduling') {
    if (position === 'first')
      return point.admissionDecisionP95Ms
    if (position === 'second')
      return point.accountSelectionWaitP95Ms
    return point.capacityUtilizationP95
  }
  const prefix = activeView.value === 'firstToken' ? 'firstToken' : 'latency'
  const suffix = position === 'first' ? 'P50Ms' : position === 'second' ? 'P95Ms' : 'P99Ms'
  return point[`${prefix}${suffix}`]
}

function formatTooltip(params: unknown) {
  const rows = tooltipRows(params)
  const selected = selectedPoints.value[tooltipIndex(rows[0])]
  if (!selected)
    return ''

  const theme = palette.value
  return usageTooltipContent(theme, selected.point.label, [
    usageTooltipItem(
      seriesLabels.value[0],
      formatSeriesValue(selected.first, 'first'),
      theme.info,
    ),
    usageTooltipItem(
      seriesLabels.value[1],
      formatSeriesValue(selected.second, 'second'),
      theme.warning,
    ),
    usageTooltipItem(
      seriesLabels.value[2],
      formatSeriesValue(selected.third, 'third'),
      activeView.value === 'scheduling' ? theme.success : theme.danger,
    ),
  ])
}

function formatSeriesValue(value: number | null, position: 'first' | 'second' | 'third') {
  if (activeView.value === 'throughput')
    return formatThroughput(value)
  if (activeView.value === 'scheduling' && position === 'third')
    return formatPercent(value)
  return formatDuration(value)
}

function formatThroughput(value: number | null) {
  return value == null ? '—' : `${formatCompactNumber(value)} tok/s`
}
</script>

<template>
  <BaseCard
    as="article"
    title="响应速度"
    description="延迟、吞吐与调度分位"
    class="min-h-90 xl:h-full"
  >
    <template #actions>
      <BaseSegmented v-model="activeView" label="性能指标" :options="viewOptions" :disabled="loading" class="w-68" />
    </template>

    <template #body>
      <div class="grid min-h-66 gap-3">
        <div v-if="hasData" class="grid grid-cols-3 gap-2 rounded-xl bg-cp-fill-quaternary/45 p-2">
          <div v-for="metric in summaryMetrics" :key="metric.label" class="grid min-w-0 gap-1 px-2">
            <span class="truncate text-[10px] font-bold text-cp-text-quaternary">{{ metric.label }}</span>
            <strong class="truncate font-mono text-cp-base font-heavy tabular-nums text-cp-text" :title="metric.value">
              {{ metric.value }}
            </strong>
          </div>
        </div>
        <BaseChart v-if="hasData" :option="chartOption" :height="210" />
        <BaseEmpty
          v-else
          size="sm"
          surface="none"
          :title="loading ? '正在加载性能数据' : '暂无性能数据'"
          :description="
            activeView === 'firstToken'
              ? '当前范围没有首字耗时样本'
              : activeView === 'throughput'
                ? '当前范围没有吞吐样本'
                : activeView === 'scheduling'
                  ? '部署迁移后才会开始积累调度与容量样本'
                  : '当前范围没有总耗时样本'
          "
          class="h-52.5 place-content-center"
        />
      </div>
    </template>
  </BaseCard>
</template>
