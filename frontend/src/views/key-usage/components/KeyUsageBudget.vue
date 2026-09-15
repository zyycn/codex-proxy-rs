<script setup lang="ts">
import type { KeyUsageBudget } from '@/api/modules/key-usage'
import { Clock3, Gauge, Network } from '@lucide/vue'
import { computed } from 'vue'
import BaseCard from '@/components/base/BaseCard.vue'
import { keyUsageTime, money } from '../utils/format'

const props = defineProps<{ budget: KeyUsageBudget }>()
const windows = computed(() => [
  { label: '今日额度', limit: props.budget.dailyLimitUsd, used: props.budget.dailyUsedUsd, reset: props.budget.dailyResetsAt },
  { label: '七日额度', limit: props.budget.weeklyLimitUsd, used: props.budget.weeklyUsedUsd, reset: props.budget.weeklyResetsAt },
].map(window => ({
  ...window,
  limited: Number(window.limit) > 0,
  remaining: Math.max(0, Number(window.limit) - Number(window.used)),
  percentage: Number(window.limit) > 0 ? Number(window.used) / Number(window.limit) * 100 : 0,
})))
</script>

<template>
  <BaseCard title="额度概览">
    <div class="flex flex-1 flex-col justify-between gap-6">
      <div class="grid flex-1 gap-6 sm:grid-cols-2">
        <div v-for="window in windows" :key="window.label" class="flex min-w-0 flex-col justify-between gap-4">
          <div>
            <div class="text-cp-sm text-cp-text-secondary">
              {{ window.label }}
            </div>
            <div class="mt-3 flex flex-wrap items-baseline gap-1.5">
              <strong class="font-mono text-2xl text-cp-text">{{ window.limited ? money(window.remaining) : '不限额' }}</strong>
              <span v-if="window.limited" class="text-cp-xs text-cp-text-tertiary">剩余 / {{ money(window.limit) }}</span>
            </div>
          </div>
          <div>
            <div class="grid grid-cols-[repeat(20,minmax(0,1fr))] gap-0.75" :aria-label="window.limited ? `已用 ${window.percentage.toFixed(1)}%` : '不限额'">
              <span v-for="block in 20" :key="block" class="h-5 rounded-xs" :class="block <= Math.ceil(Math.min(100, window.percentage) / 5) ? (window.percentage >= 100 ? 'bg-cp-error' : 'bg-cp-success') : 'bg-cp-fill-secondary'" />
            </div>
            <div class="mt-2 flex justify-between gap-2 font-mono text-cp-xs text-cp-text-secondary">
              <span>已用 {{ money(window.used) }}</span><span v-if="window.limited">{{ window.percentage.toFixed(1) }}%</span>
            </div>
          </div>
          <div class="text-cp-xs leading-relaxed text-cp-text-tertiary">
            <span class="flex items-center gap-1.5"><Clock3 class="size-3" />重置时间</span>
            <span class="mt-1 block font-mono">{{ window.reset ? keyUsageTime(window.reset) : '首次使用后开始计时' }}</span>
          </div>
        </div>
      </div>
      <div class="grid grid-cols-2 gap-3">
        <div class="flex items-center justify-between gap-2 rounded-cp bg-cp-fill-quaternary p-3 text-cp-xs text-cp-text-secondary">
          <span class="flex items-center gap-1.5"><Network class="size-3.5" />并发上限</span><strong class="font-mono">{{ budget.maxConcurrency || '∞' }}</strong>
        </div>
        <div class="flex items-center justify-between gap-2 rounded-cp bg-cp-fill-quaternary p-3 text-cp-xs text-cp-text-secondary">
          <span class="flex items-center gap-1.5"><Gauge class="size-3.5" />每分钟请求</span><strong class="font-mono">{{ budget.requestsPerMinute || '∞' }}</strong>
        </div>
      </div>
    </div>
  </BaseCard>
</template>
