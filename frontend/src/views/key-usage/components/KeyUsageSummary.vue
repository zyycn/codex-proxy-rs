<script setup lang="ts">
import type { KeyUsageMetrics } from '@/api/modules/key-usage'
import { Gauge, Zap } from '@lucide/vue'
import { computed } from 'vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseMotionIcon from '@/components/base/BaseMotionIcon.vue'
import { formatCompactNumber, formatInteger } from '@/utils/number'
import AnimatedMetricValue from '@/views/dashboard/components/AnimatedMetricValue.vue'
import { money } from '../utils/format'
import { keyUsageTokenMetrics, keyUsageTokenValue } from '../utils/metrics'

const props = defineProps<{ summary: KeyUsageMetrics }>()
const hitRate = computed(() => props.summary.inputTokens > 0 ? props.summary.cachedTokens / props.summary.inputTokens * 100 : null)
const metrics = computed(() => [
  ...keyUsageTokenMetrics.map(metric => ({ ...metric, value: keyUsageTokenValue(props.summary, metric.key), percentage: false })),
  { label: '缓存命中率', value: hitRate.value, icon: Gauge, tone: 'text-cp-success-text', percentage: true },
].map(item => ({
  ...item,
  display: item.value === null ? '—' : item.percentage ? `${item.value.toFixed(1)}%` : formatCompactNumber(item.value),
  exact: item.value === null ? '—' : item.percentage ? `${item.value.toFixed(1)}%` : formatInteger(item.value),
})))
</script>

<template>
  <section class="grid min-w-0 gap-4 xl:grid-cols-[minmax(340px,0.95fr)_minmax(0,3fr)]" aria-label="用量汇总">
    <BaseCard as="article" padding="compact" class="flex min-h-28 min-w-0 flex-col justify-between gap-4">
      <div class="flex min-w-0 flex-1 items-center gap-3">
        <BaseMotionIcon class="inline-flex size-8.5 shrink-0 items-center justify-center rounded-cp-lg bg-cp-info-container text-cp-info-on-container" aria-hidden="true">
          <Zap :size="18" />
        </BaseMotionIcon>
        <span class="sr-only">消耗 Tokens</span>
        <div class="flex min-w-0 flex-wrap items-baseline gap-x-2 gap-y-1">
          <strong class="min-w-0 font-mono text-[32px] leading-[1.05] font-heavy wrap-anywhere tabular-nums text-cp-text" :title="formatCompactNumber(summary.totalTokens)">
            <AnimatedMetricValue :value="formatInteger(summary.totalTokens)" :raw-value="summary.totalTokens" :formatter="formatInteger" />
          </strong>
          <span class="shrink-0 font-mono text-cp-sm leading-none whitespace-nowrap text-cp-text-tertiary">≈ {{ formatCompactNumber(summary.totalTokens) }}</span>
        </div>
      </div>
      <dl class="m-0 flex min-h-5 flex-wrap items-baseline justify-between gap-x-6 gap-y-2">
        <div class="flex min-w-0 items-baseline gap-2.5">
          <dt class="shrink-0 text-cp-sm leading-none text-cp-text-secondary">
            总请求
          </dt>
          <dd class="m-0 min-w-0 font-mono text-cp-lg leading-none font-bold wrap-anywhere tabular-nums text-cp-text">
            {{ formatInteger(summary.requests) }}
          </dd>
        </div>
        <div class="flex min-w-0 items-baseline gap-2.5">
          <dt class="shrink-0 text-cp-sm leading-none text-cp-text-secondary">
            总成本
          </dt>
          <dd class="m-0 min-w-0 font-mono text-cp-lg leading-none font-bold wrap-anywhere tabular-nums text-cp-success-text">
            {{ money(summary.costUsd) }}
          </dd>
        </div>
      </dl>
    </BaseCard>
    <BaseCard as="article" padding="compact" class="flex min-h-28 min-w-0 items-center" aria-label="用量明细">
      <dl class="m-0 grid w-full min-w-0 grid-cols-2 gap-x-6 gap-y-5 sm:grid-cols-3 2xl:grid-cols-6">
        <div v-for="item in metrics" :key="item.label" class="min-w-0">
          <dt class="flex min-w-0 items-center gap-2 text-cp-lg leading-[1.15] font-emphasis text-cp-text-secondary">
            <component :is="item.icon" class="size-4.5 shrink-0" :class="item.tone" aria-hidden="true" />
            {{ item.label }}
          </dt>
          <dd class="mx-0 mt-5 mb-0 truncate font-mono text-2xl leading-[1.05] font-heavy tabular-nums text-cp-text" :title="item.exact">
            {{ item.display }}
          </dd>
        </div>
      </dl>
    </BaseCard>
  </section>
</template>
