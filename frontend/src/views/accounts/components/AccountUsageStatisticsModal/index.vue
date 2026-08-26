<script setup lang="ts">
import type { AccountRow } from '../../constants'
import { ArrowLeft, ArrowRight, ChartNoAxesColumnIncreasing, TriangleAlert } from '@lucide/vue'
import { computed, toRef } from 'vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import { formatCompactNumber } from '@/utils/number'
import { useAccountUsageStatistics } from '../../composables/useAccountUsageStatistics'
import { formatUsageCost, formatUsagePercent } from '../../utils/accountUsageStatistics'
import AccountUsageStatisticsDailyTable from './DailyTable.vue'
import AccountUsageStatisticsModelTable from './ModelTable.vue'
import AccountUsageStatisticsSkeleton from './Skeleton.vue'

const props = defineProps<{
  account: AccountRow
}>()
const open = defineModel<boolean>({ required: true })
const accountId = toRef(() => props.account.id)
const {
  loading,
  error,
  view,
  load,
  previousCycle,
  nextCycle,
} = useAccountUsageStatistics(accountId, open)

const accountIdentity = computed(() =>
  props.account.email?.trim()
  || props.account.accountId?.trim()
  || props.account.name.trim()
  || props.account.id,
)
const title = computed(() => `用量统计 · ${accountIdentity.value}`)
const quotaRailStyle = computed(() => ({
  width: `${view.value?.cycle.usedPercent ?? 0}%`,
}))
const summaryLayoutClass = computed(() => view.value?.cycle.isCurrent
  ? 'sm:grid-cols-[minmax(0,1.35fr)_minmax(0,1fr)]'
  : undefined)
const summaryMetricsClass = computed(() => view.value?.cycle.isCurrent
  ? 'border-t border-cp-split pt-4 sm:border-t-0 sm:border-l sm:pt-0 sm:pl-5'
  : undefined)
const cycleLabel = computed(() => view.value?.cycle.isCurrent
  ? '本周期'
  : `前第 ${Math.abs(view.value?.cycle.offset ?? 0)} 个周期 · 推算`)
const usageNotice = '每日数据可能延迟 1–2 天；周期在一天中途重置时，边界日只能整日归入一个周期。历史周期边界为按当前窗口长度推算，金额不等同于账单。'
const cycleRange = computed(() => {
  const cycle = view.value?.cycle
  if (!cycle)
    return ''
  const formatter = new Intl.DateTimeFormat('zh-CN', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  })
  return `${formatter.format(new Date(cycle.startAt))} → ${formatter.format(new Date(cycle.endAt))}`
})

function formatProjectedTokens(value: number | null) {
  if (value === null)
    return '未知'
  return formatCompactNumber(value)
}
</script>

<template>
  <BaseModal
    v-model="open"
    :title="title"
    description="按官方额度周期查看模型、Token 与估算金额"
    size="xl"
  >
    <AccountUsageStatisticsSkeleton v-if="loading && !view" />

    <BaseEmpty
      v-else-if="error && !view"
      title="统计加载失败"
      :description="error"
      :icon="TriangleAlert"
      surface="none"
    >
      <template #action>
        <BaseButton size="sm" :loading="loading" @click="load(true)">
          重试
        </BaseButton>
      </template>
    </BaseEmpty>

    <div v-else-if="view" class="flex min-h-0 flex-col gap-5">
      <section class="grid shrink-0 gap-2 rounded-cp-lg bg-cp-bg-elevated px-3 py-2 sm:grid-cols-[1fr_auto_1fr] sm:items-center">
        <div class="flex items-center gap-1">
          <BaseIconButton
            label="查看上一个额度周期"
            size="sm"
            :disabled="loading || !view.cycle.canGoPrevious"
            @click="previousCycle"
          >
            <ArrowLeft class="size-3.5" />
          </BaseIconButton>
          <BaseIconButton
            label="查看下一个额度周期"
            size="sm"
            :disabled="loading || !view.cycle.canGoNext"
            @click="nextCycle"
          >
            <ArrowRight class="size-3.5" />
          </BaseIconButton>
        </div>

        <div class="flex min-w-0 flex-wrap items-center gap-1.5 text-left sm:justify-center">
          <span class="font-mono text-cp-sm font-bold tabular-nums text-cp-text">
            {{ cycleRange }}
          </span>
          <span class="rounded-full bg-cp-info-bg px-2 py-0.75 text-cp-xs font-bold text-cp-info-text">
            {{ cycleLabel }}
          </span>
        </div>

        <div
          class="flex min-w-0 items-center gap-1.5 text-cp-xs font-semibold text-cp-warning-text sm:justify-self-end"
          role="note"
          :aria-label="usageNotice"
          :title="usageNotice"
        >
          <TriangleAlert class="size-3.5 shrink-0" aria-hidden="true" />
          <span class="truncate">数据延迟 1–2 天，金额仅供估算</span>
        </div>
      </section>

      <section class="shrink-0 overflow-hidden rounded-cp-lg bg-cp-bg-elevated">
        <div class="grid gap-5 p-4 sm:p-5" :class="summaryLayoutClass">
          <div v-if="view.cycle.isCurrent" class="min-w-0">
            <div class="flex items-end justify-between gap-3">
              <div>
                <p class="m-0 text-cp-xs font-bold tracking-wide text-cp-text-quaternary uppercase">
                  当前窗口已用
                </p>
                <p class="mt-1 mb-0 font-mono text-3xl leading-none font-heavy tabular-nums text-cp-text">
                  {{ formatUsagePercent(view.cycle.usedPercent) }}
                </p>
              </div>
            </div>
            <div class="mt-4 h-1.5 overflow-hidden rounded-full bg-cp-fill-secondary" aria-hidden="true">
              <div
                class="h-full rounded-full bg-cp-info transition-[width] duration-300 motion-reduce:transition-none"
                :style="quotaRailStyle"
              />
            </div>
          </div>

          <div class="grid grid-cols-2 gap-x-4 gap-y-3" :class="summaryMetricsClass">
            <div>
              <p class="m-0 text-cp-xs font-semibold text-cp-text-quaternary">
                估算已用金额
              </p>
              <p class="mt-1 mb-0 font-mono text-cp-lg font-heavy tabular-nums text-cp-success-text">
                {{ formatUsageCost(view.summary.estimatedCost, view.summary.hasUnknownPricing) }}
              </p>
            </div>
            <div>
              <p class="m-0 text-cp-xs font-semibold text-cp-text-quaternary">
                {{ view.cycle.isCurrent ? '预计周期总额' : '该周期总额' }}
              </p>
              <p class="mt-1 mb-0 font-mono text-cp-lg font-heavy tabular-nums text-cp-text">
                {{ view.cycle.isCurrent
                  ? formatUsageCost(view.summary.projectedCost)
                  : formatUsageCost(view.summary.estimatedCost, view.summary.hasUnknownPricing) }}
              </p>
            </div>
            <div>
              <p class="m-0 text-cp-xs font-semibold text-cp-text-quaternary">
                {{ view.cycle.isCurrent ? '本周期 Tokens' : '该周期 Tokens' }}
              </p>
              <p class="mt-1 mb-0 font-mono text-cp-lg font-heavy tabular-nums text-cp-text">
                {{ formatCompactNumber(view.summary.tokens.total) }}
              </p>
            </div>
            <div>
              <p class="m-0 text-cp-xs font-semibold text-cp-text-quaternary">
                {{ view.cycle.isCurrent ? '预计周期 Tokens' : '历史周期不预测' }}
              </p>
              <p class="mt-1 mb-0 font-mono text-cp-lg font-heavy tabular-nums text-cp-text">
                {{ view.cycle.isCurrent ? formatProjectedTokens(view.summary.projectedTokens) : '—' }}
              </p>
            </div>
          </div>
        </div>

        <div class="grid grid-cols-2 border-t border-cp-split sm:grid-cols-4">
          <div class="px-4 py-3 sm:px-5">
            <p class="m-0 text-cp-xs font-semibold text-cp-text-quaternary">
              未缓存输入
            </p>
            <p class="mt-1 mb-0 font-mono font-heavy tabular-nums text-cp-text">
              {{ formatCompactNumber(view.summary.tokens.uncachedInput) }}
            </p>
          </div>
          <div class="border-l border-cp-split px-4 py-3 sm:px-5">
            <p class="m-0 text-cp-xs font-semibold text-cp-text-quaternary">
              缓存输入
            </p>
            <p class="mt-1 mb-0 font-mono font-heavy tabular-nums text-cp-text">
              {{ formatCompactNumber(view.summary.tokens.cachedInput) }}
            </p>
          </div>
          <div class="border-t border-cp-split px-4 py-3 sm:border-t-0 sm:border-l sm:px-5">
            <p class="m-0 text-cp-xs font-semibold text-cp-text-quaternary">
              输出 Tokens
            </p>
            <p class="mt-1 mb-0 font-mono font-heavy tabular-nums text-cp-text">
              {{ formatCompactNumber(view.summary.tokens.output) }}
            </p>
          </div>
          <div class="border-t border-l border-cp-split px-4 py-3 sm:border-t-0 sm:px-5">
            <p class="m-0 text-cp-xs font-semibold text-cp-text-quaternary">
              统计天数
            </p>
            <p class="mt-1 mb-0 font-mono font-heavy tabular-nums text-cp-text">
              {{ view.summary.dayCount }}
            </p>
          </div>
        </div>
      </section>

      <p
        v-if="view.summary.hasMissingTokenData"
        class="m-0 shrink-0 rounded-cp-lg bg-cp-warning-bg px-3.5 py-3 text-cp-sm leading-[1.55] font-semibold text-cp-warning-text"
      >
        当前周期部分日期暂无 Token 明细，相关 Token 与金额未计入统计。
      </p>

      <div class="flex min-h-56 flex-col gap-5">
        <AccountUsageStatisticsModelTable :rows="view.models" />

        <AccountUsageStatisticsDailyTable
          :rows="view.daily"
          :is-current-cycle="view.cycle.isCurrent"
          :used-percent="view.cycle.usedPercent"
        />
      </div>
    </div>

    <template #footer>
      <span v-if="error && view" class="mr-auto self-center text-cp-xs font-semibold text-cp-error-text">
        {{ error }}
      </span>
      <BaseButton variant="ghost" @click="open = false">
        关闭
      </BaseButton>
      <BaseButton :loading="loading" @click="load(true)">
        <template #icon>
          <ChartNoAxesColumnIncreasing class="size-4" />
        </template>
        重新统计
      </BaseButton>
    </template>
  </BaseModal>
</template>
