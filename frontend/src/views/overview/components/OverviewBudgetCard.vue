<script setup lang="ts">
import type { ClientBudgetWindow, ClientOverviewResponse } from '@/api'

import { Gauge, Waypoints } from '@lucide/vue'
import { computed } from 'vue'

import BaseCard from '@/components/base/BaseCard.vue'
import { formatDateTime } from '@/utils/date'
import { formatInteger } from '@/utils/number'

import { formatUsd } from '../model/key'

const props = defineProps<{
  budget: ClientOverviewResponse['budget']
  limits: ClientOverviewResponse['limits']
}>()

interface BudgetWindowView {
  code: string
  label: string
  budget: ClientBudgetWindow
  limited: boolean
  percentage: number
  progressStyle: { width: string }
  progressClass: string
  textClass: string
  limitLabel: string
  remainingLabel: string
  resetLabel: string
}

const windows = computed<BudgetWindowView[]>(() => [
  budgetWindowView('DAY', '当日额度', props.budget.daily),
  budgetWindowView('7D', '七日额度', props.budget.weekly),
])

const requestLimits = computed(() => [
  {
    icon: Waypoints,
    label: '并发上限',
    value: props.limits.maxConcurrency > 0 ? formatInteger(props.limits.maxConcurrency) : '不限',
    unit: props.limits.maxConcurrency > 0 ? '路' : '',
  },
  {
    icon: Gauge,
    label: '每分钟请求',
    value: props.limits.requestsPerMinute > 0 ? formatInteger(props.limits.requestsPerMinute) : '不限',
    unit: props.limits.requestsPerMinute > 0 ? 'RPM' : '',
  },
])

function budgetWindowView(code: string, label: string, budget: ClientBudgetWindow): BudgetWindowView {
  const limit = numeric(budget.limitUsd)
  const used = numeric(budget.usedUsd)
  const limited = limit > 0
  const percentage = limited ? Math.max(0, used / limit * 100) : 0
  const tone = !limited
    ? { progressClass: '', textClass: 'text-cp-text-secondary' }
    : percentage >= 95
      ? { progressClass: 'bg-cp-error', textClass: 'text-cp-error-text' }
      : percentage >= 80
        ? { progressClass: 'bg-cp-warning', textClass: 'text-cp-warning-text' }
        : { progressClass: 'bg-cp-success', textClass: 'text-cp-success-text' }

  return {
    code,
    label,
    budget,
    limited,
    percentage,
    progressStyle: { width: `${Math.min(100, percentage)}%` },
    ...tone,
    limitLabel: limited ? formatUsd(budget.limitUsd) : '不限额',
    remainingLabel: !limited
      ? '无上限'
      : used >= limit
        ? '$0.00'
        : formatUsd(budget.remainingUsd ?? 0),
    resetLabel: budget.resetsAt
      ? `重置于 ${formatDateTime(budget.resetsAt, '—', 'Asia/Shanghai')}`
      : '重置时间将在使用后确定',
  }
}

function numeric(value: string) {
  const parsed = Number.parseFloat(value)
  return Number.isFinite(parsed) ? parsed : 0
}
</script>

<template>
  <BaseCard
    as="article"
    title="限额进度"
    class="flex min-h-98 w-full flex-col"
  >
    <template #body>
      <div class="grid min-h-0 flex-1 grid-rows-[minmax(0,1fr)_minmax(0,1fr)_auto] gap-3">
        <section
          v-for="item in windows"
          :key="item.label"
          class="flex min-h-0 min-w-0 flex-col justify-center"
        >
          <div class="flex min-w-0 items-end justify-between gap-5">
            <div class="flex min-w-0 items-center">
              <span class="inline-flex h-6 min-w-9 items-center justify-center rounded-cp-sm bg-cp-fill-tertiary px-2 font-mono text-[10px] leading-none font-heavy tracking-[0.08em] text-cp-text-secondary">
                {{ item.code }}
              </span>
            </div>
            <div class="flex min-w-0 items-baseline gap-2">
              <span class="shrink-0 text-cp-xs font-emphasis text-cp-text-quaternary">已用</span>
              <strong class="truncate font-mono text-xl leading-none font-heavy tabular-nums text-cp-text" :title="formatUsd(item.budget.usedUsd)">
                {{ formatUsd(item.budget.usedUsd) }}
              </strong>
              <strong class="font-mono text-cp-base leading-none font-heavy tabular-nums" :class="item.textClass">
                {{ item.limited ? `${item.percentage.toFixed(1)}%` : '∞' }}
              </strong>
              <span v-if="!item.limited" class="text-[10px] font-emphasis text-cp-text-quaternary">
                不限额
              </span>
            </div>
          </div>

          <div
            class="budget-track relative mt-3 h-2.5 overflow-hidden rounded-full bg-cp-fill-secondary"
            role="progressbar"
            :aria-label="`${item.label}使用进度`"
            :aria-valuenow="item.limited ? Math.min(100, Math.round(item.percentage)) : undefined"
            aria-valuemin="0"
            :aria-valuemax="item.limited ? 100 : undefined"
            :aria-valuetext="item.limited ? `${item.percentage.toFixed(1)}%，剩余 ${item.remainingLabel}` : `已使用 ${formatUsd(item.budget.usedUsd)}，不限额`"
          >
            <span
              v-if="item.limited"
              class="block h-full min-w-0.75 rounded-full transition-[width,background-color] duration-200 motion-reduce:transition-none"
              :class="item.progressClass"
              :style="item.progressStyle"
            />
            <span
              v-else
              class="block h-full w-full bg-[repeating-linear-gradient(115deg,color-mix(in_srgb,var(--cp-color-primary)_38%,transparent)_0_8px,color-mix(in_srgb,var(--cp-color-primary)_9%,transparent)_8px_15px)]"
            />
          </div>

          <div class="mt-2.5 flex min-w-0 flex-wrap items-center justify-between gap-x-4 gap-y-1 text-cp-xs font-emphasis">
            <span class="text-cp-text-secondary">
              剩余
              <strong class="ml-1 font-mono font-heavy tabular-nums" :class="item.textClass">{{ item.remainingLabel }}</strong>
            </span>
            <span class="text-cp-text-quaternary">
              上限 <span class="font-mono tabular-nums">{{ item.limitLabel }}</span>
              · {{ item.resetLabel }}
            </span>
          </div>
        </section>

        <dl class="m-0 grid min-w-0 grid-cols-2 gap-3" aria-label="调用限制">
          <div
            v-for="item in requestLimits"
            :key="item.label"
            class="flex min-w-0 items-center gap-3.5 rounded-cp-lg bg-cp-fill-alter/30 px-4 py-3.5"
          >
            <span class="inline-flex size-9 shrink-0 items-center justify-center rounded-cp bg-cp-fill-tertiary/80 text-cp-text-tertiary" aria-hidden="true">
              <component :is="item.icon" class="size-4.5" />
            </span>
            <div class="min-w-0">
              <dt class="truncate text-cp-xs leading-none font-emphasis text-cp-text-quaternary">
                {{ item.label }}
              </dt>
              <dd class="mt-1.5 mb-0 flex min-w-0 items-baseline gap-1.5">
                <strong class="truncate font-mono text-cp-base leading-none font-bold tabular-nums text-cp-text-secondary">
                  {{ item.value }}
                </strong>
                <span v-if="item.unit" class="shrink-0 text-[10px] font-emphasis text-cp-text-quaternary">
                  {{ item.unit }}
                </span>
              </dd>
            </div>
          </div>
        </dl>
      </div>
    </template>
  </BaseCard>
</template>

<style scoped>
.budget-track::after {
  position: absolute;
  inset: 0;
  pointer-events: none;
  content: '';
  background-image: repeating-linear-gradient(
    90deg,
    transparent 0,
    transparent calc(10% - 1px),
    color-mix(in srgb, var(--cp-color-text) 10%, transparent) calc(10% - 1px),
    color-mix(in srgb, var(--cp-color-text) 10%, transparent) 10%
  );
}
</style>
