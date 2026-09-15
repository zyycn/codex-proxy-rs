<script setup lang="ts">
import type { ClientBudgetWindow, ClientOverviewResponse } from '@/api'

import { Clock3, Gauge, Infinity as InfinityIcon, Waypoints } from '@lucide/vue'
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
  label: string
  budget: ClientBudgetWindow
  limited: boolean
  percentage: number
  progressSegments: { width: string }[]
  progressClass: string
  textClass: string
  statusLabel: string
  limitLabel: string
  remainingLabel: string
  resetLabel: string
}

const windows = computed<BudgetWindowView[]>(() => [
  budgetWindowView('今日', props.budget.daily, '每日 00:00 · 北京时间'),
  budgetWindowView('七日', props.budget.weekly, '首次使用后确定'),
])

const requestLimits = computed(() => [
  {
    icon: Waypoints,
    label: '并发上限',
    unlimited: props.limits.maxConcurrency === 0,
    value: props.limits.maxConcurrency > 0 ? formatInteger(props.limits.maxConcurrency) : '不限',
    unit: props.limits.maxConcurrency > 0 ? '路' : '',
  },
  {
    icon: Gauge,
    label: '每分钟请求',
    unlimited: props.limits.requestsPerMinute === 0,
    value: props.limits.requestsPerMinute > 0 ? formatInteger(props.limits.requestsPerMinute) : '不限',
    unit: props.limits.requestsPerMinute > 0 ? 'RPM' : '',
  },
])

function budgetWindowView(label: string, budget: ClientBudgetWindow, resetFallback: string): BudgetWindowView {
  const limit = numeric(budget.limitUsd)
  const used = numeric(budget.usedUsd)
  const limited = limit > 0
  const percentage = limited ? Math.max(0, used / limit * 100) : 0
  const tone = !limited
    ? { progressClass: '', textClass: 'text-cp-text-secondary', statusLabel: '' }
    : percentage >= 95
      ? { progressClass: 'bg-cp-error', textClass: 'text-cp-error-text', statusLabel: percentage >= 100 ? '已用尽' : '即将用尽' }
      : percentage >= 80
        ? { progressClass: 'bg-cp-warning', textClass: 'text-cp-warning-text', statusLabel: '额度偏低' }
        : { progressClass: 'bg-cp-success', textClass: 'text-cp-text', statusLabel: '' }

  return {
    label,
    budget,
    limited,
    percentage,
    // 每格表示 5%，不足一格的用量仍按实际比例填充。
    progressSegments: Array.from({ length: 20 }, (_, index) => ({
      width: `${Math.max(0, Math.min(100, (percentage / 5 - index) * 100))}%`,
    })),
    ...tone,
    limitLabel: limited ? formatUsd(budget.limitUsd) : '不限额',
    remainingLabel: !limited
      ? '不限额'
      : used >= limit
        ? formatUsd(0)
        : formatUsd(budget.remainingUsd ?? 0),
    resetLabel: budget.resetsAt
      ? `${formatDateTime(budget.resetsAt, '—', 'Asia/Shanghai')} · 北京时间`
      : resetFallback,
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
    title="额度概览"
    aria-label="额度概览"
    class="flex min-h-80 w-full flex-col"
  >
    <template #body>
      <div class="flex min-h-0 flex-1 flex-col">
        <div class="grid flex-1 grid-cols-1 gap-y-7 py-4 sm:grid-cols-2 sm:gap-x-4 sm:pt-3 sm:pb-6">
          <section
            v-for="item in windows"
            :key="item.label"
            class="flex min-w-0 flex-col sm:justify-between"
            :aria-label="`${item.label}额度`"
          >
            <div class="flex min-w-0 flex-wrap items-baseline gap-x-3 gap-y-1">
              <h3 class="m-0 text-cp font-emphasis text-cp-text-secondary">
                {{ item.label }}额度
              </h3>
              <span v-if="item.statusLabel" class="text-cp-xs" :class="item.textClass">
                {{ item.statusLabel }}
              </span>
            </div>

            <div class="mt-4 flex min-w-0 flex-wrap items-baseline gap-x-2 gap-y-1.5">
              <strong class="truncate font-mono text-[28px] leading-none font-bold tabular-nums" :class="item.textClass" :title="item.remainingLabel">
                {{ item.remainingLabel }}
              </strong>
              <span v-if="item.limited" class="text-cp-xs text-cp-text-quaternary">
                剩余 / <span class="font-mono tabular-nums">{{ item.limitLabel }}</span>
              </span>
            </div>

            <div class="mt-5">
              <div
                v-if="item.limited"
                class="grid h-5 grid-cols-20 gap-[3px]"
                role="progressbar"
                :aria-label="`${item.label}额度使用进度`"
                :aria-valuenow="Math.min(100, Number(item.percentage.toFixed(1)))"
                aria-valuemin="0"
                aria-valuemax="100"
                :aria-valuetext="`已用 ${item.percentage.toFixed(1)}%，剩余 ${item.remainingLabel}`"
              >
                <span
                  v-for="(segment, index) in item.progressSegments"
                  :key="index"
                  class="min-w-0 overflow-hidden rounded-xs bg-cp-fill-secondary"
                  aria-hidden="true"
                >
                  <span
                    class="block h-full transition-[width,background-color] duration-200 motion-reduce:transition-none"
                    :class="item.progressClass"
                    :style="segment"
                  />
                </span>
              </div>
              <div v-else class="h-5 text-cp-xs text-cp-text-quaternary">
                未设置金额上限
              </div>
              <p class="mt-2 mb-0 flex flex-wrap items-baseline justify-between gap-x-3 gap-y-1 text-cp-xs text-cp-text-quaternary">
                <span>已用 <span class="font-mono tabular-nums">{{ formatUsd(item.budget.usedUsd) }}</span></span>
                <span v-if="item.limited" class="font-mono tabular-nums">{{ item.percentage.toFixed(1) }}%</span>
              </p>
            </div>

            <div class="mt-6 text-cp-xs leading-relaxed text-cp-text-quaternary">
              <p class="m-0 flex items-center gap-1.5">
                <Clock3 class="size-3 shrink-0 -translate-y-px" aria-hidden="true" />
                重置时间
              </p>
              <p class="mt-1 mb-0 break-keep">
                {{ item.resetLabel }}
              </p>
            </div>
          </section>
        </div>

        <dl class="m-0 grid min-w-0 grid-cols-1 gap-2.5 sm:grid-cols-2 sm:gap-4" aria-label="调用限制">
          <div
            v-for="item in requestLimits"
            :key="item.label"
            class="flex min-w-0 items-center justify-between gap-3 rounded-cp-lg bg-cp-fill-alter/60 px-3.5 py-3"
          >
            <dt class="inline-flex min-w-0 items-center gap-2 text-cp-xs text-cp-text-tertiary">
              <component :is="item.icon" class="size-3.5 shrink-0 text-cp-text-quaternary" aria-hidden="true" />
              <span>{{ item.label }}</span>
            </dt>
            <dd class="m-0 flex min-w-0 items-baseline gap-1.5" :title="`${item.label}：${item.value}${item.unit ? ` ${item.unit}` : ''}`">
              <template v-if="item.unlimited">
                <InfinityIcon class="size-5 shrink-0 text-cp-text-secondary" :stroke-width="1.75" aria-hidden="true" />
                <span class="sr-only">不限</span>
              </template>
              <template v-else>
                <strong class="truncate font-mono text-cp font-emphasis tabular-nums text-cp-text-secondary">
                  {{ item.value }}
                </strong>
                <span v-if="item.unit" class="shrink-0 text-[10px] text-cp-text-quaternary">
                  {{ item.unit }}
                </span>
              </template>
            </dd>
          </div>
        </dl>
      </div>
    </template>
  </BaseCard>
</template>
