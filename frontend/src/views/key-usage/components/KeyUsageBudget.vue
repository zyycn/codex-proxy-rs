<script setup lang="ts">
import type { KeyUsageBudget } from '@/api/modules/key-usage'

import { ZCard } from '@codex-proxy/ui'
import { Clock3, Gauge, Network } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { money } from '../utils/format'

const props = defineProps<{ budget: KeyUsageBudget }>()

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
  <ZCard :title="t('usage.budget.quotaOverview')">
    <div class="flex flex-1 flex-col justify-between gap-6">
      <div class="grid flex-1 gap-6 sm:grid-cols-2">
        <div v-for="window in windows" :key="window.label" class="flex min-w-0 flex-col justify-between gap-4">
          <div>
            <div class="text-cp-sm text-cp-text-secondary">
              {{ window.label }}
            </div>
            <div class="mt-3 flex flex-wrap items-baseline gap-1.5">
              <strong class="font-mono text-2xl text-cp-text">{{ window.limited ? money(window.remaining) : t('usage.budget.noLimit') }}</strong>
              <span v-if="window.limited" class="text-cp-xs text-cp-text-tertiary">{{ t('usage.budget.remaining', { value: money(window.limit) }) }}</span>
            </div>
          </div>
          <div>
            <div class="grid grid-cols-20 gap-0.75" :aria-label="window.limited ? t('usage.budget.used', { rate: window.percentage.toFixed(1) }) : t('usage.budget.noLimit')">
              <span v-for="block in 20" :key="block" class="h-7 rounded-xs" :class="block <= Math.ceil(Math.min(100, window.percentage) / 5) ? (window.percentage >= 100 ? 'bg-cp-error' : 'bg-cp-success') : 'bg-cp-fill-secondary'" />
            </div>
            <div class="mt-2 flex justify-between gap-2 font-mono text-cp-xs text-cp-text-secondary">
              <span>{{ t('usage.budget.usedAmount', { value: money(window.used) }) }}</span><span v-if="window.limited">{{ window.percentage.toFixed(1) }}%</span>
            </div>
          </div>
          <div class="text-cp-xs leading-relaxed text-cp-text-tertiary">
            <span class="flex items-center gap-1.5"><Clock3 class="size-3" />{{ t('usage.budget.resetTime') }}</span>
            <span class="mt-1 block font-mono">{{ window.reset ? window.resetDisplay ?? '—' : t('usage.budget.timingStartsAfterFirstUse') }}</span>
          </div>
        </div>
      </div>
      <div class="grid grid-cols-2 gap-3">
        <div class="flex items-center justify-between gap-2 rounded-cp bg-cp-fill-quaternary p-3 text-cp-xs text-cp-text-secondary">
          <span class="flex items-center gap-1.5 leading-none"><Network class="size-3.5 shrink-0 -translate-y-px" />{{ t('usage.budget.concurrencyLimit') }}</span><strong class="font-mono">{{ budget.maxConcurrency || '∞' }}</strong>
        </div>
        <div class="flex items-center justify-between gap-2 rounded-cp bg-cp-fill-quaternary p-3 text-cp-xs text-cp-text-secondary">
          <span class="flex items-center gap-1.5 leading-none"><Gauge class="size-3.5 shrink-0 -translate-y-px" />{{ t('usage.budget.requestsPerMinute') }}</span><strong class="font-mono">{{ budget.requestsPerMinute || '∞' }}</strong>
        </div>
      </div>
    </div>
  </ZCard>
</template>
