<script setup lang="ts">
import type { EChartsOption, LineSeriesOption } from 'echarts'

import type { getUsageRecordInsightsOverview } from '@/api'
import { ZCard, ZEmpty, ZSegmented } from '@codex-proxy/ui'

import { computed, shallowRef } from 'vue'
import { useI18n } from 'vue-i18n'
import BaseChart from '@/components/charts/BaseChart.vue'
import {
  requestActivityByBucket,
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
    cost: Cost
    activity: Activity
    loading?: boolean
  }>(),
  {
    loading: false,
  },
)

const { t } = useI18n({ useScope: 'global' })

type Cost = Awaited<ReturnType<typeof getUsageRecordInsightsOverview>>['cost']
type Activity = Awaited<ReturnType<typeof getUsageRecordInsightsOverview>>['health']['points']
type UsageChartPalette = ReturnType<typeof useChartPalette>['palette']['value']

const activeView = shallowRef('cost')
const { palette } = useChartPalette()
const points = computed(() => props.cost.points)
const requestActivity = computed(() => requestActivityByBucket(
  points.value.map(point => point.bucket),
  props.activity,
))
const hasNoCacheCost = computed(() =>
  points.value.some(point => point.noCacheCost != null),
)
const coverageRate = computed(() => {
  const { known, partial, unknown } = props.cost.coverage
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
  const chartPoints = points.value
  if (activeView.value === 'savings') {
    return [
      ...usageGapAwareLineSeries(
        t('usage.cost.cacheSavings'),
        zeroInactiveValues(
          chartPoints.map(point => decimalDisplayNumber(point.cacheSavings)),
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
        chartPoints.map(point => point.cachedTokenRate),
        theme.normal,
      ),
      ...usageGapAwareLineSeries(
        t('usage.cost.hitRequestRate'),
        chartPoints.map(point => point.cacheHitRequestRate),
        theme.success,
      ),
    ]
  }

  const series = [
    ...usageGapAwareLineSeries(
      t('usage.cost.actualCost'),
      zeroInactiveValues(
        chartPoints.map(point => decimalDisplayNumber(point.estimatedCost)),
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
          chartPoints.map(point => decimalDisplayNumber(point.noCacheCost)),
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
        formatUsd(costAmount(point.cacheSavings, pointIndex)),
        theme.success,
      ),
    ])
  }

  if (activeView.value === 'cache') {
    return chartTooltipContent(point.label, [
      usageTooltipItem(t('usage.cost.cacheTokenProportion'), formatPercent(point.cachedTokenRate), theme.normal),
      usageTooltipItem(t('usage.cost.hitRequestRate'), formatPercent(point.cacheHitRequestRate), theme.success),
    ])
  }

  const lines = [
    usageTooltipItem(
      t('usage.cost.actualCost'),
      formatUsd(costAmount(point.estimatedCost, pointIndex)),
      theme.success,
    ),
  ]
  if (point.noCacheCost != null) {
    lines.push(usageTooltipItem(
      t('usage.cost.withoutCacheEstimate'),
      formatUsd(costAmount(point.noCacheCost, pointIndex)),
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
      <ZSegmented v-model="activeView" :aria-label="t('usage.cost.costView')" :options="viewOptions" :disabled="loading" />
    </template>

    <template #body>
      <div class="grid min-h-66 gap-3">
        <div v-if="hasData" class="grid grid-cols-2 gap-x-1 gap-y-3 rounded-xl bg-cp-fill-quaternary/45 p-2 @min-[24rem]/cost:grid-cols-3 @min-[40rem]/cost:grid-cols-5">
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
          :title="loading ? t('usage.cost.loading') : t('usage.cost.empty')"
          class="h-52.5 place-content-center"
        />
      </div>
    </template>
  </ZCard>
</template>
