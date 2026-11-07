<script setup lang="ts">
import type { KeyUsageMetrics } from '@/api/modules/key-usage'

import { ZCard, ZMotionIcon } from '@codex-proxy/ui'
import { Gauge, Zap } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import AnimatedMetricValue from '@/components/AnimatedMetricValue.vue'
import { formatCompactNumber, formatInteger } from '@/utils/format'
import { money } from '../utils/format'
import { keyUsageTokenMetrics, keyUsageTokenValue } from '../utils/metrics'

const props = defineProps<{ summary: KeyUsageMetrics }>()

const { t } = useI18n({ useScope: 'global' })

const hitRate = computed(() => props.summary.inputTokens > 0
  ? props.summary.cachedInputTokens / props.summary.inputTokens * 100
  : null)
const metrics = computed(() => [
  ...keyUsageTokenMetrics.value.map(metric => ({
    ...metric,
    value: keyUsageTokenValue(props.summary, metric.key),
    percentage: false,
  })),
  {
    label: t('usage.page.cacheHitRate'),
    value: hitRate.value,
    icon: Gauge,
    tone: 'text-z-success-text',
    percentage: true,
  },
].map(item => ({
  ...item,
  display: item.value === null
    ? '—'
    : item.percentage
      ? `${item.value.toFixed(1)}%`
      : formatCompactNumber(item.value),
  exact: item.value === null
    ? '—'
    : item.percentage
      ? `${item.value.toFixed(1)}%`
      : formatInteger(item.value),
})))
</script>

<template>
  <section class="grid min-w-0 gap-4 xl:grid-cols-[minmax(340px,0.95fr)_minmax(0,3fr)]" :aria-label="t('usage.summary.usageSummary')">
    <ZCard as="article" padding="compact" class="flex min-h-28 min-w-0 flex-col justify-between gap-4">
      <div class="flex min-w-0 flex-1 items-center gap-3">
        <ZMotionIcon class="inline-flex size-9 shrink-0 items-center relative -top-0.5 justify-center rounded-z-lg bg-z-info-container text-z-info-on-container" aria-hidden="true">
          <Zap :size="19" />
        </ZMotionIcon>
        <span class="sr-only">{{ t('usage.summary.consumeTokens') }}</span>
        <div class="flex min-w-0 flex-wrap items-baseline gap-x-2 gap-y-1">
          <strong class="min-w-0 font-mono text-[32px] leading-[1.05] font-heavy wrap-anywhere tabular-nums text-z-text">
            <AnimatedMetricValue :value="formatInteger(summary.totalTokens)" :raw-value="summary.totalTokens" :formatter="formatInteger" />
          </strong>
          <span class="shrink-0 font-mono text-z-sm leading-none whitespace-nowrap text-z-text-tertiary">≈ {{ formatCompactNumber(summary.totalTokens) }}</span>
        </div>
      </div>
      <dl class="m-0 flex min-h-5 flex-wrap items-baseline justify-between gap-x-6 gap-y-2">
        <div class="flex min-w-0 items-baseline gap-2.5">
          <dt class="shrink-0 text-z-sm leading-none text-z-text-secondary">
            {{ t('usage.summary.totalRequests') }}
          </dt>
          <dd class="m-0 min-w-0 font-mono text-z-lg leading-none font-bold wrap-anywhere tabular-nums text-z-text">
            {{ formatInteger(summary.requests) }}
          </dd>
        </div>
        <div class="flex min-w-0 items-baseline gap-2.5">
          <dt class="shrink-0 text-z-sm leading-none text-z-text-secondary">
            {{ t('usage.summary.totalCost') }}
          </dt>
          <dd class="m-0 min-w-0 font-mono text-z-lg leading-none font-bold wrap-anywhere tabular-nums text-z-success-text">
            {{ money(summary.costUsd) }}
          </dd>
        </div>
      </dl>
    </ZCard>
    <ZCard as="article" padding="compact" class="flex min-h-28 min-w-0 items-center" :aria-label="t('usage.summary.usageDetails')">
      <dl class="m-0 grid w-full min-w-0 grid-cols-2 gap-x-6 gap-y-5 sm:grid-cols-3 2xl:grid-cols-6">
        <div v-for="item in metrics" :key="item.label" class="min-w-0">
          <dt class="flex min-w-0 items-center gap-2 text-z-lg leading-[1.15] font-emphasis text-z-text-secondary">
            <component :is="item.icon" class="size-4.5 shrink-0" :class="item.tone" aria-hidden="true" />
            {{ item.label }}
          </dt>
          <dd class="mx-0 mt-5 mb-0 truncate font-mono text-2xl leading-[1.05] font-heavy tabular-nums text-z-text" :title="item.exact">
            {{ item.display }}
          </dd>
        </div>
      </dl>
    </ZCard>
  </section>
</template>
