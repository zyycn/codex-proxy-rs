<script setup lang="ts">
import type { EChartsOption, LineSeriesOption } from 'echarts'
import type { KeyUsageTrendPoint } from '@/api/modules/key-usage'
import { computed } from 'vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseChart from '@/components/charts/BaseChart.vue'
import { chartTooltipStyle } from '@/components/charts/tooltip'
import { useChartPalette } from '@/composables/useChartPalette'
import { formatCompactNumber, formatInteger } from '@/utils/number'
import { keyUsageTime, money } from '../utils/format'
import { keyUsageTokenMetrics, keyUsageTokenValue } from '../utils/metrics'

const props = defineProps<{ points: KeyUsageTrendPoint[] }>()
const { color, palette } = useChartPalette()
const option = computed<EChartsOption>(() => {
  const colors = palette.value
  const lines: Array<{ name: string, color: string, data: Array<number | null>, cost?: boolean }> = [
    ...keyUsageTokenMetrics.map(metric => ({
      name: metric.label,
      color: color(metric.colorToken, metric.fallback),
      data: props.points.map(point => keyUsageTokenValue(point, metric.key)),
    })),
    { name: '成本', color: colors.danger, data: props.points.map(point => point.costUsd === null ? null : Number(point.costUsd)), cost: true },
  ]
  return {
    animation: false,
    textStyle: { fontFamily: 'Inter Variable, Inter, sans-serif' },
    grid: { left: 8, right: 12, top: 48, bottom: 0, containLabel: true },
    tooltip: {
      trigger: 'axis',
      ...chartTooltipStyle(colors, { axisPointer: true, confine: true }),
      // 富文本渲染不把模型等外部内容插入 HTML。
      renderMode: 'richText',
      valueFormatter: value => typeof value === 'number' ? formatInteger(value) : '—',
    },
    legend: { top: 0, right: 0, type: 'plain', icon: 'circle', itemWidth: 7, itemHeight: 7, textStyle: { color: colors.textSecondary, fontSize: 11 } },
    xAxis: {
      type: 'category',
      boundaryGap: false,
      data: props.points.map(point => keyUsageTime(point.time).slice(5, 16)),
      axisLine: { show: false },
      axisTick: { show: false },
      axisLabel: { color: colors.textMuted, fontSize: 10, hideOverlap: true, formatter: value => props.points[0]?.bucketSeconds === 86400 ? value.slice(0, 5) : value },
    },
    yAxis: [
      { type: 'value', min: 0, axisLabel: { color: colors.textMuted, fontSize: 10, formatter: (value: number) => formatCompactNumber(value) }, splitLine: { lineStyle: { color: colors.grid, type: 'dashed' } } },
      { type: 'value', min: 0, axisLabel: { color: colors.textMuted, fontSize: 10, formatter: (value: number) => money(value) }, splitLine: { show: false } },
    ],
    series: lines.map((line): LineSeriesOption => ({
      name: line.name,
      type: 'line',
      data: line.data,
      yAxisIndex: line.cost ? 1 : 0,
      smooth: 0.2,
      showSymbol: props.points.length <= 1,
      connectNulls: false,
      symbolSize: 5,
      lineStyle: { color: line.color, width: 2, type: line.cost ? 'dashed' : 'solid' },
      itemStyle: { color: line.color },
      areaStyle: line.name === '缓存命中' ? { color: line.color, opacity: 0.1 } : undefined,
      tooltip: { valueFormatter: value => typeof value === 'number' ? (line.cost ? money(value) : formatInteger(value)) : '—' },
    })),
  }
})
</script>

<template>
  <BaseCard title="使用趋势" description="用量随时间的变化">
    <BaseChart v-if="points.some(point => point.requests > 0)" :option="option" :height="285" />
    <div v-else class="grid h-71 place-items-center text-cp-sm text-cp-text-tertiary">
      所选时间内暂无请求
    </div>
  </BaseCard>
</template>
