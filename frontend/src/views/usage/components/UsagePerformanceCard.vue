<script setup lang="ts">
import type { EChartsOption, LineSeriesOption } from 'echarts'
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
import { escapeTooltip, formatDuration, formatDurationAxis } from '../utils/format'

const props = withDefaults(
  defineProps<{
    performance: any
    loading?: boolean
  }>(),
  {
    loading: false,
  },
)

const activeView = shallowRef('total')
const { palette } = useUsageChartPalette()
const performancePoints = computed<any[]>(() => props.performance.points ?? [])

const viewOptions = [
  { label: '总耗时', value: 'total' },
  { label: '首字', value: 'firstToken' },
]

const percentileLabels = {
  p50: '常规响应',
  p95: '较慢响应',
  p99: '极慢响应',
}

const selectedPoints = computed(() =>
  performancePoints.value.map((point) => ({
    point,
    p50: percentileValue(point, 'p50'),
    p95: percentileValue(point, 'p95'),
    p99: percentileValue(point, 'p99'),
  })),
)

const hasData = computed(
  () =>
    !props.loading &&
    selectedPoints.value.some(
      (point) => point.p50 != null || point.p95 != null || point.p99 != null,
    ),
)

const chartOption = computed<EChartsOption>(() => {
  const theme = palette.value
  const points = selectedPoints.value

  return {
    animationDuration: 240,
    grid: { left: 52, right: 12, top: 40, bottom: 28 },
    legend: {
      top: 0,
      right: 4,
      itemWidth: 8,
      itemHeight: 8,
      icon: 'circle',
      data: Object.values(percentileLabels),
      textStyle: {
        color: theme.textSecondary,
        fontSize: 11,
        fontFamily: 'Inter Variable, Inter, system-ui, sans-serif',
        fontWeight: 650,
      },
    },
    tooltip: usageTooltip(theme, formatTooltip),
    xAxis: usageCategoryAxis(
      points.map(({ point }) => point.label),
      theme,
    ),
    yAxis: usageValueAxis(theme, formatDurationAxis),
    series: [
      lineSeries(
        percentileLabels.p50,
        points.map((point) => point.p50),
        theme.info,
        true,
      ),
      lineSeries(
        percentileLabels.p95,
        points.map((point) => point.p95),
        theme.warning,
      ),
      lineSeries(
        percentileLabels.p99,
        points.map((point) => point.p99),
        theme.danger,
      ),
    ],
  }
})

function percentileValue(point: any, percentile: 'p50' | 'p95' | 'p99') {
  if (activeView.value === 'firstToken') {
    if (percentile === 'p50') return point.firstTokenP50Ms
    if (percentile === 'p95') return point.firstTokenP95Ms
    return point.firstTokenP99Ms
  }
  if (percentile === 'p50') return point.latencyP50Ms
  if (percentile === 'p95') return point.latencyP95Ms
  return point.latencyP99Ms
}

function lineSeries(
  name: string,
  data: Array<number | null | undefined>,
  color: string,
  area = false,
): LineSeriesOption {
  return {
    name,
    type: 'line',
    data: data.map((value) => value ?? null),
    connectNulls: false,
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
              { offset: 0, color: `${color}18` },
              { offset: 1, color: `${color}02` },
            ],
          },
        }
      : undefined,
  }
}

function formatTooltip(params: unknown) {
  const rows = tooltipRows(params)
  const selected = selectedPoints.value[tooltipIndex(rows[0])]
  if (!selected) return ''

  return [
    escapeTooltip(selected.point.label),
    `${percentileLabels.p50}: ${formatDuration(selected.p50)}`,
    `${percentileLabels.p95}: ${formatDuration(selected.p95)}`,
    `${percentileLabels.p99}: ${formatDuration(selected.p99)}`,
  ].join('<br/>')
}
</script>

<template>
  <BaseCard
    as="article"
    :padded="false"
    title="响应速度"
    description="总耗时或首字的分位耗时，越低越快"
    header-collapse-at="none"
    header-class="px-5 pt-4"
    body-class="px-5 pt-3 pb-4"
    class="h-full min-h-90"
  >
    <template #actions>
      <BaseSegmented v-model="activeView" :options="viewOptions" :disabled="loading" class="w-43" />
    </template>

    <template #body>
      <div class="min-h-66">
        <BaseChart v-if="hasData" :option="chartOption" :height="264" />
        <BaseEmpty
          v-else
          compact
          plain
          :title="loading ? '正在加载性能数据' : '暂无性能数据'"
          :description="
            activeView === 'firstToken' ? '当前范围没有首字耗时样本' : '当前范围没有总耗时样本'
          "
          class="h-66 place-content-center"
        />
      </div>
    </template>
  </BaseCard>
</template>
