<script setup lang="ts">
import type { EChartsOption } from 'echarts'
import { computed } from 'vue'

import BaseCard from '../../../components/base/BaseCard.vue'
import BaseChart from '../../../components/charts/BaseChart.vue'

const props = defineProps<{
  metric: any
}>()

const iconToneClasses: Record<string, string> = {
  normal: 'bg-(--cp-normal-bg) text-(--cp-normal)',
  info: 'bg-(--cp-info-bg) text-(--cp-info)',
  success: 'bg-(--cp-success-bg) text-(--cp-success)',
  warning: 'bg-(--cp-warning-bg) text-(--cp-warning)',
  danger: 'bg-(--cp-danger-bg) text-(--cp-danger)',
}

const detailToneClasses: Record<string, string> = {
  normal: 'text-(--cp-normal-text)',
  info: 'text-(--cp-info-text)',
  success: 'text-(--cp-success-text)',
  warning: 'text-(--cp-warning-text)',
  danger: 'text-(--cp-danger-text)',
}

const trendToneClasses: Record<string, string> = {
  normal: 'bg-(--cp-normal-text)',
  info: 'bg-(--cp-info-text)',
  success: 'bg-(--cp-success-text)',
  warning: 'bg-(--cp-warning-text)',
  danger: 'bg-(--cp-danger-text)',
}

const sparklineColors: Record<string, string> = {
  normal: '#94A3B8',
  info: '#60A5FA',
  success: '#5CCB8A',
  warning: '#E3B658',
  danger: '#E87972',
}

const sparklineOption = computed<EChartsOption | null>(() => {
  const values = (props.metric.sparkline?.values ?? []) as number[]
  if (values.length < 2) return null

  const color = sparklineColors[props.metric.sparkline?.tone ?? 'normal']
  return {
    animation: false,
    grid: { left: 0, right: 0, top: 4, bottom: 4 },
    xAxis: { type: 'category', show: false, data: values.map((_, index) => index) },
    yAxis: { type: 'value', show: false, min: 'dataMin', max: 'dataMax' },
    series: [
      {
        type: 'line',
        data: values,
        smooth: true,
        symbol: 'none',
        lineStyle: { color, width: 1.75, opacity: 0.9 },
        areaStyle: {
          color: {
            type: 'linear',
            x: 0,
            y: 0,
            x2: 0,
            y2: 1,
            colorStops: [
              { offset: 0, color: `${color}18` },
              { offset: 1, color: `${color}00` },
            ],
          },
        },
      },
    ],
    tooltip: { show: false },
  }
})
</script>

<template>
  <BaseCard
    as="article"
    :padded="false"
    radius-class="rounded-2xl"
    class="relative h-38.5 w-full px-6 pt-5"
  >
    <div class="flex items-start gap-3">
      <span
        class="inline-flex size-8.5 shrink-0 items-center justify-center rounded-[10px]"
        :class="iconToneClasses[metric.tone]"
      >
        <component :is="metric.icon" :size="18" />
      </span>
      <span class="mt-1 text-[13px] leading-[1.15] font-[650] text-(--cp-text-secondary)">{{
        metric.title
      }}</span>
    </div>

    <div class="mt-3.25 flex h-7.75 items-end gap-2">
      <strong
        class="font-mono text-[28px] leading-[1.05] font-[780] tabular-nums text-(--cp-text-primary)"
      >
        {{ metric.value }}
      </strong>
      <i
        v-if="metric.trend && metric.trend.direction !== 'flat'"
        class="mb-1.25 block size-3"
        :class="[
          trendToneClasses[metric.trend.tone],
          metric.trend.direction === 'up'
            ? '[clip-path:polygon(50%_0,100%_58%,66%_58%,66%_100%,34%_100%,34%_58%,0_58%)]'
            : '[clip-path:polygon(34%_0,66%_0,66%_42%,100%_42%,50%_100%,0_42%,34%_42%)]',
        ]"
      />
    </div>

    <div v-if="sparklineOption" class="pointer-events-none absolute top-7 right-6 h-16 w-[42%]">
      <BaseChart :option="sparklineOption" :height="64" />
    </div>

    <div
      class="mt-3 grid h-7.5 w-full grid-cols-[minmax(0,1fr)_minmax(0,1fr)] items-center rounded-[10px] bg-(--cp-bg-subtle) px-3"
    >
      <span
        class="inline-grid min-w-0 w-full grid-cols-[auto_minmax(0,1fr)] items-baseline gap-2.5"
      >
        <span class="shrink-0 text-[11px] leading-none font-[650] text-(--cp-text-muted)">{{
          metric.details[0]?.label
        }}</span>
        <b
          class="min-w-0 truncate font-mono text-xs leading-none font-bold tabular-nums"
          :class="metric.details[0]?.tone ? detailToneClasses[metric.details[0].tone] : undefined"
        >
          {{ metric.details[0]?.value }}
        </b>
      </span>
      <span
        class="inline-grid min-w-0 w-full grid-cols-[auto_minmax(0,auto)] items-baseline justify-end gap-2.5"
      >
        <span class="shrink-0 text-[11px] leading-none font-[650] text-(--cp-text-muted)">{{
          metric.details[1]?.label
        }}</span>
        <b
          class="min-w-0 truncate font-mono text-xs leading-none font-bold tabular-nums"
          :class="metric.details[1]?.tone ? detailToneClasses[metric.details[1].tone] : undefined"
        >
          {{ metric.details[1]?.value }}
        </b>
      </span>
    </div>
  </BaseCard>
</template>
