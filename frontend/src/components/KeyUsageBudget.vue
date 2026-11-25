<script setup lang="ts">
import type { KeyUsageBudget } from '@/api'

import { Clock3, Gauge, Network } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { formatUsageCost } from '@/utils/format'

const props = defineProps<{ budget: KeyUsageBudget, compact?: boolean }>()

const { t } = useI18n({ useScope: 'global' })

const windows = computed(() => [
  {
    label: t('usage.budget.todaySQuota'),
    limit: props.budget.dailyLimitUsd,
    used: props.budget.dailyUsedUsd,
    reset: props.budget.dailyResetsAt,
    resetDisplay: props.budget.dailyResetsAtDisplay,
  },
  {
    label: t('usage.budget.weeklyQuota'),
    limit: props.budget.weeklyLimitUsd,
    used: props.budget.weeklyUsedUsd,
    reset: props.budget.weeklyResetsAt,
    resetDisplay: props.budget.weeklyResetsAtDisplay,
  },
].map(window => ({
  ...window,
  limited: Number(window.limit) > 0,
  remaining: Math.max(0, Number(window.limit) - Number(window.used)),
  percentage: Number(window.limit) > 0
    ? Number(window.used) / Number(window.limit) * 100
    : 0,
})))
</script>

<template>
  <div class="flex flex-1 flex-col justify-between gap-6">
    <div class="grid flex-1 gap-6 sm:grid-cols-2">
      <div v-for="window in windows" :key="window.label" class="flex min-w-0 flex-col justify-between gap-4">
        <div>
          <div class="text-z-sm text-z-text-secondary">
            {{ window.label }}
          </div>
          <div class="mt-3 flex flex-wrap items-baseline gap-1.5">
            <strong class="font-mono text-2xl text-z-text">{{ window.limited ? formatUsageCost(window.remaining) : t('usage.budget.noLimit') }}</strong>
            <span v-if="window.limited" class="text-z-xs text-z-text-tertiary">{{ t('usage.budget.remaining', { value: formatUsageCost(window.limit) }) }}</span>
          </div>
        </div>
        <div>
          <div class="grid grid-cols-20 gap-0.75" :aria-label="window.limited ? t('usage.budget.used', { rate: window.percentage.toFixed(1) }) : t('usage.budget.noLimit')">
            <span v-for="block in 20" :key="block" class="h-7 rounded-xs" :class="block <= Math.ceil(Math.min(100, window.percentage) / 5) ? (window.percentage >= 100 ? 'bg-z-error' : 'bg-z-success') : 'bg-z-fill-secondary'" />
          </div>
          <div class="mt-2 flex justify-between gap-2 font-mono text-z-xs text-z-text-secondary">
            <span>{{ t('usage.budget.usedAmount', { value: formatUsageCost(window.used) }) }}</span><span v-if="window.limited">{{ window.percentage.toFixed(1) }}%</span>
          </div>
        </div>
        <div class="text-z-xs leading-relaxed text-z-text-tertiary">
          <span class="flex items-center gap-1.5"><Clock3 class="size-3" />{{ t('usage.budget.resetTime') }}</span>
          <span class="mt-1 block font-mono">{{ window.reset ? window.resetDisplay ?? '—' : t('usage.budget.timingStartsAfterFirstUse') }}</span>
        </div>
      </div>
    </div>
    <div class="grid grid-cols-2 gap-3">
      <div class="flex items-center justify-between gap-2 text-z-xs text-z-text-secondary" :class="{ 'rounded-z bg-z-fill-quaternary p-3': !compact }">
        <span class="flex items-center gap-1.5 leading-none"><Network class="size-3.5 shrink-0 -translate-y-px" />{{ t('usage.budget.concurrencyLimit') }}</span><strong class="font-mono">{{ budget.maxConcurrency || '∞' }}</strong>
      </div>
      <div class="flex items-center justify-between gap-2 text-z-xs text-z-text-secondary" :class="{ 'rounded-z bg-z-fill-quaternary p-3': !compact }">
        <span class="flex items-center gap-1.5 leading-none"><Gauge class="size-3.5 shrink-0 -translate-y-px" />{{ t('usage.budget.requestsPerMinute') }}</span><strong class="font-mono">{{ budget.requestsPerMinute || '∞' }}</strong>
      </div>
    </div>
  </div>
</template>
