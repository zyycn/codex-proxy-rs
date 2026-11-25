<script setup lang="ts">
import type { EChartsOption } from 'echarts'
import type { ApiKey, KeyStatistics, KeyStatisticsPeriod, KeyUsageMetric } from '@/api'
import { vLoading, ZButton, ZDialog, ZEmpty, ZSegmented } from '@codex-proxy/ui'
import { color as chartColor } from 'echarts/core'
import { computed, shallowRef, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { getKeyStatistics } from '@/api'
import BaseChart from '@/components/base-chart/index.vue'
import { chartTooltipStyle } from '@/components/base-chart/tooltip'
import KeyUsageBudget from '@/components/KeyUsageBudget.vue'
import { useChartPalette } from '@/composables/useChartPalette'
import { useRequestState } from '@/composables/useRequestState'
import { formatCompactNumber, formatInteger, formatUsd } from '@/utils/format'
import ApiKeyModelUsageTable from './ApiKeyModelUsageTable.vue'

const props = defineProps<{ apiKey: ApiKey }>()
const emit = defineEmits<{ close: [] }>()
const { t } = useI18n({ useScope: 'global' })
const open = shallowRef(true)
const trend = useTemplateRef<HTMLElement>('trend')
const period = shallowRef<KeyStatisticsPeriod>('7d')
const metric = shallowRef<KeyUsageMetric>('costUsd')
const data = shallowRef<(KeyStatistics & { keyId: string, period: KeyStatisticsPeriod, metric: KeyUsageMetric }) | null>(null)
const request = useRequestState()
const { loading, error } = request
const { palette } = useChartPalette()
const empty = computed(() => data.value?.modelCount === 0)
const displayMetric = computed(() => data.value?.metric ?? metric.value)
const metricTotal = computed(() => data.value?.totals[displayMetric.value] ?? null)
const colors = computed(() => {
  const p = palette.value
  return [
    p.info,
    p.success,
    p.reasoning,
    p.warning,
    p.danger,
    p.normal,
  ].map(tone => chartColor.lerp(0.18, [tone, p.surface]))
})
const periods = computed(() => [7, 30].map(days => ({
  value: `${days}d`,
  label: t('keys.statistics.days', { days }),
})))
const metrics = computed<{ value: KeyUsageMetric, label: string }[]>(() => [
  {
    value: 'costUsd',
    label: t('keys.statistics.costUsd'),
  },
  {
    value: 'tokens',
    label: t('keys.statistics.tokens'),
  },
  {
    value: 'requests',
    label: t('keys.statistics.requests'),
  },
])
function format(metric: KeyUsageMetric, value: number | string | null) {
  if (value === null)
    return '—'
  return metric === 'costUsd'
    ? formatUsd(value)
    : formatInteger(Number(value))
}
const axisUsd = new Intl.NumberFormat('en-US', {
  style: 'currency',
  currency: 'USD',
  minimumFractionDigits: 0,
  maximumFractionDigits: 12,
})
const chart = computed<EChartsOption>(() => ({
  animation: false,
  color: colors.value,
  grid: {
    top: 14,
    bottom: 0,
    left: 2,
    right: 0,
    outerBoundsMode: 'same',
    outerBoundsContain: 'axisLabel',
  },
  tooltip: {
    show: !empty.value && metricTotal.value !== null,
    trigger: 'axis',
    ...chartTooltipStyle(palette.value, { axisPointer: true, confine: true }),
    valueFormatter: value => format(displayMetric.value, typeof value === 'number'
      ? value
      : null),
  },
  xAxis: {
    type: 'category',
    data: data.value?.dates ?? [],
    boundaryGap: false,
    axisTick: { show: false },
    axisLine: { show: false },
    axisLabel: {
      color: palette.value.textMuted,
      fontSize: 10,
      alignMaxLabel: 'right',
      showMaxLabel: true,
      hideOverlap: true,
      formatter: (date: string) => date.slice(5),
    },
  },
  yAxis: {
    type: 'value',
    min: 0,
    max: ({ max }) => max > 0
      ? max * 1.05
      : 1,
    minInterval: displayMetric.value === 'costUsd'
      ? undefined
      : 1,
    splitNumber: 4,
    axisLabel: {
      color: palette.value.textMuted,
      fontSize: 10,
      showMaxLabel: false,
      formatter: (value: number) => displayMetric.value === 'costUsd'
        ? axisUsd.format(value)
        : formatCompactNumber(value),
    },
    splitLine: {
      lineStyle: { color: palette.value.grid, type: 'dashed', opacity: 0.65 },
    },
  },
  series: (data.value?.series ?? []).map((item, index) => {
    const points = new Map(item.points.map(point => [point.date, point]))
    return {
      id: String(index),
      name: item.model === null
        ? t('keys.statistics.other', { count: data.value!.modelCount - 5 })
        : item.model || t('keys.statistics.unknownModel'),
      type: 'line',
      stack: 'models',
      smooth: 0.5,
      symbol: 'none',
      lineStyle: {
        width: 1.2,
        opacity: 0.6,
        cap: 'round',
        join: 'round',
      },
      areaStyle: { opacity: 0.24 },
      data: data.value!.dates.map((date) => {
        const point = points.get(date)
        return point?.value === null
          ? null
          : Number(point?.value ?? 0)
      }),
    }
  }),
}))
async function load() {
  const id = request.start()
  if (data.value?.keyId !== props.apiKey.id)
    data.value = null
  try {
    const requestedPeriod = period.value
    const requestedMetric = metric.value
    const asOf = data.value?.period === requestedPeriod
      ? data.value.asOf
      : undefined
    const result = await getKeyStatistics(props.apiKey.id, requestedPeriod, requestedMetric, asOf, { signal: request.signal, silent: true })
    if (request.isCurrent(id)) {
      data.value = {
        ...result,
        keyId: props.apiKey.id,
        metric: requestedMetric,
        period: requestedPeriod,
      }
    }
  }
  catch (cause) {
    request.fail(id, cause)
  }
  finally {
    request.finish(id)
  }
}
watch([
  () => props.apiKey.id,
  period,
  metric,
], load, { immediate: true })
</script>

<template>
  <ZDialog v-model="open" :title="t('keys.statistics.heading', { name: apiKey.name })" width="70rem" @closed="emit('close')">
    <div class="grid min-w-0 gap-3">
      <div class="flex shrink-0 flex-wrap items-center justify-between gap-2">
        <ZSegmented v-model="metric" size="small" :options="metrics" :aria-label="t('keys.statistics.metric')" />
        <ZSegmented v-model="period" size="small" :options="periods" :aria-label="t('keys.statistics.period')" />
      </div>
      <div class="grid min-w-0 gap-6">
        <div v-if="error" class="flex flex-wrap items-center gap-2 text-z-sm text-z-error-text" role="alert">
          <span>{{ error }}</span>
          <span v-if="data">{{ t('keys.statistics.showingPrevious', { days: data.dates.length }) }}</span>
          <ZButton size="small" @click="trend?.focus(); load()">
            {{ t('common.actions.refresh') }}
          </ZButton>
        </div>
        <div class="grid min-w-0 items-stretch gap-y-3 lg:gap-x-8 lg:grid-cols-[minmax(0,1.55fr)_minmax(22rem,1fr)]">
          <section ref="trend" tabindex="-1" class="flex min-w-0 flex-col" :aria-label="t('keys.statistics.trend')">
            <div v-loading="{ loading, text: t('common.status.loading'), preserveContent: !!data }" class="relative flex min-h-46 min-w-0 flex-1 flex-col">
              <ZEmpty v-if="data && (empty || metricTotal === null)" class="flex-1 content-center" surface="none" :title="empty ? t('keys.statistics.empty') : t('keys.statistics.missing')" />
              <BaseChart v-else-if="data" class="grow" :option="chart" :height="184" />
            </div>
          </section>
          <section class="flex min-w-0 flex-col" :aria-label="t('usage.budget.quotaOverview')">
            <KeyUsageBudget :budget="{ ...apiKey, ...data?.budget }" compact />
          </section>
        </div>
        <ApiKeyModelUsageTable :key="apiKey.id" :key-id="apiKey.id" :statistics="data" :period="data?.period ?? period" :metric="displayMetric" :pending="loading && !data" />
      </div>
    </div>
  </ZDialog>
</template>
