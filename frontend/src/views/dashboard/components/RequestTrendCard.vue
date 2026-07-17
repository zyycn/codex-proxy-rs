<script setup lang="ts">
import type { dashboardTrendView, normalizeDashboardTrendKind } from '../presenter'

import { toRef } from 'vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseChart from '@/components/charts/BaseChart.vue'
import { useRequestTrendChart } from '../composables/useRequestTrendChart'

type TrendKind = ReturnType<typeof normalizeDashboardTrendKind>
type TrendView = ReturnType<typeof dashboardTrendView>

const props = defineProps<{
  points: TrendView['points']
  summary: TrendView['summary']
  loading?: boolean
}>()

const emit = defineEmits<{
  trendChange: [kind: TrendKind]
}>()

const activeKind = defineModel<TrendKind>('kind', { required: true })
const {
  tabs,
  pinnedSummaryLabel,
  hasSamples,
  chartOption,
  summaryMarkerStyle,
  handleTrendChange,
  toggleSummarySeries,
  isSummarySeriesActive,
} = useRequestTrendChart({
  points: toRef(props, 'points'),
  summary: toRef(props, 'summary'),
  activeKind,
  onTrendChange: kind => emit('trendChange', kind),
})
</script>

<template>
  <BaseCard as="article" variant="dashboard" title="使用趋势" class="min-h-95 w-full">
    <template #actions>
      <BaseSegmented
        v-model="activeKind"
        :options="tabs"
        class="w-full max-w-61.5 sm:w-61.5"
        @update:model-value="handleTrendChange"
      />
    </template>

    <template #body>
      <div class="mt-4.5 grid gap-3.5">
        <div
          class="grid h-14.25 min-w-0 grid-cols-3 gap-1.5 rounded-xl bg-(--cp-bg-subtle)/45 p-1.5"
        >
          <button
            v-for="item in props.summary"
            :key="item.label"
            type="button"
            :aria-label="`突出显示${item.label}曲线`"
            :aria-pressed="pinnedSummaryLabel === item.label"
            class="group grid min-w-0 grid-cols-[8px_minmax(0,1fr)] items-center gap-x-2 rounded-lg border-0 bg-transparent px-2.5 py-2 text-left outline-none focus-visible:ring-2 focus-visible:ring-(--cp-info-border)"
            @click="toggleSummarySeries(item.label)"
          >
            <i
              aria-hidden="true"
              class="size-2 rounded-full transition-transform duration-200 ease-out group-hover:scale-125 motion-reduce:transition-none"
              :style="summaryMarkerStyle(item)"
              :class="[
                isSummarySeriesActive(item.label) ? 'scale-125' : undefined,
                {
                  'bg-(--cp-info)': item.tone === 'info',
                  'bg-(--cp-success)': item.tone === 'success',
                  'bg-(--cp-warning)': item.tone === 'warning',
                  'bg-(--cp-danger)': item.tone === 'danger',
                  'bg-(--cp-normal)': item.tone === 'normal',
                },
              ]"
            />
            <span class="grid min-w-0 gap-1">
              <span class="truncate text-[10px] leading-none font-[680] text-(--cp-text-secondary)">
                {{ item.label }}
              </span>
              <strong
                class="truncate font-mono text-[15px] leading-none font-[760] tabular-nums text-(--cp-text-primary)"
                :title="item.value"
              >
                {{ item.value }}
              </strong>
            </span>
          </button>
        </div>

        <div class="relative h-55 w-full overflow-hidden">
          <BaseChart v-if="hasSamples" :option="chartOption" :height="220" />
          <BaseEmpty
            v-if="!hasSamples"
            compact
            title="暂无趋势数据"
            description="当日暂无请求日志"
            class="h-full place-content-center bg-transparent"
          />
        </div>
      </div>
    </template>
  </BaseCard>
</template>
