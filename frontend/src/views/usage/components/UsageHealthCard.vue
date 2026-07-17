<script setup lang="ts">
import type { EChartsOption } from 'echarts'
import type { getUsageRecordInsightsOverview } from '@/api'
import { BarChart } from 'echarts/charts'
import { use } from 'echarts/core'

import { computed } from 'vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseChart from '@/components/charts/BaseChart.vue'
import { useUsageChartPalette } from '../composables/useUsageChartPalette'

import {
  tooltipIndex,
  tooltipRows,
  usageCategoryAxis,
  usageTooltip,
  usageValueAxis,
} from '../utils/chart'
import { escapeTooltip, formatCompactNumber, formatPercent } from '../utils/format'

type Health = Awaited<ReturnType<typeof getUsageRecordInsightsOverview>>['health']
type HealthPoint = Health['points'][number]

const props = withDefaults(
  defineProps<{
    health: Health
    granularity: string
    loading?: boolean
  }>(),
  {
    loading: false,
  },
)

use([BarChart])

const { palette } = useUsageChartPalette()
const points = computed<HealthPoint[]>(() => props.health.points)

const hasData = computed(
  () => !props.loading && points.value.some(point => requestCount(point) > 0),
)

const granularityText = computed(() => {
  const labels: Record<string, string> = {
    '15m': '15 分钟',
    '1h': '小时',
    '1d': '天',
    'day': '天',
    'hour': '小时',
  }
  return labels[props.granularity] ?? props.granularity
})

const chartOption = computed<EChartsOption>(() => {
  const theme = palette.value
  const chartPoints = points.value
  const activePointCount = chartPoints.filter(point => requestCount(point) > 0).length

  return {
    animationDuration: 240,
    grid: { left: 0, right: 0, top: 40, bottom: 0, containLabel: true },
    legend: {
      top: 0,
      right: 4,
      itemWidth: 8,
      itemHeight: 8,
      icon: 'circle',
      data: ['请求量', '成功率', '失败'],
      textStyle: {
        color: theme.textSecondary,
        fontSize: 11,
        fontFamily: 'Inter Variable, Inter, system-ui, sans-serif',
        fontWeight: 650,
      },
    },
    tooltip: usageTooltip(theme, formatTooltip),
    xAxis: usageCategoryAxis(
      chartPoints.map(point => point.label),
      theme,
    ),
    yAxis: [
      usageValueAxis(theme, value => formatCompactNumber(value)),
      usageValueAxis(theme, value => formatPercent(value), {
        min: 0,
        max: 1,
        splitLine: false,
      }),
    ],
    series: [
      {
        name: '请求量',
        type: 'bar',
        data: chartPoints.map(requestCount),
        barMaxWidth: 24,
        itemStyle: { color: theme.info, opacity: 0.32, borderRadius: [3, 3, 0, 0] },
      },
      {
        name: '成功率',
        type: 'line',
        yAxisIndex: 1,
        data: chartPoints.map(requestSuccessRate),
        connectNulls: true,
        smooth: 0.25,
        symbol: activePointCount <= 16 ? 'circle' : 'none',
        symbolSize: 4,
        lineStyle: { color: theme.success, width: 2.2 },
        itemStyle: { color: theme.success },
      },
      {
        name: '失败',
        type: 'line',
        data: chartPoints.map(point => (point.failedRequests > 0 ? requestCount(point) : null)),
        connectNulls: false,
        showSymbol: true,
        symbol: 'circle',
        symbolSize: 7,
        lineStyle: { opacity: 0 },
        itemStyle: {
          color: theme.danger,
          borderColor: theme.surface,
          borderWidth: 2,
        },
        z: 4,
      },
    ],
  }
})

function requestCount(point: HealthPoint) {
  return Math.max(0, point.successRequests ?? 0) + Math.max(0, point.failedRequests ?? 0)
}

function requestSuccessRate(point: HealthPoint) {
  const total = requestCount(point)
  return total > 0 ? Math.max(0, point.successRequests ?? 0) / total : null
}

function formatTooltip(params: unknown) {
  const rows = tooltipRows(params)
  const point = points.value[tooltipIndex(rows[0])]
  if (!point)
    return ''

  const successRate = requestSuccessRate(point)

  return [
    escapeTooltip(point.label),
    `请求量: ${formatCompactNumber(requestCount(point))}`,
    `成功: ${formatCompactNumber(point.successRequests)}`,
    `失败: ${formatCompactNumber(point.failedRequests)}`,
    `成功率: ${successRate == null ? '无请求' : formatPercent(successRate)}`,
  ].join('<br/>')
}
</script>

<template>
  <BaseCard
    as="article"
    :padded="false"
    title="请求健康"
    :description="`按${granularityText}观察请求量、成功率与失败时段`"
    header-class="px-5 pt-4"
    body-class="px-5 pt-3 pb-4"
    class="h-full min-h-90"
  >
    <template #body>
      <div class="min-h-66">
        <BaseChart v-if="hasData" :option="chartOption" :height="264" />
        <BaseEmpty
          v-else
          compact
          plain
          :title="loading ? '正在加载请求健康数据' : '暂无请求健康数据'"
          description="当前范围没有可绘制的请求记录"
          class="h-66 place-content-center"
        />
      </div>
    </template>
  </BaseCard>
</template>
