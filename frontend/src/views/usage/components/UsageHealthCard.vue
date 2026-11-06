<script setup lang="ts">
import type { EChartsOption } from 'echarts'

import type { getUsageRecordInsightsOverview } from '@/api'
import { ZCard, ZEmpty } from '@codex-proxy/ui'
import { BarChart } from 'echarts/charts'

import { use } from 'echarts/core'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import BaseChart from '@/components/charts/BaseChart.vue'
import { chartTooltipContent, tooltipIndex, tooltipRows } from '@/components/charts/tooltip'
import { useChartPalette } from '@/composables/useChartPalette'

import { formatLocalizedCompactNumber as formatCompactNumber, formatPercent } from '@/utils/format'

import {
  usageCategoryAxis,
  usageLegend,
  usageTooltip,
  usageTooltipItem,
  usageValueAxis,
} from '../utils/chart'

const props = withDefaults(
  defineProps<{
    health: Health | null
    error?: string
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
const points = computed<HealthPoint[]>(() => props.health?.points ?? [])

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
  const series = [
    outcomeSeries(t('common.status.success'), chartPoints.map(point => point.successRequests), theme.success),
    outcomeSeries(t('usage.health.point.serviceFailed'), chartPoints.map(point => point.failedRequests), theme.danger),
    outcomeSeries(t('usage.status.cancelled'), chartPoints.map(point => point.cancelledRequests), theme.info),
    outcomeSeries(t('usage.health.notCompleted'), chartPoints.map(point => point.incompleteRequests), theme.warning),
    outcomeSeries(t('usage.health.point.callerError'), chartPoints.map(point => point.callerErrorRequests), theme.normal),
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
    legend: usageLegend(theme, series.map(item => item.name)),
    tooltip: usageTooltip(theme, formatTooltip),
    xAxis: usageCategoryAxis(
      chartPoints.map(point => point.label),
      theme,
    ),
    yAxis: usageValueAxis(theme, value => formatCompactNumber(value)),
    series,
  }
})

function requestCount(point: HealthPoint) {
  return Math.max(0, point.totalRequests ?? 0)
}

function serviceSuccessRateText(rate: number | null) {
  return rate == null
    ? t('usage.noValidSample')
    : formatPercent(rate)
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
      serviceSuccessRateText(point.errorRate == null ? null : 1 - point.errorRate),
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
        <div v-if="hasData && health" class="grid grid-cols-2 gap-2 rounded-xl bg-z-fill-quaternary/45 p-2 sm:grid-cols-4">
          <div class="grid gap-1 px-2">
            <span class="text-[10px] font-bold text-z-text-quaternary">{{ t('usage.health.serviceSuccessRate') }}</span>
            <strong class="font-mono text-z-base font-heavy tabular-nums text-z-green-text">
              {{ serviceSuccessRateText(health.successRate) }}
            </strong>
          </div>
          <div class="grid gap-1 px-2">
            <span class="text-[10px] font-bold text-z-text-quaternary">{{ t('usage.health.completionRate') }}</span>
            <strong class="font-mono text-z-base font-heavy tabular-nums text-z-text">
              {{ formatPercent(health.completionRate) }}
            </strong>
          </div>
          <div class="grid gap-1 px-2">
            <span class="text-[10px] font-bold text-z-text-quaternary">{{ t('usage.status.cancelled') }}</span>
            <strong class="font-mono text-z-base font-heavy tabular-nums text-z-blue-text">
              {{ formatCompactNumber(health.cancelledRequests) }}
            </strong>
          </div>
          <div class="grid gap-1 px-2">
            <span class="text-[10px] font-bold text-z-text-quaternary">{{ t('usage.health.notCompleted') }}</span>
            <strong class="font-mono text-z-base font-heavy tabular-nums text-z-orange-text">
              {{ formatCompactNumber(health.incompleteRequests) }}
            </strong>
          </div>
        </div>
        <BaseChart v-if="hasData" :option="chartOption" :height="210" />
        <ZEmpty
          v-else
          size="small"
          surface="none"
          :title="error || (loading ? t('usage.health.loading') : t('usage.health.empty'))"
          class="h-full place-content-center"
        />
      </div>
    </template>
  </ZCard>
</template>
