<script setup lang="ts">
import type { EChartsOption, LineSeriesOption } from 'echarts'

import type { UsageCostMetric, UsageMetricResponse } from '@/api'
import { ZCard, ZEmpty, ZSegmented } from '@codex-proxy/ui'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import BaseChart from '@/components/charts/BaseChart.vue'
import {
  zeroInactiveValues,
} from '@/components/charts/timeSeriesGap'
import { chartTooltipContent, tooltipIndex, tooltipRows } from '@/components/charts/tooltip'
import { useChartPalette } from '@/composables/useChartPalette'
import { decimalDisplayNumber, formatLocalizedCompactNumber as formatCompactNumber, formatPercent, formatUsd } from '@/utils/format'

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

type UsageChartPalette = ReturnType<typeof useChartPalette>['palette']['value']

const activeView = defineModel<UsageCostMetric>({ required: true })
const { palette } = useChartPalette()
const cost = computed(() => props.result?.data.summary ?? null)
const points = computed(() => props.result?.data.points ?? [])
const costPoints = computed(() => props.result?.metric === 'cost'
  ? props.result.data.points
  : [])
const savingsPoints = computed(() => props.result?.metric === 'savings'
  ? props.result.data.points
  : [])
const cachePoints = computed(() => props.result?.metric === 'cache'
  ? props.result.data.points
  : [])
const requestActivity = computed(() => points.value.map(point => point.requestCount > 0))
const hasNoCacheCost = computed(() => costPoints.value.some(point => point.value.noCacheCost != null))
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

const hasData = computed(() => {
  if (props.loading || props.result?.metric !== activeView.value)
    return false
  if (activeView.value === 'cost')
    return costPoints.value.some(point => point.value.estimatedCost != null || point.value.noCacheCost != null)
  if (activeView.value === 'savings')
    return savingsPoints.value.some(point => point.value != null)
  return cachePoints.value.some(({ value }) => value.inputTokens > 0 || value.cachedTokenRate > 0 || (value.cacheHitRequestRate ?? 0) > 0)
})

const chartOption = computed<EChartsOption>(() => {
  const theme = palette.value
  const legend = legendNames()
  const series = chartSeries(theme)

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
      min: activeView.value === 'cache'
        ? 0
        : undefined,
      max: activeView.value === 'cache'
        ? 1
        : undefined,
    }),
    series,
  }
})

function legendNames() {
  if (activeView.value === 'savings') {
    return [
      t('usage.cost.cacheSavings'),
    ]
  }
  if (activeView.value === 'cache') {
    return [
      t('usage.cost.cacheTokenProportion'),
      t('usage.cost.hitRequestRate'),
    ]
  }
  return hasNoCacheCost.value
    ? [
        t('usage.cost.actualCost'),
        t('usage.cost.withoutCacheEstimate'),
      ]
    : [
        t('usage.cost.actualCost'),
      ]
}

function axisFormatter() {
  if (activeView.value === 'cost' || activeView.value === 'savings')
    return formatUsdAxis
  if (activeView.value === 'cache')
    return (value: number) => formatPercent(value)
  return (value: number) => formatCompactNumber(value)
}

function chartSeries(theme: UsageChartPalette): LineSeriesOption[] {
  if (activeView.value === 'savings') {
    return [
      ...usageGapAwareLineSeries(
        t('usage.cost.cacheSavings'),
        zeroInactiveValues(
          savingsPoints.value.map(point => decimalDisplayNumber(point.value)),
          requestActivity.value,
        ),
        theme.success,
        { area: 'strong' },
      ),
    ]
  }

  if (activeView.value === 'cache') {
    return [
      ...usageGapAwareLineSeries(
        t('usage.cost.cacheTokenProportion'),
        cachePoints.value.map(point => point.value.cachedTokenRate),
        theme.normal,
      ),
      ...usageGapAwareLineSeries(
        t('usage.cost.hitRequestRate'),
        cachePoints.value.map(point => point.value.cacheHitRequestRate),
        theme.success,
      ),
    ]
  }

  const series = [
    ...usageGapAwareLineSeries(
      t('usage.cost.actualCost'),
      zeroInactiveValues(
        costPoints.value.map(point => decimalDisplayNumber(point.value.estimatedCost)),
        requestActivity.value,
      ),
      theme.success,
      { area: 'strong' },
    ),
  ]
  if (hasNoCacheCost.value) {
    series.push(
      ...usageGapAwareLineSeries(
        t('usage.cost.withoutCacheEstimate'),
        zeroInactiveValues(
          costPoints.value.map(point => decimalDisplayNumber(point.value.noCacheCost)),
          requestActivity.value,
        ),
        theme.textMuted,
      ),
    )
  }
  return series
}

function formatTooltip(params: unknown) {
  const rows = tooltipRows(params)
  const pointIndex = tooltipIndex(rows[0])
  const point = points.value[pointIndex]
  if (!point)
    return ''
  const theme = palette.value

  if (activeView.value === 'savings') {
    return chartTooltipContent(point.label, [
      usageTooltipItem(
        t('usage.cost.cacheSavings'),
        formatUsd(costAmount(savingsPoints.value[pointIndex]?.value ?? null, pointIndex)),
        theme.success,
      ),
    ])
  }

  if (activeView.value === 'cache') {
    return chartTooltipContent(point.label, [
      usageTooltipItem(t('usage.cost.cacheTokenProportion'), formatPercent(cachePoints.value[pointIndex]?.value.cachedTokenRate ?? null), theme.normal),
      usageTooltipItem(t('usage.cost.hitRequestRate'), formatPercent(cachePoints.value[pointIndex]?.value.cacheHitRequestRate ?? null), theme.success),
    ])
  }

  const lines = [
    usageTooltipItem(
      t('usage.cost.actualCost'),
      formatUsd(costAmount(costPoints.value[pointIndex]?.value.estimatedCost ?? null, pointIndex)),
      theme.success,
    ),
  ]
  if (costPoints.value[pointIndex]?.value.noCacheCost != null) {
    lines.push(usageTooltipItem(
      t('usage.cost.withoutCacheEstimate'),
      formatUsd(costAmount(costPoints.value[pointIndex]?.value.noCacheCost ?? null, pointIndex)),
      theme.textMuted,
    ))
  }
  return chartTooltipContent(point.label, lines)
}

function costAmount(value: string | null, pointIndex: number) {
  return value ?? (requestActivity.value[pointIndex] === false
    ? '0'
    : null)
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
      <div class="grid min-h-66" :class="hasData ? 'gap-3' : 'flex-1'">
        <div v-if="hasData && cost" class="grid grid-cols-2 gap-x-1 gap-y-3 rounded-xl bg-cp-fill-quaternary/45 p-2 @min-[24rem]/cost:grid-cols-3 @min-[40rem]/cost:grid-cols-5">
          <div class="grid min-w-0 gap-1 px-1.5">
            <span class="text-[10px] font-bold text-cp-text-quaternary" :title="t('usage.billing.totalCost')">{{ t('usage.cost.actualCost') }}</span>
            <strong class="wrap-anywhere font-mono text-cp-sm font-heavy tabular-nums text-cp-text" :title="formatUsd(cost.estimatedCost)">
              {{ formatUsd(cost.estimatedCost) }}
            </strong>
          </div>
          <div class="grid min-w-0 gap-1 px-1.5">
            <span class="text-[10px] font-bold text-cp-text-quaternary" :title="t('usage.cost.cacheSavings')">{{ t('usage.cost.savings') }}</span>
            <strong class="wrap-anywhere font-mono text-cp-sm font-heavy tabular-nums text-cp-green-text" :title="formatUsd(cost.cacheSavings)">
              {{ formatUsd(cost.cacheSavings) }}
            </strong>
          </div>
          <div class="grid min-w-0 gap-1 px-1.5">
            <span class="text-[10px] font-bold text-cp-text-quaternary" :title="t('usage.cost.tierPremium')">{{ t('usage.cost.premium') }}</span>
            <strong class="wrap-anywhere font-mono text-cp-sm font-heavy tabular-nums text-cp-orange-text" :title="formatUsd(cost.tierPremium)">
              {{ formatUsd(cost.tierPremium) }}
            </strong>
          </div>
          <div class="grid min-w-0 gap-1 px-1.5">
            <span class="text-[10px] font-bold text-cp-text-quaternary" :title="t('usage.cost.perSuccessfulRequestHelp')">{{ t('usage.cost.perSuccessfulRequest') }}</span>
            <strong class="wrap-anywhere font-mono text-cp-sm font-heavy tabular-nums text-cp-text" :title="formatUsd(cost.costPerSuccessfulRequest, true)">
              {{ formatUsd(cost.costPerSuccessfulRequest, true) }}
            </strong>
          </div>
          <div class="grid min-w-0 gap-1 px-1.5">
            <span class="text-[10px] font-bold text-cp-text-quaternary" :title="t('usage.cost.costCoverage')">{{ t('usage.cost.coverage') }}</span>
            <strong class="wrap-anywhere font-mono text-cp-sm font-heavy tabular-nums text-cp-text">
              {{ formatPercent(coverageRate) }}
            </strong>
          </div>
        </div>
        <BaseChart v-if="hasData" :option="chartOption" :height="210" />
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
