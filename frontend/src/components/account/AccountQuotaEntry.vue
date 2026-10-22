<script setup lang="ts">
import type { AccountQuotaWindow } from '@/api'

import { computed } from 'vue'
import AccountRequestTimeline from './account-usage-window/AccountRequestTimeline.vue'
import {
  quotaWindowLocalUsageDisplay,
  quotaWindowPresentation,
  resolveAccountUsageWindowPresentation,
} from './account-usage-window/presenter'

const props = defineProps<{
  label: string | null
  windows: AccountQuotaWindow[]
  remaining?: boolean
}>()

const grouped = computed(() => props.windows.length > 1 && Boolean(props.label))
const items = computed(() => props.windows.map((window) => {
  const presentation = quotaWindowPresentation(window, '4px')
  const view = resolveAccountUsageWindowPresentation({
    window,
    variant: 'detail',
    showLocalValue: true,
  })
  const percent = props.remaining && window.usedPercent !== null ? 100 - window.usedPercent : window.usedPercent
  const percentDisplay = props.remaining ? (window.remainingPercentDisplay ?? '—') : window.usedPercentDisplay

  return {
    key: window.key,
    local: view.mode === 'local',
    localLabel: view.local.label,
    requestDisplay: view.local.requestDisplay,
    requestBars: view.local.requestBars,
    timelineTitle: view.local.timelineTitle,
    durationDisplay: view.local.durationDisplay,
    label: grouped.value
      ? window.windowLabelDisplay
      : window.labelDisplay,
    ariaLabel: `${window.labelDisplay}${props.remaining ? '剩余' : '已使用'}`,
    percent,
    percentDisplay,
    localUsageDisplay: quotaWindowLocalUsageDisplay(window),
    resetAtDisplay: window.resetAtDisplay,
    percentTextClass: presentation.percentTextClass,
    barClass: presentation.barClass,
    barStyle: props.remaining ? { width: `${percent ?? 0}%` } : presentation.barStyle,
  }
}))
</script>

<template>
  <section class="grid min-w-0 gap-2">
    <h4
      v-if="grouped && label"
      class="m-0 truncate text-cp-xs leading-4 font-heavy text-cp-text-secondary"
      :title="label"
    >
      {{ label }}
    </h4>

    <div
      class="grid min-w-0 gap-3"
      :class="windows.length > 1 ? 'grid-cols-2' : 'grid-cols-1'"
    >
      <div v-for="item in items" :key="item.key" class="grid min-w-0 gap-1.5">
        <template v-if="item.local">
          <div class="flex min-w-0 items-baseline justify-between gap-2 text-cp-xs leading-3.5">
            <span class="min-w-0 truncate font-bold text-cp-text-quaternary">
              {{ item.localLabel }}
            </span>
            <strong
              class="shrink-0 font-mono font-heavy tabular-nums text-cp-text"
              :title="`窗口请求：${item.requestDisplay} 次`"
            >
              {{ item.requestDisplay }} 次
            </strong>
          </div>

          <AccountRequestTimeline
            class="h-1.5 w-full"
            :bars="item.requestBars"
            :label="item.timelineTitle"
          />

          <p class="m-0 flex min-w-0 items-center justify-between gap-2 text-[10px] leading-3.5 text-cp-text-tertiary">
            <span class="shrink-0 font-emphasis">统计范围</span>
            <span
              class="min-w-0 truncate font-mono font-emphasis tabular-nums"
              :title="`滚动 ${item.durationDisplay}`"
            >
              滚动 {{ item.durationDisplay }}
            </span>
          </p>
        </template>
        <template v-else>
          <div class="flex min-w-0 items-baseline justify-between gap-2 text-cp-xs leading-3.5">
            <span class="min-w-0 truncate font-bold text-cp-text-quaternary">
              {{ item.label }}
            </span>
            <span class="flex shrink-0 items-baseline gap-1.5 font-mono font-heavy tabular-nums">
              <strong
                v-if="item.localUsageDisplay"
                class="text-cp-text-quaternary"
                :title="`窗口消耗：${item.localUsageDisplay}`"
              >
                {{ item.localUsageDisplay }}
              </strong>
              <strong
                :class="item.percentTextClass"
                :title="`额度${remaining ? '剩余' : '已使用'}：${item.percentDisplay}`"
              >
                <template v-if="remaining">剩余 </template>{{ item.percentDisplay }}
              </strong>
            </span>
          </div>

          <div
            class="h-1.5 w-full overflow-hidden rounded-full bg-cp-border-secondary"
            role="progressbar"
            :aria-label="item.ariaLabel"
            aria-valuemin="0"
            aria-valuemax="100"
            :aria-valuenow="item.percent ?? undefined"
            :aria-valuetext="item.percentDisplay"
          >
            <div
              class="h-full rounded-full transition-[width,background-color] duration-200 motion-reduce:transition-none"
              :class="item.barClass"
              :style="item.barStyle"
            />
          </div>

          <p class="m-0 flex min-w-0 items-center justify-between gap-2 text-[10px] leading-3.5 text-cp-text-tertiary">
            <span class="shrink-0 font-emphasis">重置</span>
            <span class="min-w-0 truncate font-mono font-emphasis tabular-nums" :title="item.resetAtDisplay">
              {{ item.resetAtDisplay }}
            </span>
          </p>
        </template>
      </div>
    </div>
  </section>
</template>
