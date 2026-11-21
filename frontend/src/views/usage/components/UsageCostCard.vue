<script setup lang="ts">
import type { EChartsOption } from 'echarts'

import type { UsageCostMetric, UsageMetricResponse } from '@/api'
import { ZCard, ZEmpty, ZSegmented } from '@codex-proxy/ui'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import BaseChart from '@/components/charts/BaseChart.vue'
import { zeroInactiveValues } from '@/components/charts/timeSeriesGap'
import { chartTooltipContent, tooltipIndex, tooltipRows } from '@/components/charts/tooltip'
import { useChartPalette } from '@/composables/useChartPalette'
import { decimalDisplayNumber, formatPercent, formatUsd } from '@/utils/format'

import {
  formatUsdAxis,
  usageCategoryAxis,
  usageGapAwareLineSeries,
  usageLegend,
  usageTooltip,
  usageTooltipItem,
  usageValueAxis,
} from '../utils/chart'

const props = withDefaults(
  defineProps<{
    result: UsageMetricResponse<UsageCostMetric> | null
    error?: string
    loading?: boolean
  }>(),
  {
    loading: false,
  },
)

const { t } = useI18n({ useScope: 'global' })

interface CostSeries {
  label: string
  color: string
  values: (number | null)[]
  area?: 'strong'
  tooltipValue: (index: number) => string | null
}

const activeView = defineModel<UsageCostMetric>({ required: true })
const { palette } = useChartPalette()
const cost = computed(() => props.result?.data.summary ?? null)
const points = computed(() => props.result?.data.points ?? [])
const requestActivity = computed(() => points.value.map(point => point.requestCount > 0))
const coverageRate = computed(() => {
  if (!cost.value)
    return null
  const { known, partial, unknown } = cost.value.coverage
  const total = known + partial + unknown
  return total > 0
    ? (known + partial) / total
    : null
})

const viewOptions = computed(() => ([
  {
    label: t('usage.fields.cost'),
    value: 'cost',
  },
  {
    label: t('usage.cost.savings'),
    value: 'savings',
  },
  {
    label: t('usage.cost.cache'),
    value: 'cache',
  },
]))

function amountValues(amounts: (string | null)[]) {
  return zeroInactiveValues(amounts.map(amount => decimalDisplayNumber(amount)), requestActivity.value)
}

function primaryAmountSeries(label: string, amounts: (string | null)[]): CostSeries {
  return {
    label,
    color: palette.value.success,
    area: 'strong',
    values: amountValues(amounts),
    tooltipValue: index => formatUsd(amounts[index] ?? (requestActivity.value[index] === false
      ? '0'
      : null)),
  }
}

function rateSeries(label: string, color: string, values: (number | null)[]): CostSeries {
  return {
    label,
    color,
    values,
    tooltipValue: index => formatPercent(values[index] ?? null),
  }
}

const chart = computed<{ hasData: boolean, series: CostSeries[] }>(() => {
  const response = props.result
  if (props.loading || !response || response.metric !== activeView.value)
    return { hasData: false, series: [] }

  if (response.metric === 'cost') {
    const actual = response.data.points.map(({ value }) => value.estimatedCost)
    const comparison = response.data.points.map(({ value }) => value.noCacheCost)
    const hasComparison = comparison.some(amount => amount != null)
    const series = [
      primaryAmountSeries(t('usage.cost.actualCost'), actual),
    ]
    if (hasComparison) {
      series.push({
        label: t('usage.cost.withoutCacheEstimate'),
        color: palette.value.textMuted,
        values: amountValues(comparison),
        // 对比曲线允许补零，提示只展示该时间桶实际提供的估算
        tooltipValue: index => comparison[index] == null
          ? null
          : formatUsd(comparison[index]),
      })
    }
    return { hasData: hasComparison || actual.some(amount => amount != null), series }
  }
  if (response.metric === 'savings') {
    const savings = response.data.points.map(point => point.value)
    return {
      hasData: savings.some(amount => amount != null),
      series: [
        primaryAmountSeries(t('usage.cost.cacheSavings'), savings),
      ],
    }
  }

  const { points } = response.data
  return {
    hasData: points.some(({ value }) => value.inputTokens > 0 || value.cachedTokenRate > 0 || (value.cacheHitRequestRate ?? 0) > 0),
    series: [
      rateSeries(t('usage.cost.cacheTokenProportion'), palette.value.normal, points.map(({ value }) => value.cachedTokenRate)),
      rateSeries(t('usage.cost.hitRequestRate'), palette.value.success, points.map(({ value }) => value.cacheHitRequestRate)),
    ],
  }
})

const chartOption = computed<EChartsOption>(() => {
  const theme = palette.value
  const series = chart.value.series
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
    legend: usageLegend(theme, series.map(item => item.label)),
    tooltip: usageTooltip(theme, formatTooltip),
    xAxis: usageCategoryAxis(
      points.value.map(point => point.label),
      theme,
    ),
    yAxis: activeView.value === 'cache'
      ? usageValueAxis(theme, formatPercent, { min: 0, max: 1 })
      : usageValueAxis(theme, formatUsdAxis),
    series: series.flatMap(item => usageGapAwareLineSeries(item.label, item.values, item.color, { area: item.area })),
  }
})

function formatTooltip(params: unknown) {
  const index = tooltipIndex(tooltipRows(params)[0])
  const point = points.value[index]
  if (!point)
    return ''
  const lines = chart.value.series.flatMap((series) => {
    const value = series.tooltipValue(index)
    return value == null
      ? []
      : [
          usageTooltipItem(series.label, value, series.color),
        ]
  })
  return chartTooltipContent(point.label, lines)
}
</script>

<template>
  <ZCard
    as="article"
    :title="t('usage.cost.costEfficiency')"
    :description="t('usage.cost.description')"
    class="@container/cost min-h-90 xl:h-full"
  >
    <template #actions>
      <div class="flex justify-end">
        <ZSegmented v-model="activeView" :aria-label="t('usage.cost.costView')" :options="viewOptions" />
      </div>
    </template>

    <template #body>
      <div class="grid min-h-66" :class="chart.hasData ? 'gap-3' : 'flex-1'">
        <div v-if="chart.hasData && cost" class="grid grid-cols-2 gap-x-1 gap-y-3 rounded-xl bg-z-fill-quaternary/45 p-2 @min-[24rem]/cost:grid-cols-3 @min-[40rem]/cost:grid-cols-5">
          <div class="grid min-w-0 gap-1 px-1.5">
            <span class="text-[10px] font-bold text-z-text-quaternary" :title="t('usage.billing.totalCost')">{{ t('usage.cost.actualCost') }}</span>
            <strong class="wrap-anywhere font-mono text-z-sm font-heavy tabular-nums text-z-text">
              {{ formatUsd(cost.estimatedCost) }}
            </strong>
          </div>
          <div class="grid min-w-0 gap-1 px-1.5">
            <span class="text-[10px] font-bold text-z-text-quaternary" :title="t('usage.cost.cacheSavings')">{{ t('usage.cost.savings') }}</span>
            <strong class="wrap-anywhere font-mono text-z-sm font-heavy tabular-nums text-z-green-text">
              {{ formatUsd(cost.cacheSavings) }}
            </strong>
          </div>
          <div class="grid min-w-0 gap-1 px-1.5">
            <span class="text-[10px] font-bold text-z-text-quaternary" :title="t('usage.cost.tierPremium')">{{ t('usage.cost.premium') }}</span>
            <strong class="wrap-anywhere font-mono text-z-sm font-heavy tabular-nums text-z-orange-text">
              {{ formatUsd(cost.tierPremium) }}
            </strong>
          </div>
          <div class="grid min-w-0 gap-1 px-1.5">
            <span class="text-[10px] font-bold text-z-text-quaternary" :title="t('usage.cost.perSuccessfulRequestHelp')">{{ t('usage.cost.perSuccessfulRequest') }}</span>
            <strong class="wrap-anywhere font-mono text-z-sm font-heavy tabular-nums text-z-text">
              {{ formatUsd(cost.costPerSuccessfulRequest, true) }}
            </strong>
          </div>
          <div class="grid min-w-0 gap-1 px-1.5">
            <span class="text-[10px] font-bold text-z-text-quaternary" :title="t('usage.cost.costCoverage')">{{ t('usage.cost.coverage') }}</span>
            <strong class="wrap-anywhere font-mono text-z-sm font-heavy tabular-nums text-z-text">
              {{ formatPercent(coverageRate) }}
            </strong>
          </div>
        </div>
        <BaseChart v-if="chart.hasData" :option="chartOption" :height="210" />
        <ZEmpty
          v-else
          size="small"
          surface="none"
          :title="error || (loading ? t('usage.cost.loading') : t('usage.cost.empty'))"
          class="place-content-center"
        />
      </div>
    </template>
  </ZCard>
</template>
