<script setup lang="ts">
import type { DashboardTrendKind, DashboardTrendResponse } from '@/api'

import { ZCard, ZEmpty, ZSegmented } from '@codex-proxy/ui'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import BaseChart from '@/components/base-chart/index.vue'
import { trendSeries } from './series'
import { useRequestTrendChart } from './useRequestTrendChart'

const props = withDefaults(defineProps<{
  trend: DashboardTrendResponse | null
  loading?: boolean
  error?: string
}>(), {
  loading: false,
  error: '',
})

const { t } = useI18n({ useScope: 'global' })

const series = computed(() => trendSeries(props.trend))
const activeKind = defineModel<DashboardTrendKind>('kind', { required: true })
const {
  tabs,
  pinnedSummaryMetric,
  hasSamples,
  chartOption,
  summaryMarkerStyle,
  toggleSummarySeries,
  isSummarySeriesActive,
} = useRequestTrendChart({
  points: computed(() => series.value.points),
  activeKind,
})

// 分段控件发出字符串，在卡片边界收窄为接口支持的指标
function selectKind(value: string) {
  if (value === 'usage' || value === 'latency' || value === 'errors')
    activeKind.value = value
}
</script>

<template>
  <ZCard as="article" :title="t('dashboard.trends.title')" class="min-h-95 w-full">
    <template #actions>
      <ZSegmented
        :model-value="activeKind"
        :aria-label="t('dashboard.trends.metric')"
        :options="tabs"
        class="w-full max-w-61.5 sm:w-61.5"
        @update:model-value="selectKind"
      />
    </template>

    <template #body>
      <div class="grid gap-3.5">
        <div class="grid h-14.25 min-w-0 grid-cols-3 gap-1.5 rounded-xl bg-z-fill-quaternary/45 p-1.5">
          <button
            v-for="item in series.summary"
            :key="item.metric"
            type="button"
            :aria-label="t('dashboard.trends.highlight', { label: item.label })"
            :aria-pressed="pinnedSummaryMetric === item.metric"
            class="group grid min-w-0 grid-cols-[8px_minmax(0,1fr)] items-center gap-x-2 rounded-lg border-0 bg-transparent px-2.5 py-2 text-left outline-none focus-visible:ring-2 focus-visible:ring-z-control-outline"
            @click="toggleSummarySeries(item.metric)"
          >
            <i
              class="size-2 rounded-full transition-transform duration-200 ease-out group-hover:scale-125 motion-reduce:transition-none"
              :style="summaryMarkerStyle(item)"
              :class="[
                isSummarySeriesActive(item.metric) ? 'scale-125' : undefined,
                {
                  'bg-z-info': item.tone === 'info',
                  'bg-z-success': item.tone === 'success',
                  'bg-z-warning': item.tone === 'warning',
                  'bg-z-error': item.tone === 'danger',
                  'bg-z-text-tertiary': item.tone === 'normal',
                },
              ]"
            />
            <span class="grid min-w-0 gap-1.5">
              <span class="truncate text-[10px] leading-none font-bold text-z-text-secondary">
                {{ item.label }}
              </span>
              <strong
                class="truncate font-mono text-z-xl leading-none font-heavy tabular-nums text-z-text"
                :title="item.value"
              >
                {{ item.value }}
              </strong>
            </span>
          </button>
        </div>

        <div class="relative h-55 w-full overflow-hidden">
          <BaseChart v-if="hasSamples" :option="chartOption" :height="220" />
          <ZEmpty
            v-else-if="!loading && !error"
            surface="none"
            :title="t('dashboard.trends.empty')"
            class="h-full place-content-center"
          />
        </div>
      </div>
    </template>
  </ZCard>
</template>
