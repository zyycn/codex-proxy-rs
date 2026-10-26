<script setup lang="ts">
import type { BarSeriesOption, EChartsOption, LineSeriesOption } from 'echarts'

import type { getUsageRecordInsightsOverview } from '@/api'
import { ZCard, ZEmpty } from '@codex-proxy/ui'
import { BarChart } from 'echarts/charts'

import { use } from 'echarts/core'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import BaseChart from '@/components/charts/BaseChart.vue'
import { chartTooltipContent, tooltipIndex, tooltipRows } from '@/components/charts/tooltip'
import { healthEligibleRequestCount } from '@/components/usage/shared/health'
import { useChartPalette } from '@/composables/useChartPalette'

import { formatLocalizedCompactNumber as formatCompactNumber, formatPercent } from '@/utils/format'

import {
  usageCategoryAxis,
  usageGapAwareLineSeries,
  usageLegend,
  usageTooltip,
  usageTooltipItem,
  usageValueAxis,
} from '../utils/chart'

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

const { t } = useI18n({ useScope: 'global' })

type Health = Awaited<ReturnType<typeof getUsageRecordInsightsOverview>>['health']
type HealthPoint = Health['points'][number]

use([BarChart])

const { palette } = useChartPalette()
const points = computed<HealthPoint[]>(() => props.health.points)

const hasData = computed(
  () => !props.loading && points.value.some(point => requestCount(point) > 0),
)

const granularityText = computed(() => {
  const labels: Record<string, string> = {
    '15m': t('usage.health.point.minutes'),
    '1h': t('usage.health.hours'),
    '1d': t('usage.health.day'),
    'day': t('usage.health.day'),
    'hour': t('usage.health.hours'),
  }
  return labels[props.granularity] ?? props.granularity
})

const description = computed(() => props.loading || !granularityText.value
  ? t('usage.health.description')
  : t('usage.health.descriptionWithGranularity', { granularityText: granularityText.value }))

const chartOption = computed<EChartsOption>(() => {
  const theme = palette.value
  const chartPoints = points.value
  const activePointCount = chartPoints.filter(point => requestCount(point) > 0).length
  const successRateSeries = usageGapAwareLineSeries(
    t('usage.health.serviceSuccessRate'),
    chartPoints.map(serviceSuccessRate),
    theme.success,
    { yAxisIndex: 1 },
  )
  const series: Array<BarSeriesOption | LineSeriesOption> = [
    outcomeSeries(t('common.status.success'), chartPoints.map(point => point.successRequests), theme.success),
    outcomeSeries(t('usage.health.point.serviceFailed'), chartPoints.map(point => point.failedRequests), theme.danger),
    outcomeSeries(t('usage.status.cancelled'), chartPoints.map(point => point.cancelledRequests), theme.info),
    outcomeSeries(t('usage.health.notCompleted'), chartPoints.map(point => point.incompleteRequests), theme.warning),
    outcomeSeries(t('usage.health.point.callerError'), chartPoints.map(point => point.callerErrorRequests), theme.normal),
    ...successRateSeries.map((item, index) => index === 0
      ? {
          ...item,
          symbol: activePointCount <= 16
            ? 'circle'
            : 'none',
          symbolSize: 4,
        }
      : item),
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
    legend: usageLegend(theme, [
      t('common.status.success'),
      t('usage.health.point.serviceFailed'),
      t('usage.status.cancelled'),
      t('usage.health.notCompleted'),
      t('usage.health.point.callerError'),
      t('usage.health.serviceSuccessRate'),
    ]),
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
    series,
  }
})

function requestCount(point: HealthPoint) {
  return Math.max(0, point.totalRequests ?? 0)
}

function serviceSuccessRate(point: HealthPoint) {
  const total = healthEligibleRequestCount(point)
  if (total > 0)
    return 1 - point.errorRate
  return requestCount(point) === 0
    ? 0
    : null
}

function outcomeSeries(name: string, data: number[], color: string) {
  return {
    name,
    type: 'bar' as const,
    stack: 'outcome',
    data,
    barMaxWidth: 24,
    itemStyle: { color, opacity: 0.72 },
  }
}

function formatTooltip(params: unknown) {
  const rows = tooltipRows(params)
  const point = points.value[tooltipIndex(rows[0])]
  if (!point)
    return ''

  const successRate = serviceSuccessRate(point)
  const theme = palette.value

  return chartTooltipContent(point.label, [
    usageTooltipItem(t('usage.health.requestVolume'), formatCompactNumber(requestCount(point)), theme.textSecondary),
    usageTooltipItem(t('common.status.success'), formatCompactNumber(point.successRequests), theme.success),
    usageTooltipItem(t('usage.health.point.serviceFailed'), formatCompactNumber(point.failedRequests), theme.danger),
    usageTooltipItem(t('usage.status.cancelled'), formatCompactNumber(point.cancelledRequests), theme.info),
    usageTooltipItem(t('usage.health.notCompleted'), formatCompactNumber(point.incompleteRequests), theme.warning),
    usageTooltipItem(t('usage.health.point.callerError'), formatCompactNumber(point.callerErrorRequests), theme.normal),
    usageTooltipItem(
      t('usage.health.serviceSuccessRate'),
      successRate == null
        ? t('usage.health.noServiceResult')
        : formatPercent(successRate),
      theme.success,
    ),
  ])
}
</script>

<template>
  <ZCard
    as="article"
    :title="t('usage.health.title')"
    :description="description"
    class="min-h-90 xl:h-full"
  >
    <template #body>
      <div class="grid min-h-66" :class="hasData ? 'gap-3' : 'h-full'">
        <div v-if="hasData" class="grid grid-cols-3 gap-2 rounded-xl bg-cp-fill-quaternary/45 p-2">
          <div class="grid gap-1 px-2">
            <span class="text-[10px] font-bold text-cp-text-quaternary">{{ t('usage.health.serviceSuccessRate') }}</span>
            <strong class="font-mono text-cp-base font-heavy tabular-nums text-cp-green-text">
              {{ formatPercent(health.successRate) }}
            </strong>
          </div>
          <div class="grid gap-1 px-2">
            <span class="text-[10px] font-bold text-cp-text-quaternary">{{ t('usage.health.completionRate') }}</span>
            <strong class="font-mono text-cp-base font-heavy tabular-nums text-cp-text">
              {{ formatPercent(health.completionRate) }}
            </strong>
          </div>
          <div class="grid gap-1 px-2">
            <span class="text-[10px] font-bold text-cp-text-quaternary">{{ t('usage.health.abnormalEnd') }}</span>
            <strong class="font-mono text-cp-base font-heavy tabular-nums text-cp-orange-text">
              {{ formatCompactNumber(health.cancelledRequests + health.incompleteRequests) }}
            </strong>
          </div>
        </div>
        <BaseChart v-if="hasData" :option="chartOption" :height="210" />
        <ZEmpty
          v-else
          size="small"
          surface="none"
          :title="loading ? t('usage.health.loading') : t('usage.health.empty')"
          class="h-full place-content-center"
        />
      </div>
    </template>
  </ZCard>
</template>
