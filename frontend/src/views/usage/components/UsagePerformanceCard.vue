<script setup lang="ts">
import type { EChartsOption } from 'echarts'

import type { UsageMetricResponse, UsageOverviewLoad, UsagePerformanceMetric } from '@/api'
import { ZCard, ZEmpty, ZSegmented } from '@codex-proxy/ui'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import BaseChart from '@/components/charts/BaseChart.vue'
import { zeroInactiveValues } from '@/components/charts/timeSeriesGap'
import { chartTooltipContent, tooltipIndex, tooltipRows } from '@/components/charts/tooltip'
import { useChartPalette } from '@/composables/useChartPalette'
import { formatLocalizedCompactNumber as formatCompactNumber, formatDuration } from '@/utils/format'

import {
  formatDurationAxis,
  usageCategoryAxis,
  usageGapAwareLineSeries,
  usageLegend,
  usageTooltip,
  usageTooltipItem,
  usageValueAxis,
} from '../utils/chart'

const props = withDefaults(
  defineProps<{
    performance: UsageMetricResponse<UsagePerformanceMetric> | null
    error?: string
    loading?: boolean
  }>(),
  {
    loading: false,
  },
)

const { t } = useI18n({ useScope: 'global' })

interface PerformanceSeries {
  field: string
  summary: number | null
  values: (number | null)[]
  label: string
  color: string
  yAxisIndex: number
  format: (value: number | null) => string
}

const activeView = defineModel<UsagePerformanceMetric>({ required: true })
const { palette } = useChartPalette()
const performancePoints = computed(() => props.performance?.data.points ?? [])
const requestActivity = computed(() => performancePoints.value.map(point => point.requestCount > 0))

const viewOptions = computed(() => ([
  {
    label: t('usage.latency.totalLatency'),
    value: 'latency',
  },
  {
    label: t('usage.ttft'),
    value: 'firstToken',
  },
  {
    label: t('usage.outputThroughput'),
    value: 'throughput',
  },
  {
    label: t('usage.performance.load'),
    value: 'load',
  },
]))

const seriesDefinitions = computed<PerformanceSeries[]>(() => {
  const response = props.performance
  if (!response || response.metric !== activeView.value)
    return []
  const theme = palette.value
  if (response.metric === 'load') {
    const fields: { field: keyof UsageOverviewLoad, label: string, color: string, yAxisIndex: number, format: (value: number | null) => string }[] = [
      {
        field: 'averageConcurrency',
        label: t('usage.performance.averageConcurrency'),
        color: theme.info,
        yAxisIndex: 0,
        format: formatConcurrency,
      },
      {
        field: 'peakConcurrency',
        label: t('usage.performance.peakConcurrency'),
        color: theme.warning,
        yAxisIndex: 0,
        format: formatConcurrency,
      },
      {
        field: 'admissionDecisionP95Ms',
        label: t('usage.performance.admissionP95'),
        color: theme.success,
        yAxisIndex: 1,
        format: formatDuration,
      },
      {
        field: 'accountSelectionWaitP95Ms',
        label: t('usage.performance.accountSelectionP95'),
        color: theme.reasoning,
        yAxisIndex: 1,
        format: formatDuration,
      },
    ]
    return fields.map(item => ({
      ...item,
      summary: response.data.summary[item.field],
      values: response.data.points.map(point => point.value[item.field]),
    }))
  }
  const colors = [theme.info, theme.warning, theme.danger]
  if (response.metric === 'throughput') {
    return (['p10', 'p50', 'p90'] as const).map((field, index) => ({
      field,
      summary: response.data.summary[field],
      values: response.data.points.map(point => point.value[field]),
      label: field.toUpperCase(),
      color: colors[index]!,
      yAxisIndex: 0,
      format: formatThroughput,
    }))
  }
  return (['p50Ms', 'p95Ms', 'p99Ms'] as const).map((field, index) => ({
    field,
    summary: response.data.summary[field],
    values: response.data.points.map(point => point.value[field]),
    label: ['P50', 'P95', 'P99'][index]!,
    color: colors[index]!,
    yAxisIndex: 0,
    format: formatDuration,
  }))
})

const selectedSeries = computed(() => seriesDefinitions.value.map(series => ({
  ...series,
  // 并发包含之前时间桶开始的请求，不按本桶新请求数补零
  values: series.field === 'averageConcurrency' || series.field === 'peakConcurrency'
    ? series.values
    : zeroInactiveValues(series.values, requestActivity.value),
})))

const summaryMetrics = computed(() => seriesDefinitions.value.map(series => ({
  label: series.label,
  value: series.format(series.summary),
})))

const hasData = computed(() => !props.loading
  && seriesDefinitions.value.some(series => series.values.some(value => value != null)))

const chartOption = computed<EChartsOption>(() => {
  const theme = palette.value
  const load = activeView.value === 'load'
  const series = selectedSeries.value.flatMap((item, index) => usageGapAwareLineSeries(
    item.label,
    item.values,
    item.color,
    {
      yAxisIndex: item.yAxisIndex,
      area: index === 0
        ? 'subtle'
        : false,
    },
  ))

  return {
    animationDuration: 240,
    grid: {
      left: 0,
      right: 0,
      top: load ? 60 : 40,
      bottom: 0,
      outerBoundsMode: 'same',
      outerBoundsContain: 'axisLabel',
    },
    legend: usageLegend(theme, selectedSeries.value.map(series => series.label)),
    tooltip: usageTooltip(theme, formatTooltip),
    xAxis: usageCategoryAxis(performancePoints.value.map(point => point.label), theme),
    yAxis: load
      ? [
          {
            ...usageValueAxis(theme, formatCompactNumber, { min: 0 }),
            name: t('usage.performance.concurrency'),
            nameTextStyle: { color: theme.textMuted, fontSize: 10, align: 'left' },
          },
          {
            ...usageValueAxis(theme, formatDurationAxis, { min: 0, splitLine: false }),
            name: t('usage.performance.waitTime'),
            nameTextStyle: { color: theme.textMuted, fontSize: 10, align: 'right' },
          },
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

function formatTooltip(params: unknown) {
  const rows = tooltipRows(params)
  const index = tooltipIndex(rows[0])
  const point = performancePoints.value[index]
  if (!point)
    return ''

  return chartTooltipContent(point.label, selectedSeries.value.map(series => usageTooltipItem(
    series.label,
    series.format(series.values[index] ?? null),
    series.color,
  )))
}

function formatConcurrency(value: number | null) {
  return value == null
    ? '—'
    : formatCompactNumber(value)
}

function formatThroughput(value: number | null) {
  return value == null
    ? '—'
    : `${formatCompactNumber(value)} tok/s`
}
</script>

<template>
  <ZCard
    as="article"
    :title="t('usage.performance.title')"
    :description="t('usage.performance.description')"
    class="min-h-90 xl:h-full"
  >
    <template #actions>
      <div class="flex justify-end">
        <ZSegmented v-model="activeView" :aria-label="t('usage.performance.performanceIndicators')" :options="viewOptions" />
      </div>
    </template>

    <template #body>
      <div class="grid min-h-66" :class="hasData ? 'gap-3' : 'flex-1'">
        <div
          v-if="hasData"
          class="grid gap-2 rounded-xl bg-cp-fill-quaternary/45 p-2"
          :class="activeView === 'load' ? 'grid-cols-2 sm:grid-cols-4' : 'grid-cols-3'"
        >
          <div v-for="metric in summaryMetrics" :key="metric.label" class="grid min-w-0 gap-1 px-2">
            <span class="truncate text-[10px] font-bold text-cp-text-quaternary" :title="metric.label">{{ metric.label }}</span>
            <strong class="truncate font-mono text-cp-base font-heavy tabular-nums text-cp-text" :title="metric.value">
              {{ metric.value }}
            </strong>
          </div>
        </div>
        <BaseChart v-if="hasData" :option="chartOption" :height="210" />
        <ZEmpty
          v-else
          size="small"
          surface="none"
          :title="
            error || (loading
              ? t('usage.performance.loading')
              : activeView === 'load'
                ? t('usage.performance.noLoad')
                : activeView === 'firstToken'
                  ? t('usage.performance.noTtftSamples')
                  : activeView === 'throughput'
                    ? t('usage.performance.noThroughput')
                    : t('usage.performance.noLatency'))
          "
          class="place-content-center"
        />
      </div>
    </template>
  </ZCard>
</template>
