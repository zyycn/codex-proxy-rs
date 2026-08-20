<script setup lang="ts">
import type { AccountQuotaWindow } from '../constants'

import { computed } from 'vue'
import {
  quotaWindowLocalUsageDisplay,
  quotaWindowPresentation,
} from './AccountUsageWindow/presenter'

const props = defineProps<{
  label: string | null
  windows: AccountQuotaWindow[]
}>()

const grouped = computed(() => props.windows.length > 1 && Boolean(props.label))
const items = computed(() => props.windows.map((window) => {
  const presentation = quotaWindowPresentation(window, '4px')

  return {
    key: window.key,
    label: grouped.value
      ? window.windowLabelDisplay
      : window.labelDisplay,
    ariaLabel: window.labelDisplay,
    usedPercent: window.usedPercent,
    usedPercentDisplay: window.usedPercentDisplay,
    localUsageDisplay: quotaWindowLocalUsageDisplay(window),
    resetAtDisplay: window.resetAtDisplay,
    percentTextClass: presentation.percentTextClass,
    barClass: presentation.barClass,
    barStyle: presentation.barStyle,
  }
}))
</script>

<template>
  <section class="grid min-w-0 gap-2">
    <h4
      v-if="grouped && label"
      class="m-0 truncate text-[11px] leading-4 font-heavy text-cp-secondary"
      :title="label"
    >
      {{ label }}
    </h4>

    <div
      class="grid min-w-0 gap-3"
      :class="grouped ? 'grid-cols-2' : 'grid-cols-1'"
    >
      <div v-for="item in items" :key="item.key" class="grid min-w-0 gap-1.5">
        <div class="flex min-w-0 items-baseline justify-between gap-2 text-[11px] leading-3.5">
          <span class="min-w-0 truncate font-bold text-cp-muted-text">
            {{ item.label }}
          </span>
          <span class="flex shrink-0 items-baseline gap-1.5 font-mono font-heavy tabular-nums">
            <strong
              v-if="item.localUsageDisplay"
              class="text-cp-muted-text"
              :title="`窗口消耗：${item.localUsageDisplay}`"
            >
              {{ item.localUsageDisplay }}
            </strong>
            <strong
              :class="item.percentTextClass"
              :title="`额度已使用：${item.usedPercentDisplay}`"
            >
              {{ item.usedPercentDisplay }}
            </strong>
          </span>
        </div>

        <div
          class="h-1.5 w-full overflow-hidden rounded-full bg-cp-default-border"
          role="progressbar"
          :aria-label="item.ariaLabel"
          aria-valuemin="0"
          aria-valuemax="100"
          :aria-valuenow="item.usedPercent ?? undefined"
          :aria-valuetext="item.usedPercentDisplay"
        >
          <div
            class="h-full rounded-full transition-[width,background-color] duration-200 motion-reduce:transition-none"
            :class="item.barClass"
            :style="item.barStyle"
          />
        </div>

        <p class="m-0 flex min-w-0 items-center justify-between gap-2 text-[10px] leading-3.5 text-cp-tertiary">
          <span class="shrink-0 font-emphasis">重置</span>
          <span class="min-w-0 truncate font-mono font-emphasis tabular-nums" :title="item.resetAtDisplay">
            {{ item.resetAtDisplay }}
          </span>
        </p>
      </div>
    </div>
  </section>
</template>
